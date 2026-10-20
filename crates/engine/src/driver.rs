//! # Driver abstraction — the seam between the executor and the network.
//!
//! The executor produces a resolved `SubmissionPlan`; something must actually
//! *navigate* to each step's target and *return* the page HTML so the
//! executor can classify the outcome and extract the created link.
//! This trait is that seam:
//!
//! * **Feature #6 (HTTP driver)** — a `reqwest`-backed impl for the ~70% of
//!   engines that are plain form-POST.
//! * **Feature #7 (browser bridge)** — a WebDriver/ChromeDriver-backed impl
//!   for the JS-heavy engines (reCAPTCHA, hCaptcha, CF Turnstile).
//!
//! The executor only depends on this trait, so both drivers slot in without
//! touching executor code.
//!
//! ## Contract
//!
//! A driver `navigate`s to a URL, waits for the page to settle, and returns
//! a [`DriverResponse`] carrying the final HTML (post-redirect) plus success
//! and an optional error string.  Form *submission* (POSTing the fields) is
//! the driver's responsibility too — the executor hands it the resolved
//! `StepPlan` fields; see [`Driver::submit`].
//!
//! Every method is `async` and must be `Send` so the worker pool can share
//! one driver across tasks.

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use serde::Serialize;

/// What a driver saw after navigating to (and optionally submitting at) a
/// step's target URL.
#[derive(Debug, Clone, Serialize)]
pub struct DriverResponse {
    /// True on a 200-class response with a readable body; false on network
    /// error, timeout, HTTP >= 400, or parse failure.
    pub ok: bool,
    /// Final URL after redirects (empty if the navigate failed).
    pub final_url: String,
    /// The response body (HTML by convention).  Empty on failure or when the
    /// driver could not read the body.
    pub html: String,
    /// Human-readable error (timeout, DNS, non-2xx, …) when `ok == false`.
    pub error: Option<String>,
}

/// A minimal driver.  Two verbs:
///
/// * [`Driver::navigate`] — GET a URL, return the settled page.
/// * [`Driver::submit`]   — apply `fields` to the form at `url` and return
///   the confirmation page (this is what the executor actually drives).
///
/// Implementors should honour `timeout`.
#[async_trait]
pub trait Driver: Send + Sync {
    async fn navigate(&self, url: String) -> DriverResponse;
    async fn submit(&self, url: String, fields: HashMap<String, String>, timeout: Duration) -> DriverResponse;

    /// Raw form-urlencoded body POST (used by the 127/217 `web parse url`
    /// specs that carry `web parse url post data=...`). Default = `navigate`
    /// so `MockDriver` and any driver without POST still compile; concrete
    /// drivers override with a real POST.
    async fn post(&self, url: String, fields: HashMap<String, String>, timeout: Duration) -> DriverResponse {
        let _ = (fields, timeout);
        self.navigate(url).await
    }

    /// F#13 (proxy support, GSA `proxy list/type`): return a driver
    /// instance whose traffic is routed THROUGH `spec` (http or socks5,
    /// optional basic auth). Default = `None` → this driver kind does not
    /// support proxying (e.g. `MockDriver`, the browser bridge) and the
    /// runner falls back to direct connection. Object-safe: returns a
    /// pointer, no `async fn`.
    fn proxy_driver(&self, _spec: &crate::proxy::ProxySpec) -> Option<std::sync::Arc<dyn Driver>> {
        None
    }
}

// ===========================================================================
//  MockDriver — in-memory driver for tests (no network).
// ===========================================================================

/// A [`Driver`] that ignores its input and returns a canned HTML response.
/// Useful to exercise `executor::execute_plan` end-to-end without
/// a single socket.
#[derive(Debug, Clone, Default)]
pub struct MockDriver {
    /// HTML to return; defaults to empty.
    pub html: String,
    /// When `Some`, the response is `ok = false` and `error` is `Some(msg)`.
    pub error: Option<String>,
    /// When `Some`, overrides the `final_url` of every success response
    /// (default `"mock"`).  Lets tests simulate a driver redirected to a
    /// created resource (e.g. a new paste URL) distinct from the target.
    pub final_url: Option<String>,
}

impl MockDriver {
    /// Build a mock that returns the given HTML on success.
    pub fn with_html(html: impl Into<String>) -> Self {
        Self { html: html.into(), error: None, final_url: None }
    }

    /// Build a mock that always fails with the given error message.
    pub fn fail_with(err: impl Into<String>) -> Self {
        Self { html: String::new(), error: Some(err.into()), final_url: None }
    }

    fn response(&self) -> DriverResponse {
        if let Some(e) = &self.error {
            DriverResponse {
                ok: false,
                final_url: String::new(),
                html: String::new(),
                error: Some(e.clone()),
            }
        } else {
            DriverResponse {
                ok: true,
                final_url: self.final_url.clone().unwrap_or_else(|| "mock".into()),
                html: self.html.clone(),
                error: None,
            }
        }
    }
}

#[async_trait]
impl Driver for MockDriver {
    async fn navigate(&self, _url: String) -> DriverResponse {
        self.response()
    }

    async fn submit(&self, _url: String, _fields: HashMap<String, String>, _timeout: Duration) -> DriverResponse {
        self.response()
    }
}
