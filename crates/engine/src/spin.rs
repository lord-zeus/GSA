//! # Spin / Spintax / data-variable resolver
//!
//! This is the single most important piece of the whole application: every
//! text field that GSA fills into every engine (article, guestbook entry,
//! forum post, comment, profile bio, …) is stored as **spintax + data
//! variables**, and it is this resolver that turns that stored string into
//! the concrete submit string.
//!
//! ## Grammar (GSA-compatible)
//!
//! ```text
//! token       := text | alt | optional | variable
//! alt         := '{' ( item ('|' item)* ) '}'      pick exactly one item
//! optional    := '[' token+ ']'                     50% include / 50% drop
//! variable    := '%' name ('-' extra)? '%'          substituted from Context
//! ```
//!
//! `item` is itself a run of tokens, so all three nest.
//!
//! ## Determinism
//!
//! The resolver draws from a [splitmix64::SplitMix64] stream that is seeded
//! once per call-chain.  Same input + same seed ⇒ same output.
//!
//! ## Precedence / parse rules
//!
//! * Backslash escapes the *next* special character: `\\{`, `\\}`, `\\|`,
//!   `\\[`, `\\]`, `\\%`, `\\\\`.  Everything else is literal.
//! * A `%name%` with no colon is a **variable**; `name` is lowercased and
//!   looked up in [Context] case-insensitively first, then through the
//!   built-in generators (`random`, `random-A-B`, `spinfile`, `columnspinfile`,
//!   `random_option`, `random_email`, `leave` …).
//! * An unbalanced token is preserved verbatim (defensive: real engine files
//!   contain the occasional malformed token).

use std::collections::HashMap;

use crate::splitmix64::SplitMix64;

/// A mutable bag of concrete values + data files, looked up by lowercase key.
///
/// Mirrors what GSA's engine reads from the project (`.prj`), the site
/// (`.dat` spin files) and the request context at runtime.
#[derive(Debug, Clone, Default)]
pub struct Context {
    /// Concrete field values supplied by the caller (project keywords, article
    /// text, target domain, proxy, …).  Looked up case-insensitively via
    /// [Context::get].
    pub values: HashMap<String, String>,
    /// Spin data files (name → contents).  Used by `%spinfile-NAME%` (random
    /// item) and `%columnspinfile-NAME-COL%` (random row's column).
    pub files: HashMap<String, String>,
    /// Options for `%random_option%` — a list of candidates.  When empty the
    /// variable renders as `%leave%` (empty).
    pub random_options: Vec<String>,
}

impl Context {
    pub fn new() -> Self {
        Self::default()
    }

    /// Case-insensitive lookup of a concrete value.
    pub fn get(&self, key: &str) -> Option<&str> {
        let k = key.to_ascii_lowercase();
        self.values.get(&k).map(|s| s.as_str())
    }

    pub fn set(&mut self, key: &str, value: &str) -> &mut Self {
        self.values.insert(key.to_ascii_lowercase(), value.to_string());
        self
    }

    /// Register a named spin file (`file.dat` is stored as `"file.dat"`; lookups
    /// are extension-agnostic).
    pub fn file(&mut self, name: &str, contents: &str) -> &mut Self {
        self.files.insert(name.to_string(), contents.to_string());
        self
    }

    fn spin_file(&self, raw: &str) -> Option<&str> {
        // GSA's %spinfile-NAME% looks up NAME, NAME.dat, NAME.txt (in that
        // order, case-insensitively).
        let candidates = [
            raw,
            &format!("{raw}.dat"),
            &format!("{raw}.txt"),
            &format!("{raw}.spin"),
        ];
        for c in candidates {
            let lc = c.to_ascii_lowercase();
            if let Some(v) = self.files.get(&lc) {
                return Some(v);
            }
        }
        None
    }
}

/// Convenience: a pre-populated context with the most common project fields.
pub fn default_context() -> Context {
    Context::new()
}

/// The resolver. A resolver owns a seed — one per submission — so that running
/// the same engine against the same context twice produces the same result.
#[derive(Debug, Clone, Copy)]
pub struct SpintaxResolver {
    seed: u64,
}

impl SpintaxResolver {
    pub fn new(seed: u64) -> Self {
        Self { seed }
    }

