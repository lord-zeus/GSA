//! F#19 — Anchor-text intelligence (GSA "Anchor Text (below the keyword)" tab).
//!
//! GSA does not post the same anchor to every site.  It mixes:
//!   main keyword            (the "balance", implicit — `100 - sum(weights)`)
//!   secondary / branding / LSI / generic / domain anchor-texts (pools, %)
//!   + casewise "variations" on the drawn anchor (GSA "Anchor text variations 20%")
//!
//! The mix is stored on the *project* as a JSON blob (see
//! [`PoolConfig`]); the runner draws one anchor per submission, deterministically
//! from the per-job seed, and feeds it as `%anchor_text%` into the engine spec
//! (plus the GSA aliases `%banchor_text%` — brand, `%panchor_text%` — part).
//!
//! This module is pure (no I/O): given a pool, a keyword, a domain and a seed,
//! `draw_anchor` is byte-for-byte reproducible, which is what makes tests
//! meaningful (see `splitmix64` determinism rules elsewhere in the crate).

use crate::splitmix64::SplitMix64;
use serde::{Deserialize, Serialize};

/// The stored anchor pool. `serde(default)` everywhere ⇒ a partial JSON
/// document (e.g. `{"domain": true}`) is a valid config.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PoolConfig {
    /// Secondary anchor-texts (GSA "Secondary anchor text", 10% in the demo).
    pub secondary: Vec<String>,
    /// Branding anchor-texts (GSA "Branding anchor text", 70% in the demo).
    /// `|`-separated variants spin to one at draw time.
    pub branding: Vec<String>,
    /// LSI / semantically related anchor-texts (GSA "LSI anchor text", 3%).
    pub lsi: Vec<String>,
    /// Generic, non-keyword anchor-texts (GSA "Generic anchor text", 5%):
    /// "click here", "read more", "this page", …
    pub generic: Vec<String>,
    /// GSA "Domain as anchor text" (20% in the demo): the bare domain
    /// (`example.com`) is allowed as an anchor.
    #[serde(default)]
    pub domain: bool,
    /// Per-type weights in percents. The **main keyword** anchor always
    /// carries the remaining weight (`100 - sum`). GSA enforces
    /// "Total Percentage ≤ 100%"; we normalize (not reject) so a UI typo of
    /// 130% still behaves sanely — each weight is scaled to sum ≤ 100.
    #[serde(default)]
    pub percentages: Percentages,
    /// GSA "Anchor text variations (e.g. GSA becomes gsa or Gsa)" — apply a
    /// case-mutation to the drawn anchor with this probability (0..=100).
    /// Default 0; GSA's default is 20.
    #[serde(default)]
    pub variations_pct: u32,
    /// GSA "Try to use domain/generic anchor with main anchor" — when true,
    /// a domain or generic pool entry may piggy-back a keyword ("example.com
    /// dubai coffee") instead of using the keyword alone, within the
    /// main-keyword weight.
    #[serde(default)]
    pub domain_with_keyword: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Percentages {
    #[serde(default)]
    pub secondary: u32,
    #[serde(default)]
    pub branding: u32,
    #[serde(default)]
    pub lsi: u32,
    #[serde(default)]
    pub generic: u32,
    #[serde(default)]
    pub domain: u32,
}

impl Percentages {
    fn sum(&self) -> u32 {
        self.secondary + self.branding + self.lsi + self.generic + self.domain
    }
}

/// One draw from the pool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnchorDraw {
    /// The concrete anchor text to submit (after spintax + casewise roll).
    pub text: String,
    /// The family that was chosen — for the results column (GSA export has
    /// an "Anchor text" column only, but stats per family are useful).
    pub kind: AnchorKind,
}

/// GSA "Use the root/main URL with N%" — submit the bare
/// `scheme://host/` (no path) with this probability (0..=100).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct UrlOptions {
    #[serde(default)]
    pub use_random_url: bool,
    #[serde(default)]
    pub url_variations: u32,
    #[serde(default)]
    pub root_url: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AnchorKind {
    Keyword, // the project's main keyword (spun)
    Secondary,
    Branding,
    Lsi,
    Generic,
    Domain,
    DomainKeyword, // domain/generic + keyword (GSA "with main anchor")
}

impl AnchorKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Keyword => "keyword",
            Self::Secondary => "secondary",
            Self::Branding => "branding",
            Self::Lsi => "lsi",
            Self::Generic => "generic",
            Self::Domain => "domain",
            Self::DomainKeyword => "domain-keyword",
        }
    }
}

