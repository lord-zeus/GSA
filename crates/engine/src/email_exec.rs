//! # F#12 — Email-engine execution (the "get the inbox, extract, visit" path)
//!
//! GSA's 217 email engines split into two families (verified from the
//! shipped `.ini` catalog + the unpacked `Search_Engine_Ranker_FREE.exe`):
//!
//! * **Mailbox family (75)** — `pop3 server/port/login/password/ssl` (port
//!   995, TLS). The operator's real inbox; POP3 flow: `+OK` greeting →
//!   `USER`/`PASS` → `STAT` → `RETR n`* → `QUIT`.
//! * **WEB family (142)** — `pop3 server=WEB`, `web parse url` (+ optional
//!   `web parse url post data`), `separator`, and front/back marker ladders.
//!   The inbox is a page/JSON fetched over HTTP.
//!
//! Both families share one tail — fetch → extract → visit — which
//! is the pipeline in [`execute_email_engine`]. The inbox source is
//! pluggable ([`InboxSource`]) so the whole thing is unit-testable without
//! sockets.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};

use native_tls::{TlsConnector, TlsStream};

use crate::driver::{Driver, DriverResponse};
use crate::email::{EmailEngine, EmailSubmission};
use crate::executor::{ExecutionReport, StepOutcome, StepOutcomeRef};

/// One inbox message, whatever the family. `address` is the sender (for
/// `emails match`); `subject` is best-effort; `body` is the raw content.
#[derive(Debug, Clone)]
pub struct InboxMessage {
    pub address: String,
    pub subject: String,
    pub body: String,
}

/// Where the inbox content comes from. Synchronous + `Send + Sync` so a
/// runner worker can drive it from `spawn_blocking`.
pub trait InboxSource: std::fmt::Debug + Send + Sync {
    /// Pull the inbox now. `Ok(vec![])` is "no mail yet".
    fn fetch(&self) -> Result<Vec<InboxMessage>, String>;
    /// Optional post-run cleanup (GSA's `pop3 del message` operator
    /// setting). No default — none of the 217 specs set it.
    fn delete_seen(&self, _addresses: &[String]) {}
}

// ---------------------------------------------------------------------------
// TCP POP3 source — the 75 mailbox-family specs
// ---------------------------------------------------------------------------

/// Synchronous POP3 verbs over a bidirectional byte stream. Kept as a
/// local trait (not the async one in `pop3.rs`) because this pipeline is
/// deliberately sync: it runs inside `spawn_blocking`, and `TcpStream`/
/// `TlsStream` are already sync.
pub trait Pop3Socket {
    /// First line — `+OK …` or `-ERR …`.
    fn greeting(&mut self) -> std::io::Result<String>;
    /// `REQ` → response line (no `.` framing).
    fn cmd(&mut self, req: &str) -> std::io::Result<String>;
    /// `RETR` data payload: lines until the standalone `.` terminator.
    fn data(&mut self) -> std::io::Result<String>;
}

impl Pop3Socket for TcpStream {
    fn greeting(&mut self) -> std::io::Result<String> {
        pop3_read_line(self)
    }
    fn cmd(&mut self, req: &str) -> std::io::Result<String> {
        self.write_all(req.as_bytes())?;
        self.write_all(b"\r\n")?;
        self.flush()?;
        pop3_read_line(self)
    }
    fn data(&mut self) -> std::io::Result<String> {
        pop3_read_data(self)
    }
}

impl Pop3Socket for TlsStream<TcpStream> {
    fn greeting(&mut self) -> std::io::Result<String> {
        pop3_read_line(self)
    }
    fn cmd(&mut self, req: &str) -> std::io::Result<String> {
        self.write_all(req.as_bytes())?;
        self.write_all(b"\r\n")?;
        self.flush()?;
        pop3_read_line(self)
    }
    fn data(&mut self) -> std::io::Result<String> {
        pop3_read_data(self)
    }
}

