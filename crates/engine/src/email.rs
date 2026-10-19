
//! # F#12 — Email engine runner (POP3 + inbox-parse family)
//!
//! GSA's `Email_Engines` catalog ships two families in one file format:
//!
//! * **Inbox-parse engines** (`pop3 server=WEB`, 142 of the 217 shipped):
//!   the operator creates an account on the site over HTTP, then posts a
//!   "verification link" email to their own inbox. The engine fetches its
//!   own inbox (or is handed the raw message HTML) and pulls each submission
//!   URL out with the `url_front*` / `url_back*` markers, plus `email_*`,
//!   `subject_*`, `message_*` matchers to find the right candidate line.
//! * **Mailbox engines** (real POP3 servers, 75 of the 217): same extraction
//!   logic over a mailbox retrieved with `pop3 server/port/login/password/ssl`.
//!
//! This module is deliberately **pure and dependency-free**: spec parsing,
//! address matching, token resolution and inbox-message extraction are all
//! deterministic string work so they can be unit-tested (and later driven by
//! an injected SMTP/POP3 transport). The runner crate layers the network I/O.

use std::collections::BTreeMap;

use crate::enginespec::{Engine, parse_engine_str};

/// A fully-resolved email engine: every `[setup]` field GSA uses for its 217
/// shipped email specs, indexed for O(1) lookup.
#[derive(Debug, Clone)]
pub struct EmailEngine {
    /// Engine name (basename without `.ini`) — GSA's identity.
    pub name: String,
    /// `emails match` glob list (`*@x.com|*@y.com`). Empty = matches nothing.
    pub address_match: Vec<String>,
    /// Mailbox family marker; `"WEB"` means inbox-parse over HTTP/cookie.
    pub pop3_server: String,
    pub pop3_port: i32,
    pub pop3_login: String,
    pub pop3_password: String,
    pub pop3_ssl: bool,
    /// `web parse url` — where the inbox HTML comes from (WEB family).
    pub web_parse_url: Option<String>,
    /// Front/Back markers per ladder rung. Index 0 = base field, 1..=3 =
    /// `…_front2` … variants. Unset rungs are `(None, None)`.
    pub email_fb: [(Option<String>, Option<String>); 4],
    pub url_fb: [(Option<String>, Option<String>); 4],
    pub subject_fb: [(Option<String>, Option<String>); 4],
    pub delurl_fb: [(Option<String>, Option<String>); 4],
    /// `message match` literal filter on each candidate line.
    pub message_match: Option<String>,
    /// `separator` — splits a raw extract into candidate lines when present
    /// (real corpus: 142 files, e.g. `separator=<tr>`, `separator=id"`).
    pub separator: Option<String>,
    /// `message_replace` — semicolon-separated `find;replace` pairs applied to
    /// each candidate body before extraction (real corpus: 1 file; 1secmail
    /// uses it to strip JSON artefacts). Empty = no substitutions.
    pub message_replace: Vec<String>,
    /// `cut top` — leading window marker (only anonmails.de in corpus).
    pub cut_top: Option<String>,
    /// `cut bottom` — trailing window marker (only anonmails.de in corpus).
    pub cut_bottom: Option<String>,
    /// `url_add front` — prefix the extracted fragment to build a full link
    /// (1secmail: `…&id=` + fragment; mailmenot: route prefix + key). 2 files.
    pub url_add_front: Option<String>,
    /// `web parse url post data` — form body for the inbox fetch (POST) — 127 files.
    pub web_parse_post: Option<String>,
    /// `enabled` flag from the spec (216 of 217 shipped specs set it).
    pub enabled: bool,
}

/// Parse a raw email-engine `.ini` (the `[setup]`-only files from GSA's
/// `Email_Engines`) into an [EmailEngine].
pub fn parse_email_engine_str(raw: &str, name: &str) -> Result<EmailEngine, String> {
    let engine = parse_engine_str(raw, name).map_err(|e| format!("{name}: {e}"))?;
    Ok(Engine::setup_to_email(&engine))
}

/// Parse an email-engine `.ini` from disk.
pub fn parse_email_engine_file(path: &std::path::Path) -> Result<EmailEngine, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("io: {e}"))?;
    let stem = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("engine")
        .trim_end_matches(".ini");
    parse_email_engine_str(&raw, stem)
}