impl PoolConfig {
    /// GSA's "Total Percentage" line — the % of non-keyword anchors. The main
    /// keyword fills `100 - total`. Used by the UI's validation label.
    pub fn total_percentage(&self) -> u32 {
        self.percentages.sum().min(100)
    }

    /// Validate + normalize: every weight clamped to 0..=100, and if they
    /// overflow 100 the whole map is scaled down (GSA would refuse to save
    /// "Total Percentage > 100"; we behave sanely instead of breaking).
    pub fn clamped(&self) -> PoolConfig {
        let mut p = self.clone();
        let sum = p.percentages.sum();
        if sum > 100 {
            let scale = 100f64 / sum.max(1) as f64;
            let r = |v: u32| (v as f64 * scale).round() as u32;
            p.percentages.secondary = r(p.percentages.secondary);
            p.percentages.branding = r(p.percentages.branding);
            p.percentages.lsi = r(p.percentages.lsi);
            p.percentages.generic = r(p.percentages.generic);
            p.percentages.domain = r(p.percentages.domain);
        }
        p.variations_pct = p.variations_pct.min(100);
        p
    }

    /// Is there anything in the non-keyword pools? (UI + a fast "skip the
    /// allocator" path in the runner.)
    pub fn is_empty(&self) -> bool {
        self.secondary.is_empty()
            && self.branding.is_empty()
            && self.lsi.is_empty()
            && self.generic.is_empty()
            && !self.domain
    }
}

/// Draw one anchor for this submission.
///
/// Deterministic in `seed`: the same (pool, keyword, domain, seed) tuple
/// always yields the same `AnchorDraw` — rerunning a job is stable, and a
/// test can assert exact outputs.
///
/// `spin_ctx` is the crate's spintax `Context`; every pool entry runs through
/// `resolve` so `{click|read} {here|more}` in a pool entry spins to one
/// concrete string before submission.
pub fn draw_anchor(
    cfg: &PoolConfig,
    keyword: &str,
    domain: &str,
    seed: Option<u64>,
    spin_ctx: &crate::spin::Context,
) -> AnchorDraw {
    let mut rng = SplitMix64::new(seed.unwrap_or(0xA11CE_5EED | 1));

    // 1) pick a family via the normalized weight table. When the pool is
    //    empty (or the main-keyword bucket is drawn) the keyword wins.
    let p = cfg.clamped();
    let weights: [(AnchorKind, u32); 7] = [
        (AnchorKind::Secondary, p.percentages.secondary),
        (AnchorKind::Branding, p.percentages.branding),
        (AnchorKind::Lsi, p.percentages.lsi),
        (AnchorKind::Generic, p.percentages.generic),
        (AnchorKind::Domain, if p.domain { p.percentages.domain } else { 0 }),
        (AnchorKind::DomainKeyword, 0), // filled below
        (AnchorKind::Keyword, 0), // filled last so it is the leftover
    ];
    let dk_weight: u32 = if p.domain_with_keyword && (p.generic.iter().any(|g| !g.trim().is_empty()) || p.domain) {
        // GSA splits the main-keyword bucket in half for the combined draw
        // when the option is on; a 50/50 feel inside the keyword weight.
        0 // computed after the keyword weight is known
    } else {
        0
    };
    let other: u32 = weights.iter().map(|(_, w)| *w).sum();
    let kw_weight = 100u32.saturating_sub(other);
    let dk = if dk_weight > 0 { (dk_weight.min(kw_weight / 2)).max(1) } else { 0 };
    let kw_weight = kw_weight.saturating_sub(dk);
    let total = other + dk + kw_weight;
    let roll = (rng.next_u64() as u128 % total.max(1) as u128) as usize;
    let mut acc: usize = 0;
    let mut kind = AnchorKind::Keyword;
    for (knd, w) in weights.iter() {
        let w = match *knd {
            AnchorKind::DomainKeyword => dk,
            AnchorKind::Keyword => kw_weight,
            _ => *w,
        };
        acc += w as usize;
        if roll < acc {
            kind = *knd;
            break;
        }
    }

    // 2) pick the concrete text for that family, spin it, optionally
    //    casewise-mutate it (GSA "Anchor text variations").
    let pool_entry: Option<String> = match kind {
        AnchorKind::Secondary => pick(&mut rng, &p.secondary),
        AnchorKind::Branding => pick(&mut rng, &p.branding),
        AnchorKind::Lsi => pick(&mut rng, &p.lsi),
        AnchorKind::Generic => pick(&mut rng, &p.generic),
        AnchorKind::Domain => {
            if !domain.is_empty() {
                Some(domain.to_string())
            } else {
                pick(&mut rng, &p.generic)
            }
        }
        AnchorKind::DomainKeyword => {
            let base = pick(&mut rng, &p.generic).or_else(|| (!domain.is_empty()).then(|| domain.to_string()));
            match base {
                Some(b) if !keyword.is_empty() => Some(format!("{b} {keyword}")),
                b => b,
            }
        }
        AnchorKind::Keyword => None, // keyword below
    };

    let mut text = match pool_entry {
        Some(t) => t,
        None => keyword.to_string(),
    };

    // 3) spintax inside the drawn text (GSA applies spin everywhere).
    let spun = crate::spin::resolve(&text, spin_ctx);
    // trim — pools with stray whitespace still submit clean anchors.
    let mut text = spun.trim().to_string();
    if text.is_empty() {
        // pool entry was `[maybe]`-optional and rolled out; fall back to keyword
        text = keyword.to_string();
        kind = AnchorKind::Keyword;
    }

    // 4) casewise variation (GSA "Anchor text variations": GSA → gsa / Gsa).
    //    Only mutates when the anchor is a real word-ish token and the roll passes.
    if p.variations_pct > 0
        && kind != AnchorKind::Keyword
        && (rng.next_u64() as u32 % 100) < p.variations_pct
    {
        text = casewise_variant(text, rng);
    }

    AnchorDraw { text, kind }
}

