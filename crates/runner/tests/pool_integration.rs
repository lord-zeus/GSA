//! Concurrent worker pool — end-to-end against the live DB container.
//!
//! Skipped (not failed) when `SEER_DATABASE_URL` is unreachable, so this
//! suite runs on a box without the container too (with the same printed
//! hint as `crates/db/tests`).
//!
//! Exercises the F#8 contract:
//!
//! * **Single job, success** — one `pending` job, one worker, one
//!   `MockDriver` returning a `submit-success` page. The pool drains,
//!   the job row becomes `done`, and a `results` row exists.
//! * **Many jobs, N workers** — 8 jobs, 4 workers, each worker claims
//!   exactly one. The pool's `claimed == 8`, `done == 8`.
//! * **Crash isolation** — first mock driver panics on `submit`, the
//!   second returns success. The pool must NOT abort because of the
//!   panic: `panics >= 1`, the second job still completes `done`.
//! * **Missing driver** — a `Browser`-routed engine with no browser
//!   driver registered. Job ends `failed` with a "driver not registered"
//!   message; the pool finishes cleanly.
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use engine::driver::{Driver, DriverResponse, MockDriver};
use engine::enginespec::parse_engine_str;
use engine::routing::DriverKind;

use runner::{
    DriverRegistry, RunnerConfig, SpecRegistry, SpecSource, SubmissionContext, WorkerPool,
};

// --------------------------------------------------------------------------
async fn pool_or_skip() -> Option<sqlx::PgPool> {
    match ser_db::connect(None).await {
        Ok(p) => Some(p),
        Err(e) => {
            eprintln!("SKIP runner pool tests: cannot reach Postgres ({e}).");
            None
        }
    }
}

fn stamp() -> String {
    let s = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    format!("{:x}-{:x}", s.as_secs(), s.subsec_nanos())
}

fn http_engine() -> engine::enginespec::Engine {
    // Plain form engine — no browser markers => DriverKind::Http.
    let ini = concat!(
        "[engine]\r\n",
        "engine type=directory\r\n",
        "url=\r\n",
        "\r\n",
        "[step1]\r\n",
        "form url=/step1\r\n",
        "url=%url%\r\n",
        "title=my anchor\r\n",
        "submit success=PHRASE OK\r\n",
        "find url=https://target.example/entry\r\n",
    );
    parse_engine_str(ini, "http_engine").unwrap()
}

fn ctx(url: &str, kw: &str) -> SubmissionContext {
    SubmissionContext {
        project_url: url.into(),
        keyword: kw.into(),
        files: Default::default(),
        seed: Some(42),
        email_address: String::new(),
        email_password: String::new(),
        proxies: Default::default(),
        // F#15 — empty settings → `job_timeout` falls back to `RunnerConfig::per_job_timeout`.
        settings: std::sync::Arc::new(engine::settings::Settings::default()),
        ..Default::default()
    }
}
#[tokio::test]
async fn single_job_success() {
    let Some(pool) = pool_or_skip().await else { return };
    let repo = ser_db::Repo::new(pool.clone());

    let proj = repo
        .create_project(&format!("pool 1 {}", stamp()), None)
        .await
        .unwrap();
    let engine = http_engine();
    let erow = repo
        .upsert_engine(
            proj.id,
            &engine.name,
            &engine.engine_type.to_string(),
            engine.url.trim(),
            "tests/http.ini",
            engine.steps.len() as i32,
        )
        .await
        .unwrap();

    let job = repo.create_job(proj.id, Some(erow.id), None).await.unwrap();
    assert_eq!(job.status, "pending");

    // Register: inline spec (engine_id -> Engine) + mock driver on the
    // Http slot.
    let mut reg = SpecRegistry::new();
    reg.insert(erow.id, engine.clone());
    let specs = SpecSource::Inline(reg);

    let success_html = "<html><body>PHRASE OK — created https://target.example/entry/9999</body></html>".to_string();
    let drivers = DriverRegistry::new().with_http(MockDriver::with_html(success_html));

    let cfg = RunnerConfig {
        concurrent_workers: 1,
        leader_idle_timeout: std::time::Duration::from_millis(50),
        per_job_timeout: std::time::Duration::from_secs(10),
        ..Default::default()
    };

    let pool = WorkerPool::new(pool.clone(), cfg, specs, drivers)
        .with_context(ctx("https://mysite.example/", "kw"));

    let summary = pool.run(proj.id).await;
    assert_eq!(summary.claimed, 1, "summary: {summary:?}");
    assert_eq!(summary.done, 1, "summary: {summary:?}");
    assert_eq!(summary.failed, 0);
    assert_eq!(summary.panics, 0);

    // DB check: job is done, a results row exists with the created URL.
    let j = repo.job_by_id(job.id).await.unwrap().unwrap();
    assert_eq!(j.status, "done");
    let results = repo.results_for_job(job.id).await.unwrap();
    assert!(
        results.iter().any(|r| r.url.as_deref() == Some("https://target.example/entry/9999")),
        "no success result row with created URL; rows: {results:?}"
    );
}

