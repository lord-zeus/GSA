//! A single job: claim -> resolve spec -> resolve plan -> drive executor
//! -> persist result. The worker pool invokes `JobRunner::run_one`
//! inside each `tokio::task`.

use std::collections::HashMap;

use engine::driver::Driver;
use engine::email_exec::{EmailContext, run_email_engine};
use engine::executor::{execute_plan, resolve_plan, ExecutionReport, SubmissionInput};
use engine::proxy::ProxySpec;
use engine::routing::DriverKind;
use serde::Serialize;
use uuid::Uuid;

use crate::config::RunnerConfig;
use crate::specprovider::SpecSource;

/// Context shared across all workers in one pool run.
#[derive(Debug, Clone, Default)]
pub struct SubmissionContext {
    /// The project's target URL (where the backlink should point).
    pub project_url: String,
    /// The keyword (empty string = none).
    pub keyword: String,
    /// Spintax data files, `name -> body`.
    pub files: HashMap<String, String>,
    /// Optional spintax seed; `None` = deterministic default.
    pub seed: Option<u64>,
    /// Email-family only: the operator's receiving mailbox (GSA's
    /// `pop3 login=%email%` / inbox-parse URL target). Empty for pure
    /// web runs.
    pub email_address: String,
    /// Email-family only: the operator's mailbox password
    /// (`%password%` in `pop3 password`). Empty for pure web runs.
    pub email_password: String,
    /// F#13 — the enabled proxy list for this project (GSA "use proxies").
    /// Empty = direct connection (previous behavior). When non-empty, each
    /// job picks a proxy by deterministic round-robin (`seed_for_job`)
    /// and the attempt is retried on the next proxy if the failure looks
    /// transport-level ("down proxy → skip to next", the original's
    /// behavior). Driver kinds without a `proxy_driver` impl fall back
    /// to a direct connection transparently.
    pub proxies: Vec<ProxySpec>,
    /// F#15 — the effective (clamped) settings for this project's engines.
    /// The pool reads `threads` and `max_submit_seconds` from here; the
    /// API layer fills it from `Repo::global_settings()` + engine override
    /// (global settings only — per-keyword / per-engine overrides are
    /// handled by the *engine* executor, not the pool).
    pub settings: std::sync::Arc<engine::settings::Settings>,
}

/// The terminal state of one job's execution.
#[derive(Debug, Clone, Serialize)]
pub struct JobOutcome {
    pub job_id: Uuid,
    pub status: &'static str,
    pub report: ExecutionReport,
    /// A driver-level error, if the executor never ran.
    pub driver_error: Option<String>,
}

impl JobOutcome {
    pub fn successful(&self) -> bool {
        self.report.successful > 0
    }
    pub fn is_done(&self) -> bool {
        self.status == "done"
    }
    /// Result kind for the `results` table.
    pub fn result_kind(&self) -> ser_db::ResultKind {
        match self {
            _ if self.successful() => ser_db::ResultKind::Success,
            _ if self.report.captcha_failed > 0 => ser_db::ResultKind::Rejected,
            _ => ser_db::ResultKind::Error,
        }
    }
}

/// A registry of pre-built [`Driver`] impls, keyed by [`DriverKind`].
#[derive(Clone, Default)]
pub struct DriverRegistry {
    by_kind: HashMap<DriverKind, std::sync::Arc<dyn Driver>>,
}

impl std::fmt::Debug for DriverRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DriverRegistry")
            .field("has_http", &self.by_kind.contains_key(&DriverKind::Http))
            .field("has_browser", &self.by_kind.contains_key(&DriverKind::Browser))
            .finish()
    }
}

impl DriverRegistry {
    pub fn new() -> Self {
        Self { by_kind: HashMap::new() }
    }
    /// Register a driver for `Http`-routed engines.
    pub fn with_http(mut self, d: impl Driver + 'static) -> Self {
        self.by_kind.insert(DriverKind::Http, std::sync::Arc::new(d));
        self
    }
    /// Register a driver for `Browser`-routed engines.
    pub fn with_browser(mut self, d: impl Driver + 'static) -> Self {
        self.by_kind.insert(DriverKind::Browser, std::sync::Arc::new(d));
        self
    }
    /// Register a driver for `Email`-routed (F#12) engines. Used for the
    /// WEB-family `pop3 server=WEB` inbox fetch and for the final
    /// verification visit of both WEB and POP3 mailbox families.
    pub fn with_email(mut self, d: impl Driver + 'static) -> Self {
        self.by_kind.insert(DriverKind::Email, std::sync::Arc::new(d));
        self
    }
    /// Direct insertion — for feature-gated tests that can't name the
    /// `HttpDriver` or `WebDriver` type directly.
    pub fn insert(&mut self, kind: DriverKind, d: std::sync::Arc<dyn Driver>) {
        self.by_kind.insert(kind, d);
    }
    pub fn get(&self, kind: DriverKind) -> Option<std::sync::Arc<dyn Driver>> {
        self.by_kind.get(&kind).cloned()
    }
}