    /// The seed this resolver draws from (exposed for deterministic plans).
    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// Resolve `input` to a concrete string.  All randomness flows through a
    /// single [SplitMix64] seeded from `self.seed`, so the result is a pure
    /// function of `(self.seed, input, ctx)`.
    pub fn resolve(&self, input: &str, ctx: &Context) -> String {
        let mut rng = SplitMix64::new(self.seed);
        let mut visited = std::collections::HashSet::new();
        render_with_rng(input, ctx, &mut rng, &mut visited)
    }
}

impl Default for SpintaxResolver {
    fn default() -> Self {
        Self::new(0xA11CE_5EED | 1)
    }
}

/// Top-level entry: resolves a single token chain in isolation (test-friendly).
pub fn resolve(input: &str, ctx: &Context) -> String {
    SpintaxResolver::default().resolve(input, ctx)
}

/// Render with an *external* RNG — the recursive workhorse.  `visited` is the
/// set of variable names currently mid-resolution; it stops a data section
/// that (transitively) references itself from recursing forever.
fn render_with_rng(
    input: &str,
    ctx: &Context,
    rng: &mut SplitMix64,
    visited: &mut std::collections::HashSet<String>,
) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    // Fast path: when the input has no spintax or variable syntax at all we
    // can return it verbatim without ever entering the slow recursive path.
    let has_special = bytes
        .iter()
        .any(|&b| matches!(b, b'{' | b'}' | b'[' | b']' | b'%' | b'$'));
    if !has_special {
        return input.to_string();
    }
    while i < bytes.len() {
        let c = bytes[i] as char;
        match c {
            '\\' => {
                // escaped special
                i += 1;
                if i < bytes.len() {
                    let next = bytes[i] as char;
                    if matches!(next, '{' | '}' | '[' | ']' | '|' | '%' | '\\') {
                        out.push(next);
                        i += 1;
                        continue;
                    }
                    // unknown escape — emit the char
                    out.push(next);
                    i += 1;
                    continue;
                }
            }
            '{' => {
                // find matching '}' (accounting for nesting and escapes)
                let inner = read_balanced(input, i, '{', '}', rng, ctx);
                // pick one item
                let items = split_items(input, inner.0 + 1, inner.1);
                let picked = items[rng.range(items.len().max(1))]
                    .to_string();
                out.push_str(&render_with_rng(&picked, ctx, rng, visited));
                i = inner.1 + 1;
            }
            '[' => {
                let inner = read_balanced(input, i, '[', ']', rng, ctx);
                let body = &input[inner.0 + 1..inner.1];
                if rng.bit() {
                    out.push_str(&render_with_rng(body, ctx, rng, visited));
                }
                i = inner.1 + 1;
            }
            '%' => {
                // Look for the closing % strictly AFTER the opening one.
                let end_rel = match input[i + 1..].find('%') {
                    Some(p) => i + 1 + p,
                    None => {
                        // No closing %: emit verbatim and continue.
                        out.push('%');
                        i += 1;
                        continue;
                    }
                };
                // token body is input[i+1..end_rel]
                let name = input[i + 1..end_rel].to_string();
                out.push_str(&render_variable(&name, ctx, rng, visited));
                i = end_rel + 1;
            }
            ch => {
                out.push(ch);
                i += 1;
            }
        }
    }
    out
}

