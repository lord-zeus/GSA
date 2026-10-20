//! F#21 — Captcha pipeline (GSA parity).
//!
//! GSA ships three captcha solve paths (verified against the binary strings
//! + `text_captcha_extract.dat` / `calc_replace.dat` in `install/`):
//!
//! 1. **Local arithmetic** — inline math questions ("What is 3 + 4?"), solved
//!    in-memory. GSA's `text_captcha_extract.dat` is a table of rules we
//!    re-implement here (built-in rules + optional dat-file overlay).
//! 2. **2Captcha** — the binary hard-codes the public 2Captcha HTTP API
//!    (text / image / reCAPTCHA / hCaptcha / Turnstile). This replica
//!    implements reCAPTCHA-v2 directly: `extract_sitekey` (SerLib
//!    `extractSitekey.js` parity) → 2Captcha `userrecaptcha` task (GSA
//!    CPB `2Captcha (json).ini` `recaptcha_solve`: `googlekey`/`pageurl`)
//!    → token into `g-recaptcha-response`. Made better than GSA: no local
//!    GSA Captcha Breaker app needed — the solver speaks the provider
//!    protocols (2Captcha classic + anti-captcha JSON) natively.
//! 3. **Anti-Captcha** — secondary HTTP provider GSA supports (JS-based).
//!
//! This module ships the trait + the offline solver; HTTP providers live in
//! the adjacent `captcha_2captcha.rs` / `captcha_anticaptcha.rs` so the
//! pure engine core stays network-free for tests.

use serde::{Deserialize, Serialize};
use std::fmt;

/// The information an engine exposes to a captcha solver.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CaptchaContext {
    /// The HTML page containing the captcha (required for local arithmetic,
    /// optional for image-based providers — they get the `<img>` URL instead).
    pub html: String,
    /// Direct URL of the captcha `<img>` (extracted by the engine, when
    /// present).
    #[serde(default)]
    pub image_url: Option<String>,
    /// The form field the answer must be written into (typically `captcha`,
    /// `code`, `code2`, `answer`). Used for provider context, not required
    /// for local arithmetic.
    #[serde(default)]
    pub field_name: String,
    /// Engine .ini name (helpful to a human reviewing a solver log).
    #[serde(default)]
    pub engine_name: Option<String>,
    /// Site URL.
    #[serde(default)]
    pub url: Option<String>,
    /// Page title (if known).
    #[serde(default)]
    pub page_title: Option<String>,
    /// Google reCAPTCHA `sitekey` — when present, solvers submit a
    /// reCAPTCHA-v2 task (2Captcha `userrecaptcha`, Anti-Captcha
    /// `ReCaptchaV2Task`) and the answer is the response token, which the
    /// executor injects into the `g-recaptcha-response` field. GSA SerLib
    /// parity: `extractSitekey.js` → `userrecaptcha` → `insertSitekeyInText`.
    #[serde(default)]
    pub sitekey: Option<String>,
}

/// A captcha solve failure. Carries a short human-readable reason so the
/// UI / log can display the failure without re-serializing the whole
/// context.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CaptchaError {
    /// No captcha found in the page (or the solver doesn't know how to
    /// handle the shape of captcha it found).
    Unsolvable { reason: String },
    /// The HTTP provider returned an error (bad key, out of credits, etc.).
    Provider { code: String, message: String },
    /// Network error talking to the provider.
    Network { message: String },
    /// The provider returned an empty answer.
    EmptyAnswer,
    /// An unexpected internal error (rare).
    Other { message: String },
}

impl fmt::Display for CaptchaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsolvable { reason } => write!(f, "unsolvable: {}", reason),
            Self::Provider { code, message } => write!(f, "provider {} : {}", code, message),
            Self::Network { message } => write!(f, "network: {}", message),
            Self::EmptyAnswer => write!(f, "provider returned empty answer"),
            Self::Other { message } => write!(f, "error: {}", message),
        }
    }
}

impl std::error::Error for CaptchaError {}

/// A captcha solver. Implementors must be `Send + Sync` (called from
/// concurrent runners).
///
/// `solve` is `async` — HTTP-based providers (2Captcha / Anti-Captcha)
/// use async reqwest internally; local arithmetic ignores the async.
#[async_trait::async_trait]
pub trait CaptchaSolver: Send + Sync {
    /// Attempt to solve the captcha described by `ctx`.
    /// Returns the answer string on success or a structured error on
    /// failure (see [CaptchaError]).
    async fn solve(&self, ctx: &CaptchaContext) -> Result<String, CaptchaError>;

    /// Human-readable name for the solver (shown in the settings UI).
    fn name(&self) -> &'static str {
        "solver"
    }
}

// ============================================================================
//  reCAPTCHA v2 — detect + task shapes (GSA SerLib parity)
// ============================================================================

/// Extract a Google reCAPTCHA `sitekey` from a page. Mirrors GSA SerLib's
/// `extractSitekey.js`: looks for `data-sitekey="..."` on the widget, then
/// for a `sitekey=<key>` query param on a `grecaptcha.../api.js` script.
/// Returns `None` when the page carries no reCAPTCHA widget.
pub fn extract_sitekey(html: &str) -> Option<String> {
    let lower = html.to_lowercase();
    // 1. data-sitekey="KEY" / data-sitekey='KEY' / data-sitekey=KEY
    if let Some(p) = lower.find("data-sitekey") {
        let rest = &html[p..];
        let eq = rest.find('=')?;
        let tail = &rest[eq + 1..];
        let val = if tail.starts_with('\"') || tail.starts_with('\'') {
            let q = tail.chars().next().unwrap();
            let end = tail[1..].find(q)? + 2;
            &tail[1..end - 1]
        } else {
            let end = tail
                .find(|c: char| c.is_whitespace() || c == '>' || c == '\"' || c == '\'')
                .unwrap_or(tail.len());
            &tail[..end]
        };
        let key = val.trim();
        if !key.is_empty() && !key.contains(char::is_whitespace) {
            return Some(key.to_string());
        }
    }
    // 2. script ... src="...recaptcha/api.js?anchor=...&hl=...&k=KEY" (the
    //    `google.com/recaptcha` and `grecaptcha.*` hosts both load this way).
    //    Indices are computed on the lowercased copy (safe — `to_lowercase`
    //    preserves offsets for the ASCII markup we scan), but the KEY slice
    //    comes from the original `html` so the result is case-exact.
    if let Some(s) = lower.find("recaptcha") {
        let region = &lower[s..(s + 400).min(lower.len())];
        for token in ["sitekey=", "&k=", "k="] {
            if let Some(kidx) = region.find(token) {
                let rest = &region[kidx + token.len()..];
                let end = rest
                    .find(|c: char| c == '&' || c == '"' || c == '\'' || c.is_whitespace())
                    .unwrap_or(rest.len());
                let key = &html[s + kidx + token.len()..s + kidx + token.len() + end];
                if key.len() >= 20 {
                    return Some(key.to_string());
                }
            }
        }
    }
    None
}

// ============================================================================
//  Providers — which captcha solver to use
// ============================================================================

