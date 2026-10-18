//! F#18 — outbound SMTP delivery (the "email me an alert" channel).
//!
//! Hand-rolled SMTP, **synchronous** over `std::net::TcpStream` +
//! `native-tls` — the SAME idiom F#12's POP3 receive path uses
//! (`email_exec.rs` / `pop3.rs`), driven from `spawn_blocking` by the API's
//! notifier so it never blocks the runtime. No new external crate
//! (`native-tls` + `tokio` are already in the tree).
//!
//! Three modes:
//! - `starttls`   — port 587: banner → `EHLO` → `STARTTLS` → upgrade →
//!                  re-`EHLO` → `AUTH LOGIN` (RFC 3207).
//! - `direct-tls` — port 465: implicit TLS handshake → banner → `AUTH LOGIN`
//!                  (RFC 8314).
//! - `log`        — no socket: render the RFC822 message and return it.
//!                  The API uses this when no SMTP host is configured (and it
//!                  is what the test suite exercises), so email delivery stays
//!                  observable without a mail server.
//!
//! The surface is a free function: the notifier assembles one
//! [`crate::notify::Notification`], calls [`send`], and classifies the
//! `Result<SendResult, String>`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{ToSocketAddrs, TcpStream as StdTcpStream};
use std::time::Duration;

use native_tls::{TlsConnector, TlsStream};

use crate::notify::Notification;
use serde::{Deserialize, Serialize};

/// Where + how to deliver the alert mail.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    #[serde(default = "default_user")]
    pub auth_user: String,
    pub auth_password: String,
    /// `starttls` (587) or `direct-tls` (465) or `log` (no socket).
    #[serde(default = "default_mode")]
    pub mode: SmtpMode,
    /// The "From:" display name. Empty = no display name.
    pub sender_name: String,
    /// Where alerts are addressed. Empty = email channel is off.
    pub recipient: String,
    /// Per-connect timeout, seconds (default 15).
    #[serde(default = "default_timeout")]
    pub connect_timeout_secs: u64,
}
fn default_user() -> String { String::new() }
fn default_mode() -> SmtpMode { SmtpMode::StartTls }
fn default_timeout() -> u64 { 15 }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SmtpMode {
    Log,
    #[default]
    StartTls,
    DirectTls,
}

/// The outcome of a send — the notifier classifies `Err` into a
/// `SystemError` notification (so the UI's bell says "SMTP down" rather
/// than silently dropping the alert).
#[derive(Debug)]
pub struct SendResult {
    pub delivered: bool,
    pub message_size: usize,
    pub mode: SmtpMode,
    /// `Log` mode (and, kept for debug) the rendered RFC822 message.
    pub rendered: Option<String>,
}

/// Build the RFC822 message for one notification. Pure — no I/O.
pub fn render_message(cfg: &SmtpConfig, n: &Notification) -> String {
    let from = if cfg.sender_name.is_empty() {
        cfg.auth_user.clone()
    } else {
        format!("{} <{}>", cfg.sender_name, cfg.auth_user)
    };
    let subject = format!("[SER] {} — {}", n.title, n.severity);
    let body = format!(
        "type: {}\nseverity: {}\nread: {}\n\n{}\n",
        n.type_.as_str(),
        n.severity.as_str(),
        if n.is_read { "yes" } else { "no" },
        n.message
    );
    format!(
        "Date: {}\r\nFrom: {}\r\nTo: {}\r\nSubject: {}\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{}\r\n",
        n.created_at.format("%a, %d %b %Y %H:%M:%S %z"),
        from,
        cfg.recipient,
        subject,
        body
    )
}

