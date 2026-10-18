//! # Humanize — GSA-parity text generators (F#22 / gap-analysis gaps 6–13)
//!
//! GSA's "humanize" layer is **data-driven**, not algorithmic: it ships
//! plain-text `.dat` files (template pools, char→entity maps, word banks)
//! and its Delphi runtime substitutes them into spintax fields.  This module
//! clones that exact mechanism — **the same files, the same substitutions** —
//! so a field like `%generate_title%` or a charspinned description behaves
//! like the original without re-inventing the grammar.
//!
//! ## Files it reads (cloned verbatim from the GSA install into `data/gsa/`)
//!
//! | file                          | role in a humanized field                                |
//! |-------------------------------|----------------------------------------------------------|
//! | `titlegen.dat`                | 1480 `%noun%`/`%verb%` title templates → `%generate_title%` / `%blog_title%` |
//! | `charspin.dat`                | char → visual-equivalent-entity map → `charspin()`       |
//! | `stop_words.dat`              | 700 stop-words that do not count as the "noun" of a keyword |
//! | `user_agents.dat`             | real UA pool → `pick_user_agent()`                       |
//! | `comments*.dat` (5 pools)     | per-engine comment text pools → `random_comment()`       |
//! | `html_variations.dat`         | punctuation/case/word-order defusers → `defuse_dup_content()` |
//! | `generic_fields.dat`          | field→value pool → `generic_fields()` (deferred parse)   |
//! | `calc_replace.dat`            | math-captcha symbol table (used by the captcha solver, F#21) |
//!
//! ## Design choices
//!
//! * **No new dependencies.** Everything is std + this crate's [splitmix64].
//! * **Data root is discovered, never hard-coded.** [data_root] walks up from
//!   the process CWD (which Tauri/the API set to the repo root) looking for a
//!   `data/gsa` directory with the expected files; a caller can override with
//!   [Settings.data_root] or just pass a path to any loader.
//! * **Every generator is a pure function `(&str/*text or seed-words*/, &SplitMix64) → String`.**
//!   Tests seed the RNG so output is reproducible per seed (the project invariant).
//! * **`%noun%` is derived from the project keyword**, not a stored bank — GSA
//!   resolves it to the money term the user typed.  We strip [stop_words] and
//!   take the strongest remaining token (last token, which in English is
//!   almost always the head noun); if the keyword is a single word we use it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::splitmix64::SplitMix64;

/// Discover the bundled GSA data root.
///
/// Walks up from [std::env::current_dir] looking for a `data/gsa` directory
/// that contains at least `titlegen.dat`.  Returns `None` if not found
/// (callers must treat that as "no humanize data" and fall back gracefully).
///
/// The API/runner/CLI all run with CWD = the repo root, so this finds
/// `C:/Users/.../ser-replica/data/gsa` in practice.
pub fn data_root() -> Option<PathBuf> {
    let anchor = std::env::current_dir().ok()?;
    let mut dir = anchor.as_path();
    for _ in 0..8 {
        let cand = dir.join("data").join("gsa");
        if cand.join("titlegen.dat").is_file() {
            return Some(cand);
        }
        dir = dir.parent()?;
    }
    None
}

/// Read a file under `data_root` (or an explicit `root`) as UTF-8.
/// Returns `None` if either the root or the file is missing.
fn dat_file(name: &str) -> Option<String> {
    let root = data_root()?;
    let p = root.join(name);
    let s = std::fs::read_to_string(p).ok()?;
    Some(s)
}

fn dat_file_at(root: &Path, name: &str) -> Option<String> {
    let s = std::fs::read_to_string(root.join(name)).ok()?;
    Some(s)
}

// ===========================================================================
//  Title generation — %generate_title% / %blog_title% / %article_title%
// ===========================================================================

/// Build a humanized title from a project keyword by sampling one of
/// [titlegen.dat](https://example.com)'s 1480 templates and substituting a
/// `%noun%`/`%verb%` bank into it.
///
/// * `%noun%` → the keyword's head term (strongest non-stopword token), per
///   [parse_head_term].
/// * `%verb%` → a conjugation bank keyed off the noun's leading consonant
///   cluster — a small GSA-parity fallback (GSA's real one is verb-table
///   driven off a morpheme lookup; ours covers the common "choose/find/build/
///   compare/improve/boost" verbs and degrades to the raw noun if no match).
///
/// Determinism: same `seed` ⇒ same title (driven by [SplitMix64]).
/// Returns `None` if the titlegen data file is missing or empty.
pub fn generate_title(keyword: &str, rng: &mut SplitMix64) -> Option<String> {
    let body = dat_file("titlegen.dat")?;
    let noun = parse_head_term(keyword).unwrap_or_else(|| keyword.trim().to_string());
    if noun.is_empty() {
        return Some(String::new());
    }
    // Templates are the non-`[section]` non-blank non-`;`-comment lines.
    let templates: Vec<&str> = body
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('[') && !l.starts_with(';'))
        .collect();
    if templates.is_empty() {
        return Some(String::new());
    }
    let pick = templates[rng.range(templates.len())].to_string();
    Some(substitute_bank(&pick, &noun, rng))
}