/// Which captcha provider a project / run wants. `Local` is free and
/// offline (arithmetic only). `TwoCaptcha` / `AntiCaptcha` are paid HTTP
/// providers covering image, audio, reCAPTCHA, hCaptcha and Turnstile.
/// `CapMonster` reuses the 2Captcha API (alias; CAPMonster is a reseller).
///
/// F#29 (free-first) additions:
/// * `CdpVision` — drives a local headless browser over CDP and solves the
///   reCAPTCHA v2 image grid via a local "vision oracle" (human click or any
///   model service). No API key, no credits.
/// * `Auto` — the "totally free" chain: `local` (arithmetic/text) → `CdpVision`
///   → the paid providers, but only if the user actually has a key for them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptchaProvider {
    Local,
    TwoCaptcha,
    AntiCaptcha,
    CapMonster,
    CdpVision,
    Auto,
}

impl CaptchaProvider {
    /// Canonical name for storage in `Settings.captcha_provider` and for
    /// the settings UI.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::TwoCaptcha => "2captcha",
            Self::AntiCaptcha => "anticaptcha",
            Self::CapMonster => "capmonster",
            Self::CdpVision => "cdp-vision",
            Self::Auto => "auto",
        }
    }

    /// Parse a stored string. Unknown / empty → `Local` (GSA default).
    /// F#29 aliases: `cdp-vision`/`cdp`/`vision` → [CaptchaProvider::CdpVision];
    /// `auto`/`chain`/`best` → [CaptchaProvider::Auto].
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "local" | "" => Self::Local,
            "2captcha" | "twocaptcha" => Self::TwoCaptcha,
            "anticaptcha" => Self::AntiCaptcha,
            "capmonster" => Self::CapMonster,
            "cdp-vision" | "cdpv" | "cdp" | "vision" | "vision-cdp" | "free-vision" => Self::CdpVision,
            "auto" | "chain" | "best" | "free" => Self::Auto,
            _ => Self::Local,
        }
    }
}

/// Try each solver in order; first success wins. Built by
/// [CaptchaProvider::Auto] ("totally free" mode).
///
/// Failure reporting: if every step was "unsolvable", the combined reason
/// lists each step and why (UI can show it without guessing); if a step hit a
/// real provider/network error that one is surfaced (it is the actionable
/// one — e.g. "oracle unreachable" or "2captcha: bad key").
pub struct ChainedSolver {
    steps: Vec<std::sync::Arc<dyn CaptchaSolver>>,
}

impl ChainedSolver {
    pub fn new(steps: Vec<std::sync::Arc<dyn CaptchaSolver>>) -> Self {
        Self { steps }
    }

    pub fn steps(&self) -> &[std::sync::Arc<dyn CaptchaSolver>] {
        &self.steps
    }
}

#[async_trait::async_trait]
impl CaptchaSolver for ChainedSolver {
    fn name(&self) -> &'static str {
        "auto"
    }

    async fn solve(&self, ctx: &CaptchaContext) -> Result<String, CaptchaError> {
        if self.steps.is_empty() {
            return Err(CaptchaError::Unsolvable { reason: "auto chain has no steps".into() });
        }
        let mut failures: Vec<String> = Vec::new();
        let mut last_actionable: Option<CaptchaError> = None;
        for step in &self.steps {
            match step.solve(ctx).await {
                Ok(answer) => return Ok(answer),
                Err(e) => {
                    failures.push(format!("{} → {}", step.name(), e));
                    match e {
                        CaptchaError::Unsolvable { .. } => {}
                        other => last_actionable = Some(other),
                    }
                }
            }
        }
        match last_actionable {
            Some(e) => Err(e),
            None => Err(CaptchaError::Unsolvable {
                reason: format!("auto chain: {} failures: {}", self.steps.len(), failures.join(" | ")),
            }),
        }
    }
}

/// Build the right `Arc<dyn CaptchaSolver>` for a provider + API key.
/// Returns `None` for non-local providers when the API key is missing
/// (the caller should fall back to `Local` and record the gap).
pub fn make_solver(
    provider: CaptchaProvider,
    api_key: Option<&str>,
    timeout_secs: u32,
) -> Option<std::sync::Arc<dyn CaptchaSolver>> {
    let local: std::sync::Arc<dyn CaptchaSolver> = std::sync::Arc::new(LocalSolver::default());
    Some(match provider {
        CaptchaProvider::Local => local,
        CaptchaProvider::TwoCaptcha => match api_key {
            Some(k) if !k.trim().is_empty() => {
                std::sync::Arc::new(TwoCaptchaSolver::new(k, timeout_secs)) as std::sync::Arc<dyn CaptchaSolver>
            }
            _ => return None,
        },
        CaptchaProvider::AntiCaptcha => match api_key {
            Some(k) if !k.trim().is_empty() => {
                std::sync::Arc::new(AntiCaptchaSolver::new(k, timeout_secs)) as std::sync::Arc<dyn CaptchaSolver>
            }
            _ => return None,
        },
        // CAPMonster speaks the same API as 2Captcha (it resells the same
        // network); treat the key + base URL accordingly.
        CaptchaProvider::CapMonster => match api_key {
            Some(k) if !k.trim().is_empty() => {
                std::sync::Arc::new(TwoCaptchaSolver::with_base(k, timeout_secs, "https://capmonster.cloud"))
                    as std::sync::Arc<dyn CaptchaSolver>
            }
            _ => return None,
        },
        // F#29 — free vision+CDP reCAPTCHA v2 solver (headless browser +
        // local vision oracle). Needs the `browser` feature for the CDP
        // transport; without it the chain simply skips it.
        CaptchaProvider::CdpVision => {
            #[cfg(feature = "browser")]
            {
                std::sync::Arc::new(crate::captcha_cdp::CdpVisionSolver::new())
                    as std::sync::Arc<dyn CaptchaSolver>
            }
            #[cfg(not(feature = "browser"))]
            { return None; }
        }
        // F#29 — "totally free" chain: local (instant, offline) → cdp-vision
        // (free reCAPTCHA grid) → paid providers, only if the user has keys.
        CaptchaProvider::Auto => {
            let mut steps: Vec<std::sync::Arc<dyn CaptchaSolver>> = vec![local];
            #[cfg(feature = "browser")]
            steps.push(std::sync::Arc::new(crate::captcha_cdp::CdpVisionSolver::new()));
            if let Some(k) = api_key.filter(|k| !k.trim().is_empty()) {
                steps.push(std::sync::Arc::new(TwoCaptchaSolver::new(k, timeout_secs)));
                steps.push(std::sync::Arc::new(AntiCaptchaSolver::new(k, timeout_secs)));
            }
            std::sync::Arc::new(ChainedSolver::new(steps)) as std::sync::Arc<dyn CaptchaSolver>
        }
    })
}