fn pop3_read_line(s: &mut impl Read) -> std::io::Result<String> {
    let mut buf: Vec<u8> = Vec::new();
    let mut one = [0u8; 1];
    while buf.len() < 65536 {
        match s.read_exact(&mut one) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        }
        if one[0] == b'\n' {
            break;
        }
        if one[0] != b'\r' {
            buf.push(one[0]);
        }
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn pop3_read_data(s: &mut impl Read) -> std::io::Result<String> {
    let mut out = String::new();
    loop {
        let l = pop3_read_line(s)?;
        if l == "." {
            break;
        }
        out.push_str(&l);
        out.push('\n');
    }
    Ok(out)
}

/// A POP3 connection (plain or TLS) — one `Read+Write` stream we can
/// pattern-match on for the verb dispatch.
pub enum Pop3Conn {
    Plain(TcpStream),
    Tls(TlsStream<TcpStream>),
}

impl std::fmt::Debug for Pop3Conn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Pop3Conn")
            .field(&std::mem::discriminant(self))
            .finish()
    }
}

impl Pop3Socket for Pop3Conn {
    fn greeting(&mut self) -> std::io::Result<String> {
        match self {
            Self::Plain(s) => s.greeting(),
            Self::Tls(s) => s.greeting(),
        }
    }
    fn cmd(&mut self, req: &str) -> std::io::Result<String> {
        match self {
            Self::Plain(s) => s.cmd(req),
            Self::Tls(s) => s.cmd(req),
        }
    }
    fn data(&mut self) -> std::io::Result<String> {
        match self {
            Self::Plain(s) => s.data(),
            Self::Tls(s) => s.data(),
        }
    }
}

/// Open a socket to a POP3 endpoint: direct TLS (the 75-mailbox default,
/// port 995) or plain (110). Returns whichever socket type landed.
pub fn pop3_connect(host: &str, port: u16, ssl: bool) -> Result<Pop3Conn, String> {
    let addrs: Vec<std::net::SocketAddr> = (host, port)
        .to_socket_addrs()
        .map_err(|e| e.to_string())?
        .collect();
    let addr = addrs.into_iter().next().ok_or_else(|| format!("no addresses for {host}"))?;
    let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(15)).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .map_err(|e| e.to_string())?;
    if ssl {
        let tls = tls_wrap(&stream, host)?;
        Ok(Pop3Conn::Tls(tls))
    } else {
        Ok(Pop3Conn::Plain(stream))
    }
}

fn tls_wrap(stream: &TcpStream, host: &str) -> Result<TlsStream<TcpStream>, String> {
    // native-tls 0.2: no insecure-cert knob in this build; the 217-mailbox
    // catalog trusts the platform root store, which is the GSA behaviour.
    let conn = TlsConnector::builder().build().map_err(|e| e.to_string())?;
    let cloned = stream.try_clone().map_err(|e| format!("clone: {e}"))?;
    conn.connect(host, cloned).map_err(|e| format!("{e} (host={host})"))
}

/// A POP3 inbox source (the 75 mailbox-family specs, port 995, TLS).
///
/// `login`/`password` must already be resolved — call
/// [`crate::email::resolve_email_tokens`] on the spec fields before
/// constructing. `Pop3Inbox` is the wire-protocol layer only; token
/// policy stays in `email.rs`.
#[derive(Debug, Clone)]
pub struct Pop3Inbox {
    host: String,
    port: u16,
    ssl: bool,
    pub login: String,
    pub password: String,
}

impl Pop3Inbox {
    pub fn new(
        host: impl Into<String>,
        port: u16,
        ssl: bool,
        login: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        Self { host: host.into(), port, ssl, login: login.into(), password: password.into() }
    }
    /// Open a fresh POP3 session (no data pulled yet).
    pub fn connect(&self) -> Result<Pop3Conn, String> {
        pop3_connect(&self.host, self.port, self.ssl)
    }
}

impl InboxSource for Pop3Inbox {
    /// Full RFC 1939 mailbox pull: `USER`/`PASS` → `STAT` → `RETR n`* → `QUIT`.
    fn fetch(&self) -> Result<Vec<InboxMessage>, String> {
        let mut sock = pop3_connect(&self.host, self.port, self.ssl)?;
        let hi = sock.greeting().map_err(|e| format!("greeting: {e}"))?;
        if hi.starts_with('-') {
            return Err(format!("POP3 greeting: {hi}"));
        }
        let user = sock.cmd(&format!("USER {}", self.login)).map_err(|e| e.to_string())?;
        if !user.starts_with('+') {
            return Err(format!("USER: {user}"));
        }
        let pass = sock.cmd(&format!("PASS {}", self.password)).map_err(|e| e.to_string())?;
        if !pass.starts_with('+') {
            return Err(format!("PASS: {pass}"));
        }
        let stat = sock.cmd("STAT").map_err(|e| e.to_string())?;
        if !stat.starts_with('+') {
            return Err(format!("STAT: {stat}"));
        }
        let count = stat
            .split_whitespace()
            .nth(1)
            .and_then(|t| t.parse().ok())
            .ok_or_else(|| format!("STAT reply: {stat}"))?;
        let mut out = Vec::with_capacity(count);
        for n in 1..=count {
            let r = sock.cmd(&format!("RETR {n}")).map_err(|e| e.to_string())?;
            if !r.starts_with('+') {
                return Err(format!("RETR {n}: {r}"));
            }
            let payload = sock.data().map_err(|e| format!("data {n}: {e}"))?;
            out.push(parse_pop3_message(&payload));
        }
        let _ = sock.cmd("QUIT");
        Ok(out)
    }
}

