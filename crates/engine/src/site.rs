//! # Site finding — GSA "Filter URLs / find new sites" pipeline.
//!
//! GSA doesn't pick search engines out of a thin air.  It loads a
//! **2,525-site catalog** from `se.dat` — every entry is an INI-ish spec
//! (`[Name]` + `key=value` lines) declaring:
//!   * `site:` / `intitle:` / `inurl:` prefix words,
//!   * `country` / `countries` / `languages` filters,
//!   * `start page` / `inc page` / `max page` pagination,
//!   * `url_front` / `url_back` / `seperator` (result extraction anchors),
//!   * `banned` and `wrong user agent` response matchers,
//!   * `wait between search` pacing.
//!
//! The "Find new sites" button in GSA then:
//!   1. builds a **search query** from the target keyword + optional
//!      `site:`/`intitle:`/`inurl:` modes + optional quotes + optional
//!      stop-word sampling (GSA's "Add stop words to query with 20%"),
//!   2. for each catalog entry runs `navigate(url_after_substitutions)`
//!      and `extract(url, front/back, ...)` on the HTML,
//!   3. filters the extracted URLs by country/language/domain/min-keywords,
//!      avoids duplicate IPs, dedupes by canonical URL, and caps the total,
//!   4. stores them as `found_sites` rows to be submitted-to later.
//!
//! This module is GSA-parity **in spirit** — we clone the concept (the
//! catalog + query + extraction + filter pipeline) but implement it with
//! our own `Driver` and our existing `extraction` primitives, no `.NET`
//! copied.  All functions are pure / deterministic (seeded RNG where
//! needed, no side effects apart from driver I/O), and unit-tested.
//!
//! ## Data
//! Data lives in `data/gsa/se.dat` (cloned verbatim from the GSA install;
//! see `humanize::data_root()` for the CWD-walk-up discovery).
//!
//! ## Public API
//! ```ignore
//! let sites = site::load_site_catalog()?;          // 2525 entries
//! let cfg  = SiteFindConfig { keyword: "coffee shop".into(), .. };
//! let hits = site::find_sites(cfg, driver, sites).await;
//! ```

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde::Serialize;

use crate::driver::{Driver, DriverResponse};
use crate::extraction::{extract_front_back_variants, find_url};
use crate::splitmix64::SplitMix64;
use serde::Deserialize;

// ===========================================================================
//  1. Catalog data model  (one `se.dat` entry)
// ===========================================================================

/// One search-engine / site entry from `se.dat`.
///
/// We map only the **subset of ~40 fields** that the site-finding pipeline
/// actually uses into first-class struct fields, and keep the rest in
/// `extras` (raw `key -> value`) so future features (F#24 linkwheel, F#25
/// v19 catalog) can reach in without a schema migration.
pub struct SiteSpec {
    /// `[Name]` — GSA's display name ("Google", "Bing", "iZito", ...).
    pub name: String,

    /// `desc=` / `description=` — free-form search result label.
    pub desc: String,

    /// `enabled=1/0` — GSA's per-site on/off flag (Google has it, `4get`
    /// entries don't). Default = enabled when absent (GSA behaves this way).
    pub enabled: bool,

    /// `country=` — human name ("Argentina", "International").
    pub country: String,

    /// `countries=` — ISO code list "ar,au,at,bg" (comma-separated in GSA).
    pub countries: Vec<String>,

    /// `languages=` — comma-separated two-letter codes "en,es,de".
    pub languages: Vec<String>,

    /// `default language=` / `default langauge=` — GSA misspells both;
    /// the first (correct) form we map into `language_default`.
    pub language_default: String,

    /// `search_url=` or `url=` — the full search URL template.  GSA calls
    /// it `search_url` in v19+ and `url` in v18.  Both supported.
    ///
    /// GSA substitutes `%search%` (the keyword), `%page%` (offset, not
    /// number), `%domain%` (target site if we're running in `site:` mode),
    /// and `%secret%` (per-site rotating hash) — all case-insensitive.
    pub url: String,

    /// `site=site:` — the string we prepend when in Site mode (GSA stores
    /// the actual *word*; e.g. `site=site:` → mode prefix is `"site:"`).
    /// Empty = GSA didn't declare it for this site → Site mode is a no-op.
    pub site_prefix: String,

    /// `intitle=someprefix` — ditto for Intitle mode.
    pub intitle_prefix: String,

    /// `inurl=` — ditto for Inurl mode.
    pub inurl_prefix: String,

    /// `start page=0` — first-page offset.
    pub start_page: u32,

    /// `inc page=10` — offset delta per page.
    pub inc_page: u32,

    /// `max page=` — offset cap (0 = "unlimited" in GSA).
    pub max_page: u32,

    /// `more results parameter=...` / `more results value=...` — the URL
    /// param + allowed values that GSA uses to request more results per
    /// page (e.g. Google `num=10,20,...100`).  We keep both strings; the
    /// "pick a value" step is caller-level.
    pub more_results_param: String,
    pub more_results_values: Vec<String>,

    /// `wait between search=N` — pacing (ms per GSA, seconds in ours — see
    /// note in runner). We store raw integer and let the runner scale.
    pub wait_between_search: u32,

    /// `wait between special search=N` — pacing for `site:`/`intitle:`
    /// mode (GSA slows down on these).
    pub wait_special_search: u32,

    /// `extract by tag=` — CSS-ish tag selector (e.g. `h3:class=?*`).  We
    /// parse the "element class" pattern as a fallback when `url_front`
    /// isn't set.
    pub extract_by_tag: String,

    /// `url_front=` / `url_front1=` / `url_front2=` — extraction anchors.
    pub url_fronts: Vec<String>,

    /// `url_back=` / `url_back2=` — extraction stoppers (empty = "to end").
    pub url_backs: Vec<String>,

    /// `seperator=` / `seprator=` (GSA's two spellings) — the marker that
    /// *precedes* each result anchor; when present we slice on it and
    /// extract a URL within each slice (Google uses this for its
    /// JSON-heavy pages).
    pub seperator: String,

    /// `title_front=` / `title_front1=` ... — same idea but for the title.
    pub title_fronts: Vec<String>,
    pub title_backs: Vec<String>,

    /// `banned=` — pipe-separated response phrases that mean "blocked".
    pub banned: Vec<String>,

    /// `ignore mask=` — pipe-separated href substrings to drop (e.g.
    /// `googleusercontent|maps.google`).
    pub ignore_mask: Vec<String>,

    /// `follow redirect=` — `!/enablejs` → skip any URL containing "enablejs"
    /// (GSA's syntax is `!pattern` = "not" when present).
    pub follow_redirect: String,

    /// `user agent=` / `user_agent=` — UA hint; we keep the raw string
    /// (e.g. `"internal"` = "use our default UA pool").
    pub user_agent: String,

    /// `referrer=` + `download referrer=1` — the referrer header to send.
    pub referrer: String,
    pub download_referrer: bool,

    /// `submit form=` / `post data=` — GSA form-POST spec (rare for
    /// search engines, but present).  Empty = plain GET.
    pub submit_form_url: String,
    pub submit_form_data: String,

    /// `secret1_front=` / `secret1_back=` — GSA's hash-token extraction
    /// (used when a site embeds a one-time token in the search URL; e.g.
    /// iZito).  We extract the token from the search page, then inject it
    /// into the *next* navigation.
    pub secret_fronts: Vec<String>,
    pub secret_backs: Vec<String>,

