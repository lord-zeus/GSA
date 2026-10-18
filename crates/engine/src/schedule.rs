//! F#17 — pure schedule core for per-project scheduled runs (GSA "Scheduled
//! Posting" / the scheduler subsystem).
//!
//! DB-agnostic on purpose (no sqlx/tokio/axum): this module holds the typed
//! schedule model and the pure `next_run` algorithm that BOTH the API
//! scheduler loop and the Vue UI call. Keeping the semantics here means
//! they are unit-testable without Postgres.
//!
//! GSA fidelity (from `Search_Engine_Ranker_FREE.exe` scheduler strings:
//! `scheduler start date` / `start time`, `stop date` / `stop time`,
//! per-project `scheduled posting`):
//!   * a project schedule fires a Run (the same in-process pool as
//!     `POST /api/projects/{id}/run`) on a calendar;
//!   * `once` = one-off at a specific time; `recurring` = every
//!     `repeat_every_minutes` from `start_at`, optionally weekday-scoped
//!     and/or constrained to an active window;
//!   * `stop_at` hard-stops; `max_runs` caps the total fire count.
//!
//! Time model: all values are naive UTC (the DB stores timestamptz; the UI
//! renders local and the API converts to naive-UTC on write — see the API
//! handlers for the exact conversion). The active window is a wall-clock
//! window in UTC.
//!
//! `next_run(spec, now) -> Option<NaiveDateTime>` returns the first
//! eligible fire time `t >= now`, or `None` if the schedule is retired
//! (stop passed / cap hit / once consumed) or no eligible slot exists
//! within the one-year horizon. The API claims the result atomically in
//! Postgres (`Repo::claim_schedule`), so a restart or a double tick cannot
//! double-fire one slot.

use chrono::{Datelike, Duration, NaiveDateTime, NaiveTime};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeSet;
use thiserror::Error;

// Serde module: NaiveDateTime / NaiveTime don't have stable Deserialize
// implementations (the `serde` feature of those types is unstable). We
// hand-roll the (de)serializer to string form — the wire shape used by
// both the API JSON layer and the Vue client.
mod serde_naive {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize_dt<S>(dt: &chrono::NaiveDateTime, ser: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        ser.serialize_str(&chrono::NaiveDateTime::format(dt, super::DATE_TIME_FMT).to_string())
    }
    pub fn deserialize_dt<'de, D>(de: D) -> Result<chrono::NaiveDateTime, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(de)?;
        chrono::NaiveDateTime::parse_from_str(&s, super::DATE_TIME_FMT)
            .map_err(serde::de::Error::custom)
    }
    pub fn serialize_time<S>(t: &chrono::NaiveTime, ser: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        ser.serialize_str(&chrono::NaiveTime::format(t, "%H:%M:%S").to_string())
    }
    pub fn deserialize_time<'de, D>(de: D) -> Result<chrono::NaiveTime, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(de)?;
        chrono::NaiveTime::parse_from_str(&s, "%H:%M:%S").map_err(serde::de::Error::custom)
    }
}

// Option<NaiveDateTime> / Option<NaiveTime> (de)serializers — used by the
// #[serde(serialize_with / deserialize_with)] annotations on the optional
// fields of `ScheduleSpec`.
fn opt_dt_ser<S>(v: &Option<chrono::NaiveDateTime>, ser: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match v {
        Some(dt) => ser.serialize_some(&chrono::NaiveDateTime::format(dt, DATE_TIME_FMT).to_string()),
        None => ser.serialize_none(),
    }
}
fn opt_dt_de<'de, D>(de: D) -> Result<Option<chrono::NaiveDateTime>, D::Error>
where
    D: Deserializer<'de>,
{
    let s: Option<String> = Option::deserialize(de)?;
    match s {
        Some(s) => Ok(Some(
            chrono::NaiveDateTime::parse_from_str(&s, DATE_TIME_FMT)
                .map_err(serde::de::Error::custom)?,
        )),
        None => Ok(None),
    }
}
fn opt_time_ser<S>(v: &Option<chrono::NaiveTime>, ser: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match v {
        Some(t) => ser.serialize_some(&chrono::NaiveTime::format(t, "%H:%M:%S").to_string()),
        None => ser.serialize_none(),
    }
}
fn opt_time_de<'de, D>(de: D) -> Result<Option<chrono::NaiveTime>, D::Error>
where
    D: Deserializer<'de>,
{
    let s: Option<String> = Option::deserialize(de)?;
    match s {
        Some(s) => Ok(Some(
            chrono::NaiveTime::parse_from_str(&s, "%H:%M:%S").map_err(serde::de::Error::custom)?,
        )),
        None => Ok(None),
    }
}

