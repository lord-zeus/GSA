//! F#16 — Stats & analytics (pure core).
//!
//! Faithful to GSA's "stats" dashboard: the user wants outcome **ratios**
//! — how many successes, how many hard failures, **how many were blocked by
//! captchas** — plus a **links-over-time** timeline, all sliced per engine
//! and per project.  GSA counts a captcha wall as its own metric (it is the
//! classic "this engine/site needs a proxy or a better fingerprint" signal),
//! NOT as an ordinary failure.
//!
//! All arithmetic here is pure (no sqlx, no IO) so it is fully unit-testable
//! the same way [crate::geo] and [crate::settings] are.  The DB layer
//! supplies the raw per-bucket counts; this module turns them into the
//! summary + timeline the UI renders.

use chrono::NaiveDate;
use serde::Serialize;

/// A single `status -> count` bucket straight from `jobs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StatusBucket {
    /// `"done" | "failed" | "pending" | "running" | "cancelled"`.
    pub status: String,
    pub count: i64,
}

/// A single `kind -> count` bucket straight from `results`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KindBucket {
    /// `"success" | "failure" | "rejected" | "error" | "info"`.
    pub kind: String,
    pub count: i64,
}

/// The global summary — the numbers on the top of the GSA stats page.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct StatsSummary {
    // jobs
    pub jobs_total: i64,
    pub jobs_done: i64,
    pub jobs_failed: i64,
    pub jobs_inflight: i64, // pending + running
    pub jobs_cancelled: i64,

    // results (submission outcomes)
    pub results_total: i64,
    pub successes: i64,
    pub failures: i64,
    pub rejected: i64,
    pub errors: i64,
    pub info: i64,

    // GSA-distinct metric: how many outcomes were captcha walls — tracked
    // separately from hard failures on purpose.
    pub captchas: i64,

    // de-duplicated created links (GSA's "unique links" figure)
    pub unique_links: i64,

    /// `successes / results_total`, 0.0 when there are no results.
    pub success_rate: f64,
}

/// One day of the links-over-time timeline.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TimelinePoint {
    /// `YYYY-MM-DD`.
    pub date: String,
    /// success rows that day (created links)
    pub links: i64,
    /// how many of that day's outcomes were captcha walls
    pub captchas: i64,
    /// non-success outcomes (failure + rejected + error + info)
    pub other: i64,
}

/// Per-engine rollup row (one row per engine that has activity).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EngineStat {
    pub engine_id: String,
    pub name: String,
    pub jobs: i64,
    pub successes: i64,
    pub failures: i64,
    pub captchas: i64,
    pub unique_links: i64,
    /// successes / (successes+failures), 0.0 if no completed work.
    pub success_rate: f64,
}

/// Per-project rollup row.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProjectStat {
    pub project_id: String,
    pub name: String,
    pub jobs: i64,
    pub successes: i64,
    pub failures: i64,
    pub captchas: i64,
    pub unique_links: i64,
    pub success_rate: f64,
}

/// The full dashboard payload the API serves.
#[derive(Debug, Clone, Serialize)]
pub struct Dashboard {
    pub summary: StatsSummary,
    /// One point per calendar day in `[today-(days-1) .. today]` — days with
    /// no activity are emitted with zeros (GSA's chart never has gaps).
    pub days: Vec<TimelinePoint>,
    pub engines: Vec<EngineStat>,
    pub projects: Vec<ProjectStat>,
}

/// Divide with a zero-denominator guard.  Used for every ratio.
pub fn ratio(part: i64, whole: i64) -> f64 {
    if whole <= 0 {
        0.0
    } else if part <= 0 {
        0.0
    } else {
        part as f64 / whole as f64
    }
}

/// Build the global summary from raw buckets.
///
/// `captchas` is passed in from the DB as a `COUNT(*) FILTER (captchas)`
/// over the results table — it is independent of `kind` (a success row can
/// still carry `captchas = true` when the page went through a captcha check
/// even though the submission completed).
pub fn summarize(
    status_buckets: &[StatusBucket],
    kind_buckets: &[KindBucket],
    captchas: i64,
    unique_links: i64,
) -> StatsSummary {
    let mut s = StatsSummary {
        jobs_total: 0,
        jobs_done: 0,
        jobs_failed: 0,
        jobs_inflight: 0,
        jobs_cancelled: 0,
        results_total: 0,
        successes: 0,
        failures: 0,
        rejected: 0,
        errors: 0,
        info: 0,
        captchas,
        unique_links,
        success_rate: 0.0,
    };
    for b in status_buckets {
        let c = b.count;
        s.jobs_total += c;
        match b.status.as_str() {
            "done" => s.jobs_done += c,
            "failed" => s.jobs_failed += c,
            "cancelled" => s.jobs_cancelled += c,
            // "pending", "running" + any unknown/in-progress state
            _ => s.jobs_inflight += c,
        }
    }
    for b in kind_buckets {
        let c = b.count;
        s.results_total += c;
        match b.kind.as_str() {
            "success" => s.successes += c,
            "failure" => s.failures += c,
            "rejected" => s.rejected += c,
            "error" => s.errors += c,
            "info" => s.info += c,
            _ => {}
        }
    }
    s.success_rate = ratio(s.successes, s.results_total);
    s
}