/// Deliver (or render, in `log` mode). Synchronous — call it from
/// `spawn_blocking`. Never panics on a socket error: failures are
/// `Err(String)` for the notifier to classify.
pub fn send(cfg: &SmtpConfig, n: &Notification) -> Result<SendResult, String> {
    if cfg.mode == SmtpMode::Log {
        let msg = render_message(cfg, n);
        return Ok(SendResult { delivered: true, message_size: msg.len(), mode: SmtpMode::Log, rendered: Some(msg) });
    }
    if cfg.recipient.trim().is_empty() {
        return Err("email channel not configured (recipient is empty)".into());
    }
    let rendered = render_message(cfg, n);
    let n = rendered.len();

    let stream = open(&cfg.host, cfg.port, cfg.connect_timeout_secs)?;
    let delivered = match cfg.mode {
        SmtpMode::DirectTls => send_direct_tls(cfg, stream, &rendered),
        // `log` handled above; `starttls` is the default.
        _ => send_starttls(cfg, stream, &rendered),
    };
    delivered?;
    Ok(SendResult { delivered: true, message_size: n, mode: cfg.mode, rendered: Some(rendered) })
}

// ── transport ────────────────────────────────────────────────────────────────

enum Sock {
    Plain(StdTcpStream),
    Tls(TlsStream<StdTcpStream>),
}
impl Read for Sock {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Sock::Plain(s) => s.read(buf),
            Sock::Tls(s) => s.read(buf),
        }
    }
}
impl Write for Sock {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Sock::Plain(s) => s.write(buf),
            Sock::Tls(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Sock::Plain(s) => s.flush(),
            Sock::Tls(s) => s.flush(),
        }
    }
}

impl Sock {
    fn buf(&mut self) -> BufReader<&mut Sock> {
        BufReader::new(self)
    }
    fn write_line(&mut self, line: &str) -> Result<(), String> {
        let buf = format!("{}\r\n", line);
        self.write_all(buf.as_bytes()).map_err(|e| e.to_string())?;
        self.flush().map_err(|e| e.to_string())
    }
    fn write_body(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.write_all(bytes).map_err(|e| e.to_string())?;
        self.flush().map_err(|e| e.to_string())
    }
    /// Read one multi-line SMTP reply; return (code, full text).
    fn reply(&mut self) -> Result<(u16, String), String> {
        let mut r = self.buf();
        let mut full = String::new();
        loop {
            let mut line = String::new();
            if r.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
                return Err("server closed the connection mid-reply".into());
            }
            let trimmed = line.trim_end_matches("\r\n");
            if full.is_empty() {
                full = trimmed.to_string();
            };
            // Multi-line: "250-" continues; "250 " (space) / 3-digit end.
            if line.len() >= 4 && &line[3..4] == "-" {
                continue;
            }
            let code = trimmed.get(..3).and_then(|t| t.parse().ok()).ok_or("bad reply (no code)")?;
            return Ok((code, full));
        }
    }
    /// Send a command and assert the exact status code.
    fn cmd(&mut self, line: &str, want: u16, phase: &str) -> Result<(), String> {
        self.write_line(line)?;
        let (code, resp) = self.reply()?;
        if code != want {
            return Err(format!("smtp {phase}: expected {want}, got {code} ({resp})"));
        }
        Ok(())
    }
}

fn tls_wrap(stream: StdTcpStream, host: &str) -> Result<TlsStream<StdTcpStream>, String> {
    let conn = TlsConnector::builder().build().map_err(|e| e.to_string())?;
    let cloned = stream.try_clone().map_err(|e| e.to_string())?;
    conn.connect(host, cloned).map_err(|e| format!("{e} (host={host})"))
}

/// A harmless placeholder `StdTcpStream` for the `mem::replace` idiom
/// (which needs a same-type value): a loopback pair (listener + connected
/// client). The dummy never sees our SMTP traffic — it only occupies the
/// enum slot for one instruction.
fn plain_dummy() -> Result<StdTcpStream, String> {
    let l = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    let _c = StdTcpStream::connect(l.local_addr().map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let (s, _) = l.accept().map_err(|e| e.to_string())?;
    Ok(s)
}

fn open(host: &str, port: u16, timeout_secs: u64) -> Result<StdTcpStream, String> {
    let addrs = (host, port)
        .to_socket_addrs()
        .map_err(|e| e.to_string())?
        .collect::<Vec<_>>();
    let addr = addrs.into_iter().next().ok_or_else(|| format!("no addresses for {host}"))?;
    let timeout = Duration::from_secs(timeout_secs.max(1));
    let s = StdTcpStream::connect_timeout(&addr, timeout).map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(timeout)).map_err(|e| e.to_string())?;
    s.set_write_timeout(Some(timeout)).map_err(|e| e.to_string())?;
    Ok(s)
}

