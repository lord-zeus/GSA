//! F#29 — free reCAPTCHA v2 image-grid solver: CDP + a local vision oracle.
//!
//! No API key, no credits. The chain is:
//!   1. open the engine page in a fresh headless Edge tab (CDP :9222),
//!   2. click the "I'm not a robot" checkbox,
//!   3. if Google upgrades to the image grid: screenshot → hand the PNG to a
//!      local **vision oracle** (default `http://127.0.0.1:8090/solve`, the
//!      bundled `tools/captcha_oracle.py` human-click service — or any model
//!      service speaking the same tiny protocol) → click the returned points,
//!   4. click Next, wait for `g-recaptcha-response`, return the token.
//!
//! Oracle protocol (free, local, human- or model-driven):
//! ```json
//! POST {oracle}/solve   {"image": "<b64 png>", "question": "...",
//!                        "page_url": "...", "context": "recaptcha-v2"}
//! → 200 {"points": [{"x":..,"y":..}, ...], "next": {"x":..,"y":..} | null}
//! ```
//! `points` are **viewport coordinates** (the solver clicks them with
//! Input.dispatchMouseEvent — no bframe math, parent-page hit-testing).

use crate::browser_bridge::{Transport, WsTransport};
use crate::captcha::{CaptchaContext, CaptchaError, CaptchaSolver};
use serde_json::{json, Value};
use std::time::Duration;

const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:9222";
const DEFAULT_ORACLE: &str = "http://127.0.0.1:8090";

/// F#29 solver. `endpoint` = CDP HTTP endpoint of the headless browser the
/// API server already runs for WebDriver; `oracle` = vision-oracle base
/// URL (None → unsolvable with a clear message, chain continues).
pub struct CdpVisionSolver {
    pub endpoint: String,
    pub oracle: Option<String>,
    /// Image-grid rounds before giving up (Google may re-roll the grid).
    pub rounds: u8,
}

impl Default for CdpVisionSolver {
    fn default() -> Self {
        Self::new()
    }
}

impl CdpVisionSolver {
    /// Build from env overrides (`SER_CDP_ENDPOINT`, `SER_CAPTCHA_ORACLE`)
    /// with the 127.0.0.1 defaults. An explicit empty `SER_CAPTCHA_ORACLE`
    /// disables the oracle (checkbox-only solves still work).
    pub fn new() -> Self {
        Self {
            endpoint: env_nonempty("SER_CDP_ENDPOINT").unwrap_or_else(|| DEFAULT_ENDPOINT.to_string()),
            oracle: env_nonempty("SER_CAPTCHA_ORACLE").or_else(|| {
                std::env::var("SER_CAPTCHA_ORACLE")
                    .ok()
                    .filter(|v| v.trim().is_empty())
                    .map(|_| Some(String::new()))
                    .unwrap_or_else(|| Some(DEFAULT_ORACLE.to_string()))
            }),
            rounds: 2,
        }
    }

    /// Force a specific oracle base URL ("none" disables).
    pub fn with_oracle(mut self, oracle: &str) -> Self {
        self.oracle = if oracle.eq_ignore_ascii_case("none") {
            Some(String::new())
        } else {
            Some(oracle.to_string())
        };
        self
    }

    fn oracle_base(&self) -> Option<&str> {
        let s = self.oracle.as_deref()?.trim().trim_end_matches('/');
        if s.is_empty() { None } else { Some(s) }
    }
}

fn env_nonempty(var: &str) -> Option<String> {
    std::env::var(var).ok().filter(|v| !v.trim().is_empty())
}

/// Minimal request/response CDP client bound to ONE page target.
pub struct Cdp {
    ws: Box<dyn Transport>,
    next_id: u64,
    /// When set (attached out-of-process iframe), every frame is tagged
    /// with this `sessionId` so CDP routes it to the attached target.
    session: Option<String>,
}