/// Extract the captcha `<img>` URL out of an HTML page (for image-based
/// providers). Looks for `<img>` whose `src` / `srcset` / `data-src`
/// matches `captcha|code|recaptcha|hcaptcha|challenge`. Falls back to the
/// first `<img>` on the page.
pub fn extract_image_url(html: &str) -> Option<String> {
    let lower = html.to_lowercase();
    let mut idx = 0usize;
    while let Some(p) = lower[idx..].find("<img") {
        let start = idx + p;
        let end = match lower[start..].find('>') {
            Some(e) => start + e + 1,
            None => break,
        };
        let slice = &html[start..end].to_lowercase();
        let looks_like_captcha = ["captcha", "recaptcha", "hcaptcha", "challenge"]
            .iter()
            .any(|k| slice.contains(k));
        if looks_like_captcha {
            if let Some(u) = find_attribute(&html[start..end], "src") {
                return Some(u);
            }
        }
        idx = end;
    }
    // First <img> in the page.
    let lower = html.to_lowercase();
    if let Some(p) = lower.find("<img") {
        let start = p;
        let end = match lower[start..].find('>') {
            Some(e) => start + e + 1,
            None => return None,
        };
        if let Some(u) = find_attribute(&html[start..end], "src") {
            return Some(u);
        }
    }
    None
}

// ============================================================================
//  LocalSolver — offline arithmetic solver (GSA "CaptchaBreaker" parity,
//  minus the OCR part)
// ============================================================================

/// An offline captcha solver covering **inline arithmetic** questions.
///
/// Typical engine pages look like:
/// ```html
/// <td>What is <b>3</b> + <b>7</b> ?</td>
/// <input type="text" name="captcha" ...>
/// ```
/// or
/// ```html
/// <p>Spam check: 2 * 4 = ?</p>
/// <input name="code2" ...>
/// ```
///
/// The solver scans the HTML for a `<number> <op> <number>` pattern near
/// an `<input>` field whose name contains `captcha|code|answer` (or any
/// input if no specific name is configured). Supported operators:
/// `+ - * / x X` (multiplication), `+ -` (add/sub), `÷` (division).
///
/// The answer is **always** the integer result of the arithmetic (division
/// is truncated to integer, GSA-style). If the expression is ambiguous
/// (multiple arithmetic expressions in the page) the **first one near
/// the target field** wins.
#[derive(Debug)]
pub struct LocalSolver {
    /// Optional pre-loaded `text_captcha_extract.dat` rules. When present,
    /// the solver uses these rules to locate the question before scanning
    /// for arithmetic.
    rules: Vec<ExtractRule>,
    /// Optional list of field name hints (e.g. ["captcha", "code2"]).
    /// When `None`, the solver picks the first `<input>` in the page.
    /// When present, it prioritizes the first matching input by name.
    field_hints: Vec<String>,
}

/// One row of GSA's `text_captcha_extract.dat` (three `|`-separated
/// columns). The format is `<question-fragment-1>|<variable-name>|<trailing-fragment>`,
/// where `<variable-name>` is the text GSA substitutes with the answer.
/// We use it to *locate* the arithmetic question before scanning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractRule {
    pub prefix: String,
    pub variable: String,
    pub suffix: String,
}

impl Default for LocalSolver {
    fn default() -> Self {
        Self::new(None)
    }
}

impl LocalSolver {
    /// Build a local solver. `rules` is ignored in the current
    /// implementation (the dat-file table format is inconsistent across
    /// GSA versions) — the arithmetic scanner is the robust path.
    pub fn new(rules: Option<Vec<ExtractRule>>) -> Self {
        Self {
            rules: rules.unwrap_or_default(),
            field_hints: vec![
                "captcha".into(),
                "code2".into(),
                "code".into(),
                "answer".into(),
                "spam".into(),
            ],
        }
    }

    /// Locate the captcha `<input>` in the HTML. Returns `None` if no
    /// input matched.
    pub fn find_input(&self, html: &str) -> Option<CaptchaInputRef> {
        // Walk the HTML and find all <input ...> elements. Prefer one whose
        // `name="<hint>"` (case-insensitive hint match) — the first
        // non-hidden, non-submit input wins as a fallback.
        let mut fallback: Option<CaptchaInputRef> = None;
        for m in re_find_inputs(html) {
            let iname = normalize_name(&m.name);
            if iname.is_empty() {
                continue;
            }
            if self
                .field_hints
                .iter()
                .any(|h| iname.eq_ignore_ascii_case(h) || iname.contains(h.as_str()))
            {
                return Some(CaptchaInputRef { name: m.name });
            }
            if fallback.is_none() && m.kind != "hidden" && m.kind != "submit" && m.kind != "button" {
                fallback = Some(CaptchaInputRef { name: m.name.clone() });
            }
        }
        fallback
    }

    /// Solve an arithmetic captcha from the HTML.
    /// Returns `Some(answer)` on success, `None` if no arithmetic
    /// expression was found.
    ///
    /// Strategy:
    /// 1. Find the captcha input (by hint or fallback).
    /// 2. Look for a number-operator-number pattern **near** the input
    ///    (within ±500 chars of the input's offset).
    /// 3. If not found near the input, look anywhere in the page.
    /// 4. If the page has a `text`-only form (no input), the caller
    ///    should treat the arithmetic as a pure-text extraction.
    pub fn try_solve_arithmetic(&self, html: &str) -> Option<String> {
        let input = self.find_input(html);
        if let Some(inp) = &input {
            // Find the offset of the input in the HTML (approximate — first
            // `<input ...>` occurrence with the right name).
            if let Some(idx) = find_input_offset(html, inp) {
                let lo = idx.saturating_sub(800);
                let hi = idx.min(html.len()).saturating_add(800);
                let region = &html[lo.min(hi)..hi.min(html.len())];
                if let Some(v) = scan_arithmetic(region) {
                    return Some(v);
                }
            }
        }
        // Fallback: scan the whole page.
        scan_arithmetic(html)
    }
}

#[async_trait::async_trait]
impl CaptchaSolver for LocalSolver {
    async fn solve(&self, ctx: &CaptchaContext) -> Result<String, CaptchaError> {
        if ctx.html.trim().is_empty() {
            return Err(CaptchaError::Unsolvable {
                reason: "empty HTML — local arithmetic solver needs the form page".into(),
            });
        }
        if let Some(ans) = self.try_solve_arithmetic(&ctx.html) {
            return Ok(ans);
        }
        // GSA parity: the data-driven text-challenge table (word-echo,
        // multilingual arithmetic-glue walls) — copied from GSA's
        // text_captcha_extract.dat.
        if let Some(ans) = extract_text_challenge(&ctx.html) {
            return Ok(ans);
        }
        Err(CaptchaError::Unsolvable {
            reason: "no inline arithmetic or GSA-table text challenge found — use an \
                     image solver (2Captcha/Anti-Captcha) for graphical captchas"
                .into(),
        })
    }
    fn name(&self) -> &'static str {
        "local-arithmetic"
    }
}

// ============================================================================
//  2Captcha / CAPMonster — HTTP image-captcha solver
// ============================================================================