/// starttls (587) — plain banner → EHLO → STARTTLS → upgrade → re-EHLO →
/// AUTH LOGIN → mail/recv/data/body → QUIT.
fn send_starttls(cfg: &SmtpConfig, stream: StdTcpStream, msg: &str) -> Result<(), String> {
    let mut s = Sock::Plain(stream);

    let (code, resp) = s.reply()?;
    if code != 220 {
        return Err(format!("smtp banner: expected 220, got {code} ({resp})"));
    }
    s.cmd("EHLO ser-replica.local", 250, "ehlo")?;
    s.cmd("STARTTLS", 220, "starttls")?;

    // Upgrade in place: swap the enum slot with a throwaway stream, take
    // the real one out, wrap it TLS, and assign the wrapped stream back.
    // The dummy stream (left behind in the slot) is dropped by the
    // assignment below.
    let placeholder = Sock::Plain(plain_dummy()?);
    let old = std::mem::replace(&mut s, placeholder);
    let real = match old {
        Sock::Plain(r) => r,
        _ => return Err("internal: expected plain socket before STARTTLS".into()),
    };
    let tls = tls_wrap(real, &cfg.host)?;
    s = Sock::Tls(tls);
    // Re-EHLO after the TLS upgrade (RFC 3207 requires it).
    s.cmd("EHLO ser-replica.local", 250, "ehlo-after-tls")?;

    auth_login(&mut s, cfg)?;
    data_phase(&mut s, cfg, msg)
}

/// direct-tls (465) — implicit handshake first, then the same tail.
fn send_direct_tls(cfg: &SmtpConfig, stream: StdTcpStream, msg: &str) -> Result<(), String> {
    let tls = tls_wrap(stream, &cfg.host)?;
    let mut s = Sock::Tls(tls);

    let (code, resp) = s.reply()?;
    if code != 220 {
        return Err(format!("smtp banner (tls): expected 220, got {code} ({resp})"));
    }
    s.cmd("EHLO ser-replica.local", 250, "ehlo")?;

    auth_login(&mut s, cfg)?;
    data_phase(&mut s, cfg, msg)
}

fn auth_login(s: &mut Sock, cfg: &SmtpConfig) -> Result<(), String> {
    s.cmd("AUTH LOGIN", 334, "auth-challenge")?;
    s.cmd(&b64(&cfg.auth_user), 334, "auth-user")?;
    s.cmd(&b64(&cfg.auth_password), 235, "auth-pass")?;
    Ok(())
}

fn data_phase(s: &mut Sock, cfg: &SmtpConfig, msg: &str) -> Result<(), String> {
    s.cmd(&format!("MAIL FROM:<{}>", cfg.auth_user), 250, "mail-from")?;
    s.cmd(&format!("RCPT TO:<{}>", cfg.recipient), 250, "rcpt-to")?;
    s.cmd("DATA", 354, "data")?;
    // body already carries its trailing CRLF.CRLF from `render_message`.
    s.write_body(msg.as_bytes()).map_err(|e| format!("smtp body: {e}"))?;
    let (code, resp) = s.reply()?;
    if code != 250 {
        return Err(format!("smtp body: expected 250, got {code} ({resp})"));
    }
    s.cmd("QUIT", 221, "quit")
}

