//! Stage B — multi-family backlink selection.
//!
//! A GSA SER project posts many KINDS of backlinks, and the right play for
//! any single target is a MIX: an *article* if the site takes an article, a
//! *comment* if it takes a comment, and a *short link* for plain redirects.
//! Picking a single engine and hoping is why most SER runs produce noise.
//!
//! So `ser-replica` classifies every available engine into one of the three
//! posting families and ranks the strongest candidates inside each family by
//! the GSA signals that actually predict a clean, dofollow, low-friction
//! link:
//!
//!   * **dofollow-first** — the link must carry follow (the whole point of
//!     a SER run); nofollow engines sink to the bottom.
//!   * **liveness** — a reachable site beats an unknown one; a confirmed-dead
//!     site is penalised.  (We don't probe here — the caller supplies the
//!     probe result if it has one.)
//!   * **friction** — fewer form fields, and a single `just download` GET,
//!     mean less to break (captchas, missing field tokens) at run time.
//!
//! The result is a `BTreeMap<Family, Vec<usize>>` — the top-N candidate
//! indices (into the caller's engine list) for each family that actually has
//! candidates.  Families with no matching engines simply absent from the map,
//! so a caller enqueues exactly the backlink kinds the project can back.

use crate::enginespec::EngineType;
use std::collections::BTreeMap;

/// The three posting families a SER project should cover in a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Family {
    /// Full-page backlinks: article, web-2.0, wiki, directory, sharing.
    /// Highest value, heaviest form (multi-field, often captcha).
    Article,
    /// Inline backlinks: forum, guestbook, blog/image comment, microblog,
    /// track/pingback.  Lighter than articles; fast to submit.
    Comment,
    /// Plain redirects: URL shortener, referrer, rss, indexer, social link.
    /// Cheapest and most reliable backlink type (single GET in the best case).
    Link,
}

impl Family {
    pub fn as_str(self) -> &'static str {
        match self {
            Family::Article => "article",
            Family::Comment => "comment",
            Family::Link => "link",
        }
    }

    /// One of the three posting families, or `None` for engine kinds that are
    /// never a target of a "post a backlink to this site" run (email blasts,
    /// video uploads, exploit/indexer helpers, unknown).
    pub fn for_type(t: &EngineType) -> Option<Family> {
        use EngineType as T;
        Some(match t {
            // ── Article / full-page ──
            T::Article | T::Web20 | T::Wiki | T::Directory | T::DocumentSharing => Family::Article,
            // ── Comment / inline ──
            T::Forum | T::Guestbook | T::BlogComment | T::ImageComment
            | T::Microblog
            | T::Trackback
            | T::Pingback => Family::Comment,
            // ── Link / redirect ──
            T::UrlShortener | T::Referrer | T::Rss | T::Indexer
            | T::SocialBookmark
            | T::SocialNetwork => Family::Link,
            // ── Not a "post to this site" backlink kind ──
            T::Unknown | T::Email | T::Video | T::VideoAdult | T::Exploit => return None,
        })
    }
}

/// A single engine candidate, already reduced to the signals the ranker uses.
///
/// `is_live` is the caller's liveness probe result when it has one
/// (`Some(true)` reachable, `Some(false)` confirmed dead, `None` unknown /
/// not probed).  `field_count` is the number of form fields the engine must
/// fill — higher is more friction.
pub struct CandidateEngine {
    pub engine_name: String,
    pub engine_type: EngineType,
    pub dofollow: bool,
    pub field_count: usize,
    pub just_download: bool,
    pub is_enabled: bool,
    pub is_live: Option<bool>,
}

/// Rank a single candidate.  Higher is better.  Kept as a tiny, documented
/// point function so the weights are inspectable at one place.
pub fn score(c: &CandidateEngine) -> i64 {
    if !c.is_enabled {
        return -1_000_000;
    }
    let mut s = 0i64;
    // dofollow-first
    if c.dofollow {
        s += 40;
    } else {
        s -= 20;
    }
    // liveness: known-good > unknown > known-dead
    match c.is_live {
        Some(true) => s += 30,
        Some(false) => s -= 25,
        None => s += 3,
    }
    // friction: fewer fields is better; a single-GET engine is cleanest
    if c.just_download {
        s += 12;
    }
    s -= i64::min(c.field_count as i64, 24);
    s
}