/// One engine run, end-to-end. `Clone` + `Send + Sync` so the pool can
/// share one runner across N workers.
#[derive(Clone)]
pub struct JobRunner {
    cfg: RunnerConfig,
    spec: SpecSource,
    drivers: std::sync::Arc<DriverRegistry>,
    ctx: SubmissionContext,
}

impl JobRunner {
    pub fn new(
        cfg: RunnerConfig,
        spec: SpecSource,
        drivers: std::sync::Arc<DriverRegistry>,
        ctx: SubmissionContext,
    ) -> Self {
        Self { cfg, spec, drivers, ctx }
    }

    /// Execute one already-claimed job. Returns the terminal outcome.
    /// All failure modes are folded into the returned `JobOutcome`; a
    /// panic here is the pool's problem.
    ///
    /// `Send` — safe to move across the task boundary.
    /// F#15 — per-job timeout from the effective settings
    /// (`max_submit_seconds`, clamped to 1..=3600 s). Falls back to
    /// `RunnerConfig::per_job_timeout` for tests that construct a context
    /// with `settings` left at its struct Default (an empty `Settings`).
    fn job_timeout(&self) -> std::time::Duration {
        match self.ctx.settings.max_submit_seconds {
            Some(secs) if secs > 0 => std::time::Duration::from_secs((secs.min(3600).max(1)) as u64),
            _ => self.cfg.per_job_timeout,
        }
    }