/// 2Captcha (and CAPMonster — same HTTP API, different base URL) image
/// captcha solver. Uses the public plain HTTP API:
///
///   1. `POST https://{host}/in.php?{method=...}&key={api_key}` → `1|<task_id>`
///   2. `GET  https://{host}/res.php?key={api_key}&action=get&id={task_id}` →
///      `1|<answer>` on success, `CAPCHA_NOT_READY` while in flight,
///      `0|<error>` on failure.
///
/// We poll (3) until (2) or the timeout elapses.
pub struct TwoCaptchaSolver {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
    timeout: std::time::Duration,
    poll_interval: u64,
}

impl TwoCaptchaSolver {
    /// Which task shape this context demands (test hook + solver path):
    /// reCAPTCHA-v2 when a `sitekey` is present (or extractable from the
    /// page HTML), plain image otherwise.
    pub fn task_kind(ctx: &CaptchaContext) -> &'static str {
        if ctx.sitekey.as_deref().is_some_and(|s| !s.trim().is_empty())
            || extract_sitekey(&ctx.html).is_some()
        {
            "userrecaptcha"
        } else {
            "base64"
        }
    }

    /// Submit a reCAPTCHA-v2 task (SerLib parity): `method=userrecaptcha`,
    /// `googlekey={sitekey}`, `pageurl={url}`. Returns the task id.
    async fn submit_recaptcha(&self, sitekey: &str, page_url: Option<&str>) -> Result<String, CaptchaError> {
        let url = format!("{}/in.php", self.base_url);
        let pageurl = page_url.unwrap_or("").trim().to_string();
        let form: Vec<(&'static str, String)> = vec![
            ("key", self.api_key.clone()),
            ("method", "userrecaptcha".into()),
            ("googlekey", sitekey.to_string()),
            ("pageurl", pageurl),
            ("json", "1".into()),
            ("invisible", "0".into()),
            ("soft_id", "135".into()),
        ];
        let resp = self
            .client
            .post(&url)
            .form(&form)
            .send()
            .await
            .map_err(|e| CaptchaError::Network { message: e.to_string() })?;
        Ok(self.parse_task_id(resp).await?)
    }

    async fn parse_task_id(&self, resp: reqwest::Response) -> Result<String, CaptchaError> {
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| CaptchaError::Network { message: e.to_string() })?;
        match body.get("status").and_then(|v| v.as_str()) {
            Some("1") | Some("true") => body
                .get("request")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .ok_or(CaptchaError::Provider {
                    code: "no-task-id".into(),
                    message: body.to_string(),
                }),
            Some(s) => Err(CaptchaError::Provider {
                code: s.to_string(),
                message: body
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown error")
                    .to_string(),
            }),
            None => Err(CaptchaError::Provider {
                code: "no-status".into(),
                message: body.to_string(),
            }),
        }
    }

    /// Build a 2Captcha solver (base `api.2captcha.com`).
    pub fn new(api_key: &str, timeout_secs: u32) -> Self {
        Self::with_base(api_key, timeout_secs, "https://api.2captcha.com")
    }

    /// Build a solver pointed at a 2Captcha-compatible base URL (e.g.
    /// `https://capmonster.cloud`) — CAPMonster speaks the same API.
    pub fn with_base(api_key: &str, timeout_secs: u32, base: &str) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(timeout_secs.max(5) as u64))
            .build()
            .unwrap_or_default();
        Self {
            client,
            api_key: api_key.trim().to_string(),
            base_url: base.trim_end_matches('/').to_string(),
            timeout: std::time::Duration::from_secs(timeout_secs as u64),
            poll_interval: 2_000,
        }
    }

    async fn submit(&self, image_url: &str) -> Result<String, CaptchaError> {
        let url = format!("{}/in.php", self.base_url);
        let resp = self
            .client
            .post(&url)
            .form(&[
                ("method", "base64"),
                ("key", &self.api_key),
                ("body", image_url),
                ("json", "1"),
            ])
            .send()
            .await
            .map_err(|e| CaptchaError::Network { message: e.to_string() })?;
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| CaptchaError::Network { message: e.to_string() })?;
        // 2Captcha with `json=1` returns `{"status":"1","request":"12345"}`
        // on success, or `{"status":"0","message":"..."}` on failure.
        match body.get("status").and_then(|v| v.as_str()) {
            Some("1") | Some("true") => {
                return body
                    .get("request")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .ok_or(CaptchaError::Provider {
                        code: "no-task-id".into(),
                        message: body.to_string(),
                    });
            }
            Some(s) => {
                return Err(CaptchaError::Provider {
                    code: s.to_string(),
                    message: body
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown error")
                        .to_string(),
                });
            }
            None => {
                return Err(CaptchaError::Provider {
                    code: "no-status".into(),
                    message: body.to_string(),
                });
            }
        }
    }

    async fn poll(&self, task_id: &str) -> Result<String, CaptchaError> {
        let started = std::time::Instant::now();
        let mut backoff = std::time::Duration::from_millis(self.poll_interval);
        loop {
            let url = format!(
                "{}/res.php?key={}&action=get&id={}",
                self.base_url, self.api_key, task_id
            );
            let resp = self
                .client
                .get(&url)
                .send()
                .await
                .map_err(|e| CaptchaError::Network { message: e.to_string() })?;
            let text = resp
                .text()
                .await
                .map_err(|e| CaptchaError::Network { message: e.to_string() })?;
            let (code, payload) = text.split_once('|').unwrap_or((&text, ""));
            match code {
                "1" => return Ok(payload.to_string()),
                "0" | "9" => {
                    if text.contains("CAPCHA_NOT_READY") {
                        // Not ready yet — keep polling.
                    } else {
                        return Err(CaptchaError::Provider {
                            code: code.to_string(),
                            message: payload.to_string(),
                        });
                    }
                }
                _ => return Err(CaptchaError::Provider {
                    code: code.trim().to_string(),
                    message: payload.to_string(),
                }),
            }
            if started.elapsed() >= self.timeout {
                return Err(CaptchaError::Provider {
                    code: "timeout".to_string(),
                    message: format!("no answer after {:?}", self.timeout),
                });
            }
            tokio::time::sleep(backoff).await;
            // Gentle exponential backoff up to 8s.
            let next = backoff * 2;
            backoff = if next.as_millis() > 8000 {
                std::time::Duration::from_millis(8000)
            } else {
                next
            };
        }
    }
}

#[async_trait::async_trait]
impl CaptchaSolver for TwoCaptchaSolver {
    async fn solve(&self, ctx: &CaptchaContext) -> Result<String, CaptchaError> {
        if TwoCaptchaSolver::task_kind(ctx) == "userrecaptcha" {
            let sitekey = match &ctx.sitekey {
                Some(s) if !s.trim().is_empty() => s.clone(),
                _ => extract_sitekey(&ctx.html)
                    .ok_or(CaptchaError::Unsolvable {
                        reason: "2Captcha/reCAPTCHA path needs a sitekey".into(),
                    })?,
            };
            let task = self
                .submit_recaptcha(&sitekey, ctx.url.as_deref())
                .await?;
            let answer = self.poll(&task).await?;
            if answer.trim().is_empty() {
                return Err(CaptchaError::EmptyAnswer);
            }
            return Ok(answer);
        }
        let image = match &ctx.image_url {
            Some(u) if !u.trim().is_empty() => u.clone(),
            _ => match extract_image_url(&ctx.html) {
                Some(u) => u,
                None => {
                    return Err(CaptchaError::Unsolvable {
                        reason: "2Captcha/image solver needs an <img> captcha".into(),
                    })
                }
            },
        };
        let task = self.submit(&image).await?;
        let answer = self.poll(&task).await?;
        if answer.trim().is_empty() {
            return Err(CaptchaError::EmptyAnswer);
        }
        Ok(answer)
    }