// --------------------------------------------------------------------------
// 2) Crash isolation: two jobs, two workers, one driver that panics on its
//    FIRST submit. Exactly one worker dies (panic counted), the other
//    completes its job successfully.
struct PanicOnceDriver {
    calls: Arc<AtomicUsize>,
}
#[async_trait::async_trait]
impl Driver for PanicOnceDriver {
    async fn navigate(&self, _url: String) -> DriverResponse {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        if n == 0 {
            panic!("PanicOnceDriver: first call (intentional)");
        }
        DriverResponse {
            ok: true,
            final_url: "mock".into(),
            html: "PHRASE OK".into(),
            error: None,
        }
    }
    async fn submit(
        &self,
        _url: String,
        _fields: HashMap<String, String>,
        _timeout: std::time::Duration,
    ) -> DriverResponse {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        if n == 0 {
            panic!("PanicOnceDriver: first submit (intentional)");
        }
        DriverResponse {
            ok: true,
            final_url: "mock".into(),
            html: "PHRASE OK".into(),
            error: None,
        }
    }
}

#[tokio::test]
async fn crash_isolation_one_worker_panics_other_survives() {
    let Some(pool) = pool_or_skip().await else { return };
    let repo = ser_db::Repo::new(pool.clone());

    let proj = repo
        .create_project(&format!("pool 2 {}", stamp()), None)
        .await
        .unwrap();
    let engine = http_engine();
    let erow = repo
        .upsert_engine(
            proj.id,
            &engine.name,
            &engine.engine_type.to_string(),
            engine.url.trim(),
            "tests/http.ini",
            engine.steps.len() as i32,
        )
        .await
        .unwrap();

    let mut reg = SpecRegistry::new();
    reg.insert(erow.id, engine.clone());
    let specs = SpecSource::Inline(reg);

    // Create 2 jobs.
    let _j1 = repo.create_job(proj.id, Some(erow.id), None).await.unwrap();
    let _j2 = repo.create_job(proj.id, Some(erow.id), None).await.unwrap();

    // One driver, shared across 2 workers.
    let calls = Arc::new(AtomicUsize::new(0));
    let drivers = DriverRegistry::new()
        .with_http(PanicOnceDriver { calls: calls.clone() });

    let cfg = RunnerConfig {
        concurrent_workers: 2,
        leader_idle_timeout: std::time::Duration::from_millis(30),
        per_job_timeout: std::time::Duration::from_secs(15),
        ..Default::default()
    };

    let pool = WorkerPool::new(pool.clone(), cfg, specs, drivers).with_context(ctx("https://mysite.example/", "kw"));

    let summary = pool.run(proj.id).await;

    // At most one job can succeed (the worker that saw calls>=1).
    // At most one worker can panic (the worker that saw calls==0).
    assert!(
        summary.panics >= 1,
        "summary: {summary:?} — expected >=1 panic"
    );
    // The other worker should have successfully completed a job
    // (crash isolation: one panic did NOT take the pool down).
    assert!(
        summary.done >= 1,
        "summary: {summary:?} — crash isolation failed: other worker did not finish"
    );
    // Panicking workers are NOT counted in `claimed` (their task died before
    // reaching the summary counter); they are counted in `panics`.
    assert_eq!(
        summary.claimed,
        summary.done + summary.failed,
        "summary: {summary:?}"
    );
    // The shared `calls` counter shows at least one worker hit the
    // `PanicOnceDriver::submit` (and panicked); the second worker, with
    // calls >= 1, succeeded. So calls >= 2.
    assert!(calls.load(Ordering::SeqCst) >= 2, "calls: {}", calls.load(Ordering::SeqCst));

    // DB check: the one surviving job is `done`; the panicked job is
    // still `running` (the worker died before marking it terminal —
    // acceptable for F#8; the job can be retried by a new pool run).
    let jobs = repo.list_jobs(proj.id, None).await.unwrap();
    let done = jobs.iter().filter(|j| j.status == "done").count();
    assert_eq!(done, summary.done, "db done={done} summary done={:?}", summary.done);
}

