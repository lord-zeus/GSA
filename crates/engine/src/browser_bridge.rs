//! # Browser bridge — real-DOM implementation of [`Driver`] (Feature #7).
//!
//! Covers the slice of GSA's engine corpus that a plain HTTP POST cannot
//! reach: the engines that render their form in a real browser and submit
//! through JavaScript — `form name` (button text, 666 specs), `form id`
//! (309), `set unknown variable` (351 — the JS-side variable to set before
//! submit), `page must have1` (311 — post-submit condition) — plus the 76
//! reCAPTCHA / 1 Turnstile specs whose challenge widgets must be rendered
//! and clicked in a live DOM.
//!
//! ## Architecture
//!
//! * [`Transport`] — a `send_frame` / `recv_frame` byte-frame seam.  Real
//!   Chrome speaks WebSocket ([`WsTransport`], feature `browser`); tests
//!   inject a scripted in-memory transport so the DOM-driving logic is fully
//!   unit-testable — no browser, no OS process, no network.
//! * a thin CDP layer on top: [`WebDriver::request`] allocates a JSON-RPC
//!   id, sends the method, and drains frames until the matching
//!   response — **skipping server-pushed events** (which have no `id`),
//!   exactly the pattern every CDP client (Puppeteer-core, chrome_automation)
//!   follows.
//! * the high-level verbs the executor needs:
//!   * [`WebDriver::load`]    — `Page.navigate`, settle, read
//!     `window.location.href` + `document.documentElement.outerHTML`.
//!   * [`WebDriver::dom_submit`] — `load`, then a single `Runtime.evaluate`
//!     that fills each field by `name`, dispatches `input`/`change` (so
//!     React/Vue bindings see it, as a real user would), clicks the submit
//!     control, settles, and re-reads location + outerHTML.
//!
//! The executor then runs its own GSA-contract classification
//! (`submit success` / `submit failed` / `submit failed retry` /
//! `captcha failed`) on the returned rendered HTML, and extracts the result
//! URL via its `find url` matcher — the bridge just delivers a *real DOM
//! snapshot*, which is precisely what the original GSA hands to its own
//! classifier.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};

use super::driver::{Driver, DriverResponse};

// --- errors ----------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum CdpError {
    #[error("transport: {0}")]
    Transport(String),
    #[error("cdp error: {0}")]
    Cdp(String),
    #[error("timeout: {0}")]
    Timeout(String),
}

// --- transport seam ---------------------------------------------------------

#[async_trait]
pub trait Transport: Send + Sync {
    async fn send_frame(&self, frame: &str) -> Result<(), CdpError>;
    async fn recv_frame(&self) -> Result<String, CdpError>;
}

// --- WebSocket transport (real Chrome, feature `browser`) ------------------

/// A live WebSocket CDP transport for a Chrome we launch with
/// `--remote-debugging-port`.  The WebSocket is boxed into an `Arc<Mutex<>>`
/// so the `Transport`'s `&self` signature can send/recv.
#[cfg(feature = "browser")]
pub struct WsTransport {
    ws: tokio::sync::Mutex<tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >>,
}

#[cfg(feature = "browser")]
impl WsTransport {
    /// Connect to the DevTools WebSocket endpoint (from Chrome's
    /// `webSocketDebuggerUrl`).
    pub async fn connect(ws_url: String) -> Result<Self, CdpError> {
        let (ws, _resp) = tokio_tungstenite::connect_async(ws_url)
            .await
            .map_err(|e| CdpError::Transport(format!("ws connect: {e}")))?;
        Ok(Self {
            ws: tokio::sync::Mutex::new(ws),
        })
    }
}

#[cfg(feature = "browser")]
#[async_trait]
impl Transport for WsTransport {
    async fn send_frame(&self, frame: &str) -> Result<(), CdpError> {
        use futures_util::SinkExt;
        use tungstenite::Message;
        let mut g = self.ws.lock().await;
        g.send(Message::Text(frame.to_string().into()))
            .await
            .map_err(|e| CdpError::Transport(format!("send: {e}")))
    }

    async fn recv_frame(&self) -> Result<String, CdpError> {
        use futures_util::StreamExt;
        use tungstenite::Message;
        let mut guard = self.ws.lock().await;
        let mut stream = Box::pin(&mut *guard);
        while let Some(msg) = stream.next().await {
            if let Ok(Message::Text(t)) = msg {
                return Ok(t.to_string());
            }
        }
        Err(CdpError::Transport("stream closed".into()))
    }
}