/// Split a raw POP3 payload into sender / subject / body. `From` is
/// normalised to the bare address (`Name <a@b>` → `a@b`) since GSA's
/// `emails match` globs are full-address.
pub fn parse_pop3_message(raw: &str) -> InboxMessage {
    let norm = raw.replace('\r', "");
    let (head, body) = match norm.find("\n\n") {
        Some(i) => norm.split_at(i),
        None => (norm.as_str(), ""),
    };
    let body = body.strip_prefix('\n').unwrap_or(body);
    let mut from = String::new();
    let mut subject = String::new();
    for line in head.lines() {
        if let Some((k, v)) = line.split_once(':') {
            match k.trim().to_ascii_lowercase().as_str() {
                "from" => from = v.trim().to_string(),
                "subject" => subject = v.trim().to_string(),
                _ => {}
            }
        }
    }
    let address = from
        .rsplit('<')
        .next()
        .and_then(|s| s.split_once('>').map(|(a, _)| a))
        .map(|a| a.trim().to_string())
        .unwrap_or(from);
    InboxMessage { address, subject, body: body.to_string() }
}

// ---------------------------------------------------------------------------
// HTTP source — the 142 `pop3 server=WEB` specs
// ---------------------------------------------------------------------------

/// HTTP inbox (the `web parse url` family). `driver` is injected so both
/// the runner and the tests drive the *same* pipeline through either a
/// `reqwest`-backed `HttpDriver` or a `MockDriver`.
#[derive(Clone)]
pub struct WebInbox {
    /// `web parse url` (already token-resolved — pass
    /// [`crate::email::resolve_email_tokens`] output in; `WebInbox` does
    /// no token work itself).
    pub url: String,
    /// `web parse url post data` (form-urlencoded or `None` for GET).
    pub post_body: Option<String>,
    /// `start url` — GET to prime session cookies (anonmails SID, etc.)
    /// before the inbox fetch hits it. `None` for hidebox-style providers
    /// where the inbox URL IS the landing page.
    pub start_url: Option<String>,
    pub driver: Arc<dyn Driver>,
}

impl std::fmt::Debug for WebInbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebInbox")
            .field("url", &self.url)
            .field("post_body", &self.post_body.is_some())
            .field("start_url", &self.start_url.is_some())
            .finish()
    }
}

impl WebInbox {
    pub fn new(url: impl Into<String>, post_body: Option<String>, driver: Arc<dyn Driver>) -> Self {
        Self { url: url.into(), post_body, start_url: None, driver }
    }
    /// Prime session cookies before the inbox fetch (GSA `start url`).
    pub fn with_start_url(mut self, url: impl Into<String>) -> Self {
        self.start_url = Some(url.into());
        self
    }
}

impl InboxSource for WebInbox {
    fn fetch(&self) -> Result<Vec<InboxMessage>, String> {
        let fields = parse_form_body(self.post_body.as_deref().unwrap_or(""));
        // `start url` (GSA anonmails `start url` etc.) — GET to prime
        // session cookies (SID) so the inbox fetch below is signed in.
        // A failed priming GET is non-fatal — GSA tolerates it (the
        // inbox fetch may still work, and a follow-up retry gets a
        // fresh SID anyway).
        if let Some(s) = self.start_url.clone() {
            let _ = block_on_driver(self.driver.as_ref(), s, HashMap::new());
        }
        // With a body → `Driver::submit` (a form POST); without → `navigate`.
        let resp = block_on_driver(self.driver.as_ref(), self.url.clone(), fields);
        if !resp.ok {
            return Err(resp.error.unwrap_or_else(|| "inbox HTTP fetch failed".into()));
        }
        // The inbox *is* the response. The pipeline then runs
        // `extract_from_message` over each "message" — for the WEB family
        // the front/back ladders and `message match` apply straight to the
        // fetched page, matching GSA's behaviour for these 142 specs.
        Ok(vec![InboxMessage { address: String::new(), subject: String::new(), body: resp.html }])
    }
}