/// Two scheduling modes, mirroring GSA's recurring `scheduled posting` vs a
/// one-off run at a fixed `scheduler start date` / `time`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScheduleMode {
    /// Fire exactly once at `start_at`. After that time has passed,
    /// `next_run` returns `None` forever (until the operator edits the row).
    Once,
    /// Fire repeatedly: first at `start_at`, then every
    /// `repeat_every_minutes`, gated by the weekday filter and active
    /// window when present.
    Recurring,
}

/// Optional ISO-weekday filter — 1 = Monday ... 7 = Sunday. `None` = allow
/// every day; `Some(mask)` = allow only the set's weekdays. JSON shape is
/// a sorted array of integers (e.g. `[1,3,5]`) matching the UI checkbox
/// group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(transparent)]
pub struct WeekdayMask(BTreeSet<i64>);

impl WeekdayMask {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn add(&mut self, d: i64) {
        if (1..=7).contains(&d) {
            self.0.insert(d);
        }
    }
    pub fn contains(&self, d: i64) -> bool {
        self.0.contains(&d)
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    /// True if the date's weekday is allowed (empty mask = unrestricted).
    pub fn allows(&self, d: chrono::NaiveDate) -> bool {
        let wd = d.weekday().num_days_from_monday() as i64 + 1;
        self.is_empty() || self.0.contains(&wd)
    }
    pub fn to_vec(&self) -> Vec<i64> {
        self.0.iter().copied().collect()
    }
}

impl From<Vec<i64>> for WeekdayMask {
    fn from(v: Vec<i64>) -> Self {
        let mut m = Self::new();
        for x in v {
            m.add(x);
        }
        m
    }
}

/// The pure schedule model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScheduleSpec {
    /// Display name (UI).
    pub name: String,
    /// `once` or `recurring`.
    pub mode: ScheduleMode,
    /// Naive UTC. For `once` this is the ONE trigger; for `recurring` it is
    /// the first eligible anchor (and the base of the cadence grid).
    #[serde(
        serialize_with = "serde_naive::serialize_dt",
        deserialize_with = "serde_naive::deserialize_dt"
    )]
    pub start_at: NaiveDateTime,
    /// Optional hard stop (UTC). Once `now > stop_at`, the schedule is
    /// retired: `next_run` returns `None`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "opt_dt_ser",
        deserialize_with = "opt_dt_de"
    )]
    pub stop_at: Option<NaiveDateTime>,
    /// Cadence in minutes (1..=525600). Required for `recurring`; ignored
    /// for `once`.
    #[serde(default)]
    pub repeat_every_minutes: Option<u32>,
    /// Optional weekday filter (`None` = any day).
    #[serde(default)]
    pub repeat_weekdays: Option<WeekdayMask>,
    /// Optional UTC wall-clock active window (both sides = bounded window;
    /// one side = one-sided bound; `None` = unrestricted).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "opt_time_ser",
        deserialize_with = "opt_time_de"
    )]
    pub window_start: Option<NaiveTime>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "opt_time_ser",
        deserialize_with = "opt_time_de"
    )]
    pub window_end: Option<NaiveTime>,
    /// Optional total-fire cap. Once `run_count >= max_runs`, retired.
    #[serde(default)]
    pub max_runs: Option<i64>,
    /// How many times the schedule has actually fired (DB increment).
    #[serde(default)]
    pub run_count: i64,
}

#[derive(Debug, Error)]
pub enum ScheduleError {
    #[error("recurring schedules need a repeat interval (1..=525600 minutes)")]
    RecurringNeedsInterval,
    #[error("stop_at must be after start_at when present")]
    StopBeforeStart,
    #[error("max_runs must be a positive integer")]
    BadMaxRuns,
}

impl ScheduleSpec {
    /// Validate the invariant set. The API calls this on create/update;
    /// `next_run` assumes a valid spec.
    pub fn validate(&self) -> Result<(), ScheduleError> {
        if self.mode == ScheduleMode::Recurring {
            match self.repeat_every_minutes {
                Some(mins) if (1..=525_600).contains(&mins) => {}
                _ => return Err(ScheduleError::RecurringNeedsInterval),
            }
        }
        if let Some(stop) = self.stop_at {
            if stop < self.start_at {
                return Err(ScheduleError::StopBeforeStart);
            }
        }
        if let Some(cap) = self.max_runs {
            if cap <= 0 {
                return Err(ScheduleError::BadMaxRuns);
            }
        }
        Ok(())
    }

    fn retired_by_cap(&self) -> bool {
        match self.max_runs {
            Some(cap) => self.run_count >= cap,
            None => false,
        }
    }

    fn retired_by_stop(&self, now: NaiveDateTime) -> bool {
        self.stop_at.map(|s| now > s).unwrap_or(false)
    }