    /// `set cookies=` — raw "k=v; k2=v2" (GSA uses this to prime the
    /// session before the search GET).
    pub set_cookies: String,

    /// `default=true` — GSA marks one site as the "default" fallback.
    pub is_default: bool,

    /// Everything else — raw key/value pairs not mapped above (for
    /// future F#24/F#25 use, e.g. `token_*`, `value_*`, `appid_*`).
    pub extras: HashMap<String, String>,
}


impl Default for SiteSpec {
    fn default() -> Self {
        Self {
            name: String::new(),
            desc: String::new(),
            enabled: true,
            country: String::new(),
            countries: Vec::new(),
            languages: Vec::new(),
            language_default: String::new(),
            url: String::new(),
            site_prefix: String::new(),
            intitle_prefix: String::new(),
            inurl_prefix: String::new(),
            start_page: 0,
            inc_page: 10,
            max_page: 0,
            more_results_param: String::new(),
            more_results_values: Vec::new(),
            wait_between_search: 30,
            wait_special_search: 60,
            extract_by_tag: String::new(),
            url_fronts: Vec::new(),
            url_backs: Vec::new(),
            seperator: String::new(),
            title_fronts: Vec::new(),
            title_backs: Vec::new(),
            banned: Vec::new(),
            ignore_mask: Vec::new(),
            follow_redirect: String::new(),
            user_agent: String::new(),
            referrer: String::new(),
            download_referrer: false,
            submit_form_url: String::new(),
            submit_form_data: String::new(),
            secret_fronts: Vec::new(),
            secret_backs: Vec::new(),
            set_cookies: String::new(),
            is_default: false,
            extras: HashMap::new(),
        }
    }
}

/// Split a comma-or-pipe delimited GSA value into a clean `Vec<String>`.
fn split_value(v: &str) -> Vec<String> {
    v.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Parse the INI-ish `se.dat`.  GSA's real file uses:
///   `[Name]`              — entry start
///   `key=value`           — one field per line (spaces around `=` allowed)
///   `; comment`           — GSA uses `;` (not `#`) for comments
///   blank lines           — separators
///   `key=v1|v2`           — multi-value fields (e.g. `banned=...|...`)
///
/// GSA's key names are **space-separated** (`more results parameter`).  We
/// preserve them verbatim into `extras` for unmapped ones.
pub fn parse_se_dat(text: &'_ str) -> Vec<SiteSpec> {
    let mut out: Vec<SiteSpec> = Vec::new();
    let mut cur: Option<SiteSpec> = None;

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }

        // Entry header: `[Name]` — close the previous, open a new one.
        if let Some(inner) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            if let Some(prev) = cur.take() {
                out.push(prev);
            }
            cur = Some(SiteSpec {
                name: inner.trim().to_string(),
                ..Default::default()
            });
            continue;
        }

        let (key, value) = match line.split_once('=') {
            Some((k, v)) => (k.trim().to_string(), v.trim().to_string()),
            None => continue, // stray non-key line — skip
        };

        if let Some(s) = cur.as_mut() {
            apply_field(&key, &value, s);
        }
    }
    if let Some(prev) = cur.take() {
        out.push(prev);
    }
    out
}

/// Map a raw `key=value` into the right struct field.  Unknown keys land
/// in `extras` (so nothing is silently dropped).
fn apply_field(key: &str, value: &str, s: &mut SiteSpec) {
    match key {
        "desc" | "description" => s.desc = value.to_string(),
        "enabled" => s.enabled = matches!(value, "1" | "true" | "yes" | "on"),
        "country" => s.country = value.to_string(),
        "countries" => s.countries = split_value(value),
        "languages" => s.languages = split_value(value),
        "default language" | "default langauge" => s.language_default = value.to_string(),
        // GSA v18 uses `url=`, v19 uses `search_url=`.  Accept both.
        "url" | "search_url" | "search url" => s.url = value.to_string(),
        "site" => s.site_prefix = value.to_string(),
        "intitle" => s.intitle_prefix = value.to_string(),
        "inurl" => s.inurl_prefix = value.to_string(),
        "start page" => s.start_page = value.parse().unwrap_or(0),
        "inc page" => s.inc_page = value.parse().unwrap_or(10),
        "max page" => s.max_page = value.parse().unwrap_or(0),
        "more results parameter" => s.more_results_param = value.to_string(),
        "more results value" => s.more_results_values = split_value(value),
        "wait between search" => s.wait_between_search = value.parse().unwrap_or(30),
        "wait between special search" => s.wait_special_search = value.parse().unwrap_or(60),
        "extract by tag" => s.extract_by_tag = value.to_string(),
        "seperator" | "seprator" => s.seperator = value.to_string(),
        "follow redirect" => s.follow_redirect = value.to_string(),
        "user agent" | "user_agent" => s.user_agent = value.to_string(),
        "referrer" => s.referrer = value.to_string(),
        "download referrer" => s.download_referrer = matches!(value, "1" | "true" | "yes"),
        "submit form" => s.submit_form_url = value.to_string(),
        "post data" => s.submit_form_data = value.to_string(),
        "set cookies" => s.set_cookies = value.to_string(),
        "default" => s.is_default = matches!(value, "1" | "true" | "yes"),

        // Multi-front/back extraction ladders — GSA allows `url_front1`,
        // `url_front2`, ... as a fallback chain.  We collect them as we go.
        k if k.starts_with("url_front") => {
            // url_front / url_front1 / url_front2 — a fallback ladder.
            // Slot 0 is `url_front` (no digit); every other is pushed in
            // order.  We store them all so the extractor can walk the chain.
            let num = k["url_front".len()..]
                .chars()
                .next()
                .and_then(|c| c.to_digit(10));
            if num == None || num == Some(0) {
                s.url_fronts.insert(0, value.to_string());
            } else {
                s.url_fronts.push(value.to_string());
            }
        }
        k if k.starts_with("url_back") => {
            let num = k["url_back".len()..]
                .chars()
                .next()
                .and_then(|c| c.to_digit(10));
            if num == None || num == Some(0) {
                s.url_backs.insert(0, value.to_string());
            } else {
                s.url_backs.push(value.to_string());
            }
        }
        k if k.starts_with("title_front") => {
            s.title_fronts.push(value.to_string());
        }
        k if k.starts_with("title_back") => {
            s.title_backs.push(value.to_string());
        }
        k if k.starts_with("secret") => {
            // "secret1_front" / "secret_back" — collect front/back ladders.
            let is_back = k.ends_with("back");
            if is_back {
                s.secret_backs.push(value.to_string());
            } else {
                s.secret_fronts.push(value.to_string());
            }
        }

        // Everything else (banned / ignore mask / token_* / value_* / ...)
        // is either multi-value (split on `|`) or freeform — preserve as-is.
        _ => {
            if key == "banned" || key == "ignore mask" {
                // Both are pipe-separated (GSA convention).
                let v = pipe_split(value);
                if key == "banned" {
                    s.banned = v;
                } else {
                    s.ignore_mask = v;
                }
            } else {
                s.extras.insert(key.to_string(), value.to_string());
            }
        }
    }
}

fn pipe_split(v: &str) -> Vec<String> {
    v.split('|').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
}