/// Parse a form-urlencoded body into a `k=v` map. Empty body → empty map.
pub fn parse_form_body(body: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    if body.trim().is_empty() {
        return out;
    }
    for pair in body.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            out.insert(url_decode(k), url_decode(v));
        }
    }
    out
}

/// Percent-decoder for the small alphabet `post data` uses: `+` → space,
/// `%XX` → byte; other bytes pass through.
fn url_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < b.len() && is_hex(b[i + 1]) && is_hex(b[i + 2]) => {
                out.push(hex_byte(b[i + 1], b[i + 2]));
                i += 3;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}
fn is_hex(b: u8) -> bool {
    matches!(b, b'0'..=b'9' | b'a'..=b'f' | b'A'..=b'F')
}
fn hex_byte(h: u8, l: u8) -> u8 {
    fn hv(b: u8) -> u8 {
        match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            b'A'..=b'F' => b - b'A' + 10,
            _ => 0,
        }
    }
    (hv(h) << 4) | hv(l)
}

// ---------------------------------------------------------------------------
// Dispatch — build the right inbox source from a parsed [EmailEngine]
// ---------------------------------------------------------------------------

/// Per-job email credentials (for mailbox-family logins). `email_address`
/// is the recipient, `password` the mailbox password; either may be empty
/// for WEB-family specs (where the inbox comes from the site's HTTP
/// endpoint, not a POP3 login).
#[derive(Debug, Clone, Default)]
pub struct EmailContext {
    /// The operator's receiving mailbox (for `%email%` / `%emailuser%`).
    pub email_address: String,
    /// The operator's mailbox password (for `%password%`).
    pub password: String,
}

/// Build the [`InboxSource`] for one [EmailEngine] — the 75-mailbox /
/// 142-WEB dispatch. WEB-family specs resolve their `web parse url`
/// token once here; mailbox-family specs resolve `%email%` /
/// `%password%` on the login credentials.
pub fn build_source(
    engine: &EmailEngine,
    ctx: &EmailContext,
    driver: Arc<dyn Driver>,
) -> Box<dyn InboxSource> {
    if engine.pop3_server.eq_ignore_ascii_case("web") {
        let raw_url = engine.web_parse_url.clone().unwrap_or_default();
        let url = crate::email::resolve_email_tokens(&raw_url, &ctx.email_address, &ctx.password);
        // `web parse url post data` (127/217 specs) — token-resolved form body;
        // `fetch()` will POST it via `Driver::submit`.
        let post_body = engine.web_parse_post.clone().map(|p| {
            crate::email::resolve_email_tokens(&p, &ctx.email_address, &ctx.password)
        });
        let mut box_ = WebInbox { url, post_body, start_url: None, driver };
        // `start url` (anonmails `https://anonmails.de/` etc.) — token-resolve
        // then attach so `fetch()` GETs it first to prime the SID cookie.
        if let Some(s) = engine.start_url.clone() {
            box_ = box_.with_start_url(s);
        }
        Box::new(box_)
    } else {
        let host = engine.pop3_server.clone();
        let port = engine.pop3_port.max(1) as u16;
        let ssl = engine.pop3_ssl;
        let login = crate::email::resolve_email_tokens(&engine.pop3_login, &ctx.email_address, &ctx.password);
        let password = crate::email::resolve_email_tokens(&engine.pop3_password, &ctx.email_address, &ctx.password);
        Box::new(Pop3Inbox::new(host, port, ssl, login, password))
    }
}

// ---------------------------------------------------------------------------
// Shared pipeline
// ---------------------------------------------------------------------------

/// Result of [`execute_email_engine`]: `report` is persistable (identical
/// shape to the HTTP/browser engines — the runner can `record_result` it
/// directly), plus the raw messages + extracted candidates for UI/debug.
#[derive(Debug)]
pub struct EmailRunResult {
    pub report: ExecutionReport,
    pub messages: Vec<InboxMessage>,
    pub candidates: Vec<EmailSubmission>,
}

