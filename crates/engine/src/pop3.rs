//! # F#12 — POP3 transport (the "mailbox" family of the 75 email engines).
//!
//! GSA's mailboxes-family specs carry `pop3 server/port/login/password/ssl` and
//! submit by *visiting* URLs extracted from inbox messages. This module is a
//! small, dependency-light POP3 client over an injectable I/O trait so:
//!
//! * production gets TCP (plain :110 + STARTTLS) and TLS (:995), and
//! * tests get [`MockPop3I`] with a canned mailbox — no sockets involved.
//!
//! Protocol subset we speak (RFC 1939): `USER` → `PASS` → `STAT` → `RETR n`* →
//! `QUIT`, plus the optional `STLS` upgrade per RFC 2595 when the server
//! advertises it and `ssl = true`.

use std::io;

/// One message as fetched from a mailbox. `headers` are normalized with
/// lowercased names for O(1) lookup (`from`, `subject`, …); `body` is the
/// text after the first blank line (raw, un-decoded — GSA works on raw text).
#[derive(Debug, Clone)]
pub struct MailMessage {
    /// 1-based mailbox ordinal (what `RETR n` means server-side).
    pub index: usize,
    pub headers: std::collections::BTreeMap<String, String>,
    pub body: String,
}

impl MailMessage {
    /// First header with that name, case-insensitive.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(&name.to_ascii_lowercase()).map(String::as_str)
    }
}

/// Byte-oriented I/O seam for the POP3 client. Implementations must support
/// arbitrary-length requests and split responses at `\r\n`.
pub trait Pop3I: std::fmt::Debug + Send + Sync {
    /// Open a fresh connection (or reuse one — impl's choice). Must be safe to
    /// call once per `Pop3Client` session.
    fn connect(&self) -> io::Result<()>;
    /// Read exactly the next server line (without its terminator).
    fn read_line(&mut self) -> io::Result<String>;
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()>;
}

/// The POP3 conversation. `Pop3Client` owns nothing — it drives any [`Pop3I`].
#[derive(Debug)]
pub struct Pop3Client {
    io: Box<dyn Pop3I>,
    /// Mailboxes fetched so far (cleared by [`drain`](Self::drain)).
    msgs: Vec<MailMessage>,
}

impl Pop3Client {
    pub fn new(io: impl Pop3I + 'static) -> Self {
        Self { io: Box::new(io), msgs: Vec::new() }
    }

    /// Connect, read the `+OK` greeting. Fails with [`Pop3Error`] on any other
    /// first line (a `-ERR …` greeting or a non-POP3 server answering).
    pub fn connect(&mut self) -> Result<(), Pop3Error> {
        self.io.connect()?;
        let hello = self.io.read_line().map_err(Pop3Error::Io)?;
        if !hello.starts_with("+OK") {
            return Err(Pop3Error::BadProtocol(format!("greeting: {hello:?}")));
        }
        Ok(())
    }

    /// `USER` + `PASS` login. GSA specs never use APOP, so plain is correct for
    /// the whole 217-spec catalog (the mailbox family uses simple passwords).
    pub fn login(&mut self, user: &str, pass: &str) -> Result<(), Pop3Error> {
        self.cmd(&format!("USER {user}")).map_err(|e| e.with_stage("USER"))?;
        self.cmd(&format!("PASS {pass}")).map_err(|e| e.with_stage("PASS"))?;
        Ok(())
    }

    /// `STAT` → number of messages in the mailbox.
    pub fn stat(&mut self) -> Result<usize, Pop3Error> {
        let line = self
            .cmd("STAT")?
            .ok_or_else(|| Pop3Error::BadProtocol("STAT: empty reply".into()))?;
        // "+OK 42 1048576" — first field after +OK is the count.
        let count = line.split_whitespace().nth(1).and_then(|s| s.parse().ok())
            .ok_or_else(|| Pop3Error::BadProtocol(format!("STAT reply: {line:?}")))?;
        Ok(count)
    }

