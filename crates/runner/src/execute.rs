//! A single job: claim -> resolve spec -> resolve plan -> drive executor
//! -> persist result. The worker pool invokes `JobRunner::run_one`
//! inside each `tokio::task`.

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;

use engine::driver::Driver;
use engine::email::{matches_address};
use engine::email_create::load_email_engines;
use engine::email_exec::{build_source, EmailContext, run_email_engine};
use engine::executor::{resolve_plan, ExecutionReport, StepPhase, SubmissionInput};
use engine::proxy::ProxySpec;
use engine::{engine_uses_email_verify};
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
    /// The project id — used only for GSA-style auto-export file naming
    /// (`<data>/verified/<project_id>.csv`); does not affect submission.
    pub project_id: Uuid,
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
    /// F#19 — anchor-text mix (GSA "Anchor Text" block). `None` = submit the
    /// main keyword as anchor for every job (previous behavior). `Some` =
    /// a weighted pool the runner draws from per job (deterministic in the
    /// per-job seed) and feeds in via `%anchor_text%` / `%banchor_text%` /
    /// `%panchor_text%` / `%lanchor_text%` / `%url%`.
    pub anchor: Option<engine::anchor::PoolConfig>,
    /// F#19 — URL options (GSA "URL options" block).
    pub url_options: engine::anchor::UrlOptions,
    /// F#19 — Multi-Tier / linkwheel: upstream project ids whose *verified
    /// success URLs* this project may use as its submission target (one per
    /// job, deterministic per job). Empty = no linkwheel (target is the
    /// project's own URL).
    pub linkwheel_project_ids: Vec<Uuid>,
    /// F#24-lite — GSA "Automatically export verified URLs": when `true`,
    /// every **successful** submission appends a GSA-schema CSV row to
    /// `<data_dir>/verified/<project_id>.csv` (the "link list" feed that
    /// downstream tier projects import). Default `false` to keep old-test
    /// behavior; set per project from `projects.auto_export`.
    pub auto_export: bool,
    /// F#26 — Test Button (GSA "Test" per engine): when `true`, the runner
    /// drives the engine end-to-end (spec → plan → executor → captcha) but
    /// **persists nothing** — no job status flip, no `results` row, no
    /// auto-export CSV append. Used by the UI "Test engine" dry-run so a
    /// single submission can be observed without polluting a real run's data.
    pub dry_run: bool,
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

        // 3) plan.
        //
        // F#19 — Anchor-text rotation: when the project carries an anchor mix
        // (i.e. at least one non-keyword anchor type with weight > 0), draw one
        // concrete anchor per job (deterministic in the per-job seed) and
        // substitute it into the engine spec via `%anchor_text%`.  When the
        // mix is `None` (previous behavior) the anchor stays `None` and GSA's
        // keyword-fallback path in the executor kicks in.
        let anchor_draw = if let Some(cfg) = &self.ctx.anchor {
            if cfg.is_empty() {
                None
            } else {
                let domain = engine::anchor::domain_of(&self.ctx.project_url);
                let spin_ctx = engine::spin::Context {
                    values: std::collections::HashMap::new(),
                    files: self.ctx.files.clone(),
                    random_options: vec![],
                };
                let seed = self.seed_for_job(job).unwrap_or(0xA11CE_5EED | 1);
                Some(engine::anchor::draw_anchor(cfg, &keyword, &domain, Some(seed), &spin_ctx))
            }
        } else {
            None
        };
        let input_anchor = anchor_draw.as_ref().map(|d| d.text.clone());

        // F#20 — URL options applied to the target URL before it reaches the
        // engine spec (which resolves it into `%url%` form fields)
        //
        //   * `linkwheel_project_ids` non-empty → pick a verified URL from an
        //     upstream project (GSA "Multi-Tier/Linkwheel" — the target URL is
        //     the upstream's already-created backlink, not our own site).
        //   * `root_url` > 0 → with that probability, use the bare host URL
        //     (strip the path), e.g. `http://domain.de/link.html` →
        //     `http://domain.de/`.
        //   * `url_variations` > 0 → with that probability, emit a scheme/www/
        //     case variant of the URL.
        //
        // The linkwheel pick is deterministic in the job id so a rerun of the
        // same job always picks the same upstream URL (stable + testable).
        let effective_url = {
            let base = self.ctx.project_url.clone();
            // 1) Linkwheel: replace the target with an upstream verified URL.
            let base = if !self.ctx.linkwheel_project_ids.is_empty() {
                // Deterministic pick: stable-hash of job id → upstream id.
                let seed = job.id.as_u64_pair();
                let hash = (seed.0 ^ seed.1.rotate_left(13)).wrapping_mul(0x9E37_79B9);
                let idx = (hash as usize) % self.ctx.linkwheel_project_ids.len();
                let upstream_id = self.ctx.linkwheel_project_ids[idx];
                match repo.list_verified_urls(upstream_id, 500).await {
                    Ok(urls) if !urls.is_empty() => {
                        // Deterministic pick within the upstream pool.
                        let pick = (hash >> 20) as usize % urls.len();
                        urls[pick].clone()
                    }
                    _ => base,
                }
            } else {
                base
            };
            // 2) root_url: with `root_url`%, use the bare scheme://host/.
            let mut url = base;
            let opts = &self.ctx.url_options;
            if opts.root_url > 0 {
                let seed = job.id.as_u64_pair();
                let hash = (seed.0 ^ seed.1.rotate_left(27)).wrapping_mul(0xC2B2_AE35);
                if (hash % 100) < u64::from(opts.root_url) {
                    // Strip path — keep scheme + host.
                    let trimmed = url.trim_start_matches("https://").trim_start_matches("http://");
                    let host = trimmed.split('/').next().unwrap_or(trimmed);
                    let scheme = if url.starts_with("https://") { "https://" } else { "http://" };
                    url = format!("{scheme}{host}/");
                }
            }
            // 3) url_variations: with `url_variations`%, emit a variant
            //    (www/no-www, http/https, case).
            if opts.url_variations > 0 {
                let seed = job.id.as_u64_pair();
                let hash = (seed.0 ^ seed.1.rotate_left(41)).wrapping_mul(0x85EBC_A6B);
                let variant = (hash % 100) < u64::from(opts.url_variations);
                if variant {
                    // Toggle www: strip if present, else add.
                    let stripped = url.trim_start_matches("https://").trim_start_matches("http://").trim_start_matches("www.");
                    let scheme = if url.starts_with("https://") { "https://" } else { "http://" };
                    url = if url.contains("://www.") {
                        format!("{scheme}{stripped}")
                    } else {
                        format!("{scheme}www.{stripped}")
                    };
                    // Uppercase host (GSA example: `http://domain.de/` → `HTTP://www.Domain.de`)
                    url = url.to_uppercase();
                }
            }
            url
        };

        let input = SubmissionInput {
            url: effective_url,
            keyword: keyword.clone(),
            anchor: input_anchor,
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
        // F#21 (GSA parity: 2Captcha / Anti-Captcha / local-arithmetic) —
        // the solver, if any, is built ONCE per job from the project's
        // settings (not per-step, not per-attempt) so a paid call is made
        // at most once per captcha wall and only when the solver is
        // actually configured. `None` = fall through to the classic
        // execute_plan (no captcha attempts, the step counts as
        // captcha_failed, matching GSA's "blocked by captcha wall" bucket).
        let solver: Option<std::sync::Arc<dyn engine::captcha::CaptchaSolver>> = {
            let s = self.ctx.settings.as_ref();
            // F#29 — default is the "totally free" chain (local → cdp-vision →
            // paid-if-keyed) instead of local-only, so reCAPTCHA v2 walls are
            // attempted for free before any job gives up.
            let provider = engine::captcha::CaptchaProvider::parse(
                s.captcha_provider.as_deref().unwrap_or("auto"),
            );
            let api_key = s.captcha_api_key.clone().filter(|k| !k.trim().is_empty());
            let timeout_secs = s.captcha_timeout_secs.unwrap_or(60);
            engine::captcha::make_solver(provider, api_key.as_deref(), timeout_secs)
        };
        // F#27 (GSA parity: register → activate → login → submit).
        //
        // These article/comment specs carry `extract from email=`: the account
        // is registered, then an activation e-mail supplies the login password
        // and/or the first-login URL that the later steps consume. A single
        // linear pass no-ops after the register POST — it never reads that
        // mail, so the login / submit steps hit a dead 404. We split the plan
        // at the phase boundary: drive the register phase, fetch + parse the
        // activation mail via the operator's `Email_Engines` provider (the F#12
        // `emails match` + `extract from email=` pair), hop the activation URL
        // in the SAME browser session, then re-drive the login / submit phase
        // against a context that now carries the emailed values.
        let has_register = plan.steps.iter().any(|s| s.phase == StepPhase::Register);
        let report = if engine_uses_email_verify(&engine) && has_register {
            let reg = {
                let mut p = plan.clone();
                p.steps = plan
                    .steps
                    .iter()
                    .filter(|s| s.phase == StepPhase::Register)
                    .cloned()
                    .collect();
                p
            };
            let reg_report = self
                .run_plan_retry(
                    &reg,
                    &driver,
                    solver.as_deref(),
                    timeout,
                    n,
                    &mut attempt_idx,
                    &mut retried_down_proxy,
                )
                .await;
            // 2) read the activation mail → (activation URL, emailed credential).
            let (verify, note) = self.email_verify_bridge(&engine, &input, &driver).await;
            // 3) hop the activation link in the SAME browser session.
            if let Some(u) = verify.activation_url.clone() {
                let _ = driver.navigate(u).await;
            }
            // 4) login + the real submission, now that %emailpw% / %first_login%
            //    resolve to the emailed values (not the register step's spintax).
            let mut input2 = input.clone();
            for (k, v) in verify.fields.iter() {
                input2
                    .extra
                    .entry(k.to_ascii_lowercase())
                    .or_insert_with(|| v.clone());
            }
            let login = {
                let mut p = resolve_plan(&engine, &input2);
                p.steps = p
                    .steps
                    .iter()
                    .filter(|s| s.phase != StepPhase::Register)
                    .cloned()
                    .collect();
                p
            };
            let login_report = self
                .run_plan_retry(
                    &login,
                    &driver,
                    solver.as_deref(),
                    timeout,
                    n,
                    &mut attempt_idx,
                    &mut retried_down_proxy,
                )
                .await;
            // 5) fold both phases; surface the bridge note as a diagnostic.
            let mut merged = login_report;
            merged.failed += reg_report.failed;
            merged.retried += reg_report.retried;
            merged.captcha_failed += reg_report.captcha_failed;
            merged.errors.extend(reg_report.errors);
            if merged.submitted_url.is_none() {
                merged.submitted_url = reg_report.submitted_url;
            }
            if !note.is_empty() {
                merged.errors.push(format!("[email-verify] {note}"));
            }
            merged
        } else {
            // No `extract from email=` — the classic linear pass.
            self.run_plan_retry(
                &plan,
                &driver,
                solver.as_deref(),
                timeout,
                n,
                &mut attempt_idx,
                &mut retried_down_proxy,
            )
            .await
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

    /// F#27 — the proxy/transport retry loop over a [SubmissionPlan], factored
    /// out so the register → activate → login family can drive it once per
    /// phase against a sub-plan. This is the exact loop the classic (non-email)
    /// path runs on the whole plan.
    async fn run_plan_retry(
        &self,
        plan: &engine::executor::SubmissionPlan,
        driver: &std::sync::Arc<dyn Driver>,
        solver: Option<&dyn engine::captcha::CaptchaSolver>,
        timeout: std::time::Duration,
        n: usize,
        attempt_idx: &mut Option<usize>,
        retried_down_proxy: &mut bool,
    ) -> ExecutionReport {
        loop {
            let drv: std::sync::Arc<dyn Driver> = match *attempt_idx {
                Some(i) => {
                    let spec = &self.ctx.proxies[i];
                    driver
                        .proxy_driver(spec)
                        .unwrap_or_else(|| std::sync::Arc::clone(driver))
                }
                None => std::sync::Arc::clone(driver),
            };
            let used_idx = *attempt_idx;
            let r: ExecutionReport = match tokio::time::timeout(
                timeout,
                engine::executor::execute_plan_with_solver(plan, drv.as_ref(), solver),
            )
            .await
            {
                Ok(r) => r,
                Err(_) => empty_report_with_err(format!("job timed out after {timeout:?}")),
            };
            let down = used_idx.is_some()
                && n > 1
                && !*retried_down_proxy
                && r.successful == 0
                && looks_transport_failure(&r);
            if down {
                *retried_down_proxy = true;
                *attempt_idx = Some((used_idx.unwrap() + 1) % n);
                continue;
            }
            return r;
        }
    }

    /// F#27 — fetch the operator's activation mail for a
    /// register → activate → login → submit engine and resolve the emailed
    /// credential (password / token) + any activation URL the login steps need.
    /// Mirrors GSA's `emails match` + `extract from email=` pair, driven by the
    /// operator's receiving mailbox. Returns `(resolution, note)`: an empty
    /// `note` means success; a non-empty `note` is why the mail was unavailable
    /// (no mailbox / no matching provider / fetch failed / no activation mail
    /// yet) and is surfaced to the UI as a diagnostic error.
    async fn email_verify_bridge(
        &self,
        engine: &engine::enginespec::Engine,
        input: &SubmissionInput,
        driver: &std::sync::Arc<dyn Driver>,
    ) -> (engine::EmailVerifyResult, String) {
        let (addr, pw, roots, drv) = (
            self.ctx.email_address.clone(),
            self.ctx.email_password.clone(),
            self.spec.gsa_roots(),
            std::sync::Arc::clone(driver),
        );
        // `move` closures need `'static` — capture the owned pieces outside.
        let ctx_in = input.into_context();
        let engine_captured = engine.clone();
        let result = match tokio::task::spawn_blocking(
            move || -> Result<engine::EmailVerifyResult, String> {
                if addr.trim().is_empty() {
                    return Err("no operator mailbox configured (set the email pool)".to_string());
                }
                let provider = roots.iter().find_map(|root| {
                    let engines = load_email_engines(root);
                    engines.into_iter().find(|e| matches_address(e, &addr))
                });
                let provider =
                    provider.ok_or_else(|| format!("no Email_Engines provider matches {addr}"))?;
                let mail = EmailContext {
                    email_address: addr,
                    password: pw,
                };
                let inbox = build_source(&provider, &mail, drv);
                // Pure — build the extraction context up-front (url/keyword/files).
                // (ctx_in + engine_captured were moved into this closure from the
                // caller — they are owned.)
                // Poll the inbox: the confirmation mail lands a few seconds after
                // the register POST. Early-exit ONLY when the extractor produces
                // a real activation URL — a signed-in *empty* inbox (anonmails
                // ships `confirmDialog` JS in every page) has no verification
                // link to extract, so keep polling.
                let start = std::time::Instant::now();
                let max_wait = std::time::Duration::from_secs(120);
                let mut last_err = String::new();
                loop {
                    match inbox.fetch() {
                        Ok(messages) => {
                            for m in messages.iter().rev() {
                                let res = engine::resolve_email_verify(&engine_captured, &m.body, &ctx_in);
                                if res.activation_url.is_some() {
                                    return Ok(res);
                                }
                            }
                        }
                        Err(e) => last_err = e,
                    }
                    if std::time::Instant::now() - start >= max_wait {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_secs(8));
                }
                if last_err.is_empty() {
                    Err("activation mail not found in inbox after 120s (register pending or wrong inbox)".to_string())
                } else {
                    Err(last_err)
                }
            },
        )
        .await
        {
            Ok(Ok(res)) => res,
            Ok(Err(e)) => return (engine::EmailVerifyResult::default(), e),
            Err(je) => return (engine::EmailVerifyResult::default(), format!("inbox worker panicked: {je}")),
        };
        (result, String::new())
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

        // F#26 — Test Button: drive the engine end-to-end but persist
        // nothing. The API caller deletes the transient job afterward.
        if self.ctx.dry_run {
            return JobOutcome {
                job_id,
                status: if done { "done" } else { "failed" },
                report,
                driver_error: err,
            };
        }

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

        // F#24-lite — GSA "Automatically export verified URLs": when the
        // project has `auto_export` on AND this job succeeded, append a CSV
        // row (GSA's export schema) to `<data>/verified/<project_id>.csv`.
        // Gated on `success` because that's the only outcome GSA exports
        // (only verified URLs). Failures stay visible in the `results` table
        // but are not part of the "link list" feed.
        if success && self.ctx.auto_export {
            if let Some(sub_url) = &report.submitted_url {
                if !sub_url.trim().is_empty() {
                    self.export_verified_url(job_id, sub_url, &keyword).await;
                }
            }
        }

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

    /// F#24-lite — GSA "Automatically export verified URLs".
    ///
    /// Append one GSA-schema CSV row to `<data_dir>/verified/<project_id>.csv`.
    /// This is the "link list" feed that (a) downstream tier projects import
    /// for Multi-Tier / linkwheel, and (b) the External-APIs tab of GSA
    /// exposes as an "Import verified URLs from external file" source.
    ///
    /// Schema (matches GSA's export column order, from the binary strings we
    /// pulled): `Date,URL,Anchor text,Engine,Type,DoFollow Link,Indexable,
    /// Links (int),Links (ext),Project`. We fill the columns we can derive
    /// at job-completion time (Date, URL, Anchor text, Project) and leave
    /// the engine-specific ones blank — the engine row isn't loaded here, and
    /// those fields are populated by GSA's post-run analysis pass.
    ///
    /// Deterministic + idempotent per (project, url): a re-run of the same
    /// engine against the same target produces the same URL; the downstream
    /// linkwheel consumer dedupes by URL anyway, so a duplicate row on retry
    /// is harmless (the `list_verified_urls` SQL already `GROUP BY url`).
    ///
    /// `data_dir` resolution: `SEER_DATA_DIR` env var (operator-set), else
    /// `~/.ser` — matching the `SEER_*` prefix convention the rest of the
    /// app uses for its config surface. Best-effort: a failed write is
    /// logged to stderr but never fails the job (export is a convenience,
    /// not part of the submission contract).
    async fn export_verified_url(&self, _job_id: Uuid, url: &str, anchor: &str) {
        let anchor = anchor.trim();
        let file = self.verified_export_path();
        if file.parent().map(|p| !p.as_os_str().is_empty()).unwrap_or(true) {
            if let Err(e) = std::fs::create_dir_all(file.parent().unwrap_or(Path::new("."))) {
                eprintln!("[auto-export] could not create dir {e}");
                return;
            }
        }
        let header_written = std::fs::metadata(&file)
            .map(|m| m.len() == 0)
            .unwrap_or(true);
        let date = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let esc = |s: &str| {
            let t = s.trim();
            if t.contains(',') || t.contains('"') || t.contains('\n') {
                format!("\"{}\"", t.replace('"', "\"\""))
            } else {
                t.to_string()
            }
        };
        // GSA's 10-column export schema, exactly (gap-analysis §2 from the
        // binary strings): Date, URL, Anchor text, Engine, Type, DoFollow
        // Link, Indexable, Links (int), Links (ext), Project.
        let cells: Vec<String> = vec![
            esc(&date),
            esc(url),
            esc(anchor),
            String::new(), // Engine
            String::new(), // Type (backlink family — engine-specific)
            String::new(), // DoFollow Link
            String::new(), // Indexable
            String::new(), // Links (int)
            String::new(), // Links (ext)
            self.ctx.project_id.to_string(),
        ];
        let row = cells.join(",");
        let header = "Date,URL,Anchor text,Engine,Type,DoFollow Link,Indexable,Links (int),Links (ext),Project";
        let csv = if header_written {
            format!("{header}\n{row}\n")
        } else {
            format!("{row}\n")
        };
        let mut f = match std::fs::OpenOptions::new().create(true).append(true).open(&file) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("[auto-export] open {file:?}: {e}");
                return;
            }
        };
        if let Err(e) = f.write_all(csv.as_bytes()) {
            eprintln!("[auto-export] write {file:?}: {e}");
        }
    }

    /// Where the verified-URL feed lives. `SEER_DATA_DIR/verified/<project>.csv`,
    /// falling back to `~/.ser/verified/<project>.csv`.
    fn verified_export_path(&self) -> std::path::PathBuf {
        let base = std::env::var("SEER_DATA_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| {
                let home = std::env::var("USERPROFILE")
                    .or_else(|_| std::env::var("HOME"))
                    .unwrap_or_else(|_| ".".to_string());
                std::path::PathBuf::from(home).join(".ser")
            });
        base.join("verified").join(format!("{}.csv", self.ctx.project_id))
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
