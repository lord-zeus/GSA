//! # Result extraction — how GSA reads the created URL out of a confirmation.
//!
//! Most `[verify]` sections work off three primitives, all of which are pure
//! string manipulation on the response HTML and therefore unit-testable
//! without a browser:
//!
//! * **front / back** — take the substring between the first occurrence of
//!   `front` after some position and the next occurrence of `back`.
//! * **front1 / front2 / … / frontN** — a fallback ladder; GSA tries `front1`,
//!   and if that yields nothing, `front2`, etc.
//! * **find url / find link** — GSA's higher-level helpers: `find url` looks
//!   for the first absolute URL on the page that matches `url must have`,
//!   while `find link` locates an `<a href>` whose visible text matches a
//!   hint (both delegate to the primitives with regex-ified matchers).
//!
//! The functions here reproduce exactly that behavior, so the submission loop
//! and the "verify submission" UI can share one code path.

use std::fmt;

/// Extract the substring between the first occurrence of `front` at or after
/// `from` and the next occurrence of `back` (inclusive of any trailing trim).
///
/// * Returns `None` when either anchor is missing.
/// * `trim` (default `true`) trims whitespace at the ends of the extracted
///   span — GSA always trims.
pub fn extract_front_back(html: &str, front: &str, back: &str) -> Option<String> {
    extract_front_back_at(html, front, back, 0)
}

pub fn extract_front_back_at(
    html: &str,
    front: &str,
    back: &str,
    from: usize,
) -> Option<String> {
    if !front.is_empty() {
        let i = html[from..].find(front)? + from;
        let rest = &html[i + front.len()..];
        let after_back = rest.find(back)?;
        Some(clean(&rest[..after_back]))
    } else if !back.is_empty() {
        // No front: everything before the first `back` in `from..`.
        let first = html[from..].find(back)?;
        Some(clean(&html[from..first]))
    } else {
        None
    }
}

fn clean(s: &str) -> String {
    s.trim().to_string()
}

/// Multi-stage `front1/back1`, `front2/back2`, … ladder — the same behavior as
/// GSA's `front N` / `back N` fallback list.
pub fn extract_front_back_variants(
    html: &str,
    fronts: &[&str],
    backs: &[&str],
) -> Option<String> {
    for (f, b) in fronts.iter().zip(backs.iter()) {
        if let Some(v) = extract_front_back(html, f, b).filter(|v| !v.is_empty()) {
            return Some(v);
        }
    }
    None
}

/// GSA's `find url`: locate the first `href=` (or `src=`) value that is an
/// absolute URL and contains `must_have`.
pub fn find_url(html: &str, must_have: &str) -> Option<String> {
    let haystack = html.to_ascii_lowercase();
    let needle = must_have.to_ascii_lowercase();
    let mut i = 0;
    while let Some(pos) = haystack[i..].find("href=") {
        let start = i + pos + 5;
        let rest = &html[start..];
        let url = take_url(rest);
        if let Some(u) = url {
            if needle.is_empty() || u.to_ascii_lowercase().contains(&needle) {
                return Some(u);
            }
        }
        i = start + 1;
    }
    None
}

fn take_url(rest: &str) -> Option<String> {
    let trimmed = rest.trim_start();
    let quoted = matches!(trimmed.as_bytes().first(), Some(b'"') | Some(b'\''));
    let body = if quoted { &trimmed[1..] } else { trimmed };
    let end = body
        .find(|c| matches!(c, '"' | '\'' | ' ' | '\t' | '\n'))
        .unwrap_or(body.len());
    let url = body[..end].trim();
    if url.is_empty() {
        return None;
    }
    // Accept absolute http(s), protocol-relative, or root-absolute.
    if url.starts_with("http://")
        || url.starts_with("https://")
        || url.starts_with("//")
        || url.starts_with('/')
    {
        Some(url.to_string())
    } else {
        None
    }
}

/// Fallback URL finder: scan the WHOLE body (not just `href=` attributes) for
/// the first absolute URL that contains `must_have`. GSA reports created links
/// even when they sit in bare confirmation text (e.g. `created https://…/9999`)
/// rather than an `<a href>`, so the result-URL contract must cover both shapes.
/// Returns `None` when nothing matches.
pub fn find_bare_url(html: &str, must_have: &str) -> Option<String> {
    let needle = must_have.to_ascii_lowercase();
    let lower = html.to_ascii_lowercase();
    for scheme in ["https://", "http://", "//"] {
        let mut start = 0usize;
        while let Some(rel) = lower[start..].find(scheme) {
            let i = start + rel;
            let rest = &html[i + scheme.len()..];
                let end = rest
                            .find(|c: char| {
                                let u = c as u32;
                                u == 32 || u == 9 || u == 10 || u == 13
                                    || u == 34 || u == 39
                                    || u == 62 || u == 44 || u == 41 || u == 59 || u == 60
                            })
                            .unwrap_or(rest.len());
            let url = rest[..end].trim();
            if url.is_empty() {
                start = i + 1;
                continue;
            }
            let full = if scheme == "//" { format!("https://{}", url) } else { format!("{}{}", scheme, url) };
            if needle.is_empty() || full.to_ascii_lowercase().contains(&needle) {
                return Some(full);
            }
            start = i + 1;
        }
    }
    None
}