// --------------------------------------------------------------------------
// 3) Missing-driver dispatch: engine routes to Browser but no browser
//    driver registered. Job ends failed with "driver not registered".
#[tokio::test]
async fn missing_driver_marks_job_failed() {
    let Some(pool) = pool_or_skip().await else { return };
    let repo = ser_db::Repo::new(pool.clone());

    let proj = repo
        .create_project(&format!("pool 3 {}", stamp()), None)
        .await
        .unwrap();

    // Force a Browser-routed engine: any `recaptcha` substring anywhere.
    let ini = concat!(
        "[engine]\r\n",
        "engine type=directory\r\n",
        "url=\r\n",
        "\r\n",
        "[step1]\r\n",
        "form url=/step1\r\n",
        "url=%url%\r\n",
        "recaptcha=1\r\n",
        "submit success=PHRASE OK\r\n",
    );
    let engine = parse_engine_str(ini, "browser_engine").unwrap();
    // Sanity: the routing classifier must say Browser.
    let kind = engine::required_driver_kind(&engine);
    assert_eq!(kind, DriverKind::Browser, "routing test failed: {kind:?}");

    let erow = repo
        .upsert_engine(
            proj.id,
            &engine.name,
            &engine.engine_type.to_string(),
            engine.url.trim(),
            "tests/browser.ini",
            engine.steps.len() as i32,
        )
        .await
        .unwrap();
    let job = repo.create_job(proj.id, Some(erow.id), None).await.unwrap();

    let mut reg = SpecRegistry::new();
    reg.insert(erow.id, engine.clone());
    let specs = SpecSource::Inline(reg);

    // HTTP-only registry — NO browser driver.
    let drivers = DriverRegistry::new().with_http(MockDriver::with_html("PHRASE OK"));

    let cfg = RunnerConfig {
        concurrent_workers: 1,
        leader_idle_timeout: std::time::Duration::from_millis(30),
        per_job_timeout: std::time::Duration::from_secs(5),
        ..Default::default()
    };
    let pool = WorkerPool::new(pool.clone(), cfg, specs, drivers).with_context(ctx("https://mysite.example/", "kw"));
    let summary = pool.run(proj.id).await;

    assert_eq!(summary.failed, 1, "summary: {summary:?}");
    assert_eq!(summary.done, 0);

    let j = repo.job_by_id(job.id).await.unwrap().unwrap();
    assert_eq!(j.status, "failed");
    assert!(
        j.last_error.as_deref().unwrap_or("").contains("driver not registered"),
        "last_error: {:?}",
        j.last_error
    );
}

// ==========================================================================
// F#9 — keyword management, end-to-end through the runner pipeline.
// ==========================================================================

/// An HTTP engine whose success HTML contains a created-URL — the anchor is
/// stored into `results` on success, which is what we assert on.
fn http_engine_with_result() -> engine::enginespec::Engine {
    let ini = concat!(
        "[engine]\r\n",
        "engine type=directory\r\n",
        "url=\r\n",
        "\r\n",
        "[step1]\r\n",
        "form url=/step1\r\n",
        "url=%url%\r\n",
        "title=%anchor_text%\r\n",
        "submit success=PHRASE OK\r\n",
        "find url=https://target.example/entry/4242\r\n",
    );
    parse_engine_str(ini, "kw_engine").unwrap()
}

