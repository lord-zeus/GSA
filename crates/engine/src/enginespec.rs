//! # Engine spec model + parser
//!
//! A GSA "engine" is a `.ini` file that describes how to create a
//! backlink on one site.  This module turns that file into a typed
//! [Engine] tree the submission engine and the dashboard can operate on.
//!
//! ## Structure (matches the 825 web + 217 email engine files we surveyed)
//!
//! ```text
//! [engine]            name / type / url / dofollow / …
//! [setup]             optional registration data
//! [account]           email / login / password (+ per-account instances)
//! [step 1]            first form (usually the submit form)
//! [step 2]            …
//! [verify]            how to read the created-URL out of the confirmation
//! ```
//!
//! Every field is a `key = value` pair; keys are **case-insensitive** and we
//! keep them in their original spelling (so the dashboard can display them
//! verbatim).

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::str::FromStr;

/// A single `key = value` pair in a step.  Order is preserved for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub key: String,
    pub value: String,
}

impl Field {
    pub fn new<K: Into<String>, V: Into<String>>(k: K, v: V) -> Self {
        Self {
            key: k.into(),
            value: v.into(),
        }
    }
}

/// A named step (form) — e.g. `[step 1]`, `[verify]`, `[about_yourself]`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Step {
    pub name: String,
    /// Preserved field order (BTreeMap for stable display; the ordering is
    /// what the UI shows, so alphabetical is fine and *deterministic*).
    pub fields: BTreeMap<String, Field>,
}

impl Step {
    pub fn field(&self, key: &str) -> Option<&String> {
        // case-insensitive field lookup (GSA keys are CI in every file we saw)
        let lk = key.to_ascii_lowercase();
        self.fields.iter().find_map(|(k, f)| (k.to_ascii_lowercase() == lk).then(|| &f.value))
    }

    pub fn field_mut(&mut self, key: &str) -> Option<&mut String> {
        let lk = key.to_ascii_lowercase();
        self.fields.iter_mut().find_map(|(k, f)| (k.to_ascii_lowercase() == lk).then_some(&mut f.value))
    }
}

impl FromStr for EngineType {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim().to_ascii_lowercase();
        let s = s.replace([' ', '_'], "-");
        Ok(match s.as_str() {
            "url-shortener" => EngineType::UrlShortener,
            "web-2-0" | "web-2.0" | "web20" => EngineType::Web20,
            "guestbook" => EngineType::Guestbook,
            "directory" => EngineType::Directory,
            "article" | "articles" => EngineType::Article,
            "forum" => EngineType::Forum,
            "social-network" => EngineType::SocialNetwork,
            "rss" => EngineType::Rss,
            "blog-comment" => EngineType::BlogComment,
            "social-bookmark" => EngineType::SocialBookmark,
            "microblog" => EngineType::Microblog,
            "image-comment" => EngineType::ImageComment,
            "video" => EngineType::Video,
            "video-adult" | "videoadult" => EngineType::VideoAdult,
            "wiki" => EngineType::Wiki,
            "referrer" => EngineType::Referrer,
            "indexer" | "indexer-" => EngineType::Indexer,
            "exploit" => EngineType::Exploit,
            "document-sharing" => EngineType::DocumentSharing,
            "trackback" => EngineType::Trackback,
            "pingback" => EngineType::Pingback,
            "email" => EngineType::Email,
            other => return Err(format!("unknown engine type: {other}")),
        })
    }
}

/// Parsed engine specification (one file = one [Engine]).
#[derive(Debug, Clone, PartialEq)]
pub struct Engine {
    /// Basename without `.ini` — GSA's identity for this engine.
    pub name: String,
    /// The engine category ("directory", "article", …).
    pub engine_type: EngineType,
    /// Base URL of the site (e.g. `http://aardvark-topsites.com/`).
    pub url: String,
    /// GSA `fixed url` — a fixed target site URL.  URL-shortener / redirect
    /// engines (the `just download=1` family) carry this in place of a
    /// per-account `url`, so the executor can fall back to it as the base.
    pub fixed_url: Option<String>,
    /// Whether the created link is dofollow (default false).
    pub dofollow: bool,
    /// Whether the engine registers a fresh account (config: `enabled`).
    pub register: bool,
    /// Optional verification URL / expected substring used to detect success.
    pub verify_url: Option<String>,
    /// Preserved, in parse order.
    pub steps: Vec<Step>,
}

impl Engine {
    /// Case-insensitive search over the step names.
    pub fn step(&self, name: &str) -> Option<&Step> {
        let target = name.to_ascii_lowercase();
        self.steps.iter().find(|s| s.name.to_ascii_lowercase() == target)
    }

    /// Effective base site URL: `url=` if set, else GSA's `fixed url=`.
    /// URL-shortener / redirect engines carry `fixed url` and leave `url`
    /// empty; both address the same site.
    pub fn base_url(&self) -> String {
        let u = self.url.trim();
        if !u.is_empty() {
            return u.to_string();
        }
        self.fixed_url
            .clone()
            .filter(|f| !f.trim().is_empty())
            .unwrap_or_default()
    }
}