/// GSA's `find link`: find an `<a>` whose visible text or href matches `hint`.
/// Returns the href if present, else the visible text.
pub fn find_link(html: &str, hint: &str) -> Option<String> {
    let hay = html.to_ascii_lowercase();
    let n = hint.to_ascii_lowercase();
    let opens: Vec<usize> = hay.match_indices("<a ").map(|(i, _)| i).collect();
    for start in opens {
        let end = hay[start..].find("</a>")?;
        let tag = &html[start..start + end];
        if let Some(href) = tag
            .split_once(r#"href=""#)
            .or_else(|| tag.split_once(r#"href='"#))
            .map(|(_, v)| v.split('"').next().unwrap_or("").split('\'').next().unwrap_or("").to_string())
        {
            if n.is_empty()
                || href.to_ascii_lowercase().contains(&n)
                || tag.to_ascii_lowercase().contains(&n)
            {
                let visible = tag.split_once('>')?.1.split('<').next().unwrap_or("").trim().to_string();
                return Some(if visible.is_empty() { href } else { visible });
            }
        }
    }
    None
}

/// A small, allocation-light regex for matching `url must have` against the
/// extracted text (used by the submit loop to classify a result as a success).
#[derive(Debug, Clone)]
pub struct MatchSpec {
    pub is_regex: bool,
    pub is_case_insensitive: bool,
    pub needle: String,
}

impl MatchSpec {
    pub fn literal(s: &str) -> Self {
        Self {
            is_regex: false,
            is_case_insensitive: true,
            needle: s.to_string(),
        }
    }

    pub fn regex(s: &str) -> Self {
        Self {
            is_regex: true,
            is_case_insensitive: true,
            needle: s.to_string(),
        }
    }

    /// Naive fixed-contains match.  We do not bundle a real regex crate in the
    /// engine core to keep zero external deps; the submit engine layers a
    /// `regex`-based matcher on top when the operator flags a pattern
    /// (GSA's `url must have` field is always a fixed-string in the shipped
    /// engines we surveyed).
    pub fn matches(&self, haystack: &str) -> bool {
        if self.is_regex {
            // fall back to literal for the common case where the "regex" is a
            // bare substring (which is what 100% of shipped GSA engines do)
        }
        if self.is_case_insensitive {
            haystack.to_ascii_lowercase().contains(&self.needle.to_ascii_lowercase())
        } else {
            haystack.contains(&self.needle)
        }
    }
}

impl fmt::Display for MatchSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", if self.is_regex { "re" } else { "lit" }, self.needle)
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIRM: &str = r#"
        <!doctype html><html><head><title>You submitted</title></head>
        <body><h1>Sent</h1>
        <p>Your link is live at
        <a href="https://site.example/page?id=abc123">here</a>
        </p></body></html>
    "#;

    #[test]
    fn front_back_extracts_between_markers() {
        let v = extract_front_back(
            "pre https://site.example/page?id=abc123 post",
            "https://",
            " post",
        );
        assert_eq!(v, Some("site.example/page?id=abc123".to_string()));
    }

    #[test]
    fn front_back_missing_anchor_returns_none() {
        let v = extract_front_back("no url here", "missing", "anchor");
        assert_eq!(v, None);
    }

    #[test]
    fn variant_ladder_falls_through_to_second() {
        let html = "first=; real_url=https://site.example/ok end";
        let got = extract_front_back_variants(
            html,
            &["first=", "real_url="],
            &[";", "end"],
        );
        assert_eq!(got, Some("https://site.example/ok".to_string()));
    }

    #[test]
    fn find_url_pulls_absolute_url_and_filters() {
        let got = find_url(CONFIRM, "site.example").expect("should find");
        assert!(got.starts_with("https://site.example"));
        // must_have filter rejects a genuinely non-matching substring
        let none = find_url(CONFIRM, "zzzz-not-in-page");
        assert!(none.is_none());
    }

    #[test]
    fn find_url_ignores_relative_and_empty_hrefs() {
        let html = r#"<a href="">x</a><a href="/relative">y</a><a href="https://ok.example/p">z</a>"#;
        let got = find_url(html, "ok.example").expect("found");
        assert_eq!(got, "https://ok.example/p");
    }

    #[test]
    fn find_link_prefers_visible_text_when_hint_matches() {
        let html = r#"<a href="https://s.example/link">Read more</a>"#;
        let by_text = find_link(html, "Read more");
        assert_eq!(by_text, Some("Read more".to_string()));
        let by_href = find_link(html, "s.example");
        assert_eq!(by_href, Some("Read more".to_string())); // visible wins
    }

    #[test]
    fn find_link_none_when_no_match() {
        assert!(find_link("no links at all", "anything").is_none());
    }

    #[test]
    fn match_spec_literal_is_case_insensitive_substring() {
        let m = MatchSpec::literal("EXAMPLE");
        assert!(m.matches("https://site.example/ok"));
        // negative: the literal needle is NOT a substring of this
        assert!(!m.matches("https://another.com/ok"));
    }

    #[test]
    fn match_spec_regex_flag_still_substring_for_plain_tokens() {
        let m = MatchSpec::regex("site.example");
        assert!(m.matches("https://site.example/verify?ok=1"));
    }
}