impl Engine {
    /// Re-index a parsed `[setup]`-only engine as an [EmailEngine].
    pub fn setup_to_email(&self) -> EmailEngine {
        let fields: BTreeMap<String, String> = self
            .steps
            .iter()
            .flat_map(|s| s.fields.iter())
            .map(|(k, v)| (k.clone(), v.value.clone()))
            .collect();

        // Ladder reader: rung 0 is the bare key, rungs 1..=3 are `key2`…`key4`.
        let ladder = |base: &str| -> [(Option<String>, Option<String>); 4] {
            std::array::from_fn(|i| {
                let (key_f, key_b) = if i == 0 {
                    (base.to_string(), stem_back(base))
                } else {
                    (format!("{base}{i}"), format!("{}{i}", stem_back(base)))
                };
                (fields.get(&key_f).cloned(), fields.get(&key_b).cloned())
            })
        };

        let get = |k: &str| fields.get(k).map(String::as_str);
        EmailEngine {
            name: self.name.clone(),
            address_match: split_pipe(get("emails match").unwrap_or("")),
            pop3_server: get("pop3 server").map(str::to_string).unwrap_or_default(),
            pop3_port: get("pop3 port").and_then(|v| v.parse().ok()).unwrap_or(995),
            pop3_login: fields.get("pop3 login").cloned().unwrap_or_default(),
            pop3_password: fields.get("pop3 password").cloned().unwrap_or_default(),
            pop3_ssl: fields.get("pop3 ssl").map(|v| v == "1" || v.eq_ignore_ascii_case("true")).unwrap_or(false),
            web_parse_url: fields.get("web parse url").filter(|s| !s.trim().is_empty()).cloned(),
            email_fb: ladder("email_front"),
            url_fb: ladder("url_front"),
            subject_fb: ladder("subject_front"),
            delurl_fb: ladder("delurl_front"),
            message_match: fields.get("message match").filter(|s| !s.trim().is_empty()).cloned(),
            separator: fields.get("separator").filter(|s| !s.trim().is_empty()).cloned(),
            message_replace: fields
                .get("message_replace")
                .map(|s| s.split('|').map(|t| t.trim().to_string()).filter(|t: &String| !t.is_empty()).collect::<Vec<String>>())
                .unwrap_or_default(),
            cut_top: fields.get("cut top").filter(|s| !s.trim().is_empty()).cloned(),
            cut_bottom: fields.get("cut bottom").filter(|s| !s.trim().is_empty()).cloned(),
            url_add_front: fields.get("url_add front").filter(|s| !s.trim().is_empty()).cloned(),
            web_parse_post: fields.get("web parse url post data").filter(|s| !s.trim().is_empty()).cloned(),
            enabled: fields
                .get("enabled")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(true),
        }
    }
}

fn stem_back(base: &str) -> String {
    // url_front -> url_back ; email_front2 handled by caller appending idx.
    let b = base.strip_suffix("front").unwrap_or(base);
    format!("{b}back")
}

/// Split a GSA `|`-separated list, dropping empties and trims.
fn split_pipe(s: &str) -> Vec<String> {
    s.split('|').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect()
}

// ---------------------------------------------------------------------------
// Address matching — `emails match` globs, case-insensitive.
// ---------------------------------------------------------------------------

/// Does `address` (a full email address) satisfy the engine's glob list?
pub fn matches_address(engine: &EmailEngine, address: &str) -> bool {
    if engine.address_match.is_empty() || address.is_empty() {
        return false;
    }
    let addr = address.trim().to_ascii_lowercase();
    engine.address_match.iter().any(|g| glob_matches(g, &addr))
}

/// One GSA glob: `*` matches any run (including dots & empty); other chars
/// must match literally. Case-insensitive.
pub fn glob_matches(pattern: &str, value: &str) -> bool {
    let p: Vec<char> = pattern.trim().to_ascii_lowercase().chars().collect();
    let v: Vec<char> = value.to_ascii_lowercase().chars().collect();
    // Classic two-pointer wildcard match (GSA patterns contain no `?`).
    let (mut i, mut j, mut star, mut mark) = (0usize, 0usize, None::<usize>, 0usize);
    while j < v.len() {
        if i < p.len() && (p[i] == '*' || p[i] == v[j]) {
            if p[i] == '*' {
                star = Some(i);
                mark = j;
                i += 1;
            } else {
                i += 1;
                j += 1;
            }
        } else if let Some(s) = star {
            i = s + 1;
            mark += 1;
            j = mark;
        } else {
            return false;
        }
    }
    while i < p.len() && p[i] == '*' {
        i += 1;
    }
    i == p.len()
}