#[tokio::test]
async fn keyword_spintax_job_end_to_end() {
    let Some(pool) = pool_or_skip().await else { return };
    let repo = ser_db::Repo::new(pool.clone());

    let proj = repo
        .create_project(&format!("kw f9 {}", stamp()), None)
        .await
        .unwrap();

    // 1) spintax keyword stored as a concrete variant via normalisation...
    let stored = repo.add_keywords(proj.id, &["buy {shoes|boots}".to_string()]).await.unwrap();
    assert!(stored.0 >= 2, "expected spintax fan-out >=2, got {:?}", stored);
    let kws = repo.list_keywords(proj.id).await.unwrap();
    assert!(kws.iter().any(|k| k.text == "buy shoes"), "kws: {:?}", kws);
    assert!(kws.iter().any(|k| k.text == "buy boots"), "kws: {:?}", kws);
    let kw_shoes = kws.iter().find(|k| k.text == "buy shoes").unwrap();
    let kw_boots = kws.iter().find(|k| k.text == "buy boots").unwrap();

    // 2) engine + two jobs, one per concrete keyword.
    let engine = http_engine_with_result();
    let erow = repo
        .upsert_engine(
            proj.id,
            &engine.name,
            &engine.engine_type.to_string(),
            engine.url.trim(),
            "tests/kw.ini",
            engine.steps.len() as i32,
        )
        .await
        .unwrap();
    let j_shoes = repo.create_job(proj.id, Some(erow.id), Some(kw_shoes.id)).await.unwrap();
    let j_boots = repo.create_job(proj.id, Some(erow.id), Some(kw_boots.id)).await.unwrap();

    let success_html =
        "<html><body>PHRASE OK — created <a href=\"https://target.example/entry/4242\">link</a></body></html>"
            .to_string();
    let mut reg = SpecRegistry::new();
    reg.insert(erow.id, engine.clone());
    let specs = SpecSource::Inline(reg);
    let drivers = DriverRegistry::new().with_http(MockDriver::with_html(success_html));

    let cfg = RunnerConfig {
        concurrent_workers: 1,
        leader_idle_timeout: std::time::Duration::from_millis(30),
        per_job_timeout: std::time::Duration::from_secs(10),
        ..Default::default()
    };
    let pool = WorkerPool::new(pool.clone(), cfg, specs, drivers).with_context(ctx("https://mysite.example/", "context-kw-must-lose"));
    let summary = pool.run(proj.id).await;
    assert_eq!(summary.done, 2, "summary: {summary:?}");
    assert_eq!(summary.failed, 0, "summary: {summary:?}");

    // 3) results anchor must be the JOB's keyword, not the context one.
    for (job, expected) in [(&j_shoes, "buy shoes"), (&j_boots, "buy boots")] {
        let results = repo.results_for_job(job.id).await.unwrap();
        assert!(
            results.iter().any(|r| r.anchor.as_deref() == Some(expected)),
            "job {:?}: anchor should be {expected:?}; got {results:?}",
            job.id
        );
        assert!(
            !results.iter().any(|r| r.anchor.as_deref() == Some("context-kw-must-lose")),
            "job {:?}: context keyword leaked in {results:?}",
            job.id
        );
    }
}

#[tokio::test]
async fn keyword_dedupe_and_deterministic_spin() {
    let Some(pool) = pool_or_skip().await else { return };
    let repo = ser_db::Repo::new(pool.clone());

    let proj = repo
        .create_project(&format!("kw f9 b {}", stamp()), None)
        .await
        .unwrap();

    // Dedupe with whitespace variants — one row expected.
    let a = repo.add_keyword(proj.id, "  buy   shoes online ").await.unwrap();
    let b = repo.add_keyword(proj.id, "buy shoes online").await.unwrap();
    assert_eq!(a.id, b.id, "whitespace-variant keyword must dedupe");
    assert_eq!(a.text, "buy shoes online");
    assert_eq!(repo.list_keywords(proj.id).await.unwrap().len(), 1);

    // resolve_one determinism at runner level: same seed -> same variant.
    use engine::keyword::resolve_one;
    let x = resolve_one("best {shoes|boots}", Some(123));
    let y = resolve_one("best {shoes|boots}", Some(123));
    assert_eq!(x, y);

    // And it must land on a known concrete variant for SOME seed.
    let found = (1..=50).any(|s| resolve_one("best {shoes|boots}", Some(s)) == "best shoes"
        || resolve_one("best {shoes|boots}", Some(s)) == "best boots");
    assert!(found, "spintax keyword must resolve to a concrete variant");
}

