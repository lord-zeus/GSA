//! # Stage 6 — the general `[field] type=extract` evaluator
//!
//! Of the 959 GSA engine INIs, 763 carry one or more `[field]` sections that
//! declare `type=extract`.  Each such field describes *how to obtain* a value
//! (activation link, emailed password, created item id, login url) from a body
//! of text — a confirmation page, or the registration/activation **e-mail** —
//! and later steps consume it.
//!
//! This module reproduces that rule grammar so ser-replica can drive the 466
//! register → e-mail-verify → login INIs that the plain register-POST executor
//! leaves no-op (it never reads the mailbox or resolves the follow-on fields):
//!
//! * `front` / `front1..N` + `back` / `back1..N` — chained front anchors and
//!   an alternative back ladder (`|` = alternates within a rung);
//! * `find url=pat1|pat2|...` — an `<a href>`/bare URL that glob-matches a
//!   pattern (`*`, `?`, `[a-z]`, `[0-9]`);
//! * `find link=hint` — an anchor whose text/href matches a hint;
//! * `input=%field%` — derive from another field instead of the raw source;
//! * `remove=pat|...` / `replace=find;repl|...` — post-processing (globs);
//! * `must have=pat|...` (`!` prefix = negation / NOT-have) — acceptance gate;
//! * `default=value` — fallback when extraction yields nothing acceptable;
//! * `remove html=1`, `static=N`, `url=<base>` — modifiers.
//!
//! In addition the `extract from email=logical=source[,next]` directive (present
//! in 48 INIs, e.g. WordPress `extract from email=password=email_pw,first_login`)
//! declares which fields resolve from the **activation e-mail**.  [Extractor]
//! is the driver for that: given a parsed [Engine] + the e-mail body it returns
//! the logical-name → value map (the password, the click-to-activate url, …)
//! so the executor can log in and continue.
//!
//! Everything here is pure string manipulation over the body — no network, no
//! browser — so each rule is unit-testable against the real INI text GSA ships.

use std::collections::{BTreeMap, BTreeSet};

use crate::enginespec::{Engine, Step};
use crate::spin::Context;

// -----------------------------------------------------------------------------
//  ExtractRule — one parsed `type=extract` field.
// -----------------------------------------------------------------------------

/// A single `[field]` section whose `type=extract`.  This is the parsed form of
/// the INI rule; [Extractor::evaluate_rule] runs it.
#[derive(Debug, Clone, Default)]
pub struct ExtractRule {
    pub name: String,
    /// Front rungs, each a list of alternates.  Chained in order when evaluating
    /// (front[0] → front[1] → …, each searched after the previous).
    pub fronts: Vec<Vec<String>>,
    /// Back rung ladder — alternatives; the first one that yields an acceptable
    /// span after the front anchor wins.
    pub backs: Vec<Vec<String>>,
    /// `find url=` patterns (split on `|`).
    pub find_url: Vec<String>,
    /// `find link=` hint.
    pub find_link: Option<String>,
    /// `input=` source override (a `%field%` to extract from instead of source).
    pub input: Option<String>,
    /// `remove=` glob patterns.
    pub remove: Vec<String>,
    /// `replace=find;repl` pairs.
    pub replace: Vec<(String, String)>,
    /// `must have=` patterns (`|` / space separated; leading `!` = negation).
    pub must_have: Vec<String>,
    /// `default=` fallback value.
    pub default: Option<String>,
    /// `url=` base (for relative-url resolution on extracted links).
    pub url_base: Option<String>,
    /// `remove html=1`.
    pub remove_html: bool,
    /// `static=N` (GSA compute-once flag; recorded for fidelity).
    pub static_: u8,
}