    pub async fn run_one(&self, repo: &ser_db::Repo, job: &ser_db::Job) -> JobOutcome {
        let (engine_id, file_path) = match self
            .resolve_engine(repo, job)
            .await
            .map_err(|e| e.to_string())
        {
            Ok(p) => p,
            Err(msg) => return self.finish(repo, job.id, false, Some(msg), empty_report(), String::new(), None).await,
        };

        // 1b) keyword — job-level keyword_id wins over the pool context.
        //     A spintax keyword is spun here, once, to a concrete variant
        //     (deterministic per job id).
        let keyword = self.resolve_keyword(repo, job).await;

        // 2) parsed spec
        let engine = match self.spec.resolve(engine_id, &file_path) {
            Some(e) => e,
            None => {
                let msg = format!("spec missing: {file_path}");
                                return self.finish(repo, job.id, false, Some(msg), empty_report(), String::new(), None).await;
            }
        };

        // 3) plan
        let input = SubmissionInput {
            url: self.ctx.project_url.clone(),
            keyword: keyword.clone(),
            anchor: None,
            seed: self.ctx.seed,
            extra: Default::default(),
            files: self.ctx.files.clone(),
            random_options: vec![],
        };
        let plan = resolve_plan(&engine, &input);

        // 3b) F#12 — mail-engine family (real POP3 mailbox + `pop3
        //     server=WEB` inbox-parse). These specs are `[setup]`-only:
        //     `plan.steps` is empty, so the generic step-loop executor
        //     would no-op. Dispatch to the dedicated email pipeline
        //     instead — fetch the inbox (real POP3 socket or site-over-
        //     HTTP), extract the submission link via the `url_front*` /
        //     `url_back*` ladder, then make the verification visit through
        //     the registered Driver.
        //
        //     The mailbox + password come from the pool context (GSA's
        //     operator-level "your e-mail" setting — one inbox across the
        //     whole catalog).
        if plan.required_driver == DriverKind::Email {
            let d = match self.drivers.get(DriverKind::Email) {
                Some(d) => d,
                None => {
                    let msg = "driver not registered for Email (F#12)".into();
                    return self.finish(repo, job.id, false, Some(msg), empty_report(), keyword, None).await;
                }
            };
            let mail_engine = engine.setup_to_email();
            // Operator mailbox: the pool context is authoritative when set
            // (e.g. test harnesses / per-run override). Otherwise fall back
            // to the stored GSA operator ("your e-mail") config so a real
            // API-driven run is not required to pass it in-band.
            let (addr, pw) = match (self.ctx.email_address.clone(), self.ctx.email_password.clone()) {
                (a, p) if !a.is_empty() => (a, p),
                _ => repo.email_mailbox().await.unwrap_or_default(),
            };
            let mail_ctx = EmailContext {
                email_address: addr,
                password: pw,
            };
            let timeout = self.job_timeout();
            // run_email_engine is sync — it spawns a one-shot runtime per
            // Driver call. Drive it from `spawn_blocking`, wrapped in the
            // per-job timeout, exactly the way the pool drives other sync
            // work.
            //
            // F#13 — email submissions (the verification visit inside
            // `run_email_engine`) route through the same proxy-aware runner:
            // `d` is replaced by a proxy-driver instance per attempt when the
            // pool carries proxies, with one re-roll on the next proxy in
            // the list if the failure looks transport-level ("proxy down").
            // Driver kinds without a `proxy_driver` impl (e.g. a future
            // browser bridge) keep using the direct driver transparently.
            let n = self.ctx.proxies.len();
            let seed = self.seed_for_job(job).unwrap_or(0xA11CE_5EED | 1);
            let mut attempt_idx: Option<usize> = if n > 0 { Some((seed as usize) % n) } else { None };
            let mut retried_down_proxy = false;
            let (mut join_outcome, mut join_err) = (r_email_default(), None);
            loop {
                let drv: std::sync::Arc<dyn Driver> = match attempt_idx {
                    Some(i) => d.proxy_driver(&self.ctx.proxies[i]).unwrap_or_else(|| d.clone()),
                    None => d.clone(),
                };
                let (r, e) = {
                    let engine = mail_engine.clone();
                    match tokio::time::timeout(
                        timeout,
                        tokio::task::spawn_blocking({
                            let (engine, ctx) = (engine, mail_ctx.clone());
                            let d2 = std::sync::Arc::clone(&drv);
                            move || run_email_engine(&engine, &ctx, &d2, timeout)
                        }),
                    )
                    .await
                    {
                        Ok(Ok(r)) => (r, None),
                        Ok(Err(join_err)) => {
                            (
                                r_email_default(),
                                Some(format!("email worker panicked: {join_err}")),
                            )
                        }
                        Err(_) => {
                            (
                                r_email_default(),
                                Some(format!("email job timed out after {timeout:?}")),
                            )
                        }
                    }
                };
                let down_proxy_retriable = attempt_idx.is_some()
                    && n > 1
                    && !retried_down_proxy
                    && r.report.successful == 0
                    && looks_transport_failure(&r);
                if down_proxy_retriable {
                    retried_down_proxy = true;
                    attempt_idx = Some((attempt_idx.unwrap() + 1) % n);
                    continue; // next loop iteration runs on the next proxy
                }
                join_outcome = r;
                join_err = e;
                break;
            }
            // F#13: record the proxy used in the result payload.
            let proxy_used: Option<String> = attempt_idx.map(|i| self.ctx.proxies[i].url());
            let done = join_outcome.report.successful > 0;
            let done_err = if done {
                None
            } else if let Some(e) = join_err {
                if join_outcome.report.errors.is_empty() {
                    Some(e)
                } else {
                    Some(format!("{}; {}", e, join_outcome.report.errors.join("; ")))
                }
            } else if join_outcome.report.errors.is_empty() {
                Some("no submission link found in inbox".into())
            } else {
                Some(join_outcome.report.errors.join("; "))
            };
            return self.finish(repo, job.id, done, done_err, join_outcome.report, keyword, proxy_used.as_deref()).await;
        }

        // 4) driver by kind
        let driver = match self.drivers.get(plan.required_driver) {
            Some(d) => d,
            None => {
                let msg = format!("driver not registered for kind {:?}", plan.required_driver);
                                return self.finish(repo, job.id, false, Some(msg), empty_report(), String::new(), None).await;
            }
        };

        // 5) execute with a per-job timeout.
        //
        // F#13 (GSA "use proxies" / `proxy list` / "proxy seems down"):
        // when the pool context carries enabled proxies, the job routes
        // through ONE proxy per attempt — picked deterministically from the
        // per-job seed so the distribution round-robins across the list
        // (and repeats identically on reruns). A failed attempt whose
        // errors look transport-level (connection refused / proxy connect /
        // reset / DNS) is what GSA calls "proxy seems down": we then retry
        // ONCE on the next proxy in the list before failing the job.
        // Driver kinds without proxy support (browser bridge) transparently
        // keep using the direct driver.
        let timeout = self.job_timeout();
        let seed = self.seed_for_job(job).unwrap_or(0xA11CE_5EED | 1);
        let n = self.ctx.proxies.len();
        let mut attempt_idx: Option<usize> = if n > 0 { Some((seed as usize) % n) } else { None };
        let mut retried_down_proxy = false;
        let mut report = loop {
            let drv: std::sync::Arc<dyn Driver> = match attempt_idx {
                Some(i) => {
                    let spec = &self.ctx.proxies[i];
                    driver
                        .proxy_driver(spec)
                        .unwrap_or_else(|| std::sync::Arc::clone(&driver))
                }
                None => std::sync::Arc::clone(&driver),
            };
            let used_idx = attempt_idx;
            let r: engine::executor::ExecutionReport =
                match tokio::time::timeout(timeout, execute_plan(&plan, drv.as_ref())).await {
                    Ok(r) => r,
                    Err(_) => {
                        // Transport-level: the proxy/socket is what broke —
                        // a timeout is the same "proxy seems down" class of
                        // failure as a connection reset.
                        empty_report_with_err(format!("job timed out after {timeout:?}"))
                    }
                };
            let down_proxy_retriable = used_idx.is_some()
                && n > 1
                && !retried_down_proxy
                && r.successful == 0
                && looks_transport_failure(&r);
            if down_proxy_retriable {
                retried_down_proxy = true;
                attempt_idx = Some((used_idx.unwrap() + 1) % n);
                continue; // next loop iteration runs on the next proxy
            }
            break r;
        };
        let proxy_used = attempt_idx.map(|i| self.ctx.proxies[i].url());

        // 6) persist.
        let done = report.successful > 0;
        let err = if done {
            None
        } else if report.errors.is_empty() {
            Some("no step matched submit success".into())
        } else {
            Some(report.errors.join("; "))
        };
        self.finish(repo, job.id, done, err, report, keyword.clone(), proxy_used.as_deref()).await
    }