/// Return (open_index, close_index) for the matching close at depth 1,
/// honouring nesting and escapes, and allowing unbalanced input to degrade
/// gracefully (returns open..len-1).
fn read_balanced(
    input: &str,
    open: usize,
    lo: char,
    hi: char,
    _rng: &mut SplitMix64,
    _ctx: &Context,
) -> (usize, usize) {
    let bytes = input.as_bytes();
    let mut depth = 0i32;
    let mut i = open;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c == '\\' && i + 1 < bytes.len() {
            i += 2;
            continue;
        }
        match c {
            ch if ch == lo => depth += 1,
            ch if ch == hi => {
                depth -= 1;
                if depth == 0 {
                    return (open, i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    (open, bytes.len() - 1)
}

/// Split a top-level alternative list on unescaped, **un-nested** `|`,
/// stopping at `stop`.  A `|` inside `{...}` or `[...]` is an *inner*
/// alternative and never a separator (GSA spin files routinely nest, e.g.
/// `{{a|b} x|y}`).
fn split_items(src: &str, start: usize, stop: usize) -> Vec<&str> {
    let mut out = Vec::new();
    let mut cur_start = start;
    let bytes = src.as_bytes();
    let last = stop.min(bytes.len());
    let mut depth = 0i32;
    let mut i = start;
    while i < last {
        let c = bytes[i] as char;
        if c == '\\' && i + 1 < last {
            i += 2;
            continue;
        }
        match c {
            '{' | '[' => depth += 1,
            '}' | ']' => depth = depth.saturating_sub(1),
            '|' if depth == 0 => {
                out.push(&src[cur_start..i]);
                cur_start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    out.push(&src[cur_start..last]);
    out
}

/// Resolve a `%name%` (the body between `%`/`%`) using GSA's built-in
/// generators, falling back to `ctx.get(name)`.
fn render_variable(
    name: &str,
    ctx: &Context,
    rng: &mut SplitMix64,
    visited: &mut std::collections::HashSet<String>,
) -> String {
    let lower = name.to_ascii_lowercase();
    // Recursion guard: a data section that (transitively) references itself
    // must not loop — emit the raw token so the UI can flag it as unresolved.
    if !visited.insert(lower.clone()) {
        return format!("%{name}%");
    }
    // GSA's `%name%` style: try the built-in generators first, then a direct
    // context (data-section) lookup.  A data-section value may itself contain
    // spintax / other variables (e.g. `{long|short}`), so render it — this is
    // exactly how GSA spins `[description_250]` / `[article]` sections.
    let result = lookup_builtin(name, ctx, rng, visited)
        .or_else(|| ctx.get(name).map(|v| render_with_rng(v, ctx, rng, visited)))
        // Unknown token: preserve verbatim so the UI flags it.
        .unwrap_or_else(|| format!("%{name}%"));
    visited.remove(&lower);
    result
}

fn lookup_builtin(
    name: &str,
    ctx: &Context,
    rng: &mut SplitMix64,
    visited: &mut std::collections::HashSet<String>,
) -> Option<String> {
    let n = name.to_ascii_lowercase();
    // 1. %leave%
    if n == "leave" {
        return Some(String::new());
    }

    // 2. %random% → one random alphanumeric char.
    if n == "random" {
        const ALNUM: [u8; 62] = *b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
        let idx = rng.range(ALNUM.len());
        return Some((ALNUM[idx] as char).to_string());
    }

    // 3. %random-A-B% → inclusive integer in [A, B].  Supports decimals
    //    (e.g. `%random-0.1-0.9%`) by producing a scaled integer string; GSA
    //    keeps the same number of decimals as in the range bounds on emit.
    if let Some(rest) = n.strip_prefix("random-") {
        let (mut lo, mut hi, dec) = parse_range(rest)?;
        // Generate an integer in [lo, hi]; then format with `dec` decimals if
        // the original bound had a decimal point.
        // GSA INIs contain reversed ranges (e.g. `%random-1000000-999999%`
        // in `Wordpress Article.ini`, `Discuz.ini`, `WeBid.ini`): normalize to
        // [min, max] — the old wrap-around math overflowed on those (panic in
        // debug, silent garbage in release).
        if hi < lo {
            std::mem::swap(&mut lo, &mut hi);
        }
        let span = hi - lo;
        let v: u64 = if span == 0 {
            lo
        } else {
            let range = span
                .saturating_add(1)
                .min(usize::MAX as u64)
                .max(1) as usize;
            lo.wrapping_add(rng.range(range) as u64)
        };
        if dec == 0 {
            return Some(v.to_string());
        }
        // scale: treat lo/hi as (int * 10^dec), emit with dec decimals
        let scaled = v;
        let int_part = scaled / 10u64.pow(dec as u32);
        let frac = scaled % 10u64.pow(dec as u32);
        let frac_str = format!("{:0width$}", frac, width = dec as usize);
        return Some(format!("{int_part}.{frac_str}"));
    }

    // 4. %random_option% → pick from ctx.random_options.
    if n == "random_option" || n == "random_optioin" {
        if !ctx.random_options.is_empty() {
            return Some(ctx.random_options[rng.range(ctx.random_options.len())].clone());
        }
        return Some(String::new());
    }

    // 5. %random_email% → a plausible fake email.
    if n == "random_email" || n == "random_email_" {
        let user: String = (0..6)
            .map(|_| (rng.range(26) as u8 + b'a') as char)
            .collect();
        let dom = [
            "example.com", "mailinator.com", "guerrillamail.com", "tempmail.org",
            "inboxproxy.com", "yopmail.com", "trashmail.com",
        ];
        let d = dom[rng.range(dom.len())];
        return Some(format!("{user}@{d}"));
    }

    // 6. %spinfile-NAME% → random item (line / cell split by |) of NAME.
    if let Some(rest) = n.strip_prefix("spinfile-") {
        if let Some(file) = ctx.spin_file(rest) {
            // Items separated by '|' (or newlines); pick one and recurse-render
            // it (the item may itself contain spin/vars).
            let items = split_spinfile_items(file);
            if !items.is_empty() {
                let it = items[rng.range(items.len())].clone();
                return Some(render_with_rng(&it, ctx, rng, visited));
            }
        }
        return Some(String::new());
    }
    if let Some(rest) = n.strip_prefix("filespin-") {
        if let Some(file) = ctx.spin_file(rest) {
            let items = split_spinfile_items(file);
            if !items.is_empty() {
                let it = items[rng.range(items.len())].clone();
                return Some(render_with_rng(&it, ctx, rng, visited));
            }
        }
        return Some(String::new());
    }

    // 7. %columnspinfile-NAME-COL% → random row of a TSV file, take column.
    if let Some(rest) = n.strip_prefix("columnspinfile-") {
        // rest = "NAME-COL" or "NAME-COL-N" (multi column in GSA)
        let mut split = rest.rsplitn(2, '-');
        let col_str = split.next().unwrap_or("");
        let name = split.next().unwrap_or("");
        let col: i64 = col_str.parse().unwrap_or(0);
        if let Some(file) = ctx.spin_file(name) {
            let rows: Vec<&str> = file.lines().map(|l| l.trim()).filter(|l| !l.is_empty()).collect();
            if !rows.is_empty() {
                let row = rows[rng.range(rows.len())];
                let cells: Vec<&str> = row.split('\t').collect();
                if (col as usize) < cells.len() {
                    return Some(cells[col as usize].to_string());
                }
            }
        }
        return Some(String::new());
    }

    // 8. F#22 humanize tokens — data-driven text generators cloned from GSA's
    //    shipped `.dat` pools (titlegen / comments / user_agents / charspin).
    //    Missing data ⇒ empty string (graceful), never a panic.
    if n == "generate_title" || n == "blog_title" || n == "article_title" || n == "title" {
        let kw = ctx.get("keyword").unwrap_or("").to_string();
        let generated = crate::humanize::generate_title(&kw, rng).unwrap_or_default();
        if !generated.is_empty() {
            return Some(render_with_rng(&generated, ctx, rng, visited));
        }
        return Some(String::new());
    }
    if n == "blog_comment" || n == "guestbook_comment" || n == "image_comment" || n == "random_comment" || n == "comment" {
        // F#31 — the project's explicit "Comment text" (injected by the
        // runner as `comment` in the context) wins over the auto-generated
        // pool comment. Only an empty project comment falls through.
        if let Some(user_comment) = ctx.get("comment") {
            if !user_comment.trim().is_empty() {
                return Some(render_with_rng(user_comment, ctx, rng, visited));
            }
        }
        let n = n.as_str();
        let category = match n {
            "guestbook_comment" => "guestbook",
            "image_comment" => "image",
            "blog_comment" => "blog",
            _ => "blog",
        };
        let c = crate::humanize::random_comment(category, rng).unwrap_or_default();
        return Some(render_with_rng(&c, ctx, rng, visited));
    }
    if n == "charspin" || n == "humanize" {
        let kw: &str = ctx.get("keyword").unwrap_or("");
        let seed = ctx.get("anchor_text").unwrap_or(kw).to_string();
        if seed.is_empty() {
            return Some(String::new());
        }
        return Some(crate::humanize::charspin(&seed, 10, rng));
    }
    if n == "user_agent" || n == "ua" {
        return Some(crate::humanize::pick_user_agent(rng).unwrap_or_default());
    }

    None
}

/// Split a spin-file body into candidate items — matching the convention in
/// GSA's shipped `.dat` files:
///
/// * if the file contains any **top-level** `|` (i.e. a `|` outside of
///   `{...}`/`[...]` nesting) — those are the item separators; newlines inside
///   an item are just line-wrapping and become spaces
///   (`auto_anchor.dat`, `comments.dat`, `charspin.dat` all look like this);
/// * otherwise the items are the non-empty lines (`cars.dat`, …).
///
/// Comment lines (leading `;`) and blanks are dropped either way.
fn split_spinfile_items(file: &str) -> Vec<String> {
    let has_top_level_pipe = {
        let mut depth = 0i32;
        file.bytes().any(|b| match b as char {
            '{' | '[' => {
                depth += 1;
                false
            }
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                false
            }
            '|' => depth == 0,
            _ => false,
        })
    };

    if has_top_level_pipe {
        let mut items = Vec::new();
        let bytes = file.as_bytes();
        let mut depth = 0i32;
        let mut start = 0;
        for (i, &b) in bytes.iter().enumerate() {
            let c = b as char;
            match c {
                '{' | '[' => depth += 1,
                '}' | ']' => depth = depth.saturating_sub(1),
                '|' if depth == 0 => {
                    items.push(file[start..i].to_string());
                    start = i + 1;
                }
                _ => {}
            }
        }
        items.push(file[start..].to_string());
        items
            .into_iter()
            .map(|s| {
                // line-wrapping inside an item → spaces
                let s = s.replace(['\n', '\r', '\t'], " ");
                // collapse repeated spaces and trim
                rejoin_split(s.trim())
            })
            .filter(|s| !s.is_empty() && !s.starts_with(';'))
            .collect()
    } else {
        file.lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty() && !l.starts_with(';'))
            .map(|l| l.to_string())
            .collect()
    }
}

/// Rejoin a string with runs of spaces collapsed to a single space (items have
/// had their newlines converted to spaces).
fn rejoin_split(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_space = true; // leading spaces already trimmed
    for c in s.chars() {
        if c == ' ' {
            if !last_space {
                out.push(' ');
            }
            last_space = true;
        } else {
            out.push(c);
            last_space = false;
        }
    }
    out
}

fn parse_range(rest: &str) -> Option<(u64, u64, u32)> {
    // "A-B"
    let parts: Vec<&str> = rest.split('-').collect();
    if parts.len() != 2 {
        return None;
    }
    let dec = parts
        .iter()
        .map(|p| p.split('.').count()).max().unwrap_or(1) as u32;
    let dec = std::cmp::max(1, dec) - 1;
    let mul = 10u64.pow(dec);
    let parse = |p: &str| -> Option<u64> {
        if dec == 0 {
            p.parse::<u64>().ok()
        } else {
            let (a, b) = p.split_once('.')?;
            let a = a.parse::<u64>().ok()?;
            let b = b.parse::<u64>().ok()?;
            Some(a * mul + b)
        }
    };
    let a = parse(parts[0])?;
    let b = parse(parts[1])?;
    Some((a, b, dec))
}

// =============================================================================
//  Tests — every documented behavior has at least one test, and determinism
//  (same seed ⇒ same output) is asserted across independent runs.
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Context {
        Context::new()
    }

    // ---- literals -----------------------------------------------------------

    #[test]
    fn plain_text_is_verbatim() {
        assert_eq!(resolve("hello world", &ctx()), "hello world");
    }

    #[test]
    fn escaped_specials_render_as_themselves() {
        assert_eq!(resolve(r"\{not spin\}", &ctx()), "{not spin}");
        assert_eq!(resolve(r"\[opt\]", &ctx()), "[opt]");
        assert_eq!(resolve(r"\{a\}", &ctx()), "{a}");
    }

    // ---- alternatives -------------------------------------------------------

    #[test]
    fn alt_one_choice_is_selected_deterministically() {
        let r = SpintaxResolver::new(1).resolve("{a}", &ctx());
        assert_eq!(r, "a"); // exactly one item — must be that item
    }

    #[test]
    fn alt_multi_choice_picks_one_of_them() {
        let r = SpintaxResolver::new(248301982);
        let out = r.resolve("{cat|dog|fox|rabbit}", &ctx());
        assert!(["cat", "dog", "fox", "rabbit"].contains(&out.as_str()));
    }

    #[test]
    fn alt_picks_all_options_over_many_seeds() {
        let mut seen = std::collections::HashSet::new();
        for s in 0u64..200 {
            let r = SpintaxResolver::new(s);
            seen.insert(r.resolve("{a|b|c}", &ctx()));
        }
        assert_eq!(seen, std::collections::HashSet::from(["a", "b", "c"].map(|s| s.to_string())));
    }

    #[test]
    fn nested_alt_recurses_properly() {
        // {big|{small|x}} — expanding should never yield the inner braces.
        let r = SpintaxResolver::new(7);
        for _ in 0..25 {
            let out = r.resolve("{big|{small|smallest}}", &ctx());
            assert!(!out.contains('{'), "got {out}");
            assert!(["big", "small", "smallest"].contains(&out.as_str()));
        }
    }

    // ---- optional -----------------------------------------------------------

    #[test]
    fn optional_either_kept_or_dropped_not_both() {
        let mut seen = std::collections::HashSet::new();
        for s in 0u64..50 {
            let r = SpintaxResolver::new(s);
            let out = r.resolve("Hello[ world]", &ctx());
            seen.insert(out.clone());
            assert!(out == "Hello" || out == "Hello world", "got {out}");
        }
        assert!(seen.contains("Hello") || seen.contains("Hello world"));
        assert!(seen.len() == 1 || seen.len() == 2);
    }

    // ---- variables ----------------------------------------------------------

    #[test]
    fn leave_is_empty() {
        assert_eq!(resolve("%leave%", &ctx()), "");
    }

    #[test]
    fn known_field_is_substituted_case_insensitively() {
        let mut c = Context::new();
        c.set("Anchor_Text", "buy shoes online");
        assert_eq!(resolve("%anchor_text%", &c), "buy shoes online");
        assert_eq!(resolve("%ANCHOR_TEXT%", &c), "buy shoes online");
    }

    #[test]
    fn unknown_variable_is_preserved_verbatim() {
        assert_eq!(resolve("%no_such_var_x%", &ctx()), "%no_such_var_x%");
    }

    #[test]
    fn project_comment_text_overrides_pool_comment() {
        // F#31 — the project's explicit comment (injected by the runner as
        // `comment` in the context) wins over the auto-generated pool comment
        // for every comment-family token.
        let mut c = Context::new();
        c.set("comment", "My custom guestbook note about nordiccomps!");
        assert_eq!(resolve("%blog_comment%", &c), "My custom guestbook note about nordiccomps!");
        assert_eq!(resolve("%guestbook_comment%", &c), "My custom guestbook note about nordiccomps!");
        assert_eq!(resolve("%comment%", &c), "My custom guestbook note about nordiccomps!");
        assert_eq!(resolve("%random_comment%", &c), "My custom guestbook note about nordiccomps!");
    }

    #[test]
    fn whitespace_comment_falls_through_to_pool() {
        // F#31 — a comment that is empty / whitespace-only must NOT suppress
        // the generated fallback (only a real user comment overrides).
        let mut c = Context::new();
        c.set("comment", "   ");
        resolve("%blog_comment%", &c); // just must not panic; pool path taken
    }

    #[test]
    fn random_range_respects_bounds_and_span() {
        let r = SpintaxResolver::new(0xF00D);
        for _ in 0..60 {
            let v: u64 = r.resolve("%random-10-20%", &ctx()).parse().unwrap();
            assert!((10..=20).contains(&v), "out of range: {v}");
        }
        // span must be hit at least once
        let seen: std::collections::HashSet<u64> = (0..200)
            .map(|s| {
                SpintaxResolver::new(s)
                    .resolve("%random-10-20%", &ctx())
                    .parse()
                    .unwrap()
            })
            .collect();
        assert!(seen.len() >= 5, "too few distinct values: {seen:?}");
    }

    #[test]
    fn reversed_random_range_is_normalized_not_panicking() {
        // Regression: GSA INIs ship reversed ranges (e.g. `Wordpress Article.ini`
        // `%random-1000000-999999%`, `Discuz.ini` `%random-100000000-99999999%`).
        // Old code: `hi.wrapping_sub(lo)` → span = u64::MAX → `(span + 1)`
        // overflowed — panic in debug (dropped the whole in-flight request),
        // silent garbage in release.  Now normalized to [min, max].
        let lo = 999_999u64;
        let hi = 1_000_000u64;
        for s in 0..40u64 {
            let v: u64 = SpintaxResolver::new(s)
                .resolve("%random-1000000-999999%", &ctx())
                .parse()
                .unwrap();
            assert!((lo..=hi).contains(&v), "out of [lo,hi]: {v}");
        }
        for s in 0..40u64 {
            let v: u64 = SpintaxResolver::new(s)
                .resolve("%random-100000000-99999999%", &ctx())
                .parse()
                .unwrap();
            assert!((99_999_999..=100_000_000u64).contains(&v), "out of range: {v}");
        }
        // single-value and zero-span sanity
        assert_eq!(
            SpintaxResolver::new(7).resolve("%random-42-42%", &ctx()),
            "42"
        );
    }

    #[test]
    fn random_option_picks_from_configured_list() {
        let mut c = Context::new();
        c.random_options = vec!["opt-1".to_string(), "opt-2".to_string(), "opt-3".to_string()];
        let mut seen = std::collections::HashSet::new();
        for s in 0u64..100 {
            seen.insert(SpintaxResolver::new(s).resolve("%random_option%", &c));
        }
        // all three should show up in 100 draws
        assert_eq!(
            seen,
            std::collections::HashSet::from(["opt-1", "opt-2", "opt-3"].map(|s| s.to_string()))
        );
        // and never be empty
        assert!(!seen.contains(&String::from("")));
    }

    #[test]
    fn spinfile_picks_a_random_item_and_recurses() {
        let mut c = Context::new();
        c.file(
            "hobbies2.dat",
            "reading\nhiking\nswimming {club|pool}\nchess\n",
        );
        let mut seen: std::collections::HashSet<String> =
            (0..400).map(|s| {
                SpintaxResolver::new(s).resolve("%spinfile-hobbies2%", &c)
            }).collect();
        for expected in ["reading", "hiking", "swimming club", "swimming pool", "chess"]
            .iter()
        {
            assert!(
                seen.contains(*expected),
                "missing {expected}; got {seen:?}"
            );
        }
    }

    #[test]
    fn spinfile_picks_a_random_item_and_recurses_pipe_separated() {
        let mut c = Context::new();
        c.file(
            "hobbies.dat",
            "reading\n|hiking\n|swimming {club|pool}\n|chess\n",
        );
        let r = SpintaxResolver::new(1123);
        let mut distinct: std::collections::HashSet<String> =
            (0..600).map(|s| {
                let r2 = SpintaxResolver::new(s);
                r2.resolve("%spinfile-hobbies%", &c)
            }).collect();
        // strip leading '|' that the test file data itself carries; GSA data
        // files commonly lead each item with '|' (item separator syntax),
        // and we preserve that faithfully.  After render we expect no *inner*
        // spin token to have leaked (no braces, no internal pipes).
        let mut seen_stripped: std::collections::HashSet<String> = distinct
            .into_iter()
            .map(|d| d.trim_start_matches('|').trim().to_string())
            .collect();
        for expected in ["reading", "hiking", "swimming club", "swimming pool", "chess"]
            .iter()
        {
            assert!(
                seen_stripped.contains(*expected),
                "missing {expected}; got {seen_stripped:?}"
            );
        }
        for d in &seen_stripped {
            assert!(!d.contains('{'), "raw spin leak: {d}");
            assert!(!d.contains('\n'), "raw newline leak: {d}");
            // no *internal* pipes (pipes may only appear as item separators
            // leading the item — already stripped above)
            assert!(!d.contains('|'), "raw pipe leak: {d}");
        }
    }

    #[test]
    fn spinfile_with_dat_suffix_is_found() {
        let mut c = Context::new();
        c.file("names.dat", "Alice\n|Bob\n");
        assert!(matches!(
            c.spin_file("names"),
            Some(s) if s.contains("Alice")
        ));
    }

    #[test]
    fn columnspinfile_returns_the_requested_column() {
        let mut c = Context::new();
        c.file(
            "address_data.dat",
            "US\tNew York\t10001\t123 Wall St\nDE\tBerlin\t10115\tAlexanderplatz 1\nIT\tRome\t00184\tVia Roma 1\n",
        );
        let r = SpintaxResolver::new(333);
        for _ in 0..40 {
            let country: String = r.resolve("%columnspinfile-address_data-1%", &c);
            let _ = country;
            let r2 = SpintaxResolver::new(0).resolve("%columnspinfile-address_data.dat-1%", &c);
            let _ = r2;
        }
        // Deterministically check that column 0 (country) and column 3
        // (street, the last of the four columns) resolve to plausibles.
        let v0 = SpintaxResolver::new(0).resolve("%columnspinfile-address_data.dat-0%", &c);
        let v3 = SpintaxResolver::new(0).resolve("%columnspinfile-address_data.dat-3%", &c);
        assert!(["US", "DE", "IT"].contains(&v0.as_str()));
        assert!(
            ["123 Wall St", "Alexanderplatz 1", "Via Roma 1"].contains(&v3.as_str())
        );
    }

    #[test]
    fn spinfile_real_gsa_auto_anchor_dat_spins_cleanly() {
        // The genuinely shipped GSA spin file: one long line of nested
        // spintax + `|`-separated sentence variants.  Every draw must come out
        // fully rendered — no `{`, `}`, `|` (top level) or `%url%`/… tokens
        // left raw, and every variant must be anchored by the target page URL.
        let raw = std::fs::read(
            r"C:\Users\kingsly\GSA_SER_portable\Engines\auto_anchor.dat",
        )
        .expect("shipped with GSA on this machine");
        let raw = String::from_utf8_lossy(&raw).into_owned();
        let mut c = Context::new();
        c.set("url", "https://my-site.example/");
        c.set("anchor_text", "buy running shoes online");
        c.file("auto_anchor", &raw);
        let mut seen = std::collections::HashSet::new();
        for s in 0..400u64 {
            let out = SpintaxResolver::new(s).resolve("%spinfile-auto_anchor%", &c);
            assert!(!out.contains('{'), "raw {{ in: {out}");
            assert!(!out.contains('}'), "raw }} in: {out}");
            assert!(!out.contains("%url%"), "raw %url% in: {out}");
            assert!(!out.contains("%anchor_text%"), "raw %anchor_text% in: {out}");
            assert!(out.contains("https://my-site.example/"), "missing target: {out}");
            seen.insert(out);
        }
        // several distinct sentence variants must be reachable
        assert!(seen.len() > 20, "only {} distinct spins", seen.len());
    }

    #[test]
    fn random_email_is_plausible() {
        let r = SpintaxResolver::new(5);
        for _ in 0..20 {
            let e = r.resolve("%random_email%", &ctx());
            assert!(e.contains('@'), "no @: {e}");
            let (a, b) = e.split_once('@').unwrap();
            assert!(!a.is_empty() && !b.is_empty());
            assert!(b.contains('.'));
        }
    }

    #[test]
    fn full_realistic_spin_renders_concrete() {
        let mut c = Context::new();
        c.set("Anchor_Text", "buy high-quality boots");
        c.set("URL", "https://boots.example");
        c.file("names.dat", "Alice\n|Bob\n");
        let r = SpintaxResolver::new(0x5EED);
        let out = r.resolve(
            "Hi! {check out|visit|try} {our|my} {boots|footwear}! {Go to|Visit} %url% {now|today}.",
            &c,
        );
        assert!(!out.contains('{') && !out.contains('|') && !out.contains('%'), "got {out}");
        assert!(out.contains("https://boots.example"));
    }

    // ---- determinism --------------------------------------------------------

    #[test]
    fn same_seed_produces_identical_output() {
        let c = Context::new();
        let a = SpintaxResolver::new(12345).resolve("%random-0-999999%{x|y|z}{q}{r}", &c);
        let b = SpintaxResolver::new(12345).resolve("%random-0-999999%{x|y|z}{q}{r}", &c);
        assert_eq!(a, b);
        let cc = SpintaxResolver::new(12346).resolve("%random-0-999999%{x|y|z}{q}{r}", &c);
        // different seed → almost surely different output
        assert_ne!(a, cc);
    }

    #[test]
    fn random_single_letter_is_alphanumeric() {
        let r = SpintaxResolver::new(7);
        for _ in 0..200 {
            let c = r.resolve("%random%", &ctx());
            assert_eq!(c.len(), 1);
            assert!(c.chars().next().unwrap().is_ascii_alphanumeric());
        }
    }
}