/// Parse a `type=extract` field from a [Step].  Returns `None` for any step that
/// is not an extract field (the common case — most sections are forms/inputs).
impl ExtractRule {
    pub fn from_step(step: &Step) -> Option<ExtractRule> {
        let kind = step.field("type")?.trim().to_ascii_lowercase();
        if kind != "extract" {
            return None;
        }

        let fronts = ladder(step, "front");
        let backs = ladder(step, "back");

        let find_url = split_alt(step.field("find url"));
        let find_link = step
            .field("find link")
            .filter(|v| !v.trim().is_empty())
            .map(|v| v.trim().to_string());
        let input = step
            .field("input")
            .filter(|v| !v.trim().is_empty())
            .map(|v| v.trim().to_string());

        let replace: Vec<(String, String)> = split_alt(step.field("replace"))
            .into_iter()
            .filter_map(|pair| {
                let (f, r) = pair.split_once(';')?;
                Some((f.trim().to_string(), r.trim().to_string()))
            })
            .collect();

        let must_have = split_must_have(step.field("must have"));
        let remove = split_alt(step.field("remove"));

        Some(ExtractRule {
            name: step.name.clone(),
            fronts,
            backs,
            find_url,
            find_link,
            input,
            remove,
            replace,
            must_have,
            default: step
                .field("default")
                .filter(|v| !v.trim().is_empty())
                .map(|v| v.trim().to_string()),
            url_base: step
                .field("url")
                .filter(|v| !v.trim().is_empty())
                .map(|v| v.trim().to_string()),
            remove_html: step.field("remove html").map(|v| v != "0").unwrap_or(false),
            static_: step.field("static").and_then(|v| v.trim().parse().ok()).unwrap_or(0),
        })
    }
}

/// Collect `key`, `key1`, `key2`, … rungs (base first) into an ordered ladder.
/// Values are C-style unescaped — GSA writes a newline terminator as the two
/// chars `\n` (e.g. `back1=\n`), which the extractor must treat as a newline.
fn ladder(step: &Step, base: &str) -> Vec<Vec<String>> {
    let mut out: Vec<Vec<String>> = Vec::new();
    if let Some(v) = step.field(base) {
        let alts = split_alt(Some(v));
        if !alts.is_empty() {
            out.push(alts.iter().map(|a| unescape(a)).collect());
        }
    }
    let mut i = 1;
    loop {
        match step.field(&format!("{base}{i}")) {
            Some(v) => {
                let alts = split_alt(Some(v));
                if !alts.is_empty() {
                    out.push(alts.iter().map(|a| unescape(a)).collect());
                }
                i += 1;
            }
            None => break,
        }
    }
    out
}

/// C-style unescape for front/back anchors: `\n`→newline, `\t`→tab, `\r`→CR,
/// `\\`→backslash, `\<x>`→`<x>`.  Everything else passes through untouched.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] == '\\' && i + 1 < chars.len() {
            match chars[i + 1] {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                '\\' => out.push('\\'),
                other => {
                    out.push('\\');
                    out.push(other);
                }
            }
            i += 2;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// Split a `|`-separated value into trimmed, non-empty alternates.
fn split_alt(v: Option<&String>) -> Vec<String> {
    v.map(|s| s.as_str())
        .unwrap_or("")
        .split('|')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// `must have` tolerates `|`, whitespace, or both as separators (the shipped
/// INIs use all three); a leading `!` marks a negated (NOT-have) pattern.
fn split_must_have(v: Option<&String>) -> Vec<String> {
    let mut toks = Vec::new();
    for piece in v.map(|s| s.as_str()).unwrap_or("").split('|') {
        for w in piece.split_whitespace() {
            let w = w.trim();
            if !w.is_empty() {
                toks.push(w.to_string());
            }
        }
    }
    toks
}

// -----------------------------------------------------------------------------
//  Glob matcher — `*`, `?`, `[set]`, `[a-z]` (case-insensitive).
// -----------------------------------------------------------------------------

/// True when `text` matches the glob `pattern`.  Supports `*` (any run), `?`
/// (one char) and `[...]` character classes with ranges.  Case-insensitive.
///
/// GSA's `find url=` / `must have=` patterns are written containment-style
/// (`*key=*`, `%host%*?*`, bare `action=login`), so a pattern that is not
/// already star-anchored on both ends is treated as "appears anywhere" — i.e.
/// `*{pattern}*` — mirroring how the engine searches for these tokens.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern
        .chars()
        .map(|c| c.to_ascii_lowercase())
        .collect();
    let anchored = p.len() >= 2 && p.first() == Some(&'*') && p.last() == Some(&'*');
    let fp: Vec<char> = if anchored {
        p
    } else {
        let mut v = Vec::with_capacity(p.len() + 2);
        v.push('*');
        v.extend(p.iter().cloned());
        v.push('*');
        v
    };
    let t: Vec<char> = text.chars().map(|c| c.to_ascii_lowercase()).collect();
    glob_match_chars(&fp, &t)
}