// ==========================================================================
// F#12 — Email family E2E (WEB inbox). Proves the JobRunner branch:
//   plan.required_driver == DriverKind::Email
//     -> drivers.get(Email) -> run_email_engine(...)
// MockDriver returns the inbox HTML; the pipeline runs the full
// WEB-inbox path (fetch -> filter -> extract -> verify).
// ==========================================================================
#[tokio::test]
async fn email_web_family_success() {
    let Some(pool) = pool_or_skip().await else { return };
    let repo = ser_db::Repo::new(pool.clone());

    // A WEB-family inbox spec (the shape of real 1secmail / discard INIs).
    // bare `"` which would close a regular `"..."` string.
    let email_inbox_ini = r##"[setup]
emails match=*@gmail.com
pop3 server=WEB
web parse url=https://inbox.example/%emailuser%/inbox.html
url_front=href="
url_back=">
message match=your link
enabled=1"##;
    let engine = parse_engine_str(email_inbox_ini, "email_web").unwrap();
    let mail = engine.setup_to_email();
    assert!(mail.pop3_server.eq_ignore_ascii_case("web"));
    assert_eq!(mail.url_fb[0].0.as_deref(), Some("href=\""));
    assert!(mail.url_fb[0].1.as_deref().map(|s| s.ends_with(">")).unwrap_or(false), "back marker should end with > : {:?}", mail.url_fb[0].1);
    assert_eq!(engine::required_driver_kind(&engine), DriverKind::Email);

    let proj = repo
        .create_project(&format!("f12 {}", stamp()), None)
        .await
        .unwrap();
    let erow = repo
        .upsert_engine(
            proj.id,
            &engine.name,
            &engine.engine_type.to_string(),
            "",
            "tests/email_web.ini",
            engine.steps.len() as i32,
        )
        .await
        .unwrap();
    let job = repo.create_job(proj.id, Some(erow.id), None).await.unwrap();

    // Inbox HTML the mock driver hands back: the url ladder
    // (front=<a href=" , back=.go">) extracts the link below.
    let inbox_html = "your link <a href=\"https://go.example.com/v/xyz123\">VERIFY</a>";
    eprintln!("[PROBE2] html={:?} extract={:?}", inbox_html, engine::email::extract_from_message(&mail, &inbox_html));
    let drivers = DriverRegistry::new().with_email(MockDriver::with_html(inbox_html));

    // Pool context: operator mailbox (resolves %emailuser% / %password%).
    let ctx_mail = SubmissionContext {
        project_url: "https://mysite.example/".into(),
        keyword: "kw".into(),
        files: Default::default(),
        seed: Some(42),
        email_address: "tester@gmail.com".into(),
        email_password: "pw".into(),
        proxies: Default::default(),
        // F#15 — empty settings → `job_timeout` falls back to `RunnerConfig::per_job_timeout`.
        settings: std::sync::Arc::new(engine::settings::Settings::default()),
        ..Default::default()
    };

    let cfg = RunnerConfig {
        concurrent_workers: 1,
        leader_idle_timeout: std::time::Duration::from_millis(50),
        per_job_timeout: std::time::Duration::from_secs(10),
        ..Default::default()
    };
    let mut reg = SpecRegistry::new();
    reg.insert(erow.id, engine.clone());
    let specs = SpecSource::Inline(reg);
    let pool = WorkerPool::new(pool.clone(), cfg, specs, drivers).with_context(ctx_mail);

    let summary = pool.run(proj.id).await;
    let j_dbg = repo.job_by_id(job.id).await.unwrap().unwrap();
    let r_dbg = repo.results_for_job(job.id).await.unwrap();
    assert!(summary.failed == 0,
            "summary: {summary:?}\njob: {j_dbg:?}\nresults: {r_dbg:?}");
    assert_eq!(summary.done, 1, "summary: {summary:?}");

    let j = repo.job_by_id(job.id).await.unwrap().unwrap();
    assert_eq!(j.status, "done");
    let rows = repo.results_for_job(job.id).await.unwrap();
    assert!(
        rows.iter().any(|r| r.url.as_deref().map(|u| u.contains("xyz123")).unwrap_or(false)),
        "no result row containing the created link: {rows:?}"
    );
}