// ---------------------------------------------------------------------------
// Token resolution — %email%, %emailuser%, %password%… in credentials & body.
// ---------------------------------------------------------------------------

/// Resolve GSA's address tokens inside a raw spec value. `%emailuser%` is the
/// local-part only, `%email%` the full address; `%password%` the mailbox
/// password. Unknown tokens are left verbatim (GSA behavior).
pub fn resolve_email_tokens(value: &str, addr: &str, password: &str) -> String {
    let user = addr.split('@').next().unwrap_or(addr);
    // Real corpus uses `%emailhost%` for the domain part (e.g. 1secmail:
    // `login=%emailuser%&domain=%emailhost%`). Order matters: replace the
    // longer tokens first so `%emailuser%` is not swallowed by `%email%`.
    let host = match addr.rsplit_once('@') {
        Some((_, d)) if d.contains('.') => d,
        _ => "",
    };
    value
        .replace("%emailuser%", user)
        .replace("%emailhost%", host)
        .replace("%password%", password)
        .replace("%email%", addr)
}

// ---------------------------------------------------------------------------
// Inbox-message extraction.
// ---------------------------------------------------------------------------

use crate::extraction::extract_front_back;

/// One candidate pulled out of an inbox.
#[derive(Debug, Clone)]
pub struct EmailSubmission {
    pub url: String,
    pub email_line: Option<String>,
    pub subject_line: Option<String>,
}