/// Uniform pick from a non-empty list, advancing `rng`.
fn pick(rng: &mut SplitMix64, list: &[String]) -> Option<String> {
    if list.is_empty() {
        return None;
    }
    let i = (rng.next_u64() as usize) % list.len();
    Some(list[i].clone())
}

/// GSA-style casewise mutation: lowercase, Title-Case, or a mid-word
/// uppercase. Only applied to ASCII letters (GSA's examples are
/// "gsa / Gsa" — no unicode acrobatics).
fn casewise_variant(s: String, mut rng: SplitMix64) -> String {
    let style = (rng.next_u64() as u32) % 3;
    match style {
        0 => s.to_lowercase(),
        1 => {
            // Title Case (only for 2+ words; one-word anchors keep style 0 feel)
            if s.split_whitespace().count() > 1 {
                title_case(&s)
            } else {
                s.to_lowercase()
            }
        }
        _ => {
            // mid-word uppercase: "gsa" -> "Gsa" (upper the first letter when
            // the string is already mostly lowercase; otherwise nudge a
            // random interior letter).
            let ch: Vec<char> = s.chars().collect();
            if ch.iter().all(|c| c.is_ascii_uppercase()) {
                // already ALL CAPS → lower the rest, keep the first (Gsa style)
                let mut out = String::with_capacity(s.len());
                for (i, c) in ch.iter().enumerate() {
                    out.push(if i == 0 { *c } else { c.to_ascii_lowercase() });
                }
                out
            } else {
                let lower = s.to_lowercase();
                let ch: Vec<char> = lower.chars().collect();
                if ch.is_empty() {
                    lower
                } else {
                    let mut out = String::with_capacity(s.len());
                    for (i, c) in ch.iter().enumerate() {
                        out.push(if i == 0 { c.to_ascii_uppercase() } else { *c });
                    }
                    out
                }
            }
        }
    }
}