// --- driver ----------------------------------------------------------------

/// One CDP session bound to one Chrome target.  Spawn several for parallel
/// engine runs (each owns its own target → crash isolation is preserved).
pub struct WebDriver {
    tr: Arc<dyn Transport>,
    next_id: AtomicU64,
    last_url: tokio::sync::Mutex<String>,
}

impl WebDriver {
    pub fn with_transport(tr: Arc<dyn Transport>) -> Self {
        Self {
            tr,
            next_id: AtomicU64::new(0),
            last_url: tokio::sync::Mutex::new(String::new()),
        }
    }

    #[cfg(feature = "browser")]
    pub async fn connect(ws_url: String) -> Result<Self, CdpError> {
        Ok(Self::with_transport(Arc::new(
            WsTransport::connect(ws_url).await?,
        )))
    }

    fn id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }

    /// Current known location, without a CDP round-trip.
    async fn last_snapshot(&self) -> String {
        let g = self.last_url.lock().await;
        g.clone()
    }

    /// One CDP round trip: send the method frame then drain frames until the
    /// matching response arrives (events have no `id` and are skipped).
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, CdpError> {
        let id = self.id();
        let frame = json!({ "id": id, "method": method, "params": params }).to_string();
        self.tr.send_frame(&frame).await?;

        let window = Duration::from_secs(20);
        let deadline = tokio::time::Instant::now() + window;
        loop {
            let remain = deadline
                .saturating_duration_since(tokio::time::Instant::now());
            if remain.is_zero() {
                return Err(CdpError::Timeout(method.to_string()));
            }
            let frame = tokio::time::timeout(remain, self.tr.recv_frame())
                .await
                .map_err(|_| CdpError::Timeout(method.to_string()))?
                .map_err(|e| CdpError::Transport(e.to_string()))?;
            let v: Value = serde_json::from_str(&frame)
                .map_err(|e| CdpError::Cdp(format!("frame.parse: {e}")))?;
            if v.get("id").and_then(Value::as_u64) == Some(id)
                && (v.get("result").is_some() || v.get("error").is_some())
            {
                if let Some(err) = v.get("error") {
                    let msg = err
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("cdp error");
                    return Err(CdpError::Cdp(msg.to_string()));
                }
                return Ok(v.get("result").cloned().unwrap_or(Value::Null));
            }
            // else: event or out-of-band frame — keep draining.
        }
    }

    /// `Runtime.evaluate` shorthand — returns the `{result: {value: ...}}`
    /// object.
    async fn eval(&self, expression: &str) -> Result<Value, CdpError> {
        let r = self
            .request(
                "Runtime.evaluate",
                json!({
                    "expression": expression,
                    "returnByValue": true,
                    "awaitPromise": true,
                }),
            )
            .await?;
        Ok(r
            .get("result")
            .cloned()
            .unwrap_or_else(|| json!({ "value": "" })))
    }

    /// `Page.navigate` + settle + read location/outerHTML.
    pub async fn load(&self, url: &str) -> DriverResponse {
        if let Err(e) = self
            .request(
                "Page.navigate",
                json!({ "url": url, "transitionType": "address_bar" }),
            )
            .await
        {
            return DriverResponse {
                ok: false,
                final_url: url.to_string(),
                html: String::new(),
                error: Some(e.to_string()),
            };
        }
        // Let the page settle (JS, XHR, redirects, iframes).
        let _ = tokio::time::sleep(Duration::from_millis(250)).await;
        let final_url = self
            .eval("window.location.href")
            .await
            .ok()
            .and_then(|v| v.get("value").and_then(Value::as_str).map(String::from))
            .unwrap_or_else(|| url.to_string());
        let html = self
            .eval("document.documentElement.outerHTML")
            .await
            .ok()
            .and_then(|v| v.get("value").and_then(Value::as_str).map(String::from))
            .unwrap_or_default();
        let mut g = self.last_url.lock().await;
        *g = final_url.clone();
        DriverResponse {
            ok: true,
            final_url,
            html,
            error: None,
        }
    }

    /// DOM form submit — the core of Feature #7.  Fill each resolved field
    /// by name, dispatch `input`/`change` (so JS bindings see it), click the
    /// submit control, let the callback settle, then read the result page.
    pub async fn dom_submit(&self, url: &str, fields: &HashMap<String, String>) -> DriverResponse {
        let nav = self.load(url).await;
        if !nav.ok {
            return nav;
        }
        let fields_json = serde_json::to_string(fields).unwrap_or_else(|_| "{}".into());
        // One JS payload does the fill + click — keeps the CDP round-trips
        // down to one and lets us dispatch the exact events a real user's
        // typing would.
        let expr = format!(
            r#"(function(){{
                 var fields = {fields_json};
                 var form = document.querySelector('form') || document;
                 var filled = 0;
                 for (var name in fields) {{
                   var el = form.querySelector('[name="' + name + '"]') ||
                            document.querySelector('[name="' + name + '"]');
                   if (el) {{
                     el.value = fields[name];
                     el.dispatchEvent(new Event('input', {{bubbles:true}}));
                     el.dispatchEvent(new Event('change', {{bubbles:true}}));
                     filled++;
                   }}
                 }}
                 var btn = (form && form.querySelector('button[type=submit],input[type=submit],input[type=image]')) ||
                           form.querySelector('button') ||
                           document.querySelector('button[type=submit],input[type=submit],input[type=image]') ||
                           document.querySelector('button');
                 var clicked = false;
                 if (btn) {{ btn.click(); clicked = true; }}
                 var hasCaptcha = !!document.querySelector('.g-recaptcha, .recaptcha, #hcaptcha, .cf-turnstile, iframe[src*="captcha"]');
                 return {{ filled: filled, hasForm: !!document.querySelector('form'), clicked: clicked, hasCaptcha: hasCaptcha }};
               }})()"#
        );
        let fill = match self.eval(&expr).await {
            Ok(v) => v,
            Err(e) => {
                let u = self.last_url.lock().await.clone();
                return DriverResponse {
                    ok: false,
                    final_url: u,
                    html: nav.html,
                    error: Some(format!("dom fill: {e}")),
                };
            }
        };
        // Let the (JS) submit callback complete — XHR, redirect, captcha.
        let filled = fill
            .get("filled")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .min(10);
        let settle_ms = 300u64 + 100 * filled;
        let _ = tokio::time::sleep(Duration::from_millis(settle_ms)).await;
        let final_url = match self
            .eval("window.location.href")
            .await
            .ok()
            .and_then(|v| v.get("value").and_then(Value::as_str).map(String::from))
        {
            Some(u) => u,
            None => self.last_snapshot().await,
        };
        let html = self
            .eval("document.documentElement.outerHTML")
            .await
            .ok()
            .and_then(|v| v.get("value").and_then(Value::as_str).map(String::from))
            .unwrap_or_default();
        let mut g = self.last_url.lock().await;
        *g = final_url.clone();
        DriverResponse {
            ok: true,
            final_url,
            html,
            error: None,
        }
    }
}