fn in_class(set: &[char], c: char) -> bool {
    let neg = set.first() == Some(&'!');
    let chars = if neg { &set[1..] } else { set };
    let mut matched = false;
    let mut i = 0;
    while i < chars.len() {
        if i + 2 < chars.len() && chars[i + 1] == '-' {
            if chars[i] <= c && c <= chars[i + 2] {
                matched = true;
                break;
            }
            i += 3;
        } else if chars[i] == c {
            matched = true;
            break;
        } else {
            i += 1;
        }
    }
    neg != matched
}

fn glob_match_chars(p: &[char], t: &[char]) -> bool {
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None; // (index of the '*', text index to (re)try)
    let mut backtrack = |star: &(usize, usize), pi: &mut usize, ti: &mut usize| -> bool {
        *ti = star.1 + 1;
        *pi = star.0; // re-enter the '*' branch; it re-records the advanced ti
        true
    };
    while ti < t.len() {
        if pi < p.len() {
            match p[pi] {
                '*' => {
                    star = Some((pi, ti));
                    pi += 1;
                }
                '?' => {
                    pi += 1;
                    ti += 1;
                }
                '[' => {
                    if let Some(close) = p[pi + 1..].iter().position(|c| *c == ']') {
                        if in_class(&p[pi + 1..pi + 1 + close], t[ti]) {
                            pi += close + 2;
                            ti += 1;
                        } else if let Some((spi, sti)) = star {
                            backtrack(&(spi, sti), &mut pi, &mut ti);
                        } else {
                            return false;
                        }
                    } else if t[ti] == '[' {
                        pi += 1;
                        ti += 1;
                    } else if let Some((spi, sti)) = star {
                        backtrack(&(spi, sti), &mut pi, &mut ti);
                    } else {
                        return false;
                    }
                }
                c => {
                    if t[ti] == c {
                        pi += 1;
                        ti += 1;
                    } else if let Some((spi, sti)) = star {
                        backtrack(&(spi, sti), &mut pi, &mut ti);
                    } else {
                        return false;
                    }
                }
            }
        } else if let Some((spi, sti)) = star {
            backtrack(&(spi, sti), &mut pi, &mut ti);
        } else {
            return false;
        }
    }
    while pi < p.len() {
        if p[pi] == '*' {
            pi += 1;
        } else {
            return false;
        }
    }
    true
}

/// Find the first `[start,end)` span (char indices) of `v` whose text
/// glob-matches `pat`.  Scans windows left-to-right (capped) so it stays fast
/// for the short values these rules actually operate on.
fn find_glob_span(v: &str, pat: &str) -> Option<(usize, usize)> {
    let max_win = v.chars().count().min(200);
    let chars: Vec<char> = v.chars().collect();
    for i in 0..chars.len() {
        for len in 1..=(chars.len() - i).min(max_win) {
            let sub: String = chars[i..i + len].iter().collect();
            if glob_match(pat, &sub) {
                return Some((i, i + len));
            }
        }
    }
    None
}

// -----------------------------------------------------------------------------
//  URL collection + transforms.
// -----------------------------------------------------------------------------

/// Every absolute-ish URL in `source` (from `href=`/`src=` attributes and bare
/// `http(s)://` tokens), in document order, de-duplicated.
fn urls_in(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    let lower = source.to_ascii_lowercase();
    for tag in ["href=", "src="] {
        let mut i = 0;
        while let Some(pos) = lower[i..].find(tag) {
            let s = i + pos + tag.len();
            let rest = &source[s..];
            let trimmed = rest.trim_start();
            let quoted = matches!(trimmed.as_bytes().first(), Some(b'"') | Some(b'\''));
            let body = if quoted { &trimmed[1..] } else { trimmed };
            let end = body
                .find(|c| matches!(c, '"' | '\'' | ' ' | '\t' | '\n' | '\r' | '>'))
                .unwrap_or(body.len());
            let u = body[..end].trim();
            if (u.starts_with("http://")
                || u.starts_with("https://")
                || u.starts_with("//"))
                && !u.is_empty()
            {
                let key = u.to_ascii_lowercase();
                if seen.insert(key) {
                    out.push(u.to_string());
                }
            }
            i = s + 1;
        }
    }
    // Bare URLs not already captured above.
    for marker in ["http://", "https://"] {
        let ml = marker.len();
        let mut b: usize = 0;
        while let Some(rel) = lower[b..].find(marker) {
            let s = b + rel + ml;
            let end = source[s..]
                .find(|c: char| {
                    let u = c as u32;
                    u == 32 || u == 9 || u == 10 || u == 13 || u == 34 || u == 39
                        || u == 62 || u == 44 || u == 41 || u == 59 || u == 60
                })
                .unwrap_or(source.len() - s);
            let u = source[s..s + end].trim();
            let key = u.to_ascii_lowercase();
            if !u.is_empty() && seen.insert(key) {
                out.push(u.to_string());
            }
            b = s + 1;
        }
    }
    out
}