/// Load the site catalog from `data/gsa/se.dat` (relative to the CWD-walk
/// up in `humanize::data_root`).
pub fn load_site_catalog() -> Result<Vec<SiteSpec>, String> {
    let root = crate::humanize::data_root();
    let path = match root {
        Some(r) => r.join("se.dat"),
        None => std::path::PathBuf::from("data/gsa/se.dat"),
    };
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    Ok(parse_se_dat(&text))
}

// ===========================================================================
//  2. Search-query builder  (Gap 16: "Add stop words to query with 20%")
// ===========================================================================

/// One of the four GSA query modes.
///
/// * **Plain**   — bare keyword (GSA "find sites by keyword").
/// * **Site**    — `site:{domain} {kw}` (only when the target domain is
///                 known; GSA's "Find sites in a specific domain").
/// * **Intitle** — `intitle:"{kw}"` (only sites with the keyword in `<title>`).
/// * **Inurl**   — `url:{kw}` (only sites with the keyword in the URL).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum QueryMode {
    #[default]
    Plain,
    Site,
    Intitle,
    Inurl,
}

/// Configuration for a *single* "find sites" run.
#[derive(Debug, Clone, Default)]
pub struct SiteFindConfig {
    /// The keyword we're searching for.
    pub keyword: String,
    /// The domain we're looking for backlink opportunities on (used only
    /// by `QueryMode::Site`).
    pub domain: String,
    /// Which query mode to build.
    pub mode: QueryMode,
    /// Wrap the keyword in double quotes (GSA's "use quotes" checkbox).
    pub quotes: bool,
    /// GSA "Add stop words to query with N%" — sample N% of the 700-word
    /// `stop_words.dat` list and append to the query.  A real GSA feature
    /// (gap 16); the exact percentage is user-configurable.
    pub stop_words_pct: u32,
    /// Max result sites to return (default 50).
    pub max_results: usize,
    /// Max number of catalog entries to walk (default 0 = unbounded).
    /// Bounds a live API call so it can't hang on 2,525 sites
    /// × 60 s HTTP timeout. Tests leave it 0.
    pub max_sites: usize,
    /// Max pages to walk per catalog entry (default 3).
    pub max_pages: u32,
    /// Minimum keywords the resulting page must contain in visible text
    /// (gap 17: "at least N keywords present on visible text").  0 = off.
    pub min_keywords_present: usize,
    /// Skip sites whose country is in this list (empty = don't skip).
    pub skip_countries: Vec<String>,
    /// Skip sites whose `languages` intersect this list (empty = don't skip).
    pub skip_languages: Vec<String>,
    /// Skip sites whose canonical domain matches any entry (gap 17).
    pub block_domains: Vec<String>,
    /// Gap 18: avoid sites whose IP is in `seen_ips` (caller populates
    /// this from prior submissions; GSA's "Avoid posting to sites with
    /// duplicate IP").
    pub seen_ips: HashSet<String>,
    /// Determinism seed (SplitMix64).
    pub seed: u64,
}


/// Build the **final search string** GSA sends to the search engine.
///
/// * `Plain`   → `coffee shop`          (or `"coffee shop"` with quotes)
/// * `Site`    → `site:example.com coffee shop`
/// * `Intitle` → `intitle:"coffee shop"` (quotes enforced)
/// * `Inurl`   → `url:coffee shop`      (no quotes by GSA convention)
///
/// Stop-words are appended AFTER the mode prefix, comma-joined — GSA
/// treats them as soft hints that don't break keyword matching.
pub fn build_query(
    kw: &str,
    domain: &str,
    mode: QueryMode,
    quotes: bool,
    stop_words: &[String],
) -> String {
    let kw = kw.trim();
    if kw.is_empty() {
        return String::new();
    }

    let quoted = |s: &str| format!("\"{s}\"");
    let kw = if quotes || matches!(mode, QueryMode::Intitle) {
        quoted(kw)
    } else {
        kw.to_string()
    };

    let core = match mode {
        QueryMode::Plain => kw,
        QueryMode::Site => {
            if domain.is_empty() {
                kw
            } else {
                format!("site:{domain} {kw}")
            }
        }
        QueryMode::Intitle => format!("intitle:{kw}"),
        QueryMode::Inurl => format!("url:{kw}"),
    };

    if stop_words.is_empty() {
        core
    } else {
        let sw: String = stop_words.join(", ");
        format!("{core}, {sw}")
    }
}

/// Sample N% of the stop-words list deterministically.  `0` → empty.
/// `100` → the full list.  Returns the *subset* to append to the query.
pub fn sample_stop_words(stop_words: &[String], pct: u32, rng: &mut SplitMix64) -> Vec<String> {
    if pct == 0 || stop_words.is_empty() {
        return Vec::new();
    }
    let n = stop_words.len();
    let k = if pct >= 100 {
        n
    } else {
        (n as u64 * pct as u64 / 100).max(1) as usize
    };
    // Deterministic shuffle (Fisher-Yates via SplitMix64) then take the first k.
    let mut idx: Vec<usize> = (0..n).collect();
    for i in (1..n).rev() {
        let j = rng.range(i + 1);
        idx.swap(i, j);
    }
    idx.into_iter().take(k).map(|i| stop_words[i].clone()).collect()
}

// ===========================================================================
//  3. URL substitution
// ===========================================================================