impl Cdp {
    /// F#29 probe helper: build a Cdp client from a target websocket URL.
    pub async fn connect_ws(ws_url: &str) -> Result<Self, CaptchaError> {
        let t = WsTransport::connect(ws_url.to_string())
            .await
            .map_err(|e| CaptchaError::Network { message: format!("ws connect: {e}") })?;
        Ok(Self { ws: Box::new(t), next_id: 1, session: None })
    }

    /// Route every message through an attached session (flatten: true
    /// attach target). The ws itself stays on the browser/page endpoint.
    pub fn with_session(mut self, session: String) -> Self {
        self.session = Some(session);
        self
    }

    /// Switch the session routing (None = this connection's own target).
    /// Sessions are bound to the connection that created them, so flip
    /// this when driving attached iframes on the same ws.
    pub fn set_session(&mut self, session: Option<String>) {
        self.session = session;
    }

    pub async fn send(&mut self, method: &str, params: Value) -> Result<Value, CaptchaError> {
        let id = self.next_id;
        self.next_id += 1;
        let frame = match &self.session {
            Some(s) => json!({ "id": id, "method": method, "params": params, "sessionId": s }).to_string(),
            None => json!({ "id": id, "method": method, "params": params }).to_string(),
        };
        self.ws
            .send_frame(&frame)
            .await
            .map_err(|e| CaptchaError::Network { message: format!("cdp send {method}: {e}") })?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) =>
                    return Err(CaptchaError::Network { message: format!("cdp timeout on {method}") }),
                r = self.ws.recv_frame() => match r {
                    Ok(s) => {
                        let v: Value = match serde_json::from_str(&s) {
                            Ok(v) => v,
                            Err(_) => continue,
                        };
                        let Some(msg_id) = v.get("id").and_then(|i| i.as_u64()) else {
                            continue; // event frame
                        };
                        if msg_id != id {
                            continue;
                        }
                        if let Some(err) = v.get("error") {
                            return Err(CaptchaError::Provider {
                                code: err.get("code").and_then(|c| c.as_i64()).map(|c| c.to_string()).unwrap_or_else(|| "cdp".into()),
                                message: err.get("message").and_then(|m| m.as_str()).unwrap_or("").to_string(),
                            });
                        }
                        return Ok(v.get("result").cloned().unwrap_or(Value::Null));
                    }
                    Err(e) => return Err(CaptchaError::Network { message: format!("cdp recv: {e}") }),
                }
            }
        }
    }

    /// Evaluate JS in the top page; returns Null on error (best-effort).
    pub async fn eval(&mut self, expr: &str) -> Result<Value, CaptchaError> {
        let res = self
            .send(
                "Runtime.evaluate",
                json!({ "expression": expr, "returnByValue": true, "awaitPromise": true }),
            )
            .await?;
        Ok(res
            .get("result")
            .and_then(|r| r.get("value"))
            .cloned()
            .unwrap_or(Value::Null))
    }
}

pub async fn click(cdp: &mut Cdp, x: f64, y: f64) -> Result<(), CaptchaError> {
    // Human-ish trajectory: approach, press, short hold, release. Verified
    // to pass localendar's reCAPTCHA v2 where a raw pressed/released pair
    // is sometimes dropped as synthetic.
    let _ = cdp
        .send(
            "Input.dispatchMouseEvent",
            json!({ "type": "mouseMoved", "x": x - 24.0, "y": y - 24.0, "button": "none" }),
        )
        .await;
    tokio::time::sleep(Duration::from_millis(40)).await;
    for typ in ["mousePressed", "mouseReleased"] {
        cdp
            .send(
                "Input.dispatchMouseEvent",
                json!({
                    "type": typ,
                    "x": x, "y": y,
                    "button": "left", "buttons": if typ == "mousePressed" { 1 } else { 0 },
                    "clickCount": 1
                }),
            )
            .await?;
        tokio::time::sleep(Duration::from_millis(70)).await;
    }
    Ok(())
}