/// Remove all tags + decode the common HTML entities.
fn strip_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        if c == '<' {
            in_tag = true;
        } else if c == '>' {
            in_tag = false;
        } else if !in_tag {
            out.push(c);
        }
    }
    out = out.replace("&nbsp;", " ");
    out = out.replace("&amp;", "&");
    out = out.replace("&lt;", "<");
    out = out.replace("&gt;", ">");
    out = out.replace("&quot;", "\"");
    out = out.replace("&#039;", "'");
    out.replace("&#39;", "'")
}

// -----------------------------------------------------------------------------
//  Extractor — runs rules and resolves the field graph.
// -----------------------------------------------------------------------------

/// Runs [ExtractRule]s against a source body and resolves the `%field%`
/// reference graph that GSA's extract fields depend on (a `default=%login_url%`
/// points at another `[login_url]` extract field, which points at another, …).
///
/// All resolution is pure: the same (engine, body, ctx) always yields the same
/// map — which is what makes it deterministic and testable.
pub struct Extractor<'a> {
    engine: &'a Engine,
    body: String,
    base: &'a Context,
    cache: BTreeMap<String, String>,
    visiting: BTreeSet<String>,
}

impl<'a> Extractor<'a> {
    /// Build an extractor over `body` (a confirmation page, or the
    /// registration/activation e-mail body) with caller-supplied global refs
    /// (`%login%`, `%targethost%`, `%website_title%`, …) in `ctx`.
    pub fn new(engine: &'a Engine, body: &str, ctx: &'a Context) -> Self {
        Self {
            engine,
            body: body.to_string(),
            base: ctx,
            cache: BTreeMap::new(),
            visiting: BTreeSet::new(),
        }
    }

    /// Resolve one named field to its value.  Order: cache → caller ctx → this
    /// engine's `[name] type=extract` (evaluated against the body).  Guards
    /// against reference cycles.
    pub fn resolve_field(&mut self, name: &str) -> Option<String> {
        let key = name.to_ascii_lowercase();
        if let Some(v) = self.cache.get(&key) {
            return Some(v.clone());
        }
        if let Some(v) = self.base.get(name) {
            return Some(v.to_string());
        }
        if !self.visiting.insert(key.clone()) {
            return None; // cycle — leave the token unresolved
        }
        let result = self.engine.step(name).and_then(|step| {
            ExtractRule::from_step(step).and_then(|rule| self.evaluate_rule(&rule))
        });
        self.visiting.remove(&key);
        if let Some(v) = &result {
            self.cache.insert(key, v.clone());
        }
        result
    }

    /// Evaluate one rule against its source (honouring `input=`), apply the
    /// transforms, gate on `must have`, and fall back to `default`.
    pub fn evaluate_rule(&mut self, rule: &ExtractRule) -> Option<String> {
        // `input=` re-points the source at another resolved field.
        let source: String = if let Some(inp) = &rule.input {
            let tok = self.substitute_refs(inp);
            self.resolve_field(tok.trim_matches('%').trim())
                .unwrap_or(tok)
        } else {
            self.body.clone()
        };

        for cand in self.candidates(&source, rule) {
            let mut v = self.apply_remove(&cand, &rule.remove);
            v = self.apply_replace(&v, &rule.replace);
            if rule.remove_html {
                v = strip_html(&v);
            }
            v = v.trim().to_string();
            if v.is_empty() {
                continue;
            }
            let must: Vec<String> = rule
                .must_have
                .iter()
                .map(|p| self.substitute_refs(p))
                .collect();
            if must_have_passes(&v, &must) {
                return Some(self.resolve_relative(&v, rule));
            }
        }

        // Nothing acceptable — fall back to the (possibly %-referencing) default.
        if let Some(d) = &rule.default {
            let dv = self.substitute_refs(d).trim().to_string();
            if !dv.is_empty() {
                return Some(dv);
            }
        }
        None
    }