/// Turn per-day `(date, links, captchas, other)` rows into a continuous
/// series covering `[end-(days-1) .. end]`, inserting zero-days for the
/// gaps.  Rows outside the window are dropped; unknown future dates kept.
pub fn day_series(rows: &[(NaiveDate, i64, i64, i64)], days: u32, end: NaiveDate) -> Vec<TimelinePoint> {
    let d = days.max(1);
    let start = end - chrono::Duration::days((d as i64) - 1);
    // index rows by date (rows outside the window will simply not be read)
    let mut map: std::collections::BTreeMap<NaiveDate, (i64, i64, i64)> = std::collections::BTreeMap::new();
    for (date, links, captchas, other) in rows {
        let e = map.entry(*date).or_insert((0, 0, 0));
        e.0 += links;
        e.1 += captchas;
        e.2 += other;
    }
    (0..d)
        .map(|i| {
            let date = start + chrono::Duration::days(i as i64);
            let (links, captchas, other) = map.get(&date).copied().unwrap_or((0, 0, 0));
            TimelinePoint {
                date: date.format("%Y-%m-%d").to_string(),
                links,
                captchas,
                other,
            }
        })
        .collect()
}

/// Per-engine stat from a raw rollup row.
pub fn engine_stat(
    engine_id: &str,
    name: &str,
    jobs: i64,
    successes: i64,
    failures: i64,
    captchas: i64,
    unique_links: i64,
) -> EngineStat {
    EngineStat {
        engine_id: engine_id.to_string(),
        name: name.to_string(),
        jobs,
        successes,
        failures,
        captchas,
        unique_links,
        success_rate: ratio(successes, successes + failures),
    }
}

/// Per-project stat from a raw rollup row.
pub fn project_stat(
    project_id: &str,
    name: &str,
    jobs: i64,
    successes: i64,
    failures: i64,
    captchas: i64,
    unique_links: i64,
) -> ProjectStat {
    ProjectStat {
        project_id: project_id.to_string(),
        name: name.to_string(),
        jobs,
        successes,
        failures,
        captchas,
        unique_links,
        success_rate: ratio(successes, successes + failures),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sb(status: &str, count: i64) -> StatusBucket {
        StatusBucket { status: status.into(), count }
    }
    fn kb(kind: &str, count: i64) -> KindBucket {
        KindBucket { kind: kind.into(), count }
    }

    #[test]
    fn ratio_zero_denominator_is_zero() {
        assert_eq!(ratio(3, 0), 0.0);
        assert_eq!(ratio(0, 8), 0.0);
        assert!((ratio(1, 4) - 0.25).abs() < 1e-9);
    }

    #[test]
    fn summarize_empty_is_all_zero() {
        let s = summarize(&[], &[], 0, 0);
        assert_eq!(s.jobs_total, 0);
        assert_eq!(s.results_total, 0);
        assert_eq!(s.success_rate, 0.0);
    }

    #[test]
    fn summarize_buckets_and_success_rate() {
        let s = summarize(
            &[sb("done", 7), sb("failed", 2), sb("pending", 1), sb("cancelled", 1)],
            &[kb("success", 5), kb("failure", 2), kb("rejected", 1), kb("error", 1), kb("info", 1)],
            3,
            4,
        );
        assert_eq!(s.jobs_total, 11);
        assert_eq!(s.jobs_done, 7);
        assert_eq!(s.jobs_failed, 2);
        assert_eq!(s.jobs_inflight, 1);
        assert_eq!(s.jobs_cancelled, 1);
        assert_eq!(s.results_total, 10);
        assert_eq!(s.successes, 5);
        assert_eq!(s.failures, 2);
        assert_eq!(s.rejected, 1);
        assert_eq!(s.errors, 1);
        assert_eq!(s.info, 1);
        assert_eq!(s.captchas, 3);
        assert_eq!(s.unique_links, 4);
        assert!((s.success_rate - 0.5).abs() < 1e-9);
    }

    #[test]
    fn captchas_are_independent_of_kind() {
        // 5 captcha outcomes but they were recorded as failures (the common
        // case) — captchas is its own number, NOT subsumed by failures.
        let s = summarize(&[], &[kb("failure", 5), kb("success", 5)], 5, 5);
        assert_eq!(s.failures, 5);
        assert_eq!(s.captchas, 5);
        assert!((s.success_rate - 0.5).abs() < 1e-9);
    }

    #[test]
    fn day_series_fills_gaps_with_zeros() {
        let t = NaiveDate::from_ymd_opt(2026, 10, 15).unwrap();
        let rows = vec![
            (t - chrono::Duration::days(3), 4, 1, 2),
            (t, 6, 0, 1),
        ];
        let series = day_series(&rows, 5, t);
        assert_eq!(series.len(), 5);
        assert_eq!(series[0].links, 0, "first day empty");
        assert_eq!(series[1].links, 4, "t-3 lands at index 1 of the 5-day window");
        assert_eq!(series[1].captchas, 1);
        assert_eq!(series[2].links, 0);
        assert_eq!(series[3].links, 0, "t-1 has no activity");
        assert_eq!(series[4].links, 6, "end day");
        assert_eq!(series[4].other, 1);
        assert_eq!(series[0].date, (t - chrono::Duration::days(4)).format("%Y-%m-%d").to_string());
        assert_eq!(series[4].date, t.format("%Y-%m-%d").to_string());
    }

    #[test]
    fn day_series_min_clamps_to_one() {
        let t = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let s = day_series(&[], 0, t);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].date, "2026-01-01");
    }

    #[test]
    fn engine_and_project_stats_compute_success_rate() {
        let e = engine_stat("eng-7", "Topsites", 10, 4, 6, 2, 3);
        assert!((e.success_rate - 0.4).abs() < 1e-9);
        let p = project_stat("proj-9", "Demo", 4, 0, 2, 1, 0);
        assert_eq!(p.success_rate, 0.0);
    }
}