    fn in_window(&self, t: NaiveDateTime) -> bool {
        match (self.window_start, self.window_end) {
            (None, None) => true,
            (Some(s), None) => t.time() >= s,
            (None, Some(e)) => t.time() <= e,
            (Some(s), Some(e)) => {
                if s <= e {
                    t.time() >= s && t.time() <= e
                } else {
                    // Wrap window (e.g. 22:00..06:00): allowed if past
                    // window start OR before window end.
                    t.time() >= s || t.time() <= e
                }
            }
        }
    }

    fn weekday_ok(&self, t: NaiveDateTime) -> bool {
        match &self.repeat_weekdays {
            None => true,
            Some(mask) => mask.allows(t.date()),
        }
    }

    /// The next eligible fire at or after `now`, or `None` if the schedule
    /// is retired (stop passed / cap hit / once consumed) or no eligible
    /// slot exists within the one-year horizon.
    pub fn next_run(&self, now: NaiveDateTime) -> Option<NaiveDateTime> {
        if self.retired_by_cap() || self.retired_by_stop(now) {
            return None;
        }
        match self.mode {
            ScheduleMode::Once => {
                let c = self.start_at;
                if c < now {
                    return None; // consumed / missed
                }
                if let Some(stop) = self.stop_at {
                    if c > stop {
                        return None;
                    }
                }
                if !self.in_window(c) || !self.weekday_ok(c) {
                    return None;
                }
                Some(c)
            }
            ScheduleMode::Recurring => {
                let interval = (self.repeat_every_minutes? as i64).max(1);
                let dur = Duration::minutes(interval);
                // Candidate grid: start_at + k*interval, smallest k with
                //   start_at + k*interval >= now.
                let mut c = if self.start_at > now {
                    self.start_at
                } else {
                    let minutes = (now - self.start_at).num_minutes();
                    // Round up (ceil) the number of intervals so we land on
                    // the first grid point at or after `now`.
                    let k = if minutes % interval == 0 {
                        minutes / interval
                    } else {
                        minutes / interval + if minutes >= 0 { 1_i64 } else { -1_i64 }
                    };
                    self.start_at + Duration::minutes(k * interval)
                };
                // Bounded walk (prevents infinite loops on a schedule whose
                // window never matches).
                let horizon = now + Duration::days(365);
                loop {
                    if c > horizon {
                        return None;
                    }
                    if let Some(stop) = self.stop_at {
                        if c > stop {
                            return None;
                        }
                    }
                    if self.in_window(c) && self.weekday_ok(c) {
                        return Some(c);
                    }
                    c = c + dur;
                }
            }
        }
    }
}

impl Default for ScheduleSpec {
    fn default() -> Self {
        Self {
            name: String::new(),
            mode: ScheduleMode::Once,
            start_at: NaiveDateTime::new(
                chrono::NaiveDate::from_ymd_opt(1970, 1, 1).unwrap(),
                NaiveTime::from_hms_opt(0, 0, 0).unwrap(),
            ),
            stop_at: None,
            repeat_every_minutes: None,
            repeat_weekdays: None,
            window_start: None,
            window_end: None,
            max_runs: None,
            run_count: 0,
        }
    }
}