/// Run one email engine end-to-end: fetch → extract → visit.
///
/// `timeout` bounds the whole run (fetch + verification visit). The
/// runner's job-level timeout wraps this the same way it wraps the
/// HTTP/browser engines.
///
/// `driver` is used only for the final verification hop (clicking the
/// inbox link); inbox acquisition goes through [`InboxSource`], so both
/// concerns stay testable without sockets.
pub fn execute_email_engine(
    engine: &EmailEngine,
    source: &dyn InboxSource,
    driver: &dyn Driver,
    timeout: Duration,
) -> EmailRunResult {
    fn done(report: &mut ExecutionReport, messages: &Vec<InboxMessage>, candidates: &Vec<EmailSubmission>) -> EmailRunResult {
        EmailRunResult { report: report.clone(), messages: messages.clone(), candidates: candidates.clone() }
    }
    let start = Instant::now();
    let mut report = ExecutionReport {
        successful: 0,
        content_success: 0,
        failed: 0,
        retried: 0,
        captcha_failed: 0,
        submitted_url: None,
        submitted_url_source: None,
        errors: vec![],
        steps: vec![],
    };
    let mut messages: Vec<InboxMessage> = vec![];
    let mut candidates: Vec<EmailSubmission> = vec![];

    // 1) Fetch.
    match source.fetch() {
        Ok(m) if !m.is_empty() => messages = m,
        Ok(_) => {
            report.errors.push("inbox is empty (no mail yet)".into());
            report.failed += 1;
            return done(&mut report, &messages, &candidates);
        }
        Err(e) => {
            report.errors.push(format!("inbox fetch: {e}"));
            report.failed += 1;
            return done(&mut report, &messages, &candidates);
        }
    }
    if start.elapsed() > timeout {
        report.errors.push("inbox fetch timed out".into());
        report.failed += 1;
        return done(&mut report, &messages, &candidates);
    }

    // 2) Extract candidates.
    for m in &messages {
        candidates.extend(extract_candidates(engine, &m.body));
    }
    if candidates.is_empty() {
        report.errors.push(
            "no submission link in inbox (check `url_front*`/`url_back*` ladders and `message match`)"
                .into(),
        );
        report.failed += 1;
        return done(&mut report, &messages, &candidates);
    }
    if start.elapsed() > timeout {
        report.errors.push("extraction timed out".into());
        report.failed += 1;
        return done(&mut report, &messages, &candidates);
    }

    // 3) Verification visit (the "click the link" step).
    let target = candidates[0].url.clone();
    let resp = block_on_driver(driver, target.clone(), HashMap::new());
    if !resp.ok {
        let reason = resp.error.clone().unwrap_or_default();
        report.errors.push(format!("visit {target}: {reason}"));
        report.failed += 1;
        return done(&mut report, &messages, &candidates);
    }
    report.successful = 1;
    // T13b gate (runner counts content_success>0 as a confirmed creation):
    // for mail engines, the inbox link that extracted + verified IS the
    // creation confirmation, so it counts as content success (GSA parity —
    // a verified mail engine is a success, not a login-only wall).
    report.content_success = 1;
    report.submitted_url = Some(candidates[0].url.clone());
    report.submitted_url_source = Some(resp.final_url);
    report.steps.push(StepOutcomeRef { outcome: StepOutcome::Success, matched_phrase: None });
    // F#12-ext (copy GSA `delurl`, 22/27 INIs): best-effort DELETE of the
    // consumed message. The verify already succeeded above — a delete
    // failure is logged but must not turn a successful verify into a failure.
    // GET (not DELETE) because all shipped delurl targets (mailinator,
    // mailcatch, dispostable, spamfree24, trash-mail) are href-style
    // GET links; a POST-based inbox would just ignore the verb.
    if let Some(del) = &candidates[0].delete_url {
        let del_url = del.trim().to_string();
        if !del_url.is_empty() {
            let dresp = block_on_driver(driver, del_url.clone(), HashMap::new());
            if !dresp.ok {
                report
                    .errors
                    .push(format!("delete message {del_url}: {}", dresp.error.unwrap_or_default()));
            }
        }
    }
    done(&mut report, &messages, &candidates)
}