/// Poll for a reCAPTCHA v2 token: the hidden `g-recaptcha-response`
/// field, or — on pages (like Localendar) that render only
/// `<div class="g-recaptcha">` with no input field — the
/// `grecaptcha.getResponse()` API.
async fn wait_token(cdp: &mut Cdp, timeout: Duration) -> Option<String> {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if let Ok(v) = cdp
            .eval(
                "(() => { const el = document.querySelector('textarea[name=g-recaptcha-response], input[name=g-recaptcha-response]'); if (el && el.value && el.value.length > 20) return el.value; try { if (window.grecaptcha && grecaptcha.getResponse) { const v2 = grecaptcha.getResponse(); if (v2 && v2.length > 20) return v2; } } catch (e) {} return ''; })()",
            )
            .await
        {
            if let Some(s) = v.as_str().filter(|s| s.chars().count() > 20) {
                return Some(s.to_string());
            }
        }
        tokio::time::sleep(Duration::from_millis(700)).await;
    }
    None
}

const PROBE_WIDGET_JS: &str = r#"(() => {
  // Pick the LARGEST visible .g-recaptcha: pages can carry hidden
  // duplicates (modals), and DOM order is not a reliable pick.
  let el = null, best = 1;
  for (const e of document.querySelectorAll('.g-recaptcha, [data-sitekey]')) {
    const r = e.getBoundingClientRect();
    if (r.width > 10 && r.height > 10 && r.width * r.height > best) { best = r.width * r.height; el = e; }
  }
  if (!el) { for (const s of ['iframe[src*="recaptcha/api2/anchor"]', 'iframe[src*="recaptcha"]']) { const f = document.querySelector(s); if (f) { el = f; break; } } }
  if (!el) return { found: false };
  el.scrollIntoView({block: 'center', behavior: 'instant'});
  const b = el.getBoundingClientRect();
  // v2 checkbox sits ~28px inside the left edge, vertically centred.
  // (verified working against localendar: +28/+h*0.5 lands the checkbox;
  //  +14 misses to the frame edge and the click is dropped.)
  // NOTE: DOMRect exposes .width/.height — .w/.h are undefined and make
  // cy NaN→null→0, sending the click to the top-left corner.
  return { found: true, x: b.x, y: b.y, w: b.width, h: b.height,
           cx: b.x + 28, cy: b.y + b.height / 2 };
})()"#;

const CHALLENGE_JS: &str = r#"(() => {
  const its = Array.from(document.querySelectorAll('iframe'))
    .filter(f => (f.src || '').includes('recaptcha'));
  const c = its.find(f => f.getBoundingClientRect().height > 150);
  if (!c) return { challenge: false };
  const b = c.getBoundingClientRect();
  return { challenge: true, x: b.x, y: b.y, w: b.width, h: b.height };
})()"#;

/// Minimal percent-encoding for the `/json/new?{url}` query value.
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 16);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

fn ws_url(http_url: &str) -> String {
    if let Some(rest) = http_url.strip_prefix("http://") {
        format!("ws://{rest}")
    } else if let Some(rest) = http_url.strip_prefix("https://") {
        format!("wss://{rest}")
    } else {
        http_url.to_string()
    }
}

/// Open a fresh page target pointed at `url` under CDP endpoint `ep`.
pub async fn new_tab(ep: &str, url: &str) -> Result<(String, String), CaptchaError> {
    let base = ep.trim_end_matches('/');
    let new_url = format!("{base}/json/new?{}", esc(url));
    let resp = reqwest::Client::new()
        .put(&new_url)
        .send()
        .await
        .or_else(|e| {
            // Older CDP accepts POST.
            Err(CaptchaError::Network { message: format!("new-tab {e}") })
        })?;
    let v: Value = resp.json().await.map_err(|e| CaptchaError::Network { message: e.to_string() })?;
    let id = v.get("id").and_then(|i| i.as_str()).ok_or_else(|| CaptchaError::Unsolvable {
        reason: format!("cdp endpoint {base} did not return a target id"),
    })?;
    let ws = v.get("webSocketDebuggerUrl").and_then(|i| i.as_str()).ok_or_else(|| CaptchaError::Unsolvable {
        reason: format!("cdp target has no webSocketDebuggerUrl (kind={:?})", v.get("type")),
    })?;
    Ok((id.to_string(), ws.to_string()))
}