    /// Resolve the effective keyword for this job.
    ///
    /// Priority: job-level `keyword_id` (spun if spintax) → pool context
    /// keyword → empty.  Spinning is deterministic in the job id so the
    /// same job always spins to the same variant.
    async fn resolve_keyword(&self, repo: &ser_db::Repo, job: &ser_db::Job) -> String {
        if let Some(kid) = job.keyword_id {
            match repo.keyword_by_id(kid).await {
                Ok(Some(kw)) => {
                    let seed = self.seed_for_job(job);
                    return engine::resolve_keyword_one(&kw.text, seed);
                }
                Ok(None) => {
                    // keyword row was deleted — fall through to context
                }
                Err(e) => {
                    eprintln!("runner: keyword lookup {kid} failed: {e}");
                }
            }
        }
        if !self.ctx.keyword.is_empty() {
            return engine::resolve_keyword_one(&self.ctx.keyword, self.seed_for_job(job));
        }
        String::new()
    }

    /// Deterministic per-job seed (so one job = one stable variant).
    fn seed_for_job(&self, job: &ser_db::Job) -> Option<u64> {
        let n = job.id.as_u64_pair();
        let mixed = (n.0 ^ n.1.rotate_left(23)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        // 0xA11CE_5EED|1 is SpintaxResolver::default()'s seed.
        Some(self.ctx.seed.unwrap_or(0xA11CE_5EED | 1) ^ mixed)
    }

    /// Write the terminal status + optional success result row, then
    /// return the `JobOutcome`. F#13: `proxy_used` records which proxy
    /// (`spec.url()`) the successful attempt egressed through, so the
    /// results table can attribute the row to its proxy (GSA's
    /// `proxy stats` vocabulary).
    async fn finish(
        &self,
        repo: &ser_db::Repo,
        job_id: Uuid,
        done: bool,
        err: Option<String>,
        report: ExecutionReport,
        keyword: String,
        proxy_used: Option<&str>,
    ) -> JobOutcome {
        let status = if done { ser_db::JobStatus::Done } else { ser_db::JobStatus::Failed };
        let _ = repo
            .finish_job(job_id, status, err.as_deref())
            .await;

        let mut payload = serde_json::json!({ "engine": "runner" });
        if let Some(p) = proxy_used {
            payload["proxy"] = serde_json::json!(p);
        }
        if let Some(e) = err.as_ref() {
            payload["error"] = serde_json::json!(e);
        }
        payload["steps"] = serde_json::json!({
            "successful": report.successful,
            "failed": report.failed,
            "retried": report.retried,
            "captcha_failed": report.captcha_failed,
        });

        // F#16 — record the OUTCOME row whether the job succeeded or not.
        // GSA's stats page wants success/failure/**captcha** ratios, and a job
        // blocked by a captcha wall used to leave NO results row at all.
        // Success still takes precedence for the `kind` (matching result_kind).
        let success = report.successful > 0;
        let kind = if success {
            ser_db::ResultKind::Success
        } else if report.captcha_failed > 0 {
            // captcha wall — its own bucket in the GSA UI
            ser_db::ResultKind::Rejected
        } else if err.is_some() {
            ser_db::ResultKind::Error
        } else {
            ser_db::ResultKind::Error
        };
        let url = report.submitted_url.clone();
        let message = if success {
            None
        } else {
            err.clone().or_else(|| match (report.captcha_failed > 0, report.failed > 0) {
                (true, _) => Some("blocked by captcha wall".to_string()),
                (_, true) => Some("submission failed".to_string()),
                (false, false) => Some("no submission link found".to_string()),
            })
        };
        let captchas = report.captcha_failed > 0;
        let _ = repo
            .record_result(
                job_id,
                kind,
                url.as_deref(),
                Some(&keyword),
                message.as_deref(),
                Some(payload),
                captchas,
            )
            .await;

        JobOutcome {
            job_id,
            status: if done { "done" } else { "failed" },
            report,
            driver_error: err,
        }
    }

    async fn resolve_engine<'a>(
        &self,
        repo: &'a ser_db::Repo,
        job: &ser_db::Job,
    ) -> anyhow::Result<(Uuid, String)> {
        let engine_id = job
            .engine_id
            .ok_or_else(|| anyhow::anyhow!("job has no engine_id"))?;
        let row = match repo.engine_by_id(engine_id).await? {
            Some(r) => r,
            None => anyhow::bail!("engine row {engine_id} not found"),
        };
        Ok((engine_id, row.file_path))
    }
}