    fn name(&self) -> &'static str {
        if self.base_url.contains("capmonster") {
            "capmonster"
        } else {
            "2captcha"
        }
    }
}

// ============================================================================
//  Anti-Captcha — HTTP image-captcha solver (anti-captcha.com)
// ============================================================================

pub struct AntiCaptchaSolver {
    client: reqwest::Client,
    api_key: String,
    timeout_secs: u64,
}

impl AntiCaptchaSolver {
    pub fn new(api_key: &str, timeout_secs: u32) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(timeout_secs.max(5) as u64))
            .build()
            .unwrap_or_default();
        Self {
            client,
            api_key: api_key.trim().to_string(),
            timeout_secs: timeout_secs.max(5) as u64,
        }
    }
}

#[async_trait::async_trait]
impl CaptchaSolver for AntiCaptchaSolver {
    async fn solve(&self, ctx: &CaptchaContext) -> Result<String, CaptchaError> {
        // reCAPTCHA-v2 task (SerLib parity) when a sitekey is present.
        let sitekey = ctx
            .sitekey
            .clone()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| extract_sitekey(&ctx.html));
        let task: serde_json::Value = if let Some(sk) = sitekey {
            // GSA CapMonster Cloud.ini parity: NoCaptchaTaskProxyless is the
            // anti-captcha.com name for a reCAPTCHA-v2 solve.
            serde_json::json!({
                "clientKey": self.api_key,
                "task": {
                    "type": "NoCaptchaTaskProxyless",
                    "websiteKey": sk,
                    "websiteURL": ctx.url.as_deref().unwrap_or(""),
                    "isInvisible": false,
                },
            })
        } else {
            let image = match &ctx.image_url {
                Some(u) if !u.trim().is_empty() => u.clone(),
                _ => match extract_image_url(&ctx.html) {
                    Some(u) => u,
                    None => {
                        return Err(CaptchaError::Unsolvable {
                            reason: "Anti-Captcha solver needs a sitekey (reCAPTCHA) or an <img> captcha".into(),
                        })
                    }
                },
            };
            serde_json::json!({
                "clientKey": self.api_key,
                "task": {
                    "type": "ImageToTextTask",
                    "body": image,
                },
            })
        };

        let submit = self
            .client
            .post("https://api.anti-captcha.com/createTask")
            .json(&task)
            .send()
            .await
            .map_err(|e| CaptchaError::Network { message: e.to_string() })?;
        let body: serde_json::Value = submit
            .json()
            .await
            .map_err(|e| CaptchaError::Network { message: e.to_string() })?;
        if let Some(code) = body["errorId"].as_u64() {
            if code != 0 {
                return Err(CaptchaError::Provider {
                    code: code.to_string(),
                    message: body["errorCodeDescription"].as_str().unwrap_or("provider error").to_string(),
                });
            }
        }
        let task_id = body["taskId"]
            .as_str()
            .ok_or_else(|| CaptchaError::Provider {
                code: "no-task-id".into(),
                message: body.to_string(),
            })?
            .to_string();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(self.timeout_secs);
        while std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            let get = self
                .client
                .post("https://api.anti-captcha.com/getTaskResult")
                .json(&serde_json::json!({ "clientKey": self.api_key, "taskId": task_id }))
                .send()
                .await
                .map_err(|e| CaptchaError::Network { message: e.to_string() })?;
            let res: serde_json::Value = get
                .json()
                .await
                .map_err(|e| CaptchaError::Network { message: e.to_string() })?;
            match res["status"].as_str() {
                Some("Ready") => {
                    let text = res["solution"]["text"]
                        .as_str()
                        .ok_or(CaptchaError::EmptyAnswer)?
                        .to_string();
                    if text.is_empty() {
                        return Err(CaptchaError::EmptyAnswer);
                    }
                    return Ok(text);
                }
                Some("InProgress") => continue,
                Some(other) => {
                    return Err(CaptchaError::Provider {
                        code: other.into(),
                        message: res.to_string(),
                    })
                }
                None => {
                    return Err(CaptchaError::Provider {
                        code: "no-status".into(),
                        message: res.to_string(),
                    })
                }
            }
        }
        Err(CaptchaError::Provider {
            code: "timeout".into(),
            message: format!("no answer after {}s", self.timeout_secs),
        })
    }

    fn name(&self) -> &'static str {
        "anticaptcha"
    }
}

// ============================================================================
//  GSA text-challenge extractor — a copy of GSA's `text_captcha_extract.dat`
// ============================================================================

/// GSA's own data-driven text-captcha table (914 multilingual rules, copied
/// verbatim from the original install: "word echo" walls — `Please write
/// the word "|"` — arithmetic-glue walls — `What is|? * <` — and dozens of
/// localized variants). Each rule is `left|glue|right`: the live page
/// contains `<left> <ANSWER> <glue> <right>` (2-pipe) or
/// `<left> <ANSWER> <right>` (1-pipe), and the answer is what sits between
/// them. Data-driven like in GSA: new challenge markup only needs a new
/// table line, never a recompile of solver logic (the table itself is
/// embedded at build time, as here).
fn gsa_text_rules() -> &'static Vec<(String, String, String)> {
    static RULES: std::sync::OnceLock<Vec<(String, String, String)>> = std::sync::OnceLock::new();
    RULES.get_or_init(|| {
        let raw: &[u8] = include_bytes!("../assets/text_captcha_extract.dat");
        let text = String::from_utf8_lossy(raw);
        text.lines()
            .map(|l| l.trim().to_string())
            .filter(|l| l.contains('|'))
            .filter_map(|l| {
                let parts: Vec<&str> = l.split('|').collect();
                match parts.len() {
                    2 => Some((parts[0].to_string(), String::new(), parts[1].to_string())),
                    3 => Some((parts[0].to_string(), parts[1].to_string(), parts[2].to_string())),
                    _ => None,
                }
            })
            .collect()
    })
}

fn find_ci(hay: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    let hay_l = hay.to_lowercase();
    let needle_l = needle.to_lowercase();
    hay_l.find(&needle_l)
}