/// [`crate::email::extract_from_message`] plus a plain-URL fallback when
/// the engine defines no `url_*` markers at all (a few mailbox-family
/// specs in the catalog don't — GSA scans for the first bare link).
fn extract_candidates(engine: &EmailEngine, body: &str) -> Vec<EmailSubmission> {
    let got = crate::email::extract_from_message(engine, body);
    if !got.is_empty() {
        return got;
    }
    let has_ladder = engine
        .url_fb
        .iter()
        .any(|(f, b)| f.as_deref().map(|s| !s.is_empty()).unwrap_or(false)
            || b.as_deref().map(|s| !s.is_empty()).unwrap_or(false));
    if has_ladder {
        return Vec::new();
    }
    body.split_whitespace()
        .find(|t| t.starts_with("http://") || t.starts_with("https://"))
        .map(|t| t.trim_end_matches(|c: char| !c.is_ascii_alphanumeric()).to_string())
        .filter(|u: &String| !u.is_empty())
        .map(|u| vec![EmailSubmission { url: u, email_line: None, subject_line: None, delete_url: None, code: None }])
        .unwrap_or_default()
}

/// `block_in_place` bridge: drive async [`Driver`] from this sync pipeline.
/// Each call spawns a one-shot current-thread runtime — correct here
/// because the pipeline runs under the runner's `spawn_blocking` (no
/// runtime in context) and each call is one-shot.
pub(crate) fn block_on_driver(
    d: &dyn Driver,
    url: String,
    fields: HashMap<String, String>,
) -> DriverResponse {
    let rt =
        tokio::runtime::Builder::new_current_thread().enable_all().build().expect("tokio runtime");
    rt.block_on(async {
        if fields.is_empty() {
            d.navigate(url).await
        } else {
            d.submit(url, fields, Duration::from_secs(30)).await
        }
    })
}

/// Convenience runner-facing entry: dispatch ([`build_source`]) + execute
/// in one call. `ctx` carries the operator's receiving mailbox + password
/// (used for `%email%` / `%password%` token resolution); `driver` handles
/// the WEB-family inbox fetch (HTTP) and the final verification visit.
///
/// Synchronous by design — the runner wraps this in
/// `tokio::task::spawn_blocking` + its own per-job timeout, mirroring the
/// HTTP/browser engine path.
pub fn run_email_engine(
    engine: &EmailEngine,
    ctx: &EmailContext,
    driver: &Arc<dyn Driver>,
    timeout: Duration,
) -> EmailRunResult {
    let source = build_source(engine, ctx, driver.clone());
    execute_email_engine(engine, source.as_ref(), driver.as_ref(), timeout)
}