fn empty_report() -> ExecutionReport {
    ExecutionReport {
        successful: 0,
        failed: 0,
        retried: 0,
        captcha_failed: 0,
        submitted_url: None,
        submitted_url_source: None,
        errors: vec![],
        steps: vec![],
    }
}

/// Same as [`empty_report`] but carries a single error message — used for
/// the timeout fallback in the execution loop (a timeout is the same
/// "proxy seems down" class as a connection reset for re-roll purposes).
fn empty_report_with_err(msg: String) -> ExecutionReport {
    let mut r = empty_report();
    r.errors.push(msg);
    r
}

/// A zeroed `email_exec::EmailRunResult` for the F#12 branch's timeout /
/// panic fallbacks (where `run_email_engine` never returned one).
fn r_email_default() -> engine::email_exec::EmailRunResult {
    engine::email_exec::EmailRunResult {
        report: empty_report(),
        messages: vec![],
        candidates: vec![],
    }
}

/// F#13 — anything carrying the executor's error vector (`ExecutionReport`
/// and the email branch's `EmailRunResult` both expose one) so the
/// "transport failure?" check is shared by both execution paths.
trait TransportFailureSource {
    fn transport_errors(&self) -> &[String];
    fn succeeded(&self) -> bool;
}
impl TransportFailureSource for engine::executor::ExecutionReport {
    fn transport_errors(&self) -> &[String] {
        &self.errors
    }
    fn succeeded(&self) -> bool {
        self.successful > 0
    }
}
impl TransportFailureSource for engine::email_exec::EmailRunResult {
    fn transport_errors(&self) -> &[String] {
        &self.report.errors
    }
    fn succeeded(&self) -> bool {
        self.report.successful > 0
    }
}

/// Is this failure *transport-level* (the proxy/connection is the problem)
/// rather than application-level (a 4xx/5xx, a parse miss, a form error)?
/// This is what GSA calls "proxy seems down": the proxy socket is what
/// broke, not the target.  Only such failures should trigger a re-roll to
/// the next proxy in rotation; a clean `4xx` is not proxy-attributable and
/// would just be retried pointlessly.
fn looks_transport_failure<T: TransportFailureSource>(r: &T) -> bool {
    if r.succeeded() {
        return false;
    }
    const TRANSPORT: &[&str] = &[
        "connection refused",
        "connect: ",
        "connection reset",
        "connection closed",
        "timed out",
        "timeout",
        "dns",
        "name resolution",
        "proxy",
        "socks5",
        "socket",
        "eof",
        "stream",
        "reset by peer",
        "tls",
    ];
    for e in r.transport_errors() {
        let lc = e.to_ascii_lowercase();
        for needle in TRANSPORT {
            if lc.contains(needle) {
                return true;
            }
        }
    }
    false
}