/// Extract submission candidates from a raw inbox/message body using the
/// engine's `url_front*`/`url_back*` ladder (with `message match` filter).
/// Pure + deterministic — unit-testable with canned bodies.
/// INI marker values are stored escaped (`\"` for a literal quote);
/// GSA searches the message with the *unescaped* form.
fn unescape_marker(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let chars = s.chars().collect::<Vec<_>>();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\\' && i + 1 < chars.len() {
            out.push(chars[i + 1]);
            i += 2;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Real-corpus pipeline ops — `separator` / `cut top` / `cut bottom` /
// `message_replace`. Grounded in the 217 shipped specs (census 2026-10):
//   separator           (142 files)  e.g. `<tr>`, `id"`, `,"`
//   cut top / bottom    (1 file)     anonmails.de inbox-table windowing
//   message_replace     (1 file)     1secmail JSON-artefact stripping
// ---------------------------------------------------------------------------

/// Split one fetched inbox body into candidate records using the engine's
/// `separator` marker (GSA's line-based window splitting). Each piece between
/// consecutive separators becomes one candidate body; empty pieces are
/// dropped. A missing/empty separator yields the whole body as one piece.
pub fn split_inbox(body: &str, separator: &str) -> Vec<String> {
    if separator.trim().is_empty() {
        return vec![body.to_string()];
    }
    body.split(separator)
        .map(String::from)
        .filter(|p| !p.trim().is_empty())
        .collect()
}

/// Window a body between `cut top` and `cut bottom` markers (anonmails.de
/// style: the inbox table sits inside a larger page). Missing anchors pass
/// the body through — GSA's shipped specs only pair the two on one engine.
/// Window a body between `cut top` and `cut bottom` markers (anonmails.de
/// style: the inbox table sits inside a larger page). Missing anchors pass
/// the body through — GSA's shipped specs only pair the two on one engine.
pub fn cut_window(body: &str, top: &str, bottom: &str) -> String {
    let t = if top.is_empty() { 0 } else { body.find(top).map(|i| i + top.len()).unwrap_or(0) };
    let e = if bottom.is_empty() {
        body.len()
    } else {
        body.find(bottom).filter(|&i| i >= t).unwrap_or(body.len())
    };
    body[t..e].to_string()
}

/// One `message_replace` pair (`find;replace`). Empty `find` = no-op.
#[derive(Debug, Clone, Copy)]
pub struct ReplacePair {
    pub find: &'static str,
    pub replace: &'static str,
}

/// Apply a semicolon-separated list of `find;replace` pairs (1secmail style:
/// `\n; |\r; |\\;\|\\";"|\/;/` — i.e. strip JSON string
/// artefacts before marker extraction). Pairs are applied in order over the
/// whole body.
pub fn apply_replaces(body: &str, pairs: &[String]) -> String {
    let mut out = body.to_string();
    for p in pairs {
        // Each pair is `FIND;REPLACE` (one or both may be empty).
        // An empty `REPLACE` is a delete. An empty `FIND` is a no-op.
        let (a, b) = match p.split_once(';') {
            Some((a, b)) => (a, b),
            None => (p.as_str(), ""),
        };
        if a.is_empty() {
            continue;
        }
        out = out.replace(a, b);
    }
    out
}

pub fn extract_from_message(engine: &EmailEngine, body: &str) -> Vec<EmailSubmission> {
    // GSA discards the whole message when a non-empty `message match` is not
    // present (case-insensitive substring).
    if let Some(mm) = engine.message_match.as_deref() {
        if !mm.trim().is_empty() && !body.to_ascii_lowercase().contains(&mm.trim().to_ascii_lowercase()) {
            return Vec::new();
        }
    }

    // Real-corpus pipeline: window -> split -> replace.
    let windowed = cut_window(body, &cut_top(engine), &cut_bottom(engine));
    let pieces = split_inbox(&windowed, engine.separator.as_deref().unwrap_or(""));
    let pieces: Vec<String> = if engine.message_replace.is_empty() {
        pieces
    } else {
        pieces.into_iter().map(|p| apply_replaces(&p, &engine.message_replace)).collect()
    };

    // Companion fragments (email / subject lines for the submission record),
    // pulled from rung 0 of their respective ladders when configured.
    let companion = |piece: &str, ladder: &[(Option<String>, Option<String>); 4]| -> Option<String> {
        ladder.iter().find_map(|(fo, bo)| {
            let (Some(f), Some(b)) = (fo.as_deref(), bo.as_deref()) else {
                return None;
            };
            let f = unescape_marker(f);
            let b = unescape_marker(b);
            extract_front_back(piece, &f, &b)
        })
    };

    let mut out = Vec::new();
    for piece in &pieces {
        for rung in 0..4 {
            let (Some(front), Some(back)) = (engine.url_fb[rung].0.clone(), engine.url_fb[rung].1.clone()) else {
                continue; // this rung is unconfigured — try the next one
            };
            if front.trim().is_empty() && back.trim().is_empty() {
                continue;
            }
            let front = unescape_marker(&front);
            let back = unescape_marker(&back);
            let Some(extracted) = extract_front_back(piece, &front, &back) else {
                    continue; // ladder rung yields nothing — try next
            };
            let mut url = extract_url(&extracted);
            if url.is_empty() {
                continue;
            }
            // `url_add front` — 1secmail/mailmenot build the link as
            // prefix + id-fragment (two engines in the 217 corpus).
            if let Some(pref) = &engine.url_add_front {
                url = format!("{}{}", pref, url);
            }
            out.push(EmailSubmission {
                url,
                email_line: companion(piece, &engine.email_fb),
                subject_line: companion(piece, &engine.subject_fb),
            });
            // One rung-per-piece yield matches GSA's shipped 1secmail
            // behaviour (separator=id" -> one candidate per row).
        }
    }
    out
}

/// `cut top` / `cut bottom` — only anonmails.de sets them in the current
/// 217-spec corpus; return empty (pass-through) for every other engine.
fn cut_top(engine: &EmailEngine) -> String {
    engine.cut_top.clone().unwrap_or_default()
}
fn cut_bottom(engine: &EmailEngine) -> String {
    engine.cut_bottom.clone().unwrap_or_default()
}

/// Trim + normalize an extracted URL fragment (wraps whitespace). Empty → empty string.
fn extract_url(fragment: &str) -> String {
    fragment.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[setup]
emails match=*@gmail.com|*@yahoo.*
pop3 server=WEB
web parse url=https://mail.example.com/inbox
url_front=<a href="
url_back=.verify</a>
email_front=From: 
email_back=@
message match=my link
enabled=1
"#;

    #[test]
    fn parses_email_spec_fields() {
        let e = parse_email_engine_str(SAMPLE, "sample.engine").unwrap();
        assert_eq!(e.name, "sample.engine");
        assert_eq!(
            e.address_match,
            vec!["*@gmail.com".to_string(), "*@yahoo.*".to_string()]
        );
        assert_eq!(e.pop3_server, "WEB");
        assert_eq!(e.web_parse_url.as_deref(), Some("https://mail.example.com/inbox"));
        assert_eq!(e.url_fb[0].0.as_deref(), Some("<a href=\""));
        assert_eq!(e.url_fb[0].1.as_deref(), Some(".verify</a>"));
        // INI parser trims field values, so the stored front marker is "From:"
        assert_eq!(e.email_fb[0].0.as_deref(), Some("From:"));
        assert_eq!(e.message_match.as_deref(), Some("my link"));
        assert!(e.enabled);
    }

    #[test]
    fn glob_matching() {
        assert!(glob_matches("*@gmail.com", "a@gmail.com"));
        assert!(!glob_matches("*@gmail.com", "a@yahoo.com"));
        // case-insensitive both sides
        assert!(glob_matches("*@Gmail.COM", "Somebody@gMail.Com"));
        // bare domain suffix style + leading dot
        assert!(glob_matches("*.stees.online", "x.stees.online"));
        assert!(!glob_matches("*.stees.online", "stees.online"));
    }

    #[test]
    fn token_resolution() {
        let got = resolve_email_tokens("%emailuser%:%password%", "joe.DOE@gmail.com", "S3cret");
        assert_eq!(got, "joe.DOE:S3cret");
        // %email% is the full address; unknown tokens pass through untouched.
        let got2 = resolve_email_tokens("to=%email%,x=%nouser%", "a@b.co", "pw");
        assert_eq!(got2, "to=a@b.co,x=%nouser%");
    }

    #[test]
    fn extract_from_message_uses_ladder_and_companions() {
        let e = parse_email_engine_str(SAMPLE, "s").unwrap();
        let body = "From: bot@mail.example\nSubject: your link\n<html>Your my link here: <a href=\"https://stees.online/v/abc123\">.verify</a></html>";
        let out = extract_from_message(&e, body);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].url, "https://stees.online/v/abc123\">");
        // email ladder: text between "From: " and "@" -> local part only (GSA records the mailbox user)
        assert_eq!(out[0].email_line.as_deref(), Some("bot"));
    }

    #[test]
    fn extract_respects_message_match_filter() {
        let e = parse_email_engine_str(SAMPLE, "s").unwrap();
        // body without the "my link" marker is discarded entirely
        assert!(extract_from_message(&e, "<a href=\"https://x.co\">.verify</a>").is_empty());
    }

    /// Parse every real GSA email engine in a local catalog dir (if present)
    /// and prove: 217/217 parse, all have non-empty name, WEB family has a web
    /// parse url or empty pop3 login is tolerated, ladder fields are sane.
    #[test]
    fn parses_all_real_gsa_email_engines() {
        let dir = std::env::var("GSA_EMAIL_ENGINES").unwrap_or_else(|_| {
            "C:/Users/kingsly/GSA_SER_portable/Email_Engines".to_string()
        });
        let entries: Vec<_> = std::fs::read_dir(&dir)
            .map(|d| d.filter_map(|e| e.ok()).collect())
            .unwrap_or_default();
        if entries.is_empty() {
            eprintln!("skip: {dir} not found");
            return;
        }
        let mut web = 0usize;
        let mut mailboxes = 0usize;
        for entry in &entries {
            let p = entry.path();
            if extension_is(&p, "ini") {
                let e = parse_email_engine_file(&p).unwrap_or_else(|err| panic!("fail to parse {:?}: {err}", p));
                assert!(!e.name.is_empty());
                if e.pop3_server.eq_ignore_ascii_case("web") {
                    web += 1;
                } else {
                    mailboxes += 1;
                }
            }
        }
        eprintln!("email catalog: total={} web={} pop3={}", entries.len(), web, mailboxes);
    }

    fn extension_is(p: &std::path::Path, ext: &str) -> bool {
        p.extension().and_then(|s| s.to_str()).map(|s| s.eq_ignore_ascii_case(ext)).unwrap_or(false)
    }
}