pub async fn close_tab(ep: &str, id: &str) {
    let base = ep.trim_end_matches('/');
    let _ = reqwest::Client::new().delete(format!("{base}/json/close/{id}")).send().await;
}

/// Ask the vision oracle for the tiles to click.
async fn oracle_ask(
    oracle_base: &str,
    image_b64: &str,
    question: Option<&str>,
    page_url: &str,
) -> Result<(Vec<(f64, f64)>, Option<(f64, f64)>), CaptchaError> {
    let body = json!({
        "image": image_b64,
        "question": question,
        "page_url": page_url,
        "context": "recaptcha-v2-image-grid"
    });
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(300))
        .build()
        .map_err(|e| CaptchaError::Network { message: e.to_string() })?;
    let resp = client
        .post(format!("{oracle_base}/solve"))
        .json(&body)
        .send()
        .await
        .map_err(|e| CaptchaError::Network {
            message: format!("oracle {oracle_base} unreachable: {e}"),
        })?;
    let status = resp.status();
    let v: Value = resp
        .json()
        .await
        .map_err(|e| CaptchaError::Provider { code: status.as_str().to_string(), message: e.to_string() })?;
    if !status.is_success() {
        return Err(CaptchaError::Provider {
            code: status.as_str().to_string(),
            message: v.get("error").and_then(|m| m.as_str()).unwrap_or("oracle rejected").to_string(),
        });
    }
    let mut points: Vec<(f64, f64)> = Vec::new();
    if let Some(arr) = v.get("points").and_then(|a| a.as_array()) {
        for p in arr {
            if let (Some(x), Some(y)) = (p.get("x").and_then(|x| x.as_f64()), p.get("y").and_then(|y| y.as_f64())) {
                points.push((x, y));
            }
        }
    }
    if points.is_empty() {
        return Err(CaptchaError::EmptyAnswer);
    }
    let next = v.get("next").and_then(|n| {
        n.get("x").and_then(|x| x.as_f64()).zip(n.get("y").and_then(|y| y.as_f64()))
    });
    Ok((points, next))
}