// ---------------------------------------------------------------------------
// Tests — grounded in the shipped catalog (75 POP3 / 142 WEB) shapes.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::MockDriver;
    use crate::email::parse_email_engine_str;

    const MAIL_BODY: &str = "From: LinkBot <links@provider.com>\nSubject: your link is ready\n\n<html><a href=\"https://go.example.com/v/abc123\">Verify</a></html>";

    #[test]
    fn parse_pop3_message_extracts_from_to_bare_address() {
        let m = parse_pop3_message(MAIL_BODY);
        assert_eq!(m.address, "links@provider.com");
        assert_eq!(m.subject, "your link is ready");
        assert!(m.body.contains("go.example.com/v/abc123"));
    }

    #[test]
    fn parse_pop3_message_handles_plain_from() {
        let m = parse_pop3_message("From: bob@gmail.com\nSubject: hi\n\nbody");
        assert_eq!(m.address, "bob@gmail.com");
    }

    #[test]
    fn parse_form_body_kv() {
        let out = parse_form_body("LocalPart=joe&DomainType=public&Password=");
        assert_eq!(out.get("LocalPart"), Some(&"joe".to_string()));
        assert_eq!(out.get("DomainType"), Some(&"public".to_string()));
        assert_eq!(out.get("Password"), Some(&String::new()));
    }

    #[test]
    fn parse_form_body_decodes_percent_and_plus() {
        let out = parse_form_body("copy=a+b%2Fc%2Fd&x=%C3%A9");
        assert_eq!(out.get("copy"), Some(&"a b/c/d".to_string()));
        assert_eq!(out.get("x"), Some(&"\u{00E9}".to_string()));
    }

    /// End-to-end: canned inbox + `MockDriver`; asserts the report the
    /// runner would persist (success + visited URL) and candidate extraction.
    #[test]
    fn execute_engine_end_to_end() {
        let engine =
            parse_email_engine_str(
                "[setup]\nemails match=*@provider.com\npop3 server=WEB\nurl_front=<a href=\"\nurl_back=\">\nenabled=1\n",
                "sample",
            )
            .unwrap();
        struct Canned;
        impl std::fmt::Debug for Canned {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "Canned")
            }
        }
        impl InboxSource for Canned {
            fn fetch(&self) -> Result<Vec<InboxMessage>, String> {
                Ok(vec![InboxMessage {
                    address: "links@provider.com".into(),
                    subject: "your link".into(),
                    body: MAIL_BODY.into(),
                }])
            }
        }
        let drv = MockDriver::with_html("<html>OK</html>");
        let res = execute_email_engine(&engine, &Canned, &drv, Duration::from_secs(5));
        assert_eq!(res.report.successful, 1, "errors: {:?}", res.report.errors);
        assert!(res.report.submitted_url.as_deref().unwrap_or("").contains("go.example.com/v/abc123"));
        assert_eq!(res.candidates.len(), 1);
    }

    #[test]
    fn execute_engine_empty_inbox_fails_cleanly() {
        let engine = parse_email_engine_str("[setup]\nemails match=*@x.com\npop3 server=WEB\n", "e").unwrap();
        struct Empty;
        impl std::fmt::Debug for Empty {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "Empty")
            }
        }
        impl InboxSource for Empty {
            fn fetch(&self) -> Result<Vec<InboxMessage>, String> {
                Ok(vec![])
            }
        }
        let drv = MockDriver::with_html("x");
        let res = execute_email_engine(&engine, &Empty, &drv, Duration::from_secs(5));
        assert_eq!(res.report.failed, 1);
        assert!(res.report.errors.join("; ").contains("empty"));
    }

    #[test]
    fn execute_engine_fetch_error_surfaces() {
        let engine = parse_email_engine_str("[setup]\nemails match=*@x.com\npop3 server=WEB\n", "e").unwrap();
        struct Boom;
        impl std::fmt::Debug for Boom {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "Boom")
            }
        }
        impl InboxSource for Boom {
            fn fetch(&self) -> Result<Vec<InboxMessage>, String> {
                Err("connection refused".into())
            }
        }
        let drv = MockDriver::with_html("x");
        let res = execute_email_engine(&engine, &Boom, &drv, Duration::from_secs(5));
        assert!(res.report.errors.join("; ").contains("connection refused"));
    }

    #[test]
    fn execute_engine_mailbox_family_shape() {
        const SPEC: &str = r#"[setup]
emails match=*@btinternet.com|*@btopenworld.com
pop3 server=mail.btinternet.com
pop3 port=995
pop3 login=%email%
pop3 password=%password%
pop3 ssl=1
"#;
        let e = parse_email_engine_str(SPEC, "BT Mail").unwrap();
        assert_eq!(e.pop3_server, "mail.btinternet.com");
        assert_eq!(e.pop3_port, 995);
        assert!(e.pop3_ssl);
        assert_eq!(e.pop3_login, "%email%");
        assert_eq!(e.pop3_password, "%password%");
    }

    /// F#12 — 75-mailbox family E2E over a REAL plaintext POP3 wire:
    /// an in-process `TcpListener` + thread plays the POP3 server,
    /// `Pop3Inbox` connects with real sockets. Proves USER/PASS/STAT/RETR
    /// framing, `parse_pop3_message`, marker extraction and the visit hop
    /// all work end-to-end (the 2-message reply also exercises per-message
    /// `emails match` filtering).
    #[test]
    fn execute_engine_mailbox_family_over_real_tcp() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind mock pop3");
        let port = listener.local_addr().unwrap().port();

        let server = std::thread::spawn(move || {
            let (sock, _) = listener.accept().expect("accept client");
            run_mock_pop3(sock);
        });

        let ini = "[setup]\nemails match=*@btinternet.com\npop3 server=127.0.0.1\npop3 port=1\npop3 login=%email%\npop3 password=%password%\npop3 ssl=0\nurl_front=href=\"\nurl_back=\">\nenabled=1";
        let mut engine = parse_email_engine_str(ini, "bt-mail").unwrap();
        engine.pop3_port = port as i32;

        let ctx = EmailContext { email_address: "me@btinternet.com".into(), password: "pw123".into() };
        let drv: std::sync::Arc<dyn Driver> = std::sync::Arc::new(crate::driver::MockDriver::with_html("<ok>"));
        let source = build_source(&engine, &ctx, drv.clone());
        let res = execute_email_engine(&engine, source.as_ref(), drv.as_ref(), std::time::Duration::from_secs(5));
        assert_eq!(res.report.successful, 1, "errors: {:?}", res.report.errors);
        assert!(res.report.submitted_url.as_deref().unwrap_or("").contains("v/mailboxok"));
        server.join().unwrap();
    }

    /// Plaintext POP3 server: greeting; USER/PASS; STAT (2 msgs); RETR 1 =
    /// the submission mail; RETR 2 = unrelated; QUIT.

    /// F#12 - WEB family with `web parse url post data` (127/217 files):
    /// `build_source` token-resolves the body and `WebInbox.fetch` hands the
    /// form to `Driver::submit` (MockDriver ignores it and returns canned HTML);
    /// marker extraction finds the inbox link.
    #[test]
    fn execute_engine_web_family_with_post() {
        // Raw string r#"..."# : markers keep their literal `\"` (escaped quote)
        // exactly as the GSA .ini files carry them.
        let ini = r#"[setup]
emails match=*@gmail.com
pop3 server=WEB
web parse url=https://inbox.example/api
web parse url post data=emailAddr=%emailuser%&emailPass=%password%
url_front=<a href="
url_back=">
enabled=1"#;
        let engine = parse_email_engine_str(ini, "web-post").unwrap();
        assert_eq!(
            engine.web_parse_post.as_deref(),
            Some("emailAddr=%emailuser%&emailPass=%password%")
        );
        let ctx = EmailContext {
            email_address: "joe@gmail.com".into(),
            password: "p@ss".into(),
        };
        let url_ok = "<a href=\"https://go.example.com/p/v1\">x</a>";
        let drv = MockDriver::with_html(url_ok.clone());
        let source = build_source(&engine, &ctx, std::sync::Arc::new(drv.clone()));
        let msg = source.fetch().unwrap();
        assert_eq!(msg.len(), 1);
        assert!(msg[0].body.contains(&url_ok));
        let drv_ref: &dyn Driver = &drv;
        let res = execute_email_engine(
            &engine,
            source.as_ref(),
            drv_ref,
            std::time::Duration::from_secs(5),
        );
        assert_eq!(res.report.successful, 1, "errors: {:?}", res.report.errors);
        assert!(res
            .report
            .submitted_url
            .as_deref()
            .unwrap_or("")
            .contains("go.example.com/p/v1"));
    }


    fn run_mock_pop3(mut sock: std::net::TcpStream) {
        use std::io::{Read, Write};
        let read_cmd = |s: &mut std::net::TcpStream| -> Option<String> {
            let mut l = String::new();
            let mut c = [0u8; 1];
            loop {
                if s.read_exact(&mut c).is_err() {
                    return if l.is_empty() { None } else { Some(l) };
                }
                if c[0] == b'\n' {
                    return Some(l);
                }
                if c[0] == b'\r' {
                    continue;
                }
                l.push(c[0] as char);
            }
        };
        sock.write_all(b"+OK pop3 ready\r\n").ok();
        while let Some(cmd) = read_cmd(&mut sock) {
            let c = cmd.to_ascii_uppercase();
            if c.starts_with("USER") {
                sock.write_all(b"+OK user\r\n").ok();
            } else if c.starts_with("PASS") {
                sock.write_all(b"+OK pass\r\n").ok();
            } else if c.starts_with("STAT") {
                sock.write_all(b"+OK 2 15000\r\n").ok();
            } else if c.starts_with("RETR 1") {
                sock.write_all(b"+OK mail1\r\n").ok();
                sock.write_all(b"From: Bot <bot@btinternet.com>\r\nSubject: your link\r\n\r\n<html><a href=\"https://site.example/v/mailboxok\">go</a></html>\r\n.\r\n").ok();
            } else if c.starts_with("RETR 2") {
                sock.write_all(b"+OK mail2\r\n").ok();
                sock.write_all(b"From: Other <other@else.com>\r\nSubject: noise\r\n\r\nnothing useful\r\n.\r\n").ok();
            } else if c.starts_with("QUIT") {
                sock.write_all(b"+OK bye\r\n").ok();
                break;
            }
        }
    }
}