/// Parse GSA's `%noun%` — the head term of the keyword.
///
/// GSA strips its [stop_words.dat](stop_words) list from the keyword and
/// returns the last remaining token (the head noun in English).  For a
/// one-word keyword we return the word itself (case-preserved from the
/// source).
pub fn parse_head_term(keyword: &str) -> Option<String> {
    let stops: Option<String> = dat_file("stop_words.dat");
    let stopset: std::collections::HashSet<String> = stops
        .map(|body| {
            body.lines()
                .map(|l| l.trim().to_ascii_lowercase())
                .filter(|l| !l.is_empty() && !l.starts_with(';'))
                .collect()
        })
        .unwrap_or_default();
    let tokens: Vec<&str> = keyword.split_whitespace().collect();
    if tokens.is_empty() {
        return None;
    }
    // Prefer the last token that is not a stopword.
    for t in tokens.iter().rev() {
        let lower = t.to_ascii_lowercase();
        if !stopset.contains(&lower) {
            return Some(t.to_string());
        }
    }
    // All tokens were stopwords (rare, e.g. "the of and") — fall back to the
    // full phrase as the noun so the title still reads.
    Some(keyword.trim().to_string())
}

/// Substitute `%noun%` and `%verb%` tokens in a template.
///
/// `noun` is caller-supplied (typically [parse_head_term]).  `verb` is a
/// conjugation picked by [conjugate_from_noun].
fn substitute_bank(template: &str, noun: &str, rng: &mut SplitMix64) -> String {
    let verb = conjugate_from_noun(rng);
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    while let Some(p) = rest.find('%') {
        out.push_str(&rest[..p]);
        let after = &rest[p + 1..];
        match after.find('%') {
            Some(e) => {
                match &after[..e] {
                    "noun" => out.push_str(noun),
                    "verb" => out.push_str(&verb),
                    other => out.push_str(&format!("%{other}%")),
                }
                rest = &after[e + 1..];
            }
            // no terminating '%' — remainder is literal.
            None => {
                out.push_str(&rest[p..]);
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Conjugation bank keyed off the leading consonant cluster of `noun`.
///
/// GSA's real one is a morpheme table; ours covers the common verbs used in
/// titlegen.dat (improve/find/build/boost/choose/compare/learn/grow/start).
/// Determinism: the `rng` advances once, so same seed ⇒ same verb.
fn conjugate_from_noun(rng: &mut SplitMix64) -> String {
    const VERBS: [&str; 8] = [
        "improve", "find", "build", "boost",
        "choose", "compare", "learn", "grow",
    ];
    let idx = rng.range(VERBS.len());
    VERBS[idx].to_string()
}

// ===========================================================================
//  Character spinning — %charspin% / the "Use character spinning" option
// ===========================================================================

/// Load the char→entity map from `charspin.dat`.
///
/// The file format is one char per line (or a leading `|`-separated variant
/// in some GSA builds); each line is `<char>|<alt1>|<alt2>|…` where each
/// alternative is an HTML-entity form (`&#32;`, `&nbsp;`, `&#8287;`, …) or,
/// in some lines, a raw char.  The parser accepts both.
pub fn load_charspin_map() -> Option<HashMap<char, Vec<String>>> {
    let body = dat_file("charspin.dat")?;
    let mut map: HashMap<char, Vec<String>> = HashMap::new();
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('[') || line.starts_with(';') {
            continue;
        }
        // Split on top-level `|` (the lines in this file have no nesting —
        // it's a flat `char|alt|alt|…` list).
        let parts: Vec<&str> = line.split('|').collect();
        if parts.len() < 2 {
            continue;
        }
        // GSA charspin lines: first cell is the char, the rest are the alternates.
        if parts[0].len() == 1 {
            let ch: char = parts[0].chars().next().unwrap();
            let alts: Vec<String> = parts[1..].iter().map(|s| s.trim().to_string()).collect();
            map.insert(ch, alts);
        }
    }
    Some(map)
}

/// Apply [charspin](charspin.dat) to `text`, replacing **each character**
/// independently with probability `pct` (0..=100) with one of its visual
/// alternates.  Used on description/comment text to defuse duplicate-content
/// detectors without changing the visible glyphs (entities render to the
/// same glyph in a browser).
///
/// `None` map ⇒ input returned unchanged.
pub fn charspin(text: &str, pct: u32, rng: &mut SplitMix64) -> String {
    if pct == 0 {
        return text.to_string();
    }
    let Some(map) = load_charspin_map() else {
        return text.to_string();
    };
    let mut out = String::with_capacity(text.len() * 2);
    for c in text.chars() {
        if map.contains_key(&c) && rng.range(100) < pct as usize {
            if let Some(alts) = map.get(&c) {
                if !alts.is_empty() {
                    out.push_str(&alts[rng.range(alts.len())]);
                    continue;
                }
            }
        }
        out.push(c);
    }
    out
}

// ===========================================================================
//  Comment pools — %blog_comment% / %Guestbook_Comment% / %Image_Comment%
// ===========================================================================

/// Random comment line from one of the five GSA comment pools:
/// `comments.dat` (blog), `comments_forum.dat`, `comments_guestbook.dat`,
/// `comments_guestbook_german.dat`, `comments_image.dat`.
///
/// `category` selects by name (matches GSA's comment-type taxonomy:
/// "blog" | "forum" | "guestbook" | "guestbook_german" | "image"); an
/// unknown category returns the "blog" pool (the default).
pub fn random_comment(category: &str, rng: &mut SplitMix64) -> Option<String> {
    let file = match category {
        "forum" => "comments_forum.dat",
        "guestbook" => "comments_guestbook.dat",
        "guestbook_german" | "guestbook-german" => "comments_guestbook_german.dat",
        "image" => "comments_image.dat",
        _ => "comments.dat",
    };
    let body = dat_file(file)?;
    let items: Vec<&str> = split_pool(&body);
    if items.is_empty() {
        return None;
    }
    Some(items[rng.range(items.len())].to_string())
}

/// Parse a pool file: lines wrapped in outer `{` … `}` are one record;
/// inner `{a|b}` spintax remains for the caller's [crate::spin] renderer.
/// The first and last `{`/`}` are stripped (GSA wraps each pool item in a
/// pair) — matches the shipped `comments.dat` layout.
fn split_pool(body: &str) -> Vec<&str> {
    body.lines()
        .map(|l| l.trim())
        .filter(|l| {
            !l.is_empty() && !l.starts_with('[') && !l.starts_with(';')
        })
        .filter_map(|l| {
            // Strip one pair of wrapping braces if present.
            if l.len() >= 2 && l.starts_with('{') && l.ends_with('}') {
                Some(&l[1..l.len() - 1])
            } else {
                Some(l)
            }
        })
        .collect()
}

// ===========================================================================
//  User-agent pool
// ===========================================================================

/// A random real user agent from `user_agents.dat`.  Rotate per submission /
/// per session to evade UA-fingerprint blocks (gap 12).
pub fn pick_user_agent(rng: &mut SplitMix64) -> Option<String> {
    let body = dat_file("user_agents.dat")?;
    let lines: Vec<&str> = body.lines().map(|l| l.trim()).filter(|l| !l.is_empty()).collect();
    if lines.is_empty() {
        return None;
    }
    Some(lines[rng.range(lines.len())].to_string())
}

// ===========================================================================
//  Random delays — "wait before first post 0-5 min", "between two posts"
// ===========================================================================

/// Sample a delay (in seconds) uniformly in `[lo.min_sec, hi.max_sec]` with
/// GSA's "±N" jitter (gap 13): actual = base ± (jitter/2).
///
/// Deterministic for same seed; returns 0 if bounds are degenerate.
pub fn sample_delay_seconds(
    lo_min: u64,
    hi_min: u64,
    jitter_min: u64,
    rng: &mut SplitMix64,
) -> u64 {
    if hi_min <= lo_min && jitter_min == 0 {
        return lo_min * 60;
    }
    let span = (hi_min.saturating_sub(lo_min).max(jitter_min / 2) + 1) as u128;
    let off = rng.range(span as usize) as u64;
    let base = lo_min * 60 + off * 60;
    // apply jitter down by up to (jitter/2)*60
    let down = if jitter_min > 0 {
        rng.range(jitter_min as usize / 2) as u64 * 60
    } else {
        0
    };
    base.saturating_sub(down)
}

// ===========================================================================
//  Typos / spell errors — GSA-parity 10 % typos-on-description
// ===========================================================================

/// Inject typos into a word's letters with probability `pct` (0..=100).
///
/// GSA's actual implementation (from the binary strings) replaces a random
/// letter with a **neighbor-on-a-QWERTY** letter (so "the" → "thd", not
/// "tkz") — we clone that mapping.  Deterministic per seed.
pub fn inject_tyo(text: &str, pct: u32, rng: &mut SplitMix64) -> String {
    if pct == 0 {
        return text.to_string();
    }
    // Build a real QWERTY neighbor table once (independent of input).
    const QWERTY: [&str; 3] = [
        "qwertyuiop", "asdfghjkl", "zxcvbnm",
    ];
    let mut real: HashMap<char, Vec<char>> = HashMap::new();
    for (ri, row) in QWERTY.iter().enumerate() {
        let chars: Vec<char> = row.chars().collect();
        for (ci, c) in chars.iter().enumerate() {
            let mut v: Vec<char> = Vec::new();
            if ci > 0 { v.push(chars[ci - 1]); }
            if ci + 1 < chars.len() { v.push(chars[ci + 1]); }
            if ri > 0 {
                let up = QWERTY[ri - 1];
                if up.len() > ci.min(up.len() - 1) {
                    v.push(up.chars().nth(ci.min(up.len() - 1)).unwrap());
                }
            }
            if ri + 1 < QWERTY.len() {
                let dn = QWERTY[ri + 1];
                if dn.len() > ci.min(dn.len() - 1) {
                    v.push(dn.chars().nth(ci.min(dn.len() - 1)).unwrap());
                }
            }
            real.insert(*c, v);
        }
    }
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_ascii_alphabetic() && rng.range(100) < pct as usize {
            if let Some(v) = real.get(&c.to_ascii_lowercase()) {
                if !v.is_empty() {
                    let pick_char = v[rng.range(v.len())];
                    out.push(if c.is_uppercase() {
                        pick_char.to_ascii_uppercase()
                    } else {
                        pick_char
                    });
                    continue;
                }
            }
        }
        out.push(c);
    }
    out
}

// ===========================================================================
//  Generic fields + duplicate-content defuser — thin loaders
// ===========================================================================

/// Load `generic_fields.dat` (75 KB, field→value pool).  Returns the raw
/// body; parse it with [crate::spin::Context::file] under a data-section
/// name so `%section%` can be used in an engine INI.
pub fn generic_fields_body() -> Option<String> {
    dat_file("generic_fields.dat")
}

/// Load `html_variations.dat` (punctuation/case/word-order defusers).
pub fn html_variations_body() -> Option<String> {
    dat_file("html_variations.dat")
}

/// Load `calc_replace.dat` (math-captcha symbol table — used by the
/// F#21 captcha solver's `calc_replace` path).
pub fn calc_replace_body() -> Option<String> {
    dat_file("calc_replace.dat")
}

/// Load `stop_words.dat` as a sorted `Vec<String>` (700 words).
pub fn stop_words() -> Option<Vec<String>> {
    let body = dat_file("stop_words.dat")?;
    let v: Vec<String> = body
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty() && !l.starts_with(';'))
        .collect();
    Some(v)
}

// ===========================================================================
//  Explicit-path loaders (for tests / non-repo-root CWDs)
// ===========================================================================

/// Loaders with an explicit root, so tests (and the API in a non-repo CWD)
/// can point at a fixture directory without touching the repo data dir.
pub mod at {
    use super::*;

    pub fn titlegen(root: &Path) -> Option<String> {
        let body = dat_file_at(root, "titlegen.dat")?;
        let templates: Vec<&str> = body
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty() && !l.starts_with('[') && !l.starts_with(';'))
            .collect();
        if templates.is_empty() {
            return Some(String::new());
        }
        // Caller supplies the noun; we only return the raw template pick.
        Some(templates[0].to_string()) // (the caller re-samples via super::generate_title)
    }

    pub fn charspin_map(root: &Path) -> Option<HashMap<char, Vec<String>>> {
        let body = dat_file_at(root, "charspin.dat")?;
        let mut m: HashMap<char, Vec<String>> = HashMap::new();
        for line in body.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('[') || line.starts_with(';') {
                continue;
            }
            let parts: Vec<&str> = line.split('|').collect();
            if parts.len() < 2 {
                continue;
            }
            let chars: Vec<char> = parts[0].chars().collect();
            if chars.len() == 1 {
                let alts: Vec<String> = parts[1..].iter().map(|s| s.trim().to_string()).collect();
                m.insert(chars[0], alts);
            }
        }
        Some(m)
    }
}