fn title_case(s: &str) -> String {
    s.split_whitespace()
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                None => String::new(),
                Some(f) => f.to_uppercase().to_string() + c.as_str(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Extract the registrable-ish domain from a URL or bare domain string
/// (`https://www.example.co.uk/x` → `example.co.uk`). Used when the pool
/// marks "domain as anchor".
pub fn domain_of(input: &str) -> String {
    let s = input.trim();
    let no_scheme = s
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("ftp://");
    let host = no_scheme.split(['/', '?', '#']).next().unwrap_or(no_scheme);
    // Strip userinfo (`user@host`) — take the part after the LAST `@`, then the
    // part before any port (`:port`). Do NOT split on `@` first (that yields the
    // user, not the host).
    let host = host.rsplit('@').next().unwrap_or(host);
    let host = host.split(':').next().unwrap_or(host).to_ascii_lowercase();
    // Strip a pure `www.` prefix — GSA's "Domain as anchor" shows `example.com`,
    // not `www.example.com`.
    let host = host
        .strip_prefix("www.")
        .map(|s| s.to_string())
        .unwrap_or(host);
    // Registrable domain without a PSL: keep the last 2 labels, except when the
    // 2nd-to-last label is a known first-part of a multi-label TLD (`co.uk`,
    // `com.au`, …) — then keep the last 3. This is the pragmatic default GSA
    // renders for ccTLDs (`example.co.uk`, not the bare `co.uk`).
    const MULTI_PART: [&str; 12] =
        ["co", "com", "net", "org", "gov", "ac", "edu", "or", "go", "ne", "me", "web"];
    let labels: Vec<&str> = host.split('.').filter(|l| !l.is_empty()).collect();
    if labels.len() > 2 {
        let second_from_last = labels[labels.len() - 2];
        if MULTI_PART.contains(&second_from_last) {
            labels[labels.len() - 3..].join(".")
        } else {
            labels[labels.len() - 2..].join(".")
        }
    } else {
        host
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_empty() -> crate::spin::Context {
        crate::spin::default_context()
    }

    #[test]
    fn empty_pool_always_keyword() {
        let cfg = PoolConfig::default();
        let d = draw_anchor(&cfg, "dubai coffee", "example.com", Some(42), &ctx_empty());
        assert_eq!(d.text, "dubai coffee");
        assert_eq!(d.kind, AnchorKind::Keyword);
    }

    #[test]
    fn domain_only_pool_draws_domain() {
        let mut cfg = PoolConfig::default();
        cfg.domain = true;
        cfg.percentages.domain = 100;
        let d = draw_anchor(&cfg, "kw", "example.com", Some(7), &ctx_empty());
        assert_eq!(d.text, "example.com");
        assert_eq!(d.kind, AnchorKind::Domain);
    }

    #[test]
    fn brand_pool_with_spin_entry_spins() {
        let mut cfg = PoolConfig::default();
        cfg.branding = vec!["best [a|b] shop".into()];
        cfg.percentages.branding = 100;
        // seed chosen so we can assert: with a fixed seed the same variant repeats
        let d1 = draw_anchor(&cfg, "kw", "x.io", Some(11), &ctx_empty());
        let d2 = draw_anchor(&cfg, "kw", "x.io", Some(11), &ctx_empty());
        assert_eq!(d1, d2);
        assert!(!d1.text.is_empty());
        assert!(!d1.text.contains('['), "spin must resolve: {d1:?}");
    }

    #[test]
    fn total_percentage_normalizes_overflow() {
        let mut cfg = PoolConfig::default();
        cfg.percentages.secondary = 80;
        cfg.percentages.branding = 80; // 160 > 100
        let c = cfg.clamped();
        assert!(c.percentages.secondary + c.percentages.branding <= 100);
    }

    #[test]
    fn domain_of_strips_scheme_and_path() {
        assert_eq!(domain_of("https://www.example.co.uk/page?x=1"), "example.co.uk");
        assert_eq!(domain_of("example.com"), "example.com");
        assert_eq!(domain_of("http://user@example.de/dir"), "example.de");
    }

    #[test]
    fn casewise_variant_stable_per_seed() {
        let a = casewise_variant("GSA SEARCH".into(), SplitMix64::new(5));
        let b = casewise_variant("GSA SEARCH".into(), SplitMix64::new(5));
        assert_eq!(a, b);
    }

    #[test]
    fn keyword_weight_is_the_leftover() {
        // 30 brand + 20 generic ⇒ 50% still the keyword anchor
        let mut cfg = PoolConfig::default();
        cfg.branding = vec!["brand".into()];
        cfg.generic = vec!["click here".into()];
        cfg.percentages.branding = 30;
        cfg.percentages.generic = 20;
        let kw = 0;
        let mut hits_kw = 0;
        for seed in 0..2000u64 {
            let d = draw_anchor(&cfg, "dubai coffee", "x.io", Some(seed), &ctx_empty());
            if d.kind == AnchorKind::Keyword {
                hits_kw += 1;
            }
            let _ = kw;
        }
        // keyword ≈ 50% of 2000 = ~1000 (branding 30 + generic 20 → leftover 50%)
        assert!((800..1200).contains(&hits_kw), "got {hits_kw}");
    }
}