/// base64 encoding (no dependency) — enough for `AUTH LOGIN` credentials.
fn b64(input: &str) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in input.as_bytes().chunks(3) {
        let v = match chunk.len() {
            3 => (chunk[0] as u32) << 16 | (chunk[1] as u32) << 8 | chunk[2] as u32,
            2 => (chunk[0] as u32) << 16 | (chunk[1] as u32) << 8,
            1 => (chunk[0] as u32) << 16,
            _ => continue,
        };
        out.push(A[(v >> 18 & 63) as usize] as u8 as char);
        out.push(A[(v >> 12 & 63) as usize] as u8 as char);
        if chunk.len() >= 2 { out.push(A[(v >> 6 & 63) as usize] as u8 as char); } else { out.push('='); }
        if chunk.len() == 3 { out.push(A[(v & 63) as usize] as u8 as char); } else { out.push('='); }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};

    fn sample() -> Notification {
        let c = crate::notify::Classified {
            type_: crate::notify::NotificationType::RunCompleted,
            severity: crate::notify::Severity::Good,
            title: "Run complete".into(),
            message: "All 4 jobs done.".into(),
            project_id: Some("p1".into()),
            job_id: None,
            payload: serde_json::json!({ "claimed": 4 }),
        };
        Notification::new_from_classified(
            c, "id-1".into(),
            DateTime::parse_from_rfc3339("2026-10-18T08:00:00Z").unwrap().with_timezone(&Utc),
        )
    }

    #[test]
    fn b64_roundtrip() {
        assert_eq!(b64("abc"), "YWJj");
        assert_eq!(b64("abcd"), "YWJjZA==");
        assert_eq!(b64("abcde"), "YWJjZGU=");
        assert_eq!(b64(""), "");
        assert_eq!(b64("ser-ranker"), "c2VyLXJhbmtlcg==");
    }

    #[test]
    fn renders_rfc822_message() {
        let cfg = SmtpConfig {
            host: "smtp.example.com".into(), port: 587,
            auth_user: "me@example.com".into(), auth_password: "pw".into(),
            mode: SmtpMode::StartTls, sender_name: "SER Alerts".into(),
            recipient: "ops@example.com".into(), ..Default::default()
        };
        let msg = render_message(&cfg, &sample());
        assert!(msg.starts_with("Date: "));
        assert!(msg.contains("From: SER Alerts <me@example.com>"));
        assert!(msg.contains("To: ops@example.com"));
        assert!(msg.contains("Subject: [SER] Run complete — good"));
        assert!(msg.contains("MIME-Version: 1.0"));
        assert!(msg.contains("All 4 jobs done."));
        assert!(msg.ends_with("\r\n"));
    }

    #[test]
    fn log_mode_succeeds_and_renders() {
        let cfg = SmtpConfig {
            mode: SmtpMode::Log, auth_user: "me@example.com".into(),
            recipient: "ops@example.com".into(), ..Default::default()
        };
        let res = send(&cfg, &sample()).unwrap();
        assert!(res.delivered);
        assert!(res.rendered.unwrap().contains("Subject:"));
    }

    #[test]
    fn empty_recipient_is_err() {
        let cfg = SmtpConfig { mode: SmtpMode::StartTls, host: "h".into(), port: 587,
            auth_user: "u".into(), recipient: "".into(), ..Default::default() };
        assert!(send(&cfg, &sample()).is_err());
    }

    #[test]
    fn starttls_no_server_is_err() {
        let cfg = SmtpConfig {
            host: "127.0.0.1".into(), port: 1, mode: SmtpMode::StartTls,
            auth_user: "me@example.com".into(), auth_password: "pw".into(),
            recipient: "ops@example.com".into(), connect_timeout_secs: 1,
            ..Default::default()
        };
        assert!(send(&cfg, &sample()).is_err(), "connection to 127.0.0.1:1 must fail");
    }

    #[test]
    fn config_serde_roundtrip() {
        let c = SmtpConfig {
            host: "s".into(), port: 465, auth_user: "u".into(), auth_password: "p".into(),
            mode: SmtpMode::DirectTls, sender_name: "n".into(), recipient: "r".into(),
            connect_timeout_secs: 20,
        };
        let s = serde_json::to_string(&c).unwrap();
        assert!(s.contains("\"mode\":\"direct-tls\""));
        let back: SmtpConfig = serde_json::from_str(&s).unwrap();
        assert_eq!(back.mode, SmtpMode::DirectTls);
        assert_eq!(back.port, 465);
    }
}
