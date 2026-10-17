//! F#9 — keyword management: normalization + spintax expansion.
//!
//! GSA treats a *project keyword set* as the source of both anchor text and
//! the search term.  Keywords may be plain text (`buy shoes online`) or
//! spintax (`best {shoes|boots} for {2025|2026}`).  Rules implemented here:
//!
//! * **normalize** — trim + collapse internal whitespace.  Used for
//!   dedupe/storage so `"  buy  shoes "` and "buy shoes" are the same row.
//! * **expand** — a spintax keyword fans out into its concrete variants
//!   (each variant is stored as its own keyword row), matching how GSA
//!   expands spintaxed keyword lists.
//! * **resolve_one** — at job-execution time a spintax keyword is spun to a
//!   single concrete variant, deterministically (pure function of seed).

use crate::spin::{default_context, SpintaxResolver};

/// Normalize a keyword for storage/dedup: trim + single-space collapse.
pub fn normalize(input: &str) -> String {
    let trimmed = input.trim();
    trimmed.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// True when the text still contains spintax or `%variable%` syntax.
pub fn has_spintax(input: &str) -> bool {
    input.contains('{') || input.contains('%')
}

/// Expand a keyword into concrete variants.
///
/// * plain keyword → single-element vec (normalized)
/// * spintax keyword → up to `max_variants` distinct resolutions, in a
///   deterministic order (seed `1, 2, 3…`), each normalized
pub fn expand(input: &str, max_variants: usize) -> Vec<String> {
    let n = normalize(input);
    if n.is_empty() {
        return Vec::new();
    }
    if !has_spintax(&n) {
        return vec![n];
    }
    let mut out: Vec<String> = Vec::new();
    // Try a bounded seed sequence — consecutive seeds often hit the same
    // branch of a spintax, so we sweep more seeds than we need variants.
    let attempts = max_variants.saturating_mul(12).max(12);
    for seed in 1..=attempts as u64 {
        let v = SpintaxResolver::new(seed).resolve(&n, &default_context());
        let v = normalize(&v);
        if v.is_empty() {
            continue;
        }
        if !out.iter().any(|x| x == &v) {
            out.push(v);
            if out.len() >= max_variants {
                break;
            }
        }
    }
    if out.is_empty() {
        out.push(n); // spintax that resolves to nothing — keep the raw form
    }
    out
}

/// Spin a keyword to ONE concrete variant at job-execution time.
///
/// plain keyword → returned normalized (unchanged content)
/// spintax keyword → `SpintaxResolver::new(seed)` resolution, then normalized.
/// `seed = None` uses the deterministic default (GSA-equivalent).
pub fn resolve_one(input: &str, seed: Option<u64>) -> String {
    let n = normalize(input);
    if !has_spintax(&n) {
        return n;
    }
    let s = seed.unwrap_or(SpintaxResolver::default().seed());
    let v = SpintaxResolver::new(s).resolve(&n, &default_context());
    normalize(&v)
}

// ===========================================================================
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_trims_and_collapses_whitespace() {
        assert_eq!(normalize("  buy   shoes \n online "), "buy shoes online");
        assert_eq!(normalize(""), "");
        assert_eq!(normalize("   "), "");
    }

    #[test]
    fn has_spintax_detects_both_syntaxes() {
        assert!(has_spintax("best {shoes|boots} store"));
        assert!(has_spintax("%anchor_text%"));
        assert!(!has_spintax("buy shoes online"));
    }

    #[test]
    fn expand_plain_keyword_is_single() {
        assert_eq!(expand("  buy  shoes ", 10), vec!["buy shoes".to_string()]);
        assert_eq!(expand("", 10), Vec::<String>::new());
    }

    #[test]
    fn expand_spintax_fans_out_distinctly() {
        let out = expand("best {shoes|boots} store", 10);
        assert_eq!(out.len(), 2, "got {out:?}");
        assert!(out.iter().any(|k| k == &"best shoes store".to_string()));
        assert!(out.iter().any(|k| k == &"best boots store".to_string()));
    }

    #[test]
    fn expand_spintax_respects_max_variants() {
        let out = expand("{a|b|c|d|e}", 2);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn resolve_one_is_deterministic_for_seed() {
        let a = resolve_one("best {shoes|boots} store", Some(7));
        let b = resolve_one("best {shoes|boots} store", Some(7));
        assert_eq!(a, b);
        assert!(!a.is_empty());
        assert!(!has_spintax(&a), "resolved keyword must be concrete: {a}");
    }

    #[test]
    fn resolve_one_plain_returns_normalized() {
        assert_eq!(resolve_one("  buy   shoes ", None), "buy shoes");
    }

    #[test]
    fn resolve_one_all_variants_eventually_appear_across_seeds() {
        let seen: Vec<String> = (1..=50).map(|s| resolve_one("best {shoes|boots} store", Some(s))).collect();
        assert!(seen.contains(&"best shoes store".to_string()));
        assert!(seen.contains(&"best boots store".to_string()));
    }
}