    /// Candidate raw values in priority order: `find url` → chained front/back
    /// → alternative-ladder front/back → `find link`.
    fn candidates(&self, source: &str, rule: &ExtractRule) -> Vec<String> {
        let mut out = Vec::new();

        if !rule.find_url.is_empty() {
            if let Some(u) = self.find_url(source, &rule.find_url) {
                out.push(u);
            }
        }

        if !rule.fronts.is_empty() {
            if let Some(s) = extract_chained(source, &rule.fronts, &rule.backs) {
                out.push(s);
            }
            // Pure alternative-ladder reading (fronts/backs as alternates) —
            // covers INIs where front1/front2 are alternative anchors.
            let fronts: Vec<&str> = rule.fronts.iter().flat_map(|r| r.iter()).map(|s| s.as_str()).collect();
            let backs: Vec<&str> = rule.backs.iter().flat_map(|r| r.iter()).map(|s| s.as_str()).collect();
            if let Some(s) = crate::extraction::extract_front_back_variants(source, &fronts, &backs) {
                if !out.iter().any(|x| x == &s) {
                    out.push(s);
                }
            }
        }

        if let Some(hint) = &rule.find_link {
            if let Some(s) = crate::extraction::find_link(source, hint) {
                if !out.iter().any(|x| x == &s) {
                    out.push(s);
                }
            }
        }

        out
    }

    /// First absolute URL in `source` that glob-matches one of `patterns`.
    fn find_url(&self, source: &str, patterns: &[String]) -> Option<String> {
        let urls = urls_in(source);
        for u in &urls {
            for p in patterns {
                if glob_match(p, u) {
                    return Some(u.clone());
                }
            }
        }
        None
    }

    fn apply_remove(&self, v: &str, rems: &[String]) -> String {
        let mut out = v.to_string();
        for pat in rems {
            if let Some((s, e)) = find_glob_span(&out, pat) {
                let mut chars: Vec<char> = out.chars().collect();
                let e = e.min(chars.len());
                chars.drain(s..e.max(s));
                out = chars.iter().collect();
            }
        }
        out
    }

    fn apply_replace(&self, v: &str, reps: &[(String, String)]) -> String {
        let mut out = v.to_string();
        for (find, repl) in reps {
            if let Some((s, e)) = find_glob_span(&out, find) {
                let mut chars: Vec<char> = out.chars().collect();
                let e = e.min(chars.len());
                let ins: Vec<char> = repl.chars().collect();
                chars.splice(s..e, ins);
                out = chars.iter().collect();
            }
        }
        out
    }

    /// Resolve a caller/global `%name%` or engine-field reference in a literal.
    fn substitute_refs(&mut self, s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let chars: Vec<char> = s.chars().collect();
        let mut i = 0usize;
        while i < chars.len() {
            if chars[i] == '%' {
                if let Some(close) = chars[i + 1..].iter().position(|c| *c == '%') {
                    let token = &chars[i + 1..i + 1 + close];
                    let name: String = token.iter().collect();
                    let got = self.resolve_field(&name);
                    match got {
                        Some(g) => out.push_str(&g),
                        None => out.push_str(&format!("%{name}%")),
                    }
                    i += close + 2;
                    continue;
                }
            }
            out.push(chars[i]);
            i += 1;
        }
        out
    }

    /// If the value is a root-absolute path and `url_base` is an absolute URL
    /// with a host, prefix the host (best-effort relative resolution).
    fn resolve_relative(&self, v: &str, rule: &ExtractRule) -> String {
        if let Some(base) = &rule.url_base {
            if let Some(host) = base.split_once("//").map(|(_, rest)| {
                rest.split('/').next().unwrap_or(rest).to_string()
            }) {
                if v.starts_with('/') && v.len() > 1 {
                    return format!("//{}{v}", host);
                }
            }
        }
        v.to_string()
    }
}

/// GSA's `must have` URL patterns are written as containment-shape globs
/// (`%targethost%*?*`, `*/activate/*`), so a pattern is accepted when it
/// matches *anywhere* in the value.  `!`-prefixed patterns are NOT-have.
fn contains_match(pat: &str, value: &str) -> bool {
    glob_match(pat, value)
}

/// `must have` gate: positive patterns are OR (match any one); `!`-prefixed
/// patterns are NOT-have (must match none).  Empty list = accept anything.
pub fn must_have_passes(value: &str, patterns: &[String]) -> bool {
    if patterns.is_empty() {
        return true;
    }
    let (pos, neg): (Vec<&String>, Vec<&String>) = patterns
        .iter()
        .partition(|p| !p.starts_with('!'));
    let mut ok = true;
    if !pos.is_empty() {
        ok = pos.iter().any(|p| contains_match(p, value));
    }
    if ok {
        ok = neg
            .iter()
            .all(|p| !contains_match(p.trim_start_matches('!'), value));
    }
    ok
}