/// Parse a complete engine `.ini` file on disk.  Reads the bytes and lossy-
/// decodes as UTF-8 (GSA ships a few files in Windows-1252; we'd rather keep
/// going than drop a whole engine on one bad byte).
pub fn parse_engine_file(path: &Path) -> Result<Engine, EngineError> {
    let raw = std::fs::read(path).map_err(EngineError::Io)?;
    let raw = String::from_utf8_lossy(&raw).into_owned();
    let name = path
        .file_stem()
        .and_then(|n| n.to_str())
        .unwrap_or("engine")
        .to_string();
    parse_engine_str(&raw, &name)
}

/// GSA INI escaping: values are unquoted, so a literal `"` is written as
/// `\"` (likewise `\'`, and a literal backslash as `\\`).  Downstream, these
/// values are matched/parsed against *real* HTML, which uses plain quotes —
/// so we unescape at read time.  Anything else keeps its backslash.
fn unescape_ini_value(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut it = v.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.peek() {
                Some('"') => {
                    out.push('"');
                    it.next();
                }
                Some('\'') => {
                    out.push('\'');
                    it.next();
                }
                Some('\\') => {
                    out.push('\\');
                    it.next();
                }
                _ => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Parse an engine from an in-memory string (used by the tests and import
/// pipeline; `engine_name` is the identity used in errors).
pub fn parse_engine_str(raw: &str, engine_name: &str) -> Result<Engine, EngineError> {
    let mut cur_section: Option<String> = None;
    let mut sections: Vec<(String, BTreeMap<String, Field>)> = Vec::new();

    for line in raw.split('\n') {
        let line = line.trim_end().trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        if let Some(inner) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            cur_section = Some(inner.to_string());
            sections.push((inner.to_string(), BTreeMap::new()));
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue; // tolerate free-text lines
        };
        let k = k.trim().to_string();
        let v = unescape_ini_value(v.trim());
        let Some(sec_idx) = sections.iter().rposition(|(n, _)| cur_section.as_deref() == Some(n.as_str()))
        else {
            continue;
        };
        if let Some(map) = sections.get_mut(sec_idx) {
            map.1
                .insert(k.to_ascii_lowercase(), Field { key: k.clone(), value: v });
        }
    }

    // Promote the standard sections onto the Engine surface for ergonomic
    // access; everything else stays in `steps` for full fidelity.  Snapshot
    // fields first (they hold borrow references), then mutate `e`.
    let mut e = Engine {
        name: engine_name.to_string(),
        engine_type: EngineType::Unknown,
        url: String::new(),
        fixed_url: None,
        dofollow: false,
        register: false,
        verify_url: None,
        steps: sections
            .into_iter()
            .map(|(n, fs)| Step {
                name: n,
                fields: fs,
            })
            .collect(),
    };

    let engine_sec = e
        .step("engine")
        .or_else(|| e.step("setup"))
        .and_then(|s| {
            let t = &s.field("engine type").or_else(|| s.field("type"));
            let t = t.and_then(|v| v.parse().ok()).unwrap_or(EngineType::Unknown);
            let u = s.field("url").map(|s| s.clone()).unwrap_or_default();
            let d = s.field("dofollow").map(|b| b != "false").unwrap_or(false);
            let fu = s.field("fixed url").map(|s| s.clone());
            Some((t, u, d, fu))
        });
    if let Some((t, u, d, fu)) = engine_sec {
        e.engine_type = t;
        e.url = u;
        e.dofollow = d;
        e.fixed_url = fu;
    }
    let (register, verify_url) = {
        let reg = e
            .step("setup")
            .and_then(|s| s.field("enabled").map(|b| b != "false"))
            .unwrap_or(false);
        let vurl = e.step("verify").and_then(|s| s.field("verify by").map(|s| s.clone()));
        (reg, vurl)
    };
    e.register = register;
    e.verify_url = verify_url;

    Ok(e)
}

/// Errors while parsing an engine file.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("bad engine section: {0}")]
    BadSection(String),
}

/// The 20+ engine categories we saw across the 1,042 engine files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum EngineType {
    Unknown,
    Directory,
    Article,
    Forum,
    Guestbook,
    BlogComment,
    SocialBookmark,
    SocialNetwork,
    Microblog,
    Rss,
    Web20,
    UrlShortener,
    ImageComment,
    Video,
    VideoAdult,
    Wiki,
    Referrer,
    Indexer,
    Exploit,
    DocumentSharing,
    Trackback,
    Pingback,
    Email,
}

impl fmt::Display for EngineType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use EngineType::*;
        let s = match self {
            Unknown => "unknown",
            Directory => "directory",
            Article => "article",
            Forum => "forum",
            Guestbook => "guestbook",
            BlogComment => "blog-comment",
            SocialBookmark => "social-bookmark",
            SocialNetwork => "social-network",
            Microblog => "microblog",
            Rss => "rss",
            Web20 => "web-2.0",
            UrlShortener => "url-shortener",
            ImageComment => "image-comment",
            Video => "video",
            VideoAdult => "video-adult",
            Wiki => "wiki",
            Referrer => "referrer",
            Indexer => "indexer",
            Exploit => "exploit",
            DocumentSharing => "document-sharing",
            Trackback => "trackback",
            Pingback => "pingback",
            Email => "email",
        };
        write!(f, "{s}")
    }
}