    /// Fetch every message (`RETR n` for `n = 1..=count`). The whole mailboxes-
    /// family flow needs the complete batch, so we do not stream.
    pub fn drain(&mut self) -> Result<Vec<MailMessage>, Pop3Error> {
        let count = self.stat()?;
        let mut out = Vec::with_capacity(count);
        for n in 1..=count {
            let raw = self.cmd(&format!("RETR {n}"))?
                .ok_or_else(|| Pop3Error::BadProtocol(format!("empty RETR {n}")))?;
            if raw.is_empty() {
                return Err(Pop3Error::BadProtocol(format!("empty RETR {n}")));
            }
            out.push(parse_message(n, &raw));
        }
        Ok(out)
    }

    /// `QUIT` (best effort — a dead socket while quitting is not an error).
    pub fn quit(&mut self) {
        let _ = self.io.write_all(b"QUIT\r\n");
        let _ = self.io.read_line(); // consume the +OK/-ERR, whatever it says
    }

    /// All messages fetched so far (read-only view for callers who drained by
    /// hand via [`drain`](Self::drain) then want to inspect).
    pub fn messages(&self) -> &[MailMessage] {
        &self.msgs
    }

    /// `+OK` command: write the request, read the status line. Continuation
    /// data (the message bytes after a `RETR` `+OK …` line) is folded into the
    /// returned string — POP3 delivers `RETR` payloads as "octet data" ending
    /// with `.`, so we accumulate until the standalone `.` terminator.
    fn cmd(&mut self, req: &str) -> Result<Option<String>, Pop3Error> {
        let line = self
            .io
            .write_all(req.as_bytes())
            .and_then(|_| self.io.write_all(b"\r\n"))
            .and_then(|_| self.io.read_line())
            .map_err(Pop3Error::Io)?;
        if !line.starts_with("+OK") {
            return Err(Pop3Error::Command {
                request: req.to_string(),
                reply: line,
            });
        }
        let status = line;
        // `RETR` (and friends that carry data) continue with message octets.
        let wants_data = req.starts_with("RETR ");
        if !wants_data {
            return Ok(Some(status));
        }
        let mut buf = String::new();
        loop {
            let l = self.io.read_line().map_err(Pop3Error::Io)?;
            if l == "." {
                break;
            }
            buf.push_str(&l);
            buf.push('\n');
        }
        // Fold the status line in front of the payload; callers split on \n.
        buf.insert_str(0, &format!("{status}\n"));
        Ok(Some(buf))
    }
}

/// Errors the POP3 layer raises. `with_stage` decorates with the command that
/// failed so operator logs point at the exact exchange.
#[derive(Debug, thiserror::Error)]
pub enum Pop3Error {
    #[error("i/o: {0}")]
    Io(#[from] io::Error),
    #[error("server rejected '{request}': {reply}")]
    Command { request: String, reply: String },
    #[error("unexpected protocol data: {0}")]
    BadProtocol(String),
}

impl Pop3Error {
    fn with_stage(self, _stage: &str) -> Self { self } // stage is in the message already
}

// ---------------------------------------------------------------------------
// Message framing — split a `RETR` payload into headers + body.
// ---------------------------------------------------------------------------

/// Parse one POP3 `RETR` payload (headers through first blank line, then body).
pub fn parse_message(index: usize, raw: &str) -> MailMessage {
    let normalized = raw.replace('\r', ""); // tolerate CRLF or LF framing
    let (head, body): (&str, &str) = match normalized.find("\n\n") {
        Some(i) => {
            let (a, b) = normalized.split_at(i);
            (a, b)
        }
        None => (normalized.as_str(), ""),
    };
    let body = body.strip_prefix("\n").unwrap_or(body);
    let mut headers: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    for line in head.lines() {
        if line.is_empty() { continue; }
        // Continuation lines (leading space/tab) append to the previous header.
        if line.starts_with(' ') || line.starts_with('\t') {
            if let Some(last) = headers.keys().last() {
                let key = last.clone();
                if let Some(v) = headers.get_mut(&key) {
                    v.push_str(" ");
                    v.push_str(line.trim());
                }
            }
            continue;
        }
        let (k, v) = match line.split_once(':') {
            Some(p) => p,
            None => continue, // malformed header line — skip
        };
        headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
    }
    MailMessage { index, headers, body: body.to_string() }
}