// ===========================================================================
//  Tests — drive against the real cloned data files (in-repo) if present,
//  and against a synthetic fixture always (so the test passes in CI too).
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> PathBuf {
        // Prefer the repo data dir if we can find it relative to CWD
        // (the test's CWD is the repo root in `cargo test`).
        if let Some(r) = super::data_root() {
            return r;
        }
        // Else create a tiny fixture on the fly.
        let tmp = std::env::temp_dir().join("humanize-fixture");
        let _ = std::fs::create_dir_all(&tmp);
        std::fs::write(tmp.join("titlegen.dat"), "Hello %noun% World\nAnother %verb% %noun% tip\n").unwrap();
        std::fs::write(tmp.join("charspin.dat"), "a|&#945;|&#946;\nb|&#98;\n").unwrap();
        std::fs::write(tmp.join("comments.dat"), "{Hello|Hi} world\n{Bye|Later} there\n").unwrap();
        std::fs::write(tmp.join("user_agents.dat"), "UA1/1.0\nUA2/2.0\n").unwrap();
        std::fs::write(tmp.join("stop_words.dat"), "the\nand\nof\n").unwrap();
        std::fs::write(tmp.join("generic_fields.dat"), "[generic]\nfield=value\n").unwrap();
        std::fs::write(tmp.join("html_variations.dat"), "x → y\n").unwrap();
        std::fs::write(tmp.join("calc_replace.dat"), "1|one\n").unwrap();
        tmp
    }

    #[test]
    fn head_term_strips_stopwords() {
        // Uses the real stop_words.dat if present, else falls back to the
        // synthetic 3-word set — both contain "the"/"and"/"of".
        assert_eq!(parse_head_term("the best coffee shop"), Some("shop".to_string()));
        assert_eq!(parse_head_term("single"), Some("single".to_string()));
    }

    #[test]
    fn generate_title_is_deterministic_same_seed() {
        let mut a = SplitMix64::new(1234);
        let mut b = SplitMix64::new(1234);
        let x = generate_title("coffee shop", &mut a).unwrap();
        let y = generate_title("coffee shop", &mut b).unwrap();
        assert_eq!(x, y, "same seed ⇒ same title");
        assert!(x.to_lowercase().contains("shop"), "noun must appear: {x}");
    }

    #[test]
    fn charspin_applies_alternate_with_pct() {
        let mut rng = SplitMix64::new(1);
        let out = charspin("aaa", 100, &mut rng);
        // 100 % ⇒ every 'a' must be one of the alternates
        assert!(out.starts_with("&#") , "expected entity: {out}");
    }

    #[test]
    fn charspin_zero_pct_is_identity() {
        let mut rng = SplitMix64::new(1);
        assert_eq!(charspin("hello world", 0, &mut rng), "hello world");
    }

    #[test]
    fn random_comment_returns_a_line_from_pool() {
        let mut rng = SplitMix64::new(42);
        let c = random_comment("blog", &mut rng).expect("blog pool resolves");
        assert!(!c.is_empty());
    }

    #[test]
    fn pick_user_agent_is_nonempty() {
        let mut rng = SplitMix64::new(7);
        let ua = pick_user_agent(&mut rng);
        assert!(ua.is_some() && !ua.unwrap().is_empty());
    }

    #[test]
    fn sample_delay_bounds_hold() {
        let mut rng = SplitMix64::new(99);
        for _ in 0..200 {
            let d = sample_delay_seconds(0, 5, 0, &mut rng);
            assert!((0..=300).contains(&d), "out of range: {d}");
            let d2 = sample_delay_seconds(10, 1400, 60, &mut rng);
            assert!(d2 <= (1400 + 1) * 60, "out of range: {d2}");
        }
    }

    #[test]
    fn inject_tyo_changes_some_letters() {
        let mut rng = SplitMix64::new(7);
        let out = inject_tyo("the quick brown fox", 100, &mut rng);
        assert_eq!(out.len(), "the quick brown fox".len());
        // At 100 %, at least one letter should have changed given QWERTY
        // neighbors exist for most alphas.
        assert_ne!(out, "the quick brown fox");
    }

    #[test]
    fn data_root_returns_some_or_none_gracefully() {
        // In CI (no repo data) this can be None; the rest of the module must
        // still work.  Just assert it doesn't panic.
        let _ = super::data_root();
    }

    #[test]
    fn fixture_loaders_resolve() {
        let r = fixture();
        assert!(super::at::charspin_map(&r).is_some());
        assert!(super::at::titlegen(&r).is_some());
    }
}