/// Pick the best `per_family` candidate indices (into `cands`) for every
/// family that actually has candidates.  Disabled engines are excluded
/// (scored to the floor).  Families with no surviving candidates are omitted.
pub fn best_per_family(cands: &[CandidateEngine], per_family: usize) -> BTreeMap<Family, Vec<usize>> {
    let per = per_family.max(1);
    let mut idx_by_family: BTreeMap<Family, Vec<(i64, usize)>> = BTreeMap::new();
    for (i, c) in cands.iter().enumerate() {
        let Some(fam) = Family::for_type(&c.engine_type) else {
            continue;
        };
        idx_by_family.entry(fam).or_default().push((score(c), i));
    }
    let mut out = BTreeMap::new();
    for (fam, mut scored) in idx_by_family {
        // only enabled candidates are eligible (disabled are scored -1e6); drop them
        let mut eligible: Vec<(i64, usize)> = scored
            .into_iter()
            .filter(|(_, i)| cands[*i].is_enabled)
            .collect();
        if eligible.is_empty() {
            continue;
        }
        eligible.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
        let mut top: Vec<usize> = Vec::with_capacity(per);
        for (_, i) in eligible.iter() {
            top.push(*i);
            if top.len() >= per {
                break;
            }
        }
        out.insert(fam, top);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(t: EngineType, dofollow: bool, fields: usize, jd: bool) -> CandidateEngine {
        CandidateEngine {
            engine_name: t.to_string().to_string(),
            engine_type: t,
            dofollow,
            field_count: fields,
            just_download: jd,
            is_enabled: true,
            is_live: None,
        }
    }

    /// A GSA-style mixed pool: one engine per kind.  The ranker must group
    /// them into the three families and prefer the dofollow / low-friction
    /// engines inside each.
    #[test]
    fn multi_family_selection_groups_and_ranks() {
        use EngineType as T;
        let pool = vec![
            c(T::Web20,      true,  6, false), // Article family, dofollow, heavier
            c(T::Article,    false, 4, false), // Article family, nofollow
            c(T::Directory,  true,  3, false), // Article family, dofollow, light
            c(T::Forum,      true,  3, false), // Comment family, dofollow
            c(T::Guestbook,  true,  2, false), // Comment family, dofollow, lightest
            c(T::BlogComment,true,  5, false), // Comment family, heavier
            c(T::UrlShortener,true,1, true ),  // Link family, dofollow, single GET
            c(T::Directory,  true,  2, false), // 2nd Directory (another Article cand)
        ];
        let sel = best_per_family(&pool, 2);

        // All three families present, because the pool can back each one.
        assert!(sel.contains_key(&Family::Article));
        assert!(sel.contains_key(&Family::Comment));
        assert!(sel.contains_key(&Family::Link));

        // Each family returns at most `per_family` candidates.
        assert!(sel[&Family::Article].len() <= 2);
        assert!(sel[&Family::Comment].len() <= 2);
        assert!(sel[&Family::Link].len() <= 2);

        // Inside the Link family, the dofollow single-GET shortener wins.
        assert_eq!(sel[&Family::Link][0], 6);
        // Inside the Comment family, the dofollow guestbook (fewest fields,
        // index 4) outranks the forum (index 3), which outranks the heavier
        // blog-comment (index 5).  Top-2 = [4, 3].
        let comment = &sel[&Family::Comment];
        assert_eq!(comment.as_slice(), [4, 3].as_slice(),
            "top-2 Comment must be Guestbook (idx 4) then Forum (idx 3)");
    }

    /// A nofollow engine never beats a dofollow one in the same family.
    #[test]
    fn dofollow_beats_nofollow_in_family() {
        use EngineType as T;
        let pool = vec![
            c(T::Article, false, 1, false),
            c(T::Article, true, 8, false), // dofollow but heavier
        ];
        let sel = best_per_family(&pool, 2);
        let article = &sel[&Family::Article];
        assert!(article.iter().position(|&i| i == 1).unwrap()
            < article.iter().position(|&i| i == 0).unwrap(),
            "dofollow (index 1) must outrank the nofollow (index 0)");
    }

    /// A confirmed-live engine edges out a confirmed-dead one in a family.
    #[test]
    fn live_beats_dead_in_family() {
        use EngineType as T;
        let dead = CandidateEngine {
            is_live: Some(false),
            dofollow: true,
            ..c(T::Web20, true, 4, false)
        };
        let live = CandidateEngine {
            is_live: Some(true),
            dofollow: true,
            ..c(T::Web20, true, 4, false)
        };
        let pool = vec![dead, live];
        let sel = best_per_family(&pool, 2);
        assert_eq!(sel[&Family::Article][0], 1, "the live engine must rank first");
    }

    /// Non-postable engine kinds (email, video, unknown) never form a family
    /// on their own — a pool of only those returns an empty selection.
    #[test]
    fn non_postable_kinds_yield_no_family() {
        use EngineType as T;
        let pool = vec![
            c(T::Email,    true, 1, false),
            c(T::Video,    true, 1, false),
            c(T::Unknown,  true, 1, false),
        ];
        assert!(best_per_family(&pool, 3).is_empty());
    }

    /// `per_family` caps the result: 5 candidates in a family, ask for 2 →
    /// exactly 2 back, in best-first order.
    #[test]
    fn per_family_caps_the_result() {
        use EngineType as T;
        let pool = vec![
            c(T::Web20, true, 6, false),
            c(T::Wiki,  true, 5, false),
            c(T::Article, true, 4, false),
            c(T::Directory, true, 3, false),
            c(T::Directory, true, 2, false),
        ];
        let sel = best_per_family(&pool, 2);
        let article = &sel[&Family::Article];
        assert_eq!(article.len(), 2, "per_family must cap at 2");
        // The two lightest-dofollow engines (indices 4, 3) must be the picks.
        assert_eq!(article.as_slice(), [4, 3].as_slice());
    }

    /// Disabled candidates are excluded from the selection entirely.
    #[test]
    fn disabled_candidate_is_excluded() {
        use EngineType as T;
        let enabled = c(T::UrlShortener, true, 1, true);
        let disabled = CandidateEngine {
            is_enabled: false,
            dofollow: true,
            just_download: true,
            ..c(T::UrlShortener, true, 1, true)
        };
        // disabled first (index 0), enabled second (index 1) — selection must
        // return index 1, never the disabled 0.
        let pool = vec![disabled, enabled];
        let sel = best_per_family(&pool, 5);
        let link = &sel[&Family::Link];
        assert_eq!(link, &vec![1], "disabled engine (index 0) must NOT be picked");
    }
}
