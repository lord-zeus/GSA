//! T6 — live run events: the runner's voice.
//!
//! Before this module a job was a black box: the UI only saw the final
//! status flip. Now every interesting moment — engine pick, each HTTP step
//! (method/url/outcome/timing), captcha attempts, the final submit result —
//! is a [`RunEvent`] pushed through an [`EventSink`] at the point it
//! happens. The API implements the sink (DB `job_events` row + live SSE
//! broadcast); the runner stays transport-agnostic.
//!
//! Levels (the UI colors them):
//!   `info`    — job started, context resolved
//!   `engine`  — engine / spec / plan resolution
//!   `step`    — one HTTP request the driver executed
//!   `captcha` — a captcha wall hit / solver attempt
//!   `success` — the link was submitted
//!   `error`   — anything that ended the job without a link
//!   `auto`    — auto-build orchestration (candidate N of M, superseded, …)

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use engine::driver::{Driver, DriverResponse};
use serde::Serialize;
use uuid::Uuid;

/// One line of the project's live run log.
#[derive(Debug, Clone, Serialize)]
pub struct RunEvent {
    /// The job this line belongs to (None = project-level orchestration).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<Uuid>,
    /// Engine being driven, when known (helps the UI group the log).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    /// One of info|engine|step|captcha|success|error|auto.
    pub level: String,
    pub message: String,
}

/// Implemented by the API layer. The runner calls [`record`] exactly where
/// the event happens; the impl decides what to do with it (persist,
/// broadcast, both). Synchronous on purpose — the sink must never block a
/// worker on I/O it doesn't own (the DB-backed impl pushes into a
/// non-blocking channel drained by its own consumer task).
pub trait EventSink: std::fmt::Debug + Send + Sync {
    fn record(&self, ev: RunEvent);
}

/// Wrap any [`Driver`] so every request it executes lands in the event log
/// with method, URL, outcome and wall time — the "what is it actually
/// doing right now" line the UI console renders.
///
/// Wrapping is transparent to the executor: same responses, same timeouts;
/// one `Instant` per request for the timing note.
#[derive(Clone)]
pub struct LoggingDriver {
    inner: Arc<dyn Driver>,
    sink: Arc<dyn EventSink>,
    job_id: Option<Uuid>,
    engine: Option<String>,
}

impl std::fmt::Debug for LoggingDriver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoggingDriver")
            .field("job_id", &self.job_id)
            .field("engine", &self.engine)
            .finish_non_exhaustive()
    }
}

impl LoggingDriver {
    pub fn new(
        inner: Arc<dyn Driver>,
        sink: Arc<dyn EventSink>,
        job_id: Option<Uuid>,
        engine: Option<String>,
    ) -> Self {
        Self { inner, sink, job_id, engine }
    }

    fn emit(&self, level: &str, message: String) {
        let _ = self.sink.record(RunEvent {
            job_id: self.job_id,
            engine: self.engine.clone(),
            level: level.to_string(),
            message,
        });
    }

    /// Truncate a URL for the log line (engine step URLs carry the whole
    /// submitted article in the query string — keep the first 140 chars).
    fn short(url: &str) -> String {
        const MAX: usize = 140;
        if url.chars().count() <= MAX {
            url.to_string()
        } else {
            let cut: String = url.chars().take(MAX).collect();
            format!("{cut}…")
        }
    }

    fn log_response(&self, verb: &str, url: &str, r: &DriverResponse, started: Instant) {
        let ms = started.elapsed().as_millis();
        match &r.error {
            Some(e) => self.emit("step", format!("{verb} {} → FAIL ({e}) {ms}ms", Self::short(url))),
            None => self.emit(
                "step",
                format!(
                    "{verb} {} → OK ({} bytes) {ms}ms",
                    Self::short(url),
                    r.html.len()
                ),
            ),
        }
    }
}

#[async_trait::async_trait]
impl Driver for LoggingDriver {
    async fn navigate(&self, url: String) -> DriverResponse {
        let started = Instant::now();
        let r = self.inner.navigate(url.clone()).await;
        self.log_response("GET", &url, &r, started);
        r
    }

    async fn submit(
        &self,
        url: String,
        fields: HashMap<String, String>,
        timeout: Duration,
    ) -> DriverResponse {
        let started = Instant::now();
        let r = self.inner.submit(url.clone(), fields, timeout).await;
        self.log_response("POST", &url, &r, started);
        r
    }

    async fn post(
        &self,
        url: String,
        fields: HashMap<String, String>,
        timeout: Duration,
    ) -> DriverResponse {
        let started = Instant::now();
        let r = self.inner.post(url.clone(), fields, timeout).await;
        self.log_response("POST", &url, &r, started);
        r
    }

    fn proxy_driver(&self, spec: &engine::proxy::ProxySpec) -> Option<Arc<dyn Driver>> {
        match self.inner.proxy_driver(spec) {
            Some(d) => Some(Arc::new(LoggingDriver {
                inner: d,
                sink: Arc::clone(&self.sink),
                job_id: self.job_id,
                engine: self.engine.clone(),
            })),
            None => None,
        }
    }
}

// ── tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Recording sink: plain memory, no channels — the contract under test
    /// is what LoggingDriver feeds it.
    #[derive(Default)]
    struct Recording {
        events: Mutex<Vec<RunEvent>>,
    }
    impl std::fmt::Debug for Recording {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("Recording")
        }
    }
    impl EventSink for Recording {
        fn record(&self, ev: RunEvent) {
            self.events.lock().unwrap().push(ev);
        }
    }

    #[tokio::test]
    async fn logs_navigate_and_submit_with_url_and_outcome() {
        let rec = Arc::new(Recording::default());
        let inner: Arc<dyn Driver> = Arc::new(engine::driver::MockDriver::with_html("<a href='/x'>ok</a>"));
        let drv = LoggingDriver::new(inner, rec.clone(), Some(Uuid::new_v4()), Some("TestIN".into()));

        let r = drv.navigate("https://eng.example/form?msg=hello".into()).await;
        assert!(r.ok);

        let r = drv
            .submit("https://eng.example/send".into(), HashMap::from([("f".into(), "v".into())]), Duration::from_secs(5))
            .await;
        assert!(r.ok);

        let events = rec.events.lock().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].level, "step");
        assert!(events[0].message.starts_with("GET https://eng.example/form"));
        assert!(events[0].message.contains("→ OK"));
        assert_eq!(events[1].message.starts_with("POST https://eng.example/send"), true);
        assert_eq!(events[0].engine.as_deref(), Some("TestIN"));
    }

    #[tokio::test]
    async fn failure_is_logged_with_reason() {
        let rec = Arc::new(Recording::default());
        let inner: Arc<dyn Driver> = Arc::new(engine::driver::MockDriver::fail_with("TLS handshake"));
        let drv = LoggingDriver::new(inner, rec.clone(), None, None);

        let r = drv.navigate("https://dead.example/".into()).await;
        assert!(!r.ok);
        let events = rec.events.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert!(events[0].message.contains("FAIL (TLS handshake)"));
    }
}