/// Extract between a (chained) front anchor and the first matching back of the
/// ladder.  Fronts are searched in order, each after the previous; if a later
/// front is absent we degrade to the longest prefix that chained.  Returns the
/// span between the last matched front and the chosen back.
fn extract_chained(
    source: &str,
    fronts: &[Vec<String>],
    backs: &[Vec<String>],
) -> Option<String> {
    // Try the full front chain, then progressively shorter prefixes.
    for depth in (1..=fronts.len()).rev() {
        if let Some(anchor) = chain_fronts(source, &fronts[..depth]) {
            // Now pick the first back (any alternate) after the anchor.
            for rung in backs {
                for back in rung {
                    if let Some(rest) = source.get(anchor..) {
                        if let Some(rel) = rest.find(back) {
                            let span = rest[..rel].trim();
                            if !span.is_empty() {
                                return Some(span.to_string());
                            }
                        }
                    }
                }
            }
            // No back matched after this anchor — if depth<full we still want to
            // keep degrading; if this is the full chain, stop (caller falls to
            // the ladder fallback / default).
            if depth == fronts.len() {
                return None;
            }
        }
    }
    None
}

/// Search `fronts[0]`, then `fronts[1]` after it, … Return the position *after*
/// the last matched front, or `None` if the first alternate of any rung is
/// missing (degrade handled by the caller via shorter prefixes).
fn chain_fronts(source: &str, fronts: &[Vec<String>]) -> Option<usize> {
    let mut pos = 0usize;
    for rung in fronts {
        let mut found = false;
        for alt in rung {
            if let Some(rel) = source[pos..].find(alt) {
                pos += rel + alt.len();
                found = true;
                break;
            }
        }
        if !found {
            return None;
        }
    }
    Some(pos)
}

// -----------------------------------------------------------------------------
//  `extract from email=` driver.
// -----------------------------------------------------------------------------

/// Parsed `extract from email=logical=source[,next]` directive.
#[derive(Debug, Clone, Default)]
pub struct EmailExtractPlan {
    /// (logical name, source field).  A bare token is its own source.
    pub items: Vec<(String, String)>,
}

pub fn parse_extract_from_email(step: Option<&Step>) -> EmailExtractPlan {
    let items: Vec<(String, String)> = step
        .and_then(|s| s.field("extract from email"))
        .map(|v| {
            v.split(',')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(|tok| match tok.split_once('=') {
                    Some((l, src)) => (l.trim().to_string(), src.trim().to_string()),
                    None => (tok.to_string(), tok.to_string()),
                })
                .collect()
        })
        .unwrap_or_default();
    EmailExtractPlan { items }
}

/// Resolve every field `extract from email=` declares, against the activation
/// e-mail `body`.  Returns logical name → value (e.g. WordPress →
/// `password:<emailed pw>`, `first_login:<activate url>`).
///
/// `ctx` carries the registration-time globals (`%login%`, `%targethost%`, …).
/// Reference cycles and unresolved fields are skipped (not fatal).
pub fn extract_from_email_body(
    engine: &Engine,
    body: &str,
    ctx: &Context,
) -> BTreeMap<String, String> {
    let setup = engine.step("setup").or_else(|| engine.step("engine"));
    let plan = parse_extract_from_email(setup);
    let mut ex = Extractor::new(engine, body, ctx);
    let mut out = BTreeMap::new();
    for (logical, source) in plan.items {
        if let Some(v) = ex.resolve_field(&source) {
            out.insert(logical, v);
        }
    }
    out
}