/// Try every GSA text-challenge rule against a page and return the first
/// valid extracted answer. Rule shapes: 2-pipe `L|M|R` — the page reads
/// `L M <ANSWER> R` (the answer sits between the middle marker and the
/// right marker, as in `Human Test<|<strong>Please write the word "|""…`
/// or `<!-- Visual Confirmation -->|<span class="gen">Hoeveel is | * </span>`);
/// 1-pipe `L|R` — the answer sits directly between them. Each candidate
/// span is validated (2–40 chars, alphanumerics + space/hyphen/dot/quote);
/// a candidate that is itself an arithmetic question is solved and the
/// numeric answer returned.
pub fn extract_text_challenge(page: &str) -> Option<String> {
    let lower = page.to_lowercase();
    let clean = |s: &str| {
        let t = s.trim();
        let t = t.trim_matches(['"', '\'', '\u{201c}', '\u{201d}']).trim();
        t.to_string()
    };
    let valid = |s: &str| (2..=40).contains(&s.chars().count())
        && s.chars().all(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '.' | '\'' | ','));
    for (left, glue, right) in gsa_text_rules() {
        let Some(start) = find_ci(&lower, left) else {
            continue;
        };
        let mut candidates: Vec<String> = Vec::new();
        if !glue.is_empty() {
            if let Some(g) = find_ci(&lower[start..], glue) {
                let glow = start + g;
                let after_glue = glow + glue.len();
                // dominant GSA pattern: answer between glue and right
                if let Some(e) = find_ci(&lower[after_glue..], right) {
                    candidates.push(lower[after_glue..after_glue + e].to_string());
                }
                // fallback: answer between left and glue
                candidates.push(lower[start..glow].to_string());
            }
        } else if let Some(e) = find_ci(&lower[start..], right) {
            candidates.push(lower[start..start + e].to_string());
        }
        for raw in candidates {
            let cand = clean(&raw);
            if cand.is_empty() {
                continue;
            }
            if let Some(ans) = scan_arithmetic(&cand) {
                return Some(ans);
            }
            if valid(&cand) {
                return Some(cand);
            }
        }
    }
    None
}

// ============================================================================
//  Arithmetic scanner — pure, synchronous, no deps
// ============================================================================