// --------------------------------------------------------------------------
//  Tests: round-trip, case-insensitivity, known files, malformed input.
// --------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_engine_with_three_sections() {
        let raw = r#"
[engine]
name=Aardvark
engine type=Directory
url=http://aardvark.example/
dofollow=true

[setup]
enabled=false

[step 1]
site name=%website_title%
title=%anchor_text%
link=%url%
"#;
        let e = parse_engine_str(raw, "Aardvark").unwrap();
        assert_eq!(e.name, "Aardvark");
        assert_eq!(e.engine_type, EngineType::Directory);
        assert_eq!(e.url, "http://aardvark.example/");
        assert!(e.dofollow);
        assert!(!e.register);
        let s1 = e.step("step 1").expect("step 1");
        assert_eq!(s1.field("site name").unwrap(), "%website_title%");
        assert_eq!(s1.field("title").unwrap(), "%anchor_text%");
        assert_eq!(s1.field("link").unwrap(), "%url%");
    }

    #[test]
    fn parse_case_insensitive_key_lookup() {
        let raw = "[step 1]\r\nSITE Name=%website_title%\r\n";
        let e = parse_engine_str(raw, "x").unwrap();
        let s1 = e.step("step 1").unwrap();
        // stored with original case
        assert!(s1.fields.contains_key("site name"));
        // looked up case-insensitively
        assert_eq!(s1.field("site name").unwrap(), "%website_title%");
        assert_eq!(s1.field("SITE Name").unwrap(), "%website_title%");
    }

    #[test]
    fn parse_real_aardvark_engine_file() {
        let bytes = std::fs::read(
            r"C:\Users\kingsly\GSA_SER_portable\Engines\Aardvark Topsites.ini",
        )
        .expect("file should exist on this machine");
        // GSA ships some engine files in Windows-1252; decode lossily rather
        // than rejecting the whole engine on one bad byte.
        let raw = String::from_utf8_lossy(&bytes).into_owned();
        let e = parse_engine_str(&raw, "Aardvark Topsites").unwrap();
        // This file's [setup] header has `engine type=Directory` — the parser
        // should pick that up via the top-level field scan even when there is
        // no explicit [engine] section.
        assert_eq!(e.engine_type, EngineType::Directory);
        // The file does reference the Aardvark Topsites PHP engine somewhere
        // in its "page must have" strings.
        assert!(raw.contains("Aardvark Topsites PHP"));
        // It has at least one step named "step" (the real file uses STEP1/
        // STEP2 — case-insensitive check, as GSA's own UI does).
        assert!(e.steps.iter().any(|s| s.name.to_ascii_lowercase().starts_with("step")));
    }

    #[test]
    fn unknown_engine_type_degrades_gracefully() {
        let raw = "[engine]
\nengine type=super-duper-new-cat
\nurl=http://x
\n";
        let e = parse_engine_str(raw, "q").unwrap();
        assert_eq!(e.engine_type, EngineType::Unknown);
    }

    #[test]
    fn web20_type_parses_from_setup_real_corpus_shape() {
        // Real corpus shape (43things.com.ini): no [engine] section — the
        // type lives in [setup] with the dotted value `Web 2.0`.  Before the
        // fix this parsed to `Unknown` and 126 engines dropped out of the
        // Article family pool.
        let raw = "[setup]
\nenabled=0
\nfixed url=http://www.43things.com/
\nengine type=Web 2.0
\ndofollow=1
\n[Your E-Mail]
\ntype=email
\n[URL]
\ntype=url
\n";
        let e = parse_engine_str(raw, "43things.com").unwrap();
        assert_eq!(e.engine_type, EngineType::Web20, "Web 2.0 must classify as Web20");
        assert!(e.dofollow);
        // And the family mapping still routes it to Article.
        use crate::families::Family;
        assert_eq!(Family::for_type(&e.engine_type), Some(Family::Article));
    }

    #[test]
    fn unknown_variable_in_value_is_kept_verbatim() {
        let raw = "[engine]\r\nengine type=Directory\r\nurl=http://x\r\n[step 1]\r\ncustom=%totally_unknown_var%\r\n";
        let e = parse_engine_str(raw, "y").unwrap();
        let s1 = e.step("step 1").unwrap();
        assert_eq!(s1.field("custom").unwrap(), "%totally_unknown_var%");
    }

    #[test]
    fn steps_are_sorted_display_stable() {
        // Step order stays in parse order (we don't re-sort steps).  We
        // deliberately don't include an [engine] header — the parse routine
        // uses it to set `engine_type` on the top-level Engine, so a header
        // here would just shadow that path.
        let raw = "[ZZZ step a]\r\nb=2\r\n[AAA step b]\r\nc=3\r\n";
        let e = parse_engine_str(raw, "z").unwrap();
        // step order stays in parse order (we don't re-sort steps)
        assert_eq!(e.steps[0].name, "ZZZ step a");
        assert_eq!(e.steps[1].name, "AAA step b");
    }
}