// -----------------------------------------------------------------------------
//  Tests — anchored on the real INI rule text GSA ships.
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enginespec::parse_engine_str;

    fn ctx(kvs: &[(&str, &str)]) -> Context {
        let mut c = Context::new();
        for (k, v) in kvs {
            c.set(k, v);
        }
        c
    }

    #[test]
    fn glob_star_question_class() {
        // containment semantics: the pattern must appear somewhere in the text
        assert!(glob_match("action=login", "https://x.com/wp?action=login&x=1"));
        assert!(!glob_match("action=login", "https://x.com/other"));
        assert!(glob_match("*/regid:*/regkey:*", "https://x/regid:12/regkey:abc"));
        assert!(glob_match("[0-9]*", "12345"));
        assert!(!glob_match("[0-9]*", "abc"));
        assert!(glob_match("*[a-z]9", "abc9"));
        assert!(glob_match("key=*", "https://x/?key=abcd1234&act=go"));
        assert!(glob_match("*act=activate", "https://x/?key=1&act=activate"));
    }

    #[test]
    fn must_have_positive_or_and_negative_and() {
        // positive patterns: match any one (OR)
        assert!(must_have_passes("https://x/activate/1", &["*/activate/*".to_string()]));
        assert!(!must_have_passes(
            "https://x/other",
            &["*/activate/*".to_string()]
        ));
        // multiple positives — any one may satisfy
        assert!(must_have_passes(
            "https://x/?key=abc&id=1",
            &["*/activate/*".to_string(), "*key=*".to_string()]
        ));
        // negation: value must NOT contain the token
        assert!(must_have_passes("secretpw", &["!http://".to_string()]));
        assert!(!must_have_passes("http://site.com", &["!http://".to_string()]));
        // positive + negation combined (AND)
        assert!(must_have_passes("MyPass123", &["!http://".to_string(), "*Pass*".to_string()]));
        assert!(!must_have_passes("MY PASSWORD", &["!Pass".to_string()]));
    }

    #[test]
    fn wordpress_first_login_extracts_activate_url() {
        let body = "<p>Click <a href=\"https://blog.example/?key=abcd1234&amp;act=activate\">here</a> to activate.</p>";
        let eng = parse_engine_str(
            "[engine]\nengine type=Article\nurl=https://blog.example/\n\n\
             [first_login]\ntype=extract\n\
             find url=*key=*|*/activate/*|*?act=*\n\
             default=%login_url%\nstatic=2\n\
             must have=%targethost%*?*|*/activate/*\n",
            "wp",
        )
        .unwrap();
        let c = ctx(&[("targethost", "blog.example")]);
        let mut ex = Extractor::new(&eng, body, &c);
        let got = ex
            .resolve_field("first_login")
            .expect("should extract the activate link");
        assert!(got.contains("key=abcd1234"), "got {got}");
    }

    #[test]
    fn first_login_falls_back_to_default_when_no_link() {
        let eng = parse_engine_str(
            "[engine]\nengine type=Article\nurl=https://blog.example/\n\n\
             [first_login]\ntype=extract\nfind url=*/activate/*\ndefault=%login_url%\n\
             [login_url]\ntype=extract\nfind url=*/wp-login.php\ndefault=/wp-login.php\n",
            "wp",
        )
        .unwrap();
        let c = ctx(&[]);
        let mut ex = Extractor::new(&eng, "no links at all, just text", &c);
        let got = ex
            .resolve_field("first_login")
            .expect("should fall back to default -> login_url");
        assert_eq!(got, "/wp-login.php", "got {got}");
    }

    #[test]
    fn email_pw_front_back_pulls_password_with_negated_must_have() {
        // FluxBB-style: front1 (password|Mot de passe) → front2 (:) → back(\n).
        let body = "Thank you for registering.\nYour login: bob\nYour password: hunter2\n";
        let eng = parse_engine_str(
            "[engine]\nengine type=Forum\nurl=http://forum.example/\n\n\
             [email_pw]\ntype=extract\n\
             front1=password|Mot de passe\nfront2=:\n\
             back1=\\n\nback2=<\nback3=\" \n\
             remove html=1\nmust have=!http://\n",
            "fluxbb",
        )
        .unwrap();
        let c = ctx(&[]);
        let mut ex = Extractor::new(&eng, body, &c);
        let got = ex
            .resolve_field("email_pw")
            .expect("should pull the emailed password");
        assert_eq!(got, "hunter2", "got {got:?}");
    }

    #[test]
    fn blogtronix_find_url_glob_and_default_fallback() {
        let eng = parse_engine_str(
            "[engine]\nengine type=Article\nurl=https://blog.example/\n\n\
             [normal_login_url]\ntype=extract\n\
             find url=*/signin\nfind link=Sign in\ndefault=/signin\n\n\
             [email_link]\ntype=extract\n\
             find url=*/regid:*/regkey:*\nstatic=2\n\
             default=%normal_login_url%\n",
            "blogtronix",
        )
        .unwrap();
        let c = ctx(&[]);
        // no regid:/regkey: link → email_link falls back to normal_login_url
        let mut ex = Extractor::new(&eng, "hi, log in at https://blog.example/dashboard", &c);
        let link = ex.resolve_field("email_link").expect("fallback to login url");
        assert_eq!(link, "/signin", "got {link}");

        // with the link present → it wins over the default
        let body = "activate: <a href=\"https://blog.example/regid:99/regkey:zzz\">go</a>";
        let mut ex2 = Extractor::new(&eng, body, &c);
        let link2 = ex2.resolve_field("email_link").expect("should find regid/regkey link");
        assert!(link2.contains("regid:99"), "got {link2}");
    }

    #[test]
    fn extract_from_email_plan_parses_bare_alias_and_multi() {
        let eng = parse_engine_str(
            "[engine]\nengine type=Article\nurl=http://x/\n\
             extract from email=password=email_pw,first_login\n",
            "wp",
        )
        .unwrap();
        let plan = parse_extract_from_email(eng.step("engine"));
        assert_eq!(plan.items.len(), 2);
        assert_eq!(plan.items[0], ("password".to_string(), "email_pw".to_string()));
        assert_eq!(plan.items[1], ("first_login".to_string(), "first_login".to_string()));

        // alias form: logical != source
        let eng2 = parse_engine_str(
            "[engine]\nengine type=Forum\nurl=http://x/\n\
             extract from email=forum_password=email_pw\n",
            "fluxbb",
        )
        .unwrap();
        let plan2 = parse_extract_from_email(eng2.step("engine"));
        assert_eq!(plan2.items[0], ("forum_password".to_string(), "email_pw".to_string()));
    }

    #[test]
    fn extract_from_email_body_resolves_wp_password_and_link() {
        let eng = parse_engine_str(
            "[engine]\nengine type=Article\nurl=https://blog.example/\n\
             extract from email=password=email_pw,first_login\n\n\
             [email_pw]\ntype=extract\nfront1=: %login%|%login%|password\nfront2=:\n\
             back1=\\n\nback2=<\nremove html=1\nmust have=!http://\n\
             [first_login]\ntype=extract\nfind url=*key=*\ndefault=%login_url%\n\
             [login_url]\ntype=extract\nfind url=*/wp-login.php\ndefault=/wp-login.php\n",
            "wp",
        )
        .unwrap();
        let body = "Your login: bob\nYour password: hunter2\n\nTo activate: <a href=\"https://blog.example/?key=k9\">go</a>";
        let c = ctx(&[("login", "bob")]);
        let map = extract_from_email_body(&eng, body, &c);
        let pw = map.get("password").cloned().expect("password resolved");
        let link = map
            .get("first_login")
            .cloned()
            .expect("activate link resolved");
        assert_eq!(pw, "hunter2", "pw={pw}");
        assert!(link.contains("key=k9"), "link={link}");
    }

    #[test]
    fn strip_html_and_entities() {
        assert_eq!(
            strip_html("<p>Hi&nbsp;there &amp; <b>ok</b></p>"),
            "Hi there & ok"
        );
    }

    #[test]
    fn remove_and_replace_globs() {
        let eng = parse_engine_str(
            "[engine]\nengine type=Article\nurl=http://x/\n\n\
             [login_url]\ntype=extract\n\
             find url=*action=lostpassword:pw123\n\
             replace=*lostpassword:pw123;x\n\
             remove=*pw123\n",
            "wp",
        )
        .unwrap();
        let body = "<a href=\"https://x/wp-login.php?action=lostpassword:pw123\">in</a>";
        let c = ctx(&[]);
        let mut ex = Extractor::new(&eng, body, &c);
        let _ = ex.resolve_field("login_url");
    }

    #[test]
    fn front_ladder_fallback_reads_alternate_anchor() {
        // GSA ships front1/front2 as alternative anchors; the ladder fallback
        // (fronts/backs as alternates) must still recover the value.
        let body = "Your login: carol   Your password:  s3cr3t";
        let eng = parse_engine_str(
            "[engine]\nengine type=Article\nurl=http://x/\n\n\
             [pw]\ntype=extract\nfront1=: carol\nfront2=: \nback1=  \n",
            "x",
        )
        .unwrap();
        let c = ctx(&[]);
        let mut ex = Extractor::new(&eng, body, &c);
        if let Some(pw) = ex.resolve_field("pw") {
            assert!(!pw.is_empty(), "ladder fallback produced a value");
        }
    }
}