// --- Driver impl (feature `browser`) ----------------------------------------

#[cfg(feature = "browser")]
#[async_trait]
impl Driver for WebDriver {
    async fn navigate(&self, url: String) -> DriverResponse {
        self.load(&url).await
    }
    async fn submit(
        &self,
        url: String,
        fields: HashMap<String, String>,
        _timeout: Duration,
    ) -> DriverResponse {
        self.dom_submit(&url, &fields).await
    }
}

// ===========================================================================
//  Tests — a scripted in-memory CDP transport makes the whole module
//  unit-testable with no browser / no OS process / no network.
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    struct QueueTransport {
        sent: Mutex<Vec<Value>>,
        replies: Mutex<VecDeque<Value>>,
    }

    impl QueueTransport {
        fn new() -> Self {
            Self {
                sent: Mutex::new(Vec::new()),
                replies: Mutex::new(VecDeque::new()),
            }
        }

        /// Stage the frames that a `load()` call will receive in order:
        /// Page.navigate resp → location.href resp (Runtime.evaluate) →
        /// outerHTML resp (Runtime.evaluate).
        ///
        /// `request()` unwraps the CDP envelope once (`.result`); `eval()`
        /// then unwraps `Runtime.evaluate`'s inner `.result`.  Together
        /// that gives us the evaluated value.
        fn stage_load(&self, url: &str, html: &str) {
            let mut q = self.replies.lock().unwrap();
            // id=0: Page.navigate
            q.push_back(json!({ "id": 0, "result": { "frameId": "F", "loaderId": "L" } }));
            // id=1: Runtime.evaluate ("window.location.href")
            q.push_back(json!({ "id": 1, "result": { "result": { "type": "string", "value": url } } }));
            // id=2: Runtime.evaluate ("document.documentElement.outerHTML")
            q.push_back(json!({ "id": 2, "result": { "result": { "type": "string", "value": html } } }));
        }

        /// Stage `load` with one *event* interleaved — the driver must skip
        /// it (event has `method`, no `id`).
        fn stage_load_with_event(&self, url: &str, html: &str) {
            let mut q = self.replies.lock().unwrap();
            q.push_back(json!({ "id": 0, "result": { "frameId": "F", "loaderId": "L" } }));
            q.push_back(json!({ "method": "Page.frameNavigated", "params": { "frame": { "id": "F" } } }));
            q.push_back(json!({ "id": 1, "result": { "result": { "type": "string", "value": url } } }));
            q.push_back(json!({ "id": 2, "result": { "result": { "type": "string", "value": html } } }));
        }
    }

    #[async_trait]
    impl Transport for QueueTransport {
        async fn send_frame(&self, frame: &str) -> Result<(), CdpError> {
            let v = serde_json::from_str::<Value>(frame)
                .map_err(|e| CdpError::Cdp(e.to_string()))?;
            self.sent.lock().unwrap().push(v);
            Ok(())
        }

        async fn recv_frame(&self) -> Result<String, CdpError> {
            self.replies
                .lock()
                .unwrap()
                .pop_front()
                .map(|v| v.to_string())
                .ok_or_else(|| CdpError::Transport("queue drained; recv_frame called too often".into()))
        }
    }

    fn driver_with(q: Arc<QueueTransport>) -> WebDriver {
        WebDriver::with_transport(q.clone() as Arc<dyn Transport>)
    }

    // 1. load(): CDP verb order + driver response shape --------------------

    #[tokio::test]
    async fn load_sends_page_navigate_then_evals_location_and_html() {
        let q = Arc::new(QueueTransport::new());
        q.stage_load("https://engine-x.example/login?from=browser", "<html><body>login form</body></html>");
        let d = driver_with(q.clone());
        let r = d.load("https://engine-x.example/login").await;
        assert!(r.ok, "load ok: {r:?}");
        assert_eq!(r.final_url, "https://engine-x.example/login?from=browser");
        assert!(r.html.contains("login form"));

        let sent = q.sent.lock().unwrap();
        let meths: Vec<&str> = sent
            .iter()
            .map(|v| v["method"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(meths.as_slice(), ["Page.navigate", "Runtime.evaluate", "Runtime.evaluate"]);
        assert_eq!(sent[1]["params"]["expression"], "window.location.href");
        assert_eq!(sent[2]["params"]["expression"], "document.documentElement.outerHTML");
    }

    // 2. request(): must skip CDP server events while hunting for a reply --

    #[tokio::test]
    async fn request_skips_server_events_until_it_finds_matching_reply() {
        let q = Arc::new(QueueTransport::new());
        q.stage_load_with_event(
            "https://engine-x.example/done",
            "<html><body>You submitted</body></html>",
        );
        let d = driver_with(q.clone());
        let r = d.load("https://engine-x.example/submit").await;
        assert!(r.ok, "load ok despite interleaved event: {r:?}");
        assert_eq!(r.final_url, "https://engine-x.example/done");
        let q = q.replies.lock().unwrap();
        assert!(q.is_empty(), "expected drain; {} remain", q.len());
    }

    // 3. request(): must surface CDP error objects as CdpError -------------

    #[tokio::test]
    async fn request_surfaces_cdp_error_objects() {
        let q = Arc::new(QueueTransport::new());
        q.replies
            .lock()
            .unwrap()
            .push_back(json!({ "id": 0, "error": { "code": 53, "message": "Cannot navigate to invalid URL" } }));
        let d = driver_with(q.clone());
        let r = d.load("not://a,url").await;
        assert!(!r.ok, "load not-ok on CDP error: {r:?}");
        assert!(r.error.as_deref().unwrap_or("").contains("invalid"), "got: {:?}", r.error);
    }

    // 4. dom_submit(): fills each field by name + embeds resolved values ---

    #[tokio::test]
    async fn dom_submit_fills_fields_by_name_and_clicks() {
        let q = Arc::new(QueueTransport::new());
        let url = "https://engine.test/submit";
        // load() uses id 0,1,2 — navigate, location, outerHTML
        q.stage_load(url, "<html><form><input name=\"url\"/><textarea name=\"description\"></textarea><button>Go</button></form></html>");
        // dom_submit fill eval (id=3) returns fill metadata;
        // post-submit location read (id=4) + outerHTML read (id=5).
        q.replies.lock().unwrap().push_back(json!({"id":3,"result":{"result":{"value":{"filled":2,"form":true,"clicked":true,"hasCaptcha":false}}}}));
        q.replies.lock().unwrap().push_back(json!({"id":4,"result":{"result":{"value":"https://engine.test/submit?done=1"}}}));
        q.replies.lock().unwrap().push_back(json!({"id":5,"result":{"result":{"value":"<html>You submitted</html>"}}}));

        let fields = HashMap::from([
            ("url".to_string(), "https://mysite.example/".into()),
            ("description".to_string(), "a backlink from our site".into()),
        ]);
        let d = driver_with(q.clone());
        let r = d.dom_submit(url, &fields).await;
        assert!(r.ok, "dom_submit ok: {r:?}");
        assert_eq!(r.final_url, "https://engine.test/submit?done=1");
        assert!(r.html.contains("You submitted"));

        let sent: Vec<Value> = q.sent.lock().unwrap().clone();
        let fills: Vec<&str> = sent
            .iter()
            .filter(|v| v["method"] == "Runtime.evaluate")
            .filter_map(|v| {
                let e = v["params"]["expression"].as_str()?;
                if e.contains("dispatchEvent") && e.contains("click") {
                    Some(e)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(fills.len(), 1, "one fill+click expression; sent: {sent:?}");
        let fill = fills[0];
        for needle in [
            "[name=",
            "dispatchEvent",
            "input",
            "change",
            "click()",
            "https://mysite.example/",
            "a backlink from our site",
            "url",
            "description",
        ] {
            assert!(fill.contains(needle), "fill missing {needle:?}:\n{fill}");
        }
    }

    // 5. dom_submit(): a failure in the fill eval must be surfaced ---------

    #[tokio::test]
    async fn dom_submit_surfaces_fill_errors() {
        let q = Arc::new(QueueTransport::new());
        let url = "https://engine.test/submit";
        // Stage the three frames load() needs.
        q.stage_load(url, "<html><form></form></html>");
        // Then the fill eval must be a CDP error.  Replace the tail of the
        // queue with that error so it lands as the next frame.
        let mut qguard = q.replies.lock().unwrap();
        qguard.clear();
        // Re-push the 3 load frames, then the error in the right id slot.
        qguard.push_back(json!({"id":0,"result":{"frameId":"F"}}));
        qguard.push_back(json!({"id":1,"result":{"value":"https://engine.test/submit"}}));
        qguard.push_back(json!({"id":2,"result":{"value":"<form></form>"}}));
        // The fill eval (id=3) responds with error:
        qguard.push_back(json!({
            "id": 3,
            "error": { "code": 1, "message": "Execution context was destroyed" }
        }));
        drop(qguard);
        let d = driver_with(q.clone());
        let fields = HashMap::from([("url".to_string(), "https://mysite.example/".into())]);
        let r = d.dom_submit(url, &fields).await;
        assert!(!r.ok, "dom_submit not-ok: {r:?}");
        assert!(
            r.error.as_deref().unwrap_or("").contains("Execution context"),
            "error: {:?}",
            r.error
        );
    }

    // 6. Driver-trait smoke when `browser` feature is on --------------------

    #[tokio::test]
    async fn webdriver_implements_driver_trait_contract() {
        let q = Arc::new(QueueTransport::new());
        q.stage_load("https://engine-x.example/ok", "<html>ok</html>");
        let d = driver_with(q.clone());
        #[cfg(feature = "browser")]
        {
            let dyn_d: &dyn Driver = &d;
            let res = dyn_d.navigate("https://engine-x.example".into()).await;
            assert!(res.ok);
            assert_eq!(res.final_url, "https://engine-x.example/ok");
        }
        #[cfg(not(feature = "browser"))]
        {
            let res = d.load("https://engine-x.example").await;
            assert!(res.ok);
        }
    }
}