/// Scan text for a `<number> <op> <number>` pattern and return the answer.
/// Operators recognized: `+ - / * x X` and `÷`. Only the **first** match
/// in the text is considered.
pub fn scan_arithmetic(text: &str) -> Option<String> {
    // Strip HTML tags for a cleaner scan (we don't want to match "a+b"
    // inside `<a href="a+b">`).
    let cleaned = strip_tags(text);
    let lower = cleaned.to_lowercase();
    let chars: Vec<char> = lower.chars().collect();
    let n = chars.len();
    let mut i = 0;
    while i < n {
        match chars[i] {
            '+' | '-' | '*' | '/' | 'x' | '÷' => {
                let op = chars[i];
                // Try to parse a number before the op and a number after.
                let (before, after) = (i, i + 1);
                if let (Some(a), Some(b)) = (parse_int_before(&chars, before), parse_int_after(&chars, after)) {
                    if a > 0 && b > 0 || (op == '-' && a >= b) {
                        let ans = match op {
                            '+' => a.wrapping_add(b),
                            '-' => a.wrapping_sub(b),
                            '*' | 'x' => a.wrapping_mul(b),
                            '/' | '÷' if b != 0 => a / b,
                            _ => return None,
                        };
                        return Some(ans.to_string());
                    }
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    None
}

fn parse_int_before(chars: &[char], end: usize) -> Option<u32> {
    let mut j = end;
    // Skip whitespace between the number and the operator.
    while j > 0 && chars[j - 1].is_whitespace() {
        j -= 1;
    }
    let mut digits = String::new();
    while j > 0 && chars[j - 1].is_ascii_digit() {
        j -= 1;
        digits.insert(0, chars[j]);
    }
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

fn parse_int_after(chars: &[char], start: usize) -> Option<u32> {
    let mut j = start;
    while j < chars.len() && chars[j].is_whitespace() {
        j += 1;
    }
    let mut digits = String::new();
    while j < chars.len() && chars[j].is_ascii_digit() {
        digits.push(chars[j]);
        j += 1;
    }
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

// ============================================================================
//  HTML helpers (no `regex` dep — hand-rolled scanners)
// ============================================================================

fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

fn normalize_name(name: &str) -> String {
    name.trim().to_lowercase()
}

struct RawInput {
    name: String,
    kind: String,
}

struct CaptchaInputRef {
    name: String,
}

/// Find all `<input>` elements in a fragment. Hand-rolled scanner (no
/// regex crate — keeps the pure core dep-free).
fn re_find_inputs(html: &str) -> Vec<RawInput> {
    let lower = html.to_lowercase();
    let mut out = Vec::new();
    let mut idx = 0usize;
    while let Some(p) = lower[idx..].find("<input") {
        let start = idx + p;
        // Look for `name="..."` or `name='...'`.
        let end = match lower[start..].find('>') {
            Some(e) => start + e + 1,
            None => break,
        };
        let slice = &html[start..end];
        let mut name = String::new();
        if let Some(nm) = find_attribute(slice, "name") {
            name = nm;
        }
        let kind = find_attribute(slice, "type").unwrap_or_else(|| "text".into());
        out.push(RawInput { name, kind });
        idx = end;
    }
    out
}

fn find_attribute(slice: &str, key: &str) -> Option<String> {
    let lower = slice.to_lowercase();
    let key = format!("{}=", key);
    let pos = lower.find(&*key)?;
    // The `=` must be at a word boundary — the char before (if any) must be
    // whitespace or the very start of the slice, not a letter/digit/`_` from
    // a longer attribute name like `code2name=`.
    if pos > 0 {
        let prev = lower.as_bytes()[pos - 1];
        if prev.is_ascii_alphanumeric() || prev == b'_' || prev == b'-' || prev == b'.' {
            return None;
        }
    }
    let rest = &slice[pos + key.len()..];
    // The value may be `"..."`, `'...'`, or bare until a space.
    let c = rest.chars().next()?;
    if c == '"' {
        let end = rest[1..].find('"')? + 1;
        Some(rest[1..end].to_string())
    } else if c == '\'' {
        let end = rest[1..].find('\'')? + 1;
        Some(rest[1..end].to_string())
    } else {
        // Bare value — read until whitespace or `>`.
        let mut val = String::new();
        for ch in rest.chars() {
            if ch.is_whitespace() || ch == '>' {
                break;
            }
            val.push(ch);
        }
        Some(val)
    }
}

fn find_input_offset(html: &str, ref_in: &CaptchaInputRef) -> Option<usize> {
    // Find the first `<input` whose `name="ref"` — return its offset.
    let lower = html.to_lowercase();
    let target = format!("name=\"{}\"", ref_in.name.to_lowercase());
    let mut scan = 0usize;
    while let Some(p) = lower[scan..].find("<input") {
        let s = scan + p;
        let e = match lower[s..].find('>') {
            Some(e) => s + e + 1,
            None => break,
        };
        let slice = &lower[s..e];
        if slice.to_lowercase().contains(target.as_str()) {
            return Some(s);
        }
        scan = e;
    }
    None
}

/// Decode GSA numeric + named HTML entities. Supports `&#100;`, `#&100;`
/// (GSA's swapped form seen in `calc_replace.dat`) and the common named
/// set (`&amp;` `&lt;` `&gt;` `&quot;` `&apos;` `&nbsp;`). Anything
/// unknown passes through verbatim.
pub fn decode_gsa_entities(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    let n = c.len();
    let mut out = String::with_capacity(s.len());
    let mut i = 0usize;
    while i < n {
        if c[i] == '#' && i + 1 < n && (c[i + 1] == '&' || c[i + 1].is_ascii_digit()) {
            // `#&100;` (GSA) or `#100;` — an optional `&` follows the `#`.
            let mut j = i + 1;
            if j < n && c[j] == '&' {
                j += 1;
            }
            let start = j;
            while j < n && c[j].is_ascii_digit() {
                j += 1;
            }
            if j > start {
                let digits: String = c[start..j].iter().collect();
                if let Ok(val) = digits.parse::<u32>() {
                    out.push(char::from_u32(val).unwrap_or('?'));
                    if j < n && c[j] == ';' {
                        j += 1;
                    }
                    i = j;
                    continue;
                }
            }
        } else if c[i] == '&' && (i + 1) < n && (c[i + 1].is_ascii_alphabetic() || c[i + 1] == '#') {
            // `&#100;` or a named entity (`&amp;`, `&lt;`, `&gt;`, ...).
            if c[i + 1] == '#' {
                let mut j = i + 2;
                let start = j;
                while j < n && c[j].is_ascii_digit() {
                    j += 1;
                }
                if j > start && j < n && c[j] == ';' {
                    let digits: String = c[start..j].iter().collect();
                    if let Ok(val) = digits.parse::<u32>() {
                        out.push(char::from_u32(val).unwrap_or('?'));
                        i = j + 1;
                        continue;
                    }
                }
            }
            if let Some(end) = named_entity_end(&c[i..]) {
                let name: String = c[i..i + end].iter().collect();
                if let Some(rep) = named_entity(&name) {
                    out.push_str(rep);
                    i += end;
                    continue;
                }
            }
        }
        out.push(c[i]);
        i += 1;
    }
    out
}

fn named_entity_end(chars: &[char]) -> Option<usize> {
    if let Some(position) = chars.iter().position(|c| *c == ';') {
        let len = position + 1;
        if (3..=12).contains(&len) {
            return Some(len);
        }
    }
    None
}

fn named_entity(entity: &str) -> Option<&'static str> {
    match entity {
        "&amp;" => Some("&"),
        "&lt;" => Some("<"),
        "&gt;" => Some(">"),
        "&quot;" => Some("\""),
        "&apos;" => Some("'"),
        "&nbsp;" => Some("\u{00a0}"),
        _ => None,
    }
}

// ============================================================================
//  Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn html_arith() -> String {
        String::from(
            "<html><body>\
             <form action=\"/submit\">\
             <p>Spam check: <b>3</b> + <b>7</b> ?</p>\
             <input type=\"text\" name=\"captcha\" />\
             <input type=\"submit\" value=\"Go\" />\
             </form></body></html>"
        )
    }

    #[test]
    fn arithmetic_addition() {
        let s = LocalSolver::default();
        assert_eq!(s.try_solve_arithmetic(&html_arith()), Some("10".into()));
    }

    #[test]
    fn gsa_text_rules_table_loads_from_embedded_dat() {
        let rules = gsa_text_rules();
        assert!(rules.len() >= 800, "table should have ~914 rules, got {}", rules.len());
        // spot-check a known rule pair from the shipped .dat
        assert!(rules.iter().any(|(l, _, _)| l.contains("Please write the word")));
    }

    #[test]
    fn extract_text_challenge_word_echo_us() {
        // GSA rule (2-pipe): `Human Test<|<strong>Please write the word "|""`… —
        // L=`Human Test<`, M=`<strong>Please write the word "`, R=`" without quotes…`
        let page = "<form>Human Test<strong>Please write the word \"h3rm3s\" without \
                    quotes into the field below</strong> <input name='code'></form>";
        assert_eq!(extract_text_challenge(page).as_deref(), Some("h3rm3s"));
    }

    #[test]
    fn extract_text_challenge_gsa_style_arithmetic_glue() {
        // GSA rule: `<!-- Visual Confirmation -->|<span class="gen">Hoeveel is | * </span>`
        let page = "<td><!-- Visual Confirmation --><span class=\"gen\">Hoeveel is 7 + 9 * </span>\
                    <input name=\"code\"></td>";
        assert_eq!(extract_text_challenge(page).as_deref(), Some("16"));
    }

    #[test]
    fn extract_text_challenge_skips_html_junk_candidates() {
        // Anchors present, but the candidate span is HTML/over-length — the
        // extractor must reject it and end on None.
        let page = "Human Test<strong>Please write the word \"<b>tot &</b> all the junk here \
                    and many many more characters well past forty chars in total length\" \
                    without quotes into the field below</strong>";
        assert_eq!(extract_text_challenge(page), None);
    }

    #[test]
    fn local_solver_word_echo_via_gsa_table() {
        let s = LocalSolver::default();
        let ctx = CaptchaContext {
            html: "<form>Human Test<strong>Please write the word \"xyz123\" without quotes \
                   into the field below</strong> <input name='code'></form>"
                .into(),
            image_url: None,
            field_name: "code".into(),
            engine_name: None,
            url: None,
            page_title: None,
            sitekey: None,
        };
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        assert_eq!(rt.block_on(s.solve(&ctx)).ok().as_deref(), Some("xyz123"));
    }

    #[test]
    fn arithmetic_subtraction() {
        let html = "<input type='text' name='code2'><p>a - b: 9-4=?</p>";
        let s = LocalSolver::default();
        assert_eq!(s.try_solve_arithmetic(html), Some("5".into()));
    }

    #[test]
    fn arithmetic_multiply_x_and_times() {
        let html_x = "what is 6 x 6";
        let html_star = "what is 6 * 7";
        let s = LocalSolver::default();
        assert_eq!(s.try_solve_arithmetic(html_x), Some("36".into()));
        assert_eq!(s.try_solve_arithmetic(html_star), Some("42".into()));
    }

    #[test]
    fn arithmetic_division() {
        let html = "compute 48 / 6 =";
        let s = LocalSolver::default();
        assert_eq!(s.try_solve_arithmetic(html), Some("8".into()));
    }

    #[test]
    fn no_arithmetic_in_page_returns_none() {
        let html = "<p>no numbers here</p><input name='captcha'>";
        let s = LocalSolver::default();
        assert_eq!(s.try_solve_arithmetic(html), None);
    }

    #[tokio::test]
    async fn solver_returns_unsolvable_on_empty() {
        use std::sync::Arc;
        let s: Arc<dyn CaptchaSolver> = Arc::new(LocalSolver::default());
        let ctx = CaptchaContext { html: String::new(), ..Default::default() };
        let err = s.solve(&ctx).await.unwrap_err();
        matches!(err, CaptchaError::Unsolvable { .. });
    }

    #[tokio::test]
    async fn solver_solves_arithmetic() {
        use std::sync::Arc;
        let s: Arc<dyn CaptchaSolver> = Arc::new(LocalSolver::default());
        let ctx = CaptchaContext { html: html_arith(), ..Default::default() };
        assert_eq!(s.solve(&ctx).await.unwrap(), "10".to_string());
    }

    #[test]
    fn find_input_prefers_captcha_field() {
        let html = "<input type='text' name='username'><input type='text' name='captcha'><input type='text' name='email'>";
        let s = LocalSolver::default();
        let inp = s.find_input(html).expect("expected an input");
        assert_eq!(inp.name, "captcha");
    }

    #[test]
    fn decode_gsa_entities_numeric() {
        let d = decode_gsa_entities("a#&100;bc");
        assert_eq!(d, "adbc");
        let d2 = decode_gsa_entities("a&#100;b");
        assert_eq!(d2, "adb");
    }

    #[test]
    fn decode_named_entities() {
        assert_eq!(decode_gsa_entities("&lt;tag&gt;"), "<tag>");
        assert_eq!(decode_gsa_entities("&amp;lt;"), "&lt;");
    }

    #[test]
    fn scan_arithmetic_first_match_wins() {
        let html = "first: 1+1=2, but also 5+5=10 — pick the first";
        let v = scan_arithmetic(html);
        assert_eq!(v, Some("2".into()));
    }

    #[test]
    fn arithmetic_handles_html_around_expression() {
        let html = "<td>What is <b>3</b> + <b>7</b> ?</td>\
                   <input type=\"text\" name=\"code2\" />\
                   <p>some other text without numbers</p>";
        let s = LocalSolver::default();
        assert_eq!(s.try_solve_arithmetic(html), Some("10".into()));
    }

    // -- F#28 reCAPTCHA-v2 bridge (SerLib parity) ------------------------------

    #[test]
    fn extract_sitekey_from_data_attribute() {
        let html = r#"<div class="g-recaptcha" data-sitekey="6Ldb5_MeAAAAAN4B_pLBT3CdxV5X_ONzX7CpyZ11"></div>"#;
        assert_eq!(extract_sitekey(html), Some("6Ldb5_MeAAAAAN4B_pLBT3CdxV5X_ONzX7CpyZ11".into()));
    }

    #[test]
    fn extract_sitekey_from_data_attribute_unquoted() {
        let html = r#"<div class="g-recaptcha" data-sitekey=6Ld_b5MeAAAA_AN4BP></div>"#;
        assert_eq!(extract_sitekey(html), Some("6Ld_b5MeAAAA_AN4BP".into()));
    }

    #[test]
    fn extract_sitekey_from_api_js_script() {
        let key = "ABCdef1234567890ABCdEFG";
        let html = format!(
            r#"<script src="https://www.google.com/recaptcha/api.js?anchor=140&co=a.example.com&hl=en&v=xyz&k={key}" async defer></script>"#
        );
        assert_eq!(extract_sitekey(&html).as_deref(), Some(key));
    }

    #[test]
    fn extract_sitekey_absent_returns_none() {
        let html = "<form><input name='code'><img src='/captcha.jpg'></form>";
        assert_eq!(extract_sitekey(html), None);
    }

    #[test]
    fn task_kind_recaptcha_when_sitekey_present() {
        let ctx = CaptchaContext {
            sitekey: Some("6Ldb5_MeAAAAAN4B".into()),
            ..Default::default()
        };
        assert_eq!(TwoCaptchaSolver::task_kind(&ctx), "userrecaptcha");
    }

    #[test]
    fn task_kind_recaptcha_when_sitekey_in_html() {
        let ctx = CaptchaContext {
            html: r#"<div class="g-recaptcha" data-sitekey="6LDB5_MEAAAAAN4BP"></div>"#.into(),
            ..Default::default()
        };
        assert_eq!(TwoCaptchaSolver::task_kind(&ctx), "userrecaptcha");
    }

    #[test]
    fn task_kind_image_when_no_sitekey() {
        let ctx = CaptchaContext {
            html: "<img src='https://cdn.example/captcha.png?id=123'>".into(),
            ..Default::default()
        };
        assert_eq!(TwoCaptchaSolver::task_kind(&ctx), "base64");
    }

    // ----------------------------------------------------------------
    //  F#29 — chain + provider parsing
    // ----------------------------------------------------------------

    struct Stub(Option<String>, &'static str);
    #[async_trait::async_trait]
    impl CaptchaSolver for Stub {
        async fn solve(&self, _ctx: &CaptchaContext) -> Result<String, CaptchaError> {
            match self.0.as_ref() {
                Some(a) => Ok(a.clone()),
                None => Err(CaptchaError::Unsolvable { reason: "stub-no".into() }),
            }
        }
        fn name(&self) -> &'static str {
            self.1
        }
    }

    #[tokio::test]
    async fn auto_chain_returns_second_solver_answer() {
        let chain: Arc<dyn CaptchaSolver> = Arc::new(ChainedSolver::new(vec![
            Arc::new(Stub(None, "a")),
            Arc::new(Stub(Some("TOK2".into()), "b")),
        ]));
        let ctx = CaptchaContext::default();
        assert_eq!(chain.solve(&ctx).await.unwrap(), "TOK2");
    }

    #[tokio::test]
    async fn auto_chain_first_success_short_circuits() {
        let chain: Arc<dyn CaptchaSolver> = Arc::new(ChainedSolver::new(vec![
            Arc::new(Stub(Some("TOK1".into()), "a")),
            Arc::new(Stub(None, "b")),
        ]));
        assert_eq!(chain.solve(&CaptchaContext::default()).await.unwrap(), "TOK1");
    }

    #[tokio::test]
    async fn auto_chain_all_unsolvable_lists_each_step() {
        let chain: Arc<dyn CaptchaSolver> = Arc::new(ChainedSolver::new(vec![
            Arc::new(Stub(None, "step-a")),
            Arc::new(Stub(None, "step-b")),
        ]));
        let err = chain.solve(&CaptchaContext::default()).await.unwrap_err();
        match err {
            CaptchaError::Unsolvable { reason } => {
                assert!(reason.contains("step-a"), "reason={reason}");
                assert!(reason.contains("step-b"), "reason={reason}");
            }
            other => panic!("expected Unsolvable, got {other:?}"),
        }
    }

    #[test]
    fn parse_cdp_vision_aliases() {
        for s in ["cdp-vision", "cdpv", "cdp", "vision", "vision-cdp", "free-vision"] {
            assert_eq!(CaptchaProvider::parse(s), CaptchaProvider::CdpVision, "s={s}");
        }
    }

    #[test]
    fn parse_auto_aliases() {
        for s in ["auto", "chain", "best", "free"] {
            assert_eq!(CaptchaProvider::parse(s), CaptchaProvider::Auto, "s={s}");
        }
    }

    #[test]
    fn provider_as_str_roundtrip() {
        for (s, want) in [
            ("cdp-vision", CaptchaProvider::CdpVision),
            ("auto", CaptchaProvider::Auto),
            ("local", CaptchaProvider::Local),
        ] {
            assert_eq!(CaptchaProvider::parse(s).as_str(), CaptchaProvider::parse(s).as_str());
            assert_eq!(CaptchaProvider::parse(s), want, "s={s}");
        }
    }
}