/// Best-effort read of the challenge instruction from the reCAPTCHA bframe
/// (unstable — the oracle sees the full screenshot anyway).
async fn read_question(ep: &str) -> Option<String> {
    let url = format!("{}/json/list", ep.trim_end_matches('/'));
    let v: Value = reqwest::Client::new().get(&url).send().await.ok()?.json().await.ok()?;
    for t in v.as_array()? {
        if t.get("type").and_then(|x| x.as_str()) != Some("iframe") {
            continue;
        }
        if !t.get("url").and_then(|u| u.as_str()).unwrap_or("").contains("recaptcha") {
            continue;
        }
        let ws = t.get("webSocketDebuggerUrl").and_then(|u| u.as_str())?.to_string();
        let wt = WsTransport::connect(ws).await.ok()?;
        let box_transport: Box<dyn Transport> = Box::new(wt);
        // Small request: Runtime.evaluate of the instruction text.
        let frame = json!({"id": 1, "method": "Runtime.evaluate", "params": {
            "expression": "(()=>{const s=document.querySelector('.rc-imageselect-instruction-text, .rc-dice-icon, .rc-imageselect .rc-imageselect-instruction');return s?s.textContent.trim():'';})()",
            "returnByValue": true
        }}).to_string();
        let _ = box_transport.send_frame(&frame).await;
        for _ in 0..40 {
            match box_transport.recv_frame().await {
                Ok(s) => {
                    if let Ok(m) = serde_json::from_str::<Value>(&s) {
                        if m.get("id").and_then(|i| i.as_u64()) == Some(1) {
                            return m.get("result").and_then(|r| r.get("value"))
                                .and_then(|v| v.as_str())
                                .filter(|s| !s.is_empty())
                                .map(|s| s.to_string());
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                _ => return None,
            }
        }
    }
    None
}

/// Hand-rolled base64 decode (png is base64 standard with padding, no deps).
pub fn b64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut acc: u32 = 0;
    let mut bits = 0;
    for b in s.bytes() {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ if b == b'\r' || b == b'\n' || b == b' ' => continue,
            _ => return None,
        } as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// Screenshot-pixel → CSS-viewport scale factor (handles devicePixelRatio>1).
async fn viewport_scale(cdp: &mut Cdp, png_b64: &str) -> (f64, f64) {
    let dims: Option<(f64, f64)> = cdp
        .eval("JSON.stringify([window.innerWidth||1, window.innerHeight||1])")
        .await
        .ok()
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .and_then(|s| serde_json::from_str::<Vec<f64>>(&s).ok())
        .map(|v| (v[0].max(1.0), v[1].max(1.0)));
    let (iw, ih) = dims.unwrap_or((1.0, 1.0));
    if let Some(b) = b64_decode(png_b64).filter(|b| b.len() > 24) {
        let w = u32::from_be_bytes([b[16], b[17], b[18], b[19]]) as f64;
        let h = u32::from_be_bytes([b[20], b[21], b[22], b[23]]) as f64;
        if b[1..8] == *b"PNG\r\n\x1a\n" {
            return ((w.max(1.0) / iw), (h.max(1.0) / ih));
        }
    }
    (1.0, 1.0)
}

#[async_trait::async_trait]
impl CaptchaSolver for CdpVisionSolver {
    fn name(&self) -> &'static str {
        "cdp-vision"
    }

    async fn solve(&self, ctx: &CaptchaContext) -> Result<String, CaptchaError> {
        // 0) The sitekey is an *informational* input for API-based solvers
        //    (2Captcha / Anti-Captcha), NOT a functional requirement for
        //    this CDP+oracle path.  `drive()` (below) locates the checkbox
        //    by probing the live DOM via `PROBE_WIDGET_JS`, which finds the
        //    anchor iframe regardless of how the site injected the widget.
        //    So: extract the sitekey opportunistically for logging, but
        //    don't hard-fail when the caller couldn't provide one (common
        //    for JS-API-injected widgets on SPAs like Localendar, where the
        //    sitekey only exists in the anchor iframe URL — not in the
        //    page's static HTML).
        let sitekey: Option<String> = ctx
            .sitekey
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .or_else(|| crate::captcha::extract_sitekey(&ctx.html));
        if sitekey.is_none() {
            eprintln!(
                "[captcha_cdp] caller provided no sitekey; relying on live-DOM discovery (drive)"
            );
        }
        let page = ctx.url.as_deref().filter(|u| u.starts_with("http")).ok_or_else(|| {
            CaptchaError::Unsolvable { reason: "cdp-vision needs the page URL to drive the widget".into() }
        })?;

        // 1) Fresh tab.
        let (tid, ws) = new_tab(&self.endpoint, page).await?;
        let result = CdpVisionSolver::drive(self, ws, page).await;
        close_tab(&self.endpoint, &tid).await;
        result
    }
}

impl CdpVisionSolver {
    /// Drive one captcha to completion on an already-opened CDP tab.
    async fn drive(s: &Self, ws_url: String, page: &str) -> Result<String, CaptchaError> {
        let box_transport: Box<dyn Transport> = Box::new(
            WsTransport::connect(ws_url)
                .await
                .map_err(|e| CaptchaError::Network { message: format!("cdp ws connect: {e}") })?,
        );
        let mut cdp = Cdp { ws: box_transport, next_id: 1, session: None };
        let _ = cdp.send("Page.enable", json!({})).await;
        let _ = cdp.send("Runtime.enable", json!({})).await;
        let _ = cdp.send("Page.navigate", json!({ "url": page })).await;
        // Wait for the page to settle + the widget JS to render.
        for _ in 0..20 {
            let rs = cdp.eval("document.readyState").await.ok().and_then(|v| v.as_str().map(|s| s.to_string()));
            if rs.as_deref() == Some("complete") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        tokio::time::sleep(Duration::from_millis(1500)).await;

        // 2) Find + click the checkbox.
        // JS-rendered pages can inject the widget seconds after
        // readyState=complete — poll for it instead of failing on one shot.
        let mut info_opt: Option<Value> = None;
        for _ in 0..30 {
            if let Ok(v) = cdp.eval(PROBE_WIDGET_JS).await {
                if v.get("found").and_then(|f| f.as_bool()).unwrap_or(false) {
                    info_opt = Some(v);
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        let info = info_opt.ok_or_else(|| CaptchaError::Unsolvable {
            reason: "reCAPTCHA widget not found in the DOM after load".into(),
        })?;
        let cx = info.get("cx").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let cy = info.get("cy").and_then(|v| v.as_f64()).unwrap_or(0.0);
        // let the (instant) scroll fully settle so the measured
        // coordinates still match the painted layout at click time
        tokio::time::sleep(Duration::from_millis(250)).await;
        click(&mut cdp, cx, cy).await?;

        // 3) Some widgets pass straight to a token (good reputation / invisible).
        if let Some(tok) = wait_token(&mut cdp, Duration::from_secs(8)).await {
            return Ok(tok);
        }

        // 3b) Nothing happened — the click may have been dropped (layout
        // shift). Re-measure and click once more before calling it unsolvable.
        if let Ok(info2) = cdp.eval(PROBE_WIDGET_JS).await {
            if info2.get("found").and_then(|v| v.as_bool()).unwrap_or(false) {
                let cx2 = info2.get("cx").and_then(|v| v.as_f64()).unwrap_or(cx);
                let cy2 = info2.get("cy").and_then(|v| v.as_f64()).unwrap_or(cy);
                tokio::time::sleep(Duration::from_millis(250)).await;
                let _ = click(&mut cdp, cx2, cy2).await;
                if let Some(tok) = wait_token(&mut cdp, Duration::from_secs(8)).await {
                    return Ok(tok);
                }
            }
        }

        // 4) Image grid rounds.
        let mut last_err = CaptchaError::Unsolvable { reason: "no image challenge detected after checkbox".into() };
        for _ in 0..s.rounds {
            let ch = cdp.eval(CHALLENGE_JS).await?;
            let is_challenge = ch.get("challenge").and_then(|v| v.as_bool()).unwrap_or(false);
            if !is_challenge {
                last_err = CaptchaError::Unsolvable {
                    reason: "checkbox clicked but the widget never showed an image grid and produced no token (blocked or invisible)".into(),
                };
                break;
            }
            let shot = cdp.send("Page.captureScreenshot", json!({ "format": "png" })).await?;
            let b64 = shot.get("data").and_then(|d| d.as_str()).ok_or_else(|| {
                CaptchaError::Network { message: "screenshot without data".into() }
            })?;
            let (sx, sy) = viewport_scale(&mut cdp, b64).await;
            let question = read_question(&s.endpoint).await;

            let oracle_base = s.oracle_base().ok_or_else(|| {
                CaptchaError::Unsolvable {
                    reason: "vision oracle not configured (set SER_CAPTCHA_ORACLE or start tools/captcha_oracle.py)".into(),
                }
            })?;
            let (points, next) = oracle_ask(oracle_base, b64, question.as_deref(), page).await?;
            for (x, y) in points {
                click(&mut cdp, x / sx, y / sy).await?;
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            tokio::time::sleep(Duration::from_millis(600)).await;
            // Next/Verify button: oracle point if given (screenshot px → CSS),
            // else bottom-right of the challenge iframe.
            let (nx, ny) = match next {
                Some(n) => (n.0 / sx, n.1 / sy),
                None => (
                    ch.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0) + ch.get("w").and_then(|v| v.as_f64()).unwrap_or(0.0) - 55.0,
                    ch.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0) + ch.get("h").and_then(|v| v.as_f64()).unwrap_or(0.0) - 22.0,
                ),
            };
            click(&mut cdp, nx, ny).await?;

            if let Some(tok) = wait_token(&mut cdp, Duration::from_secs(10)).await {
                return Ok(tok);
            }
            tokio::time::sleep(Duration::from_millis(800)).await;
        }
        Err(last_err)
    }
}