/// Parse helpers for API / DB boundaries — the Vue UI emits
/// `YYYY-MM-DD HH:MM:SS` (naive UTC) and the API stores the same string in
/// the `timestamptz` column (Postgres stores it relative to the session
/// TZ; for this local replica the session TZ is fixed to UTC via the
/// `SEER_DATABASE_URL` query param `?options=-c%20timezone%3Dutc`).
pub fn parse_dt(s: &str) -> Option<NaiveDateTime> {
    NaiveDateTime::parse_from_str(s, DATE_TIME_FMT).ok()
}
pub fn parse_time(s: &str) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(s, "%H:%M:%S").ok()
}
pub const DATE_TIME_FMT: &str = "%Y-%m-%d %H:%M:%S";

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(y: i32, m: u32, d: u32, h: u32, min: u32, s: u32) -> NaiveDateTime {
        let d = chrono::NaiveDate::from_ymd_opt(y, m, d).unwrap();
        let t = NaiveTime::from_hms_opt(h, min, s).unwrap();
        NaiveDateTime::new(d, t)
    }

    fn once(start: NaiveDateTime) -> ScheduleSpec {
        ScheduleSpec {
            name: "once".into(),
            mode: ScheduleMode::Once,
            start_at: start,
            ..Default::default()
        }
    }

    fn recurring(start: NaiveDateTime, mins: u32) -> ScheduleSpec {
        ScheduleSpec {
            name: "rec".into(),
            mode: ScheduleMode::Recurring,
            start_at: start,
            repeat_every_minutes: Some(mins),
            ..Default::default()
        }
    }

    #[test]
    fn once_in_future_fires_at_start() {
        let s = once(dt(2026, 10, 20, 12, 0, 0));
        let now = dt(2026, 10, 18, 9, 0, 0);
        assert_eq!(s.next_run(now), Some(dt(2026, 10, 20, 12, 0, 0)));
    }

    #[test]
    fn once_consumed_returns_none() {
        let s = once(dt(2026, 10, 18, 9, 0, 0));
        assert_eq!(s.next_run(dt(2026, 10, 18, 9, 30, 0)), None);
    }

    #[test]
    fn recurring_aligns_to_grid_from_anchor() {
        // Anchor 09:00, every 30 min. Now 09:10 -> next slot 09:30.
        let s = recurring(dt(2026, 10, 18, 9, 0, 0), 30);
        assert_eq!(
            s.next_run(dt(2026, 10, 18, 9, 10, 0)),
            Some(dt(2026, 10, 18, 9, 30, 0))
        );
    }

    #[test]
    fn recurring_at_exact_slot_fires_now() {
        let s = recurring(dt(2026, 10, 18, 9, 0, 0), 30);
        assert_eq!(
            s.next_run(dt(2026, 10, 18, 9, 30, 0)),
            Some(dt(2026, 10, 18, 9, 30, 0))
        );
    }

    #[test]
    fn recurring_weekday_gate() {
        // 2026-10-18 (Sunday) — restrict to Mon (1). Next eligible slot is
        // 2026-10-19 09:00.
        let mut s = recurring(dt(2026, 10, 18, 9, 0, 0), 1440);
        let mut m = WeekdayMask::new();
        m.add(1); // Monday only
        s.repeat_weekdays = Some(m);
        // Grid is daily at 09:00 (Sun, Mon, Tue...); Sunday is out, so the
        // first eligible slot is Monday 09:00.
        assert_eq!(
            s.next_run(dt(2026, 10, 18, 8, 0, 0)),
            Some(dt(2026, 10, 19, 9, 0, 0))
        );
    }

    #[test]
    fn recurring_window_gate() {
        // window 09:00..17:00, every 30 min, anchor 08:00. From 08:05 the
        // next eligible slot is 09:00.
        let mut s = recurring(dt(2026, 10, 18, 8, 0, 0), 30);
        s.window_start = Some(NaiveTime::from_hms_opt(9, 0, 0).unwrap());
        s.window_end = Some(NaiveTime::from_hms_opt(17, 0, 0).unwrap());
        assert_eq!(
            s.next_run(dt(2026, 10, 18, 8, 5, 0)),
            Some(dt(2026, 10, 18, 9, 0, 0))
        );
    }

    #[test]
    fn recurring_wrap_window() {
        // window 22:00..06:00 (wrap), every 120 min, anchor 20:00. From
        // 20:05 next eligible slot is 22:00.
        let mut s = recurring(dt(2026, 10, 18, 20, 0, 0), 120);
        s.window_start = Some(NaiveTime::from_hms_opt(22, 0, 0).unwrap());
        s.window_end = Some(NaiveTime::from_hms_opt(6, 0, 0).unwrap());
        assert_eq!(
            s.next_run(dt(2026, 10, 18, 20, 5, 0)),
            Some(dt(2026, 10, 18, 22, 0, 0))
        );
    }

    #[test]
    fn stop_at_retires_schedule() {
        let mut s = recurring(dt(2026, 10, 18, 9, 0, 0), 60);
        s.stop_at = Some(dt(2026, 10, 18, 12, 0, 0));
        assert_eq!(
            s.next_run(dt(2026, 10, 18, 9, 0, 0)),
            Some(dt(2026, 10, 18, 9, 0, 0))
        );
        assert_eq!(s.next_run(dt(2026, 10, 18, 12, 30, 0)), None);
    }

    #[test]
    fn cap_retires_schedule() {
        let mut s = recurring(dt(2026, 10, 18, 9, 0, 0), 60);
        s.max_runs = Some(2);
        s.run_count = 2;
        assert_eq!(s.next_run(dt(2026, 10, 18, 9, 0, 0)), None);
    }

    #[test]
    fn validate_rejects_recurring_without_interval() {
        let mut s = ScheduleSpec::default();
        s.mode = ScheduleMode::Recurring;
        assert!(matches!(
            s.validate(),
            Err(ScheduleError::RecurringNeedsInterval)
        ));
    }

    #[test]
    fn validate_rejects_stop_before_start() {
        let mut s = ScheduleSpec::default();
        s.start_at = dt(2026, 10, 20, 12, 0, 0);
        s.stop_at = Some(dt(2026, 10, 20, 11, 0, 0));
        assert!(matches!(
            s.validate(),
            Err(ScheduleError::StopBeforeStart)
        ));
    }

    #[test]
    fn validate_rejects_nonpositive_cap() {
        let mut s = ScheduleSpec::default();
        s.max_runs = Some(0);
        assert!(matches!(s.validate(), Err(ScheduleError::BadMaxRuns)));
    }
}