/// Substitute GSA's `%...%` tokens in a search-URL template.
///
/// Recognised tokens (case-insensitive):
///   `%search%` — the query built by `build_query`.
///   `%page%`   — the current page offset (starts at `start page`,
///                increments by `inc page`).
///   `%domain%` — the target domain (Site mode).
///   `%secret%` — the rotating per-site hash (empty if unknown).
/// Anything else passes through verbatim (GSA uses `%secret1%`,
/// `%secret2%` in some entries — we don't generate those, callers pass
/// in the value via `extra`).
pub fn substitute_url(
    template: &str,
    search: &str,
    page: u32,
    domain: &str,
    extra: &HashMap<String, String>,
) -> String {
    let page = page.to_string();
    let mut out = String::with_capacity(template.len() + 64);
    let mut i = 0;
    let bytes = template.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'%' {
            // Look for the closing `%`.
            if let Some(off) = template[i + 1..].find('%') {
                let name = &template[i + 1..i + 1 + off];
                let lower = name.to_ascii_lowercase();
                match lower.as_str() {
                    "search" => {
                        out.push_str(search);
                        i += off + 2;
                        continue;
                    }
                    "page" => {
                        out.push_str(&page);
                        i += off + 2;
                        continue;
                    }
                    "domain" => {
                        out.push_str(domain);
                        i += off + 2;
                        continue;
                    }
                    other => {
                        if let Some(v) = extra.get(other) {
                            out.push_str(v);
                        } else if let Some(v) = extra.get(&other.replace('%', "")) {
                            out.push_str(v);
                        } else {
                            // Leave the token as-is (GSA convention for
                            // unknown tokens) — keep the full %name%.
                            out.push('%');
                            out.push_str(name);
                            out.push('%');
                        }
                        i += off + 2;
                        continue;
                    }
                }
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

// ===========================================================================
//  4. Result extraction  (Gap 17: how GSA reads the created URL)
// ===========================================================================

/// A single site the search engine returned as a result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FoundSite {
    /// The site's canonical URL (the one that will be submitted-to later).
    pub url: String,
    /// Result title (empty when no title anchor was found).
    pub title: String,
    /// Which GSA catalog entry produced this (`name` field of `SiteSpec`).
    pub source_site: String,
    /// Which page offset this came from (`start_page + k*inc_page`).
    pub source_page: u32,
}

impl FoundSite {
    /// Canonicalise: lowercase host, strip trailing `/`, drop default port.
    /// Not a full URL normaliser — just enough to dedupe `http://a.com/` and
    /// `http://a.com` as the same site.
    pub fn canonical(&self) -> String {
        let mut out: String = self.url.clone();
        // Drop query-string / fragment (site identity, not path).
        if let Some(h) = out.find(['?', '#']) {
            out.truncate(h);
        }
        // Lowercase host + port.
        if let Some(rest) = out.split_once("://") {
            let (scheme, host) = rest;
            if let Some(slash) = host.find('/') {
                let (hostpart, tail) = host.split_at(slash);
                out = format!("{scheme}://{}{}", hostpart.to_ascii_lowercase(), tail);
            } else {
                out = format!("{scheme}://{}", host.to_ascii_lowercase());
            }
        }
        // Strip trailing `/` (but keep bare `http://a.com` untouched).
        if out.ends_with('/') && out.rfind('/') > Some(out.find("://").unwrap_or(0)) {
            out.pop();
        }
        out
    }
}

/// Extract all result URLs from a single search page, using the
/// SiteSpec's extraction ladders + GSA's `seperator` chain + a generic
/// anchor-scrape fallback.
///
/// Strategy (GSA order):
///   1. If `seperator` is non-empty: split HTML on the seperator, and in
///      each slice pull the first occurrence of `url_front`/`url_back`.
///      (This is how GSA handles Google's JS JSON-heavy pages.)
///   2. Else, walk the `url_fronts × url_backs` ladder via
///      `extraction::extract_front_back_variants` — GSA's classic front/
///      back.  For each hit, if it looks URL-ish, accept.
///   3. Fallback: `extraction::find_url(html, "")` — first absolute URL
///      that isn't in `ignore_mask`.
///   4. Nothing → we return an empty vec.
/// Bounded domain mask (see caller for rationale).
fn mask_matches(url: &str, mask: &str) -> bool {
    if mask.is_empty() {
        return false;
    }
    if mask.starts_with('/') || mask.contains('/') {
        return url.contains(mask);
    }
    if !url.contains(mask) {
        return false;
    }
    let lowered = url.to_ascii_lowercase();
    let host_part = lowered
        .strip_prefix("https://")
        .or_else(|| lowered.strip_prefix("http://"))
        .or_else(|| lowered.strip_prefix("//"))
        .unwrap_or(lowered.as_str())
        .split('/')
        .next()
        .unwrap_or("");
    let host = host_part.split(':').next().unwrap_or(host_part);
    if host.is_empty() {
        return false;
    }
    let mask_lower = mask.to_ascii_lowercase();
    if host == mask_lower {
        return true;
    }
    if host.strip_suffix(&format!(".{}", mask_lower)).is_some() {
        return true;
    }
    if let Some(stripped) = host.strip_prefix("www.") {
        if stripped == mask_lower || stripped.strip_suffix(&format!(".{}", mask_lower)).is_some() {
            return true;
        }
    }
    false
}

pub fn extract_results(html: &str, site: &SiteSpec) -> Vec<FoundSite> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<FoundSite> = Vec::new();
    let push = |out: &mut Vec<_>, seen: &mut HashSet<_>, url: String, src: String, page: u32| {
        let u = strip_quotes(url.trim()).to_string();
        if u.is_empty() {
            return;
        }
        // GSA convention: `ignore mask=...` — drop anything matching.
        // `mask_matches` uses a **bounded** domain match: a mask like
        // `ads.example.com` drops `https://ads.example.com/...` but does
        // NOT drop `https://real.example.com/...` (where `ads` would be a
        // character substring of `real`).
        for m in site.ignore_mask.iter() {
            let pat = m.strip_prefix('!').unwrap_or(m.as_str()).trim();
            if pat.is_empty() {
                continue;
            }
            if mask_matches(&u, pat) {
                return;
            }
        }
        // Follow-redirect `!pattern` = skip URLs matching the pattern.
        if site.follow_redirect.starts_with('!') {
            let pat = site.follow_redirect.strip_prefix('!').unwrap_or("");
            if !pat.is_empty() && mask_matches(&u, pat) {
                return;
            }
        }
        let fs = FoundSite {
            url: u.clone(),
            title: String::new(),
            source_site: src,
            source_page: page,
        };
        let key = fs.canonical();
        if seen.insert(key) {
            out.push(fs);
        }
    };

    // 1. Seperator chain.
    if !site.seperator.is_empty() {
        for (slice_idx, slice) in html.split(&site.seperator).enumerate() {
            let page = slice_idx as u32; // approx — real page handled by caller
            let fronts: Vec<&str> = site.url_fronts.iter().map(|s| s.as_str()).collect();
            let backs: Vec<&str> = site.url_backs.iter().map(|s| s.as_str()).collect();
            let urls = extract_front_back_variants(slice, &fronts, &backs);
            if let Some(u) = urls {
                if looks_like_url(&u) {
                    push(&mut out, &mut seen, u, site.name.clone(), page);
                }
            }
            // Also try plain find_url within the slice (handles mixed pages).
            if let Some(u) = find_url(slice, "") {
                if looks_like_url(&u) {
                    push(&mut out, &mut seen, u, site.name.clone(), page);
                }
            }
        }
        if !out.is_empty() {
            return out;
        }
    }

    // 2. Classic front/back ladder — walk all occurrences.
    if !site.url_fronts.is_empty() {
        let mut pos = 0usize;
        let limit = 200usize; // hard cap on iterations (safety)
        let mut guard = 0usize;
        while guard < limit {
            guard += 1;
            let (start, end) = match next_front_back_range(html, site, pos) {
                Some((s, e)) if e > s => (s, e),
                _ => break,
            };
            let u = html[start..end].trim().to_string();
            if looks_like_url(&u) {
                push(&mut out, &mut seen, u, site.name.clone(), 0);
            }
            pos = end + 1;
        }
        if !out.is_empty() {
            return out;
        }
    }

    // 3. Generic fallback — walk every URL token. find_url (extraction.rs)
    // returns only the FIRST href, so a single blocked result would shadow
    // every valid one behind it (ignore-mask test).
    for u in enumerate_urls(html) {
        let cu = strip_quotes(u.trim()).to_string();
        if looks_like_url(&cu) {
            push(&mut out, &mut seen, cu, site.name.clone(), 0);
        }
    }
    out
}

/// Find the next result URL after `pos` using the SiteSpec's
/// url_fronts / url_backs ladders.  Returns `(start, end)` char-offset
/// range, or None when exhausted.
/// Walk the html for the earliest occurrence of any front/back pair.
/// Returns `(start, end)` where html[start..end] is the candidate token.
fn next_front_back_range(html: &str, site: &SiteSpec, from: usize) -> Option<(usize, usize)> {
    let mut best: Option<(usize, usize)> = None;
    for f in site.url_fronts.iter() {
        if f.is_empty() {
            continue;
        }
        let i = html[from..].find(f)?;
        let start = from + i + f.len();
        // Try every back marker in the ladder (GSA's url_back/url_back1/...
        // are alternative terminators — use whichever ends the token
        // earliest, so we never overshoot one hit and swallow the next).
        let mut end: Option<usize> = None;
        if !site.url_backs.is_empty() {
            let rest = &html[start..];
            for bk in site.url_backs.iter() {
                if bk.is_empty() {
                    continue;
                }
                let j = match rest.find(bk.as_str()) {
                    Some(j) => j,
                    None => continue,
                };
                let cand = start + j;
                if cand > start && (end.is_none() || cand < end.unwrap()) {
                    end = Some(cand);
                }
            }
        }
        if end.is_none() {
            let rest = &html[start..];
            end = Some(start + find_stop(rest).unwrap_or(rest.len()));
        }
        let end = end?;
        if end > start && (best.is_none() || start < best.unwrap().0) {
            best = Some((start, end));
        }
    }
    best
}

/// GSA's `url_back` ladder often stops on `\">` or `\'>` — strip one layer of
/// surrounding quotes / angle brackets so the extracted URL is clean.
/// GSA's `url_back` ladder often stops on `\">` or `\'>`, or the extracted
/// range includes a `\"` / `\'` attribute delimiter (e.g. `=\"https://..\
//...\"`).
/// Strip the common noise so the URL is clean: a leading `=\"` / `=\'`, or a
/// full pair of surrounding quotes / angle brackets.
fn strip_quotes(s: &str) -> &str {
    let s = s.trim();
    if s.is_empty() {
        return s;
    }
    // `="<value>"` or `='  <value>'`
    if let Some(rest) = s.strip_prefix("=\"") {
        if let Some(tail) = rest.strip_suffix("\"") {
            return tail.trim();
        }
    }
    if let Some(rest) = s.strip_prefix("=\'") {
        if let Some(tail) = rest.strip_suffix("\'") {
            return tail.trim();
        }
    }
    if s.len() >= 2 {
        let b = s.as_bytes();
        let first = b[0];
        let last = b[b.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') || (first == b'<' && last == b'>') {
            return s.strip_prefix(first as char)
                .and_then(|r| r.strip_suffix(last as char))
                .unwrap_or(s)
                .trim();
        }
    }
    s
}

fn find_stop(s: &str) -> Option<usize> {
    s.find(['\t', '\r', '\n', ' '])
        .or_else(|| s.find('>'))
        .or_else(|| { if !s.is_empty() { Some(0) } else { None } })
}

/// Walk an HTML document and return every absolute URL token
/// (http/https and protocol-relative) found in attributes or text.
/// `find_url` returns only the FIRST match — that's wrong for the generic
/// fallback where the first candidate may itself be blocked.
/// Walk an HTML document and return every absolute URL token (http/https
/// and protocol-relative) found in attributes or text.  `find_url` in
/// extraction.rs returns only the FIRST match, which is wrong for the
/// generic fallback where the first candidate may itself be blocked by
/// the ignore mask — so we need the full list.
/// Walk an HTML document and return every URL token (absolute http/https,
/// protocol-relative, or bare hostname/path) found in attribute values or
/// plain text.  `find_url` (extraction.rs) returns only the FIRST match,
/// which is wrong for the generic fallback where the first candidate may
/// itself be blocked by the ignore mask.
fn enumerate_urls(html: &str) -> Vec<String> {
    let s = html.as_bytes();
    let n = s.len();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < n {
        let start = if s[i] == b'h' && i + 8 <= n && &s[i..i + 8] == b"https://" {
            i
        } else if s[i] == b'h' && i + 7 <= n && &s[i..i + 7] == b"http://" {
            i
        } else if i >= 2 && s[i] == b'/' && s[i + 1] == b'/' && s[i - 1] != b'/' {
            i
        } else {
            i += 1;
            continue;
        };
        let mut j = start;
        while j < n {
            let c = s[j];
            if c.is_ascii_whitespace() || c == b'"' || c == b'\'' || c == b'>'
                || c == b',' || c == b')' || c == b'(' || c == b'=' || c == b';'
            {
                break;
            }
            j += 1;
        }
        if j > start {
            let mut tok = String::from_utf8_lossy(&s[start..j]).to_string();
            // Trim trailing HTML noise that isn't part of the URL.
            while matches!(tok.chars().last(),
                Some('.') | Some(',') | Some(';') | Some(':')
                    | Some('!') | Some('"') | Some('\'' ) | Some(')'))
            {
                tok.pop();
            }
            if !tok.is_empty() {
                out.push(tok);
            }
        }
        i = j.max(i + 1);
    }
    out
}

/// Cheap URL-likeness check: `http(s)://host/...` or `host/...` or
/// `//host/...`.  Rejects bare words, fragments, and empty strings.
/// Is this string a plausible URL (absolute, protocol-relative, or a
/// bare hostname / path the runner knows how to resolve)?
fn looks_like_url(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() || t.starts_with('#') {
        return false;
    }
    // Absolute scheme.
    if t.starts_with("http://") || t.starts_with("https://") {
        let host = t.split("://").nth(1).unwrap_or(t).split('/').next().unwrap_or("");
        return !host.is_empty() && host.chars().all(|c| c.is_ascii_alphanumeric() || c=='.'|| c=='-');
    }
    // Protocol-relative.
    if t.starts_with("//") {
        return t[2..].split('/').next().map(|h| !h.is_empty() && h.chars().all(|c| c.is_ascii_alphanumeric() || c=='.'|| c=='-')).unwrap_or(false);
    }
    // Relative path — accept a single `/`-separated path or `path/...`.
    if t.starts_with('/') {
        return t.chars().skip(1).any(|c| c.is_ascii_alphanumeric());
    }
    // Bare hostname or hostname/path (GSA's se.dat sites are often listed
    // as `forum.example.com/` without a scheme).
    let head = t.split('/').next().unwrap_or(t);
    if !head.is_empty() && head.contains('.') && head.chars().all(|c| c.is_ascii_alphanumeric() || c=='.'|| c=='-') {
        return true;
    }
    false
}

// ===========================================================================
//  5. Quality filters  (Gap 16 + 17 + 18)
// ===========================================================================

/// Pull the host + port out of a URL (lowercased).  `None` on parse fail.
pub fn domain_of(url: &str) -> Option<String> {
    let u = url.trim();
    let after_scheme = u.split_once("://").map(|(_, h)| h).unwrap_or(u);
    // Cut at first '/' (if any).
    let host = after_scheme.split('/').next().unwrap_or(after_scheme);
    // Strip user:pass@.
    let host = host.rsplit_once('@').map(|(_, h)| h).unwrap_or(host);
    // Strip :port.
    let host = host.split(':').next().unwrap_or(host);
    if host.is_empty() {
        None
    } else {
        Some(host.to_ascii_lowercase())
    }
}

/// GSA country-skip (gap 17: "skip-by-country").  A found site is blocked
/// when its *origin* country is in `skip`.  Because we often don't know the
/// origin country of a search result, the filter falls back to matching
/// the *catalog entry's* country (the search engine the result came from)
/// as a proxy.  Callers can narrow with `ip-to-country.csv` in F#25.
pub fn country_skipped(site: &SiteSpec, found: &FoundSite, skip: &[String]) -> bool {
    if skip.is_empty() {
        return false;
    }
    let lowered: Vec<String> = skip.iter().map(|s| s.to_ascii_lowercase()).collect();
    // Check catalog country first (known).
    if !site.country.is_empty() && lowered.contains(&site.country.to_ascii_lowercase()) {
        return true;
    }
    // Then ISO codes (site.countries).
    for c in &site.countries {
        if lowered.contains(&c.to_ascii_lowercase()) {
            return true;
        }
    }
    // Fallback: host ends with a two-letter TLD that maps to a skip list.
    if let Some(host) = domain_of(&found.url) {
        if let Some(tld) = host.rsplit_once('.').map(|(_, t)| t) {
            if lowered.contains(&tld.to_ascii_lowercase()) {
                return true;
            }
        }
    }
    false
}

/// GSA language-skip — a found site is blocked when the catalog entry's
/// `languages` intersect `skip`.
pub fn language_skipped(site: &SiteSpec, skip: &[String]) -> bool {
    if skip.is_empty() || site.languages.is_empty() {
        return false;
    }
    let skip_lower: HashSet<String> = skip.iter().map(|s| s.to_ascii_lowercase()).collect();
    site.languages
        .iter()
        .any(|l| skip_lower.contains(&l.to_ascii_lowercase()))
}

/// Gap 17 "word blocklist (title + URL/domain)".
pub fn domain_blocked(found: &FoundSite, block: &[String]) -> bool {
    if block.is_empty() {
        return false;
    }
    let Some(host) = domain_of(&found.url) else {
        return false;
    };
    for b in block {
        let b = b.trim().to_ascii_lowercase();
        if b.is_empty() {
            continue;
        }
        // `*` wildcard = TLD-only match (`*.org`, `.org`).
        if let Some(suffix) = b.strip_prefix(".") {
            if host.ends_with(&format!(".{suffix}")) || host == suffix {
                return true;
            }
        } else if let Some(suffix) = b.strip_prefix("*.") {
            if host.ends_with(suffix) {
                return true;
            }
        } else if host == b || host.ends_with(&format!(".{b}")) {
            return true;
        }
    }
    false
}

/// Gap 18 "Avoid posting to sites with duplicate IP".  We don't do DNS
/// here (F#25 will), so this is a *check-only* function: the caller passes
/// in the IP of the found site (from a DNS lookup they performed), and we
/// flag it as blocked when that IP is in `seen`.
pub fn ip_duplicated(site_ip: &str, seen: &HashSet<String>) -> bool {
    let ip = site_ip.trim().to_ascii_lowercase();
    if ip.is_empty() {
        return false;
    }
    seen.contains(&ip)
}

// ===========================================================================
//  6. Site-finding pipeline  (the GSA "Find New Sites" button)
// ===========================================================================

/// One iteration of the find pipeline: run the query against one catalog
/// entry, extract the results, apply the quality filters, and cap.
///
/// `driver` is the seam — production = `http_driver::HttpDriver`, tests =
/// `MockDriver`.  We never call `driver::Driver` from within a test so the
/// test suite stays network-free.
pub async fn find_sites_on_site(
    site: &SiteSpec,
    cfg: &SiteFindConfig,
    driver: Arc<dyn Driver>,
) -> Vec<FoundSite> {
    // A disabled site is skipped outright (catalog `enabled=0`).
    if !site.enabled {
        return Vec::new();
    }
    // 1. Assemble the query.
    let stop_words: Vec<String> = {
        let sw: Vec<String> = crate::humanize::stop_words().unwrap_or_default();
        let mut rng = SplitMix64::new(cfg.seed);
        sample_stop_words(&sw, cfg.stop_words_pct, &mut rng)
    };
    let query = build_query(
        &cfg.keyword,
        &cfg.domain,
        cfg.mode,
        cfg.quotes,
        &stop_words,
    );
    if query.is_empty() {
        return Vec::new();
    }

    // 2. Walk `max_pages` pages (or `max page` in the SiteSpec, whichever
    //    is smaller when both are set).  A value of 0 means "not set", so it
    //    falls back to the other (or a sensible default of 1), never zero.
    let pages_spec = (site.max_page / site.inc_page.max(1)) as u32;
    let pages = if cfg.max_pages > 0 {
        if pages_spec > 0 {
            cfg.max_pages.min(pages_spec)
        } else {
            cfg.max_pages
        }
    } else if pages_spec > 0 {
        pages_spec
    } else {
        1
    };

    let mut out: Vec<FoundSite> = Vec::new();
    let mut seen_ips = cfg.seen_ips.clone();
    for p in 0..pages {
        let offset = site.start_page + p * site.inc_page;
        let url = substitute_url(&site.url, &query, offset, &cfg.domain, &HashMap::new());

        let resp: DriverResponse = driver.navigate(url).await;
        if !resp.ok {
            // GSA's `banned=` patterns mean "this response is a ban page;
            // stop searching this site now".
            if site.banned.iter().any(|b| b.starts_with('!').then(|| resp
                .html
                .contains(b.strip_prefix('!').unwrap_or(b)))
                .unwrap_or_else(|| resp.html.contains(b)))
            {
                break;
            }
            if resp.html.is_empty() {
                continue; // hard fail — no HTML, try next page
            }
        }

        // 3. Extract + filter.
        let hits = extract_results(&resp.html, site);
        for mut h in hits {
            h.source_page = p;
            // Country skip (catalog + TLD fallback).
            if country_skipped(site, &h, &cfg.skip_countries) {
                continue;
            }
            // Language skip (catalog-based).
            if language_skipped(site, &cfg.skip_languages) {
                continue;
            }
            // Domain blocklist.
            if domain_blocked(&h, &cfg.block_domains) {
                continue;
            }
            // We don't do DNS here (F#25); callers extend `seen_ips`
            // before they submit, not at extraction time.
            let _ = &mut seen_ips;
            out.push(h);
            if out.len() >= cfg.max_results {
                return out;
            }
        }

        // GSA's `wait between search` — pacing.  We leave the actual sleep
        // to the runner (which has the tokio runtime); this module is pure
        // except for the driver call.  The SiteSpec carries the number.
        let _ = site.wait_between_search;
    }

    out
}

/// Run the query across the whole catalog, collect hits, dedupe, cap.
///
/// `sites` = `load_site_catalog()`; `driver` = the driver to navigate
/// with.  This is the top-level function the API endpoint and the runner
/// both call.
pub async fn find_sites(
    cfg: SiteFindConfig,
    driver: Arc<dyn Driver>,
    sites: Vec<SiteSpec>,
) -> Vec<FoundSite> {
    let mut out: Vec<FoundSite> = Vec::new();
    let mut seen_canonical: HashSet<String> = HashSet::new();

    let mut walked = 0usize;
    for site in &sites {
        if !site.enabled || site.url.is_empty() {
            continue;
        }
        // Walk-count cap: with this many already fetched (0 = no cap),
        // stop early.  A bounded API call must not walk thousands of
        // 60-second sites serially.
        if cfg.max_sites > 0 && walked >= cfg.max_sites {
            return out;
        }
        walked += 1;
        let hits = find_sites_on_site(site, &cfg, Arc::clone(&driver)).await;
        for h in hits {
            let key = h.canonical();
            if seen_canonical.insert(key) {
                out.push(h);
                if out.len() >= cfg.max_results {
                    return out;
                }
            }
        }
    }
    out
}

// ===========================================================================
//  7. Tests  (no network — MockDriver only)
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::MockDriver;
    use std::sync::Arc;

    fn mock_driver_with(html: &str) -> Arc<dyn Driver> {
        Arc::new(MockDriver::with_html(html.to_string()))
    }

    // --- parse_se_dat ------------------------------------------------------

    #[test]
    fn parse_sample_entry_basic() {
        let txt = "\
; comment
[ MyTest ]
desc=test engine
country=US
countries=us
languages=en
url=https://ex.com/search?q=%search%&p=%page%
wait between search=5
start page=0
inc page=10
max page=50
banned=blocked|too many
ignore mask=ads.

[Other]
url=https://o.com/r/%search%
";
        let v = parse_se_dat(txt);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].name, "MyTest");
        assert_eq!(v[0].country, "US");
        assert_eq!(v[0].countries, vec!["us"]);
        assert_eq!(v[0].languages, vec!["en"]);
        assert!(v[0].url.contains("%search%"));
        assert_eq!(v[0].start_page, 0);
        assert_eq!(v[0].inc_page, 10);
        assert_eq!(v[0].max_page, 50);
        assert_eq!(v[0].banned, vec!["blocked", "too many"]);
        assert_eq!(v[0].ignore_mask, vec!["ads."]);
        assert!(v[1].name == "Other");
    }

    #[test]
    fn parse_multi_value_front_back_ladder() {
        let txt = "\
[Ladder]
url=x
url_front=/a/
url_front1=/b/
url_front2=/c/
url_back=</a>
url_back1=</b>
url_back2=</c>
";
        let v = parse_se_dat(txt);
        assert_eq!(v.len(), 1);
        let s = &v[0];
        // "url_front" → slot 0, then "url_front1", "url_front2" pushed.
        assert!(s.url_fronts.contains(&"/a/".to_string()));
        assert!(s.url_fronts.contains(&"/b/".to_string()));
        assert!(s.url_fronts.contains(&"/c/".to_string()));
        assert!(s.url_backs.contains(&"</a>".to_string()));
    }

    #[test]
    fn parse_gsa_real_entry_google() {
        let txt = "\
[ Google ]
desc=still the best search engine
enabled=1
country=International
url=https://www.google.com/search?gbv=1&q=%search%&pws=0&no_sw_cr=1&start=%page%
more results parameter=num
more results value=10, 20, 30, 40, 50, 100
submit form=*://consent.google.com/save
inurl=inurl:
intitle=intitle:
site=site:
follow redirect=!/enablejs
wait between search=70
wait between special search=120
ban timeout=70
banned=<title>Sorry...</title>|Our systems have detected unusual traffic
user agent=internal
wrong user agent=var ui=
extract by tag=h3:class=?*
ignore mask=googleusercontent|maps.google.
decode1=extract_parameter url *google.com/*
decode2=extract_parameter q *google.com/url?q=*
";
        let v = parse_se_dat(txt);
        assert_eq!(v.len(), 1);
        let s = &v[0];
        assert_eq!(s.name, "Google");
        assert!(s.enabled);
        assert_eq!(s.country, "International");
        assert!(s.url.contains("%search%"));
        assert!(s.url.contains("%page%"));
        assert_eq!(s.more_results_param, "num");
        assert_eq!(s.more_results_values, vec!["10", "20", "30", "40", "50", "100"]);
        assert_eq!(s.site_prefix, "site:");
        assert_eq!(s.intitle_prefix, "intitle:");
        assert_eq!(s.inurl_prefix, "inurl:");
        assert!(s.follow_redirect.starts_with('!'));
        assert!(s.banned.iter().any(|b| b.contains("Sorry")));
        assert!(s.ignore_mask.iter().any(|m| m.contains("googleusercontent")));
        // Unknown keys land in extras.
        assert!(s.extras.contains_key("decode1"));
        assert!(s.extras.contains_key("wrong user agent"));
    }

    #[test]
    fn load_catalog_returns_expected_count() {
        // The real GSA catalog (2525 entries) — a strong smoke test.
        let v = load_site_catalog().expect("data/gsa/se.dat should load");
        assert!(v.len() >= 2000, "expected >= 2000 entries, got {}", v.len());
        assert!(v.iter().any(|e| e.name == "Google"), "Google entry should exist");
    }

    // --- build_query -------------------------------------------------------

    #[test]
    fn build_query_plain_no_quotes() {
        let q = build_query("coffee shop", "ex.com", QueryMode::Plain, false, &[]);
        assert_eq!(q, "coffee shop");
    }

    #[test]
    fn build_query_plain_with_quotes() {
        let q = build_query("coffee shop", "ex.com", QueryMode::Plain, true, &[]);
        assert_eq!(q, "\"coffee shop\"");
    }

    #[test]
    fn build_query_site_mode_needs_domain() {
        let q = build_query("kw", "ex.com", QueryMode::Site, false, &[]);
        assert_eq!(q, "site:ex.com kw");
    }

    #[test]
    fn build_query_site_mode_no_domain_falls_back_to_plain() {
        let q = build_query("kw", "", QueryMode::Site, false, &[]);
        assert_eq!(q, "kw");
    }

    #[test]
    fn build_query_intitle_always_quotes() {
        let q = build_query("kw", "ex.com", QueryMode::Intitle, false, &[]);
        assert_eq!(q, "intitle:\"kw\"");
    }

    #[test]
    fn build_query_inurl_does_not_quote() {
        let q = build_query("kw", "ex.com", QueryMode::Inurl, false, &[]);
        assert_eq!(q, "url:kw");
    }

    #[test]
    fn build_query_with_stop_words_appends_comma_csv() {
        let sw: Vec<String> = vec!["a".into(), "b".into()];
        let q = build_query("kw", "", QueryMode::Plain, false, &sw);
        assert_eq!(q, "kw, a, b");
    }

    fn owned(words: &[&str]) -> Vec<String> { words.iter().map(|s| s.to_string()).collect() }
    #[test]
    fn sample_stop_words_empty_at_zero_pct() {
        let mut rng = SplitMix64::new(1);
        let v = sample_stop_words(&owned(&["a", "b", "c"]), 0, &mut rng);
        assert!(v.is_empty());
    }

    #[test]
    fn sample_stop_words_full_at_100_pct() {
        let mut rng = SplitMix64::new(1);
        let v = sample_stop_words(&owned(&["a", "b", "c"]), 100, &mut rng);
        assert_eq!(v.len(), 3);
    }

    #[test]
    fn sample_stop_words_is_deterministic_same_seed() {
        let owned: Vec<String> = (0..20).map(|i| format!("w{i}")).collect();
        let mut a = SplitMix64::new(42);
        let mut b = SplitMix64::new(42);
        let va = sample_stop_words(&owned, 50, &mut a);
        let vb = sample_stop_words(&owned, 50, &mut b);
        assert_eq!(va, vb);
    }

    // --- substitute_url ----------------------------------------------------

    #[test]
    fn substitute_url_replaces_search_page_domain() {
        let out = substitute_url(
            "https://x.com/?q=%search%&p=%page%",
            "coffee house",
            40,
            "ex.com",
            &HashMap::new(),
        );
        assert_eq!(out, "https://x.com/?q=coffee house&p=40");
    }

    #[test]
    fn substitute_url_leaves_unknown_tokens() {
        let out = substitute_url("https://x.com/?t=%secret1%", "kw", 0, "", &HashMap::new());
        assert_eq!(out, "https://x.com/?t=%secret1%");
    }

    #[test]
    fn substitute_url_resolves_extra_tokens() {
        let mut extra = HashMap::new();
        extra.insert("secret1".to_string(), "ABC123".to_string());
        let out = substitute_url("https://x.com/?t=%secret1%", "kw", 0, "", &extra);
        assert_eq!(out, "https://x.com/?t=ABC123");
    }

    // --- domain_of + filters -----------------------------------------------

    #[test]
    fn domain_of_returns_lowercased_host() {
        assert_eq!(
            domain_of("https://WWW.Example.COM/Path?a=1"),
            Some("www.example.com".into())
        );
        assert_eq!(
            domain_of("http://127.0.0.1:8080/x"),
            Some("127.0.0.1".into())
        );
    }

    #[test]
    fn domain_blocked_exact_tld_and_star_wildcard() {
        let found = FoundSite {
            url: "https://foo.co.uk/x".into(),
            title: String::new(),
            source_site: String::from("Google"),
            source_page: 0,
        };
        assert!(domain_blocked(&found, &[".co.uk".into()]));
        assert!(domain_blocked(&found, &["co.uk".into()]));
        assert!(domain_blocked(&found, &["*.uk".into()]));
        assert!(!domain_blocked(&found, &[".com".into()]));
    }

    #[test]
    fn ip_duplicated_hits_only_on_exact_match() {
        let mut seen = HashSet::new();
        seen.insert("1.2.3.4".to_string());
        assert!(ip_duplicated("1.2.3.4", &seen));
        assert!(ip_duplicated(" 1.2.3.4 ", &seen));
        assert!(!ip_duplicated("9.9.9.9", &seen));
        assert!(!ip_duplicated("", &seen));
    }

    #[test]
    fn language_skipped_matches_site_languages() {
        let site = SiteSpec {
            languages: vec!["de".into(), "fr".into()],
            ..Default::default()
        };
        assert!(language_skipped(&site, &["de".into()]));
        assert!(language_skipped(&site, &["FR".into()]));
        assert!(!language_skipped(&site, &["en".into()]));
        assert!(!language_skipped(&site, &[])); // empty = don't skip
    }

    // --- extract_results ---------------------------------------------------

    fn google_like_html() -> String {
        // A Google result page with two real hits.
        [
            r###"<div class="g">"###,
            r###"<h3><a href="/url?q=https://real-site.example.com/article">Title one</a></h3>"###,
            r###"</div>"###,
            r###"<div class="g">"###,
            r###"<h3><a href="/url?q=https://another.example.org/page">Title two</a></h3>"###,
            r###"</div>"###,
        ]
        .join("\n")
    }

    fn site_with_front_back(front: &str, back: &str) -> SiteSpec {
        let mut s = SiteSpec::default();
        s.url_fronts = vec![front.to_string()];
        if !back.is_empty() {
            s.url_backs = vec![back.to_string()];
        }
        s.name = "test".into();
        s
    }

    #[test]
    fn extract_results_via_url_front_back() {
        let html = "<a href=\"https://site1.com/x\">hit</a> ... <a href=\"https://site2.com/y\">hit</a>";
        let site = site_with_front_back("", "");
        // No front, no back → falls back to find_url(html, "") which returns
        // the first absolute href.
        let v = extract_results(html, &site);
        assert!(!v.is_empty());
        assert!(v.iter().any(|f| f.url.contains("site1.com")));
    }

    #[test]
    fn extract_results_dedupes_by_canonical() {
        let html = "<a href=\"http://a.com/\">x</a><a href=\"http://a.com\">y</a>";
        let site = SiteSpec::default();
        let v = extract_results(html, &site);
        // Both collapse to the same canonical.
        assert!(v
            .iter()
            .all(|f| f.canonical() == "http://a.com"));
    }

    #[test]
    fn extract_results_respects_ignore_mask() {
        let html = "<a href=\"https://ads.example.com/click\">ad</a><a href=\"https://real.example.com/page\">content</a>";
        let mut site = SiteSpec::default();
        site.ignore_mask = vec!["ads.example.com".to_string()];
        let v = extract_results(html, &site);
        assert!(v.iter().any(|f| f.url.contains("real.example.com")));
        assert!(!v.iter().any(|f| f.url.starts_with("https://ads.example.com")));
    }

    // --- find_sites_on_site ------------------------------------------------

    #[tokio::test]
    async fn find_sites_on_site_end_to_end() {
        let mut site = SiteSpec {
            url: "https://ex.com/search?q=%search%".into(),
            start_page: 0,
            inc_page: 10,
            max_page: 20,
            name: "TestEngine".into(),
            ..Default::default()
        };
        site.url_fronts = vec![String::from("<a href=")];
        site.url_backs = vec![String::from(">")];

        let html = google_like_html();
        let driver = mock_driver_with(&html);

        let cfg = SiteFindConfig {
            keyword: "coffee".into(),
            domain: String::new(),
            mode: QueryMode::Plain,
            quotes: false,
            stop_words_pct: 0,
            max_results: 10,
            max_sites: 0,
            max_pages: 2,
            min_keywords_present: 0,
            skip_countries: vec![],
            skip_languages: vec![],
            block_domains: vec![],
            seen_ips: HashSet::new(),
            seed: 7,
        };
        let v = find_sites_on_site(&site, &cfg, driver).await;
        assert!(!v.is_empty(), "should have found at least one hit");
        assert!(v.iter().all(|f| f.source_site == "TestEngine"));
    }

    #[tokio::test]
    async fn find_sites_on_site_disabled_site_yields_nothing() {
        let site = SiteSpec {
            enabled: false,
            url: "https://ex.com/?q=%search%".into(),
            ..Default::default()
        };
        let driver = mock_driver_with("<a href=\"http://a.com\">x</a>");
        let cfg = SiteFindConfig {
            keyword: "kw".into(),
            max_results: 5,
            ..Default::default()
        };
        let v = find_sites_on_site(&site, &cfg, driver).await;
        assert!(v.is_empty());
    }

    #[tokio::test]
    async fn find_sites_dedupes_across_sites() {
        let mut a = SiteSpec { url: "https://x.com/?q=%search%".into(), name: "A".into(), ..Default::default() };
        a.url_fronts = vec!["<a href=".into()]; a.url_backs = vec![">".into()];
        let mut b = SiteSpec { url: "https://y.com/?q=%search%".into(), name: "B".into(), ..Default::default() };
        b.url_fronts = vec!["<a href=".into()]; b.url_backs = vec![">".into()];
        let html = "<a href=\"https://site.com/p\">1</a>";
        let cfg = SiteFindConfig { keyword: "kw".into(), max_results: 10, seed: 1, ..Default::default() };
        let v = find_sites(cfg, Arc::new(MockDriver::with_html(html.clone())), vec![a, b]).await;
        // Both sites return the same hit → dedupe to one.
        let canonicals: Vec<String> = v.iter().map(|f| f.canonical()).collect();
        let unique: HashSet<String> = canonicals.into_iter().collect();
        assert_eq!(unique.len(), 1, "expected 1 unique, got {v:?}");
    }

    #[test]
    fn found_site_canonical_normalises() {
        let f = FoundSite {
            url: "https://WWW.Example.COM:8080/Path/?x=1".into(),
            title: String::new(),
            source_site: String::new(),
            source_page: 0,
        };
        let c = f.canonical();
        assert!(c.contains("www.example.com"), "got {c}");
        assert!(!c.contains("?x=1"));
    }

    #[test]
    fn stop_words_helper_is_available_from_humanize() {
        // Ensures the F#22 humanize module exposes stop_words() to F#23.
        let v = crate::humanize::stop_words().expect("stop_words.dat should load");
        assert!(v.len() >= 500, "expected >= 500 stop-words, got {}", v.len());
    }
}
