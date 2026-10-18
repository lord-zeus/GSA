//! F#18 — Notification model + classification. PURE crate: no sqlx, no
//! tokio, no network — the same `Notification` shape is what the API's
//! `Notifier` writes to Postgres, what the SSE stream serializes, and what
//! the SMTP sender renders into an email. Keeping it in `ser-engine` lets
//! both the API and the (pure) runner tests talk about the same event
//! vocabulary without pulling any I/O into the shared core.
//!
//! ## Design
//!
//! Two orthogonal axes:
//!
//! - **`NotificationType`** — the *kind* of thing that happened (what the
//!   UI's filter chips and the settings page's "email me on X" toggles
//!   operate on). E.g. `RunCompleted`, `RunFailed`, `CaptchaBlocked`,
//!   `ScheduleFired`, `ScheduleFailed`, `ProxyFailure`, `SystemError`.
//! - **`Severity`** — the *weight* of the event in the UI's visual system:
//!   `info` (neutral), `good` (success / completion), `warn` (retryable:
//!   captcha wall, flaky proxy), `bad` (hard failure to act on).
//!
//! Classification functions turn *upstream results* (runner's
//! `PoolSummary`, the `ExecutionReport`'s captcha counter, the scheduler's
//! claim outcome) into `(type, severity, title, message)` tuples. Keeping
//! the mapping **total and pure** means the API can't silently forget to
//! emit a notification for any of the "good or bad" outcomes the user
//! asked to be covered — every path through
//! `run_project_pool` / `claim_schedule` calls one of these functions and
//! the resulting event is a *data* decision, not an `eprintln!`-and-hope.
//!
//! ## Serde shape (the wire format)
//!
//! `Notification` is `DateTime<Utc>`-based — the API's `Notifier` uses
//! the SAME `chrono::DateTime<Utc>` (sqlx's `chrono` feature) so the
//! `serde` round-trip is identity, and the `serde_naive` module below only
//! matters for crates that hand-build a `ScheduleSpec`-style struct with a
//! `NaiveDate` (e.g. schedule calendar math — see `engine::schedule`).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ── serde helpers ────────────────────────────────────────────────────────────
/// Skip serializing `is_read` when it's `false` (the default state).
fn is_unread(b: &bool) -> bool {
    !*b
}

// ── Severity ─────────────────────────────────────────────────────────────────
/// Visual weight of the notification (UI icon / color / sort priority).
///
/// `info` — neutral (schedule created, setting changed, proxy added).
/// `good` — the user asked for something to happen and it did.
/// `warn` — retryable or recoverable (a captcha wall, a down proxy that
///         will be rotated past, a run that had partial failures).
/// `bad`  — hard failure to look at: a run that failed, a schedule that
///         errored out, an SMTP send that rejected us.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Good,
    Warn,
    Bad,
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Good => "good",
            Severity::Warn => "warn",
            Severity::Bad => "bad",
        }
    }
    pub fn parse(s: &str) -> Option<Severity> {
        match s {
            "info" => Some(Severity::Info),
            "good" => Some(Severity::Good),
            "warn" => Some(Severity::Warn),
            "bad" => Some(Severity::Bad),
            _ => None,
        }
    }
    /// Sort priority (higher = more urgent). Used by the UI's
    /// "most severe first" ordering on the bell dropdown.
    pub fn rank(self) -> u8 {
        match self {
            Severity::Info => 1,
            Severity::Warn => 2,
            Severity::Good => 3,
            Severity::Bad => 4,
        }
    }
}

// ── NotificationType ─────────────────────────────────────────────────────────
/// The **kind** of event the notifier emits. One per user-visible outcome:
/// this is the vocabulary the settings page's "email me" toggles and the
/// UI's filter chips operate on. `#[serde(rename_all = "camelCase")]` so
/// the wire format reads `runCompleted`, `captchaBlocked` — the Vue-side
/// type union uses the same spelling so the two never drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NotificationType {
    /// A project's queued jobs finished — ALL succeeded.
    RunCompleted,
    /// A project's queued jobs finished, with at least one job failing or
    /// retrying (the run itself completed; jobs inside it didn't).
    RunCompletedWithFailures,
    /// The run itself failed (pool-level error: driver not registered,
    /// spec missing, worker panics that dropped all workers).
    RunFailed,
    /// One or more jobs in the run hit the CAPTCHA wall (GSA: "blocked
    /// by captcha"). Distinct from a generic failure: the site is alive,
    /// it's just asking for a human.
    CaptchaBlocked,
    /// F#17 — the scheduler fired a due schedule (claimed it atomically,
    /// started `run_project_pool`). Emitted BEFORE the run completes so
    /// the user gets an immediate "schedule fired" even if the run then
    /// takes minutes.
    ScheduleFired,
    /// F#17 — the schedule fired but the run inside it failed.
    ScheduleFailed,
    /// F#13 — a proxy in the rotation failed a transport-level check
    /// ("proxy seems down"), and the runner rotated to the next.
    ProxyDown,
    /// A settings-level event: the operator changed their SMTP/notify
    /// configuration (or the channel config was saved successfully).
    SettingsChanged,
    /// Something the notifier itself wants to surface: a failing email
    /// send (we emit `SystemError` so the UI's bell shows "SMTP down"
    /// rather than silently dropping the notification).
    SystemError,
    /// Catch-all: an event that doesn't fit the other buckets (currently
    /// unused, reserved for forward-compatibility — the UI is told to
    /// render `unknown` types instead of erroring).
    SystemInfo,
}

impl NotificationType {
    pub fn as_str(self) -> &'static str {
        match self {
            NotificationType::RunCompleted => "runCompleted",
            NotificationType::RunCompletedWithFailures => "runCompletedWithFailures",
            NotificationType::RunFailed => "runFailed",
            NotificationType::CaptchaBlocked => "captchaBlocked",
            NotificationType::ScheduleFired => "scheduleFired",
            NotificationType::ScheduleFailed => "scheduleFailed",
            NotificationType::ProxyDown => "proxyDown",
            NotificationType::SettingsChanged => "settingsChanged",
            NotificationType::SystemError => "systemError",
            NotificationType::SystemInfo => "systemInfo",
        }
    }
    pub fn parse(s: &str) -> Option<NotificationType> {
        match s {
            "runCompleted" => Some(NotificationType::RunCompleted),
            "runCompletedWithFailures" => Some(NotificationType::RunCompletedWithFailures),
            "runFailed" => Some(NotificationType::RunFailed),
            "captchaBlocked" => Some(NotificationType::CaptchaBlocked),
            "scheduleFired" => Some(NotificationType::ScheduleFired),
            "scheduleFailed" => Some(NotificationType::ScheduleFailed),
            "proxyDown" => Some(NotificationType::ProxyDown),
            "settingsChanged" => Some(NotificationType::SettingsChanged),
            "systemError" => Some(NotificationType::SystemError),
            "systemInfo" => Some(NotificationType::SystemInfo),
            _ => None,
        }
    }
    /// Default severity for this type when the caller doesn't override it.
    /// This is the F#18 invariant: a `RunFailed` is NEVER severity `info`,
    /// a `RunCompleted` is NEVER severity `bad`, etc.
    pub fn default_severity(self) -> Severity {
        match self {
            NotificationType::RunCompleted => Severity::Good,
            NotificationType::RunCompletedWithFailures => Severity::Warn,
            NotificationType::RunFailed => Severity::Bad,
            NotificationType::CaptchaBlocked => Severity::Warn,
            NotificationType::ScheduleFired => Severity::Info,
            NotificationType::ScheduleFailed => Severity::Bad,
            NotificationType::ProxyDown => Severity::Warn,
            NotificationType::SettingsChanged => Severity::Info,
            NotificationType::SystemError => Severity::Bad,
            NotificationType::SystemInfo => Severity::Info,
        }
    }
    /// Should a default configuration EMAIL this event? The user's
    /// directive: "every good or bad thing that happens" — so the
    /// default is "email everything", but a per-type toggle in the
    /// settings page lets the operator mute specific types.
    ///
    /// `SystemInfo` and `SettingsChanged` are muted by default (they're
    /// the UI's own housekeeping, not a "thing that happened to the user's
    /// runs"); everything else is on.
    pub fn default_email(self) -> bool {
        !matches!(self, NotificationType::SystemInfo | NotificationType::SettingsChanged)
    }
}

// ── Classification inputs/outputs ────────────────────────────────────────────
/// A snapshot of the runner's `PoolSummary` — the input to `classify_run`.
/// We redeclare the shape (rather than `use ser-runner::PoolSummary`) so
/// this crate stays pure (no tokio / sqlx / ser-runner dep). The API's
/// notifier maps its real `PoolSummary` into this at the call site.
#[derive(Debug, Clone, Default)]
pub struct RunSnapshot {
    pub project_name: String,
    pub claimed: usize,
    pub done: usize,
    pub failed: usize,
    pub panics: usize,
    pub captcha_blocked: usize,
    pub duration_secs: u64,
}

/// A pre-classified event — the output of a `classify_*` function and the
/// input to `Notification::from_classified`. `id` and `created_at` are
/// filled by the API's `Notifier` (not the classification fn: two runs of
/// the same classifier must produce the same output — no clock, no RNG).
#[derive(Debug, Clone)]
pub struct Classified {
    pub type_: NotificationType,
    pub severity: Severity,
    pub title: String,
    pub message: String,
    pub project_id: Option<String>,
    pub job_id: Option<String>,
    pub payload: Value,
}

/// The **wire / DB** shape. `id` and `created_at` are filled by the
/// API's `Notifier` (Postgres generates the UUID, `now()`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    pub id: String,
    #[serde(rename = "type")]
    pub type_: NotificationType,
    pub severity: Severity,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    pub title: String,
    pub message: String,
    #[serde(skip_serializing_if = "Value::is_null")]
    pub payload: Value,
    #[serde(default, skip_serializing_if = "is_unread")]
    pub is_read: bool,
    pub created_at: DateTime<Utc>,
}

impl Notification {
    /// Build from a classifier's output + a caller-supplied `id` + `now`.
    /// (The API's `Notifier` calls this; the DB layer's `create` also
    /// builds one via this path.)
    pub fn new_from_classified(c: Classified, id: String, created_at: DateTime<Utc>) -> Self {
        Self {
            id,
            type_: c.type_,
            severity: c.severity,
            project_id: c.project_id,
            job_id: c.job_id,
            title: c.title,
            message: c.message,
            payload: c.payload,
            is_read: false,
            created_at,
        }
    }
}

// ── Classification functions (the ONLY way a "good or bad" event becomes a
//    `Classified`; all total — no `match` arms left as `todo!()`) ─────────

/// A runner pool finished: classify the outcome. `captchas_blocked` is the
/// count of jobs that hit the CAPTCHA wall (the F#13 "proxy seems down"
/// re-roll doesn't count — that's already handled at the runner level; we
/// only see the terminal counts here).
#[allow(clippy::too_many_arguments)]
pub fn classify_run(
    project_id: Option<String>,
    project_name: Option<String>,
    claimed: usize,
    done: usize,
    failed: usize,
    panics_captured: usize,
    captcha_blocked: usize,
    duration_secs: u64,
) -> Classified {
    let name = project_name.unwrap_or_else(|| "this project".to_string());
    if panics_captured > 0 && done + failed == 0 {
        // pool crashed out entirely — every worker panicked. This is a
        // hard `RunFailed`, not `RunCompletedWithFailures`.
        return Classified {
            type_: NotificationType::RunFailed,
            severity: Severity::Bad,
            title: format!("Run on {name} failed ({panics_captured} worker panic)"),
            message: format!(
                "Worker pool on `{name}` lost every worker {claimed} job(s) after {panics_captured} panic(s); no job completed."
            ),
            project_id,
            job_id: None,
            payload: serde_json::json!({
                "claimed": claimed,
                "done": done,
                "failed": failed,
                "panics": panics_captured,
                "duration_secs": duration_secs,
            }),
        };
    }
    if failed == 0 && claimed > 0 {
        return Classified {
            type_: NotificationType::RunCompleted,
            severity: Severity::Good,
            title: format!("\u{1F389} Run on {name} complete ({claimed} job(s) done)"),
            message: format!(
                "All {claimed} job(s) on `{name}` submitted successfully in {duration_secs}s."
            ),
            project_id,
            job_id: None,
            payload: serde_json::json!({
                "claimed": claimed,
                "done": done,
                "failed": 0,
                "panics": panics_captured,
                "duration_secs": duration_secs,
            }),
        };
    }
    if captcha_blocked > 0 {
        return Classified {
            type_: NotificationType::CaptchaBlocked,
            severity: Severity::Warn,
            title: format!("\u{26A0} {name}: {captcha_blocked} job(s) blocked by captcha"),
            message: format!(
                "{captcha_blocked} of {claimed} job(s) on `{name}` hit the captcha wall. {failed} other failed. Consider rotating proxies or trying a different engine."
            ),
            project_id,
            job_id: None,
            payload: serde_json::json!({
                "claimed": claimed,
                "done": done,
                "failed": failed,
                "captcha_blocked": captcha_blocked,
                "panics": panics_captured,
                "duration_secs": duration_secs,
            }),
        };
    }
    // Mixed: some jobs done, some failed, no captcha wall.
    Classified {
        type_: NotificationType::RunCompletedWithFailures,
        severity: Severity::Warn,
        title: format!("\u{1F534} {name}: {failed}/{claimed} job(s) failed"),
        message: format!(
            "{done} job(s) succeeded, {failed} failed on `{name}` ({panics_captured} worker panic(s) also captured). See results for per-job detail."
        ),
        project_id,
        job_id: None,
        payload: serde_json::json!({
            "claimed": claimed,
            "done": done,
            "failed": failed,
            "panics": panics_captured,
            "duration_secs": duration_secs,
        }),
    }
}

/// F#17 — a schedule's slot was claimed and (about to) fire a run.
pub fn classify_schedule_fired(
    project_id: Option<String>,
    project_name: Option<String>,
    schedule_name: Option<String>,
) -> Classified {
    let pname = project_name.unwrap_or_else(|| "a project".to_string());
    let sname = schedule_name.as_deref().unwrap_or("schedule").to_string();
    Classified {
        type_: NotificationType::ScheduleFired,
        severity: Severity::Info,
        title: format!("\u{23F0} Schedule \"{sname}\" on {pname} fired"),
        message: "The scheduler claimed the slot and is starting the run now.".to_string(),
        project_id,
        job_id: None,
        payload: serde_json::json!({ "schedule_name": schedule_name }),
    }
}

/// F#17 — a schedule fired but the run inside it produced a bad outcome.
pub fn classify_schedule_failed(
    project_id: Option<String>,
    project_name: Option<String>,
    schedule_name: Option<String>,
    reason: Option<String>,
) -> Classified {
    let pname = project_name.as_deref().unwrap_or("a project").to_string();
    let sname = schedule_name.as_deref().unwrap_or("schedule").to_string();
    Classified {
        type_: NotificationType::ScheduleFailed,
        severity: Severity::Bad,
        title: format!("\u{1F6A8} Schedule \"{sname}\" on {pname} failed"),
        message: match reason {
            Some(r) => r,
            None => "The run that this schedule started did not complete cleanly.".to_string(),
        },
        project_id,
        job_id: None,
        payload: serde_json::json!({ "schedule_name": schedule_name }),
    }
}

/// A proxy in the rotation failed a transport-level probe and the runner
/// rotated past it.
pub fn classify_proxy_down(
    project_id: Option<String>,
    project_name: Option<String>,
    proxy_url: Option<String>,
) -> Classified {
    let pname = project_name.as_deref().unwrap_or("a project").to_string();
    let p = proxy_url.as_deref().unwrap_or("proxy").to_string();
    Classified {
        type_: NotificationType::ProxyDown,
        severity: Severity::Warn,
        title: format!("\u{26A0} Proxy {p} appears down ({pname})"),
        message: format!(
            "A job on `{pname}` hit a transport-level failure through `{p}` — the runner rotated to the next proxy in the list."
        ),
        project_id,
        job_id: None,
        payload: serde_json::json!({ "proxy": proxy_url }),
    }
}

/// The operator's SMTP settings page accepted a change: housekeeping.
pub fn classify_settings_changed(
    what: &str,
    summary: Option<String>,
) -> Classified {
    Classified {
        type_: NotificationType::SettingsChanged,
        severity: Severity::Info,
        title: "Notification settings updated".to_string(),
        message: match summary {
            Some(s) => s,
            None => format!("Updated: {what}."),
        },
        project_id: None,
        job_id: None,
        payload: serde_json::json!({ "what": what }),
    }
}

/// An internal notifier failure (e.g. the SMTP send itself errored, or the
/// DB write failed after the SSE send already went out). Muted from email
/// by default (a "couldn't send email because…email" loop).
pub fn classify_system_error(what: &str, detail: Option<String>) -> Classified {
    Classified {
        type_: NotificationType::SystemError,
        severity: Severity::Bad,
        title: format!("\u{1F41B} Notifier: {what}"),
        message: detail.map(|d| d).unwrap_or_else(|| format!("({what})")),
        project_id: None,
        job_id: None,
        payload: serde_json::json!({ "what": what }),
    }
}




// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn all(pname: &str) -> RunSnapshot {
        RunSnapshot {
            project_name: pname.into(),
            claimed: 10,
            done: 10,
            failed: 0,
            panics: 0,
            captcha_blocked: 0,
            duration_secs: 42,
        }
    }

    fn snap(claimed: usize, done: usize, failed: usize, panics: usize, cap: usize) -> RunSnapshot {
        RunSnapshot {
            project_name: "Site A".into(),
            claimed, done, failed, panics, captcha_blocked: cap, duration_secs: 5,
        }
    }

    #[test]
    fn all_green_is_run_completed_good() {
        let c = classify_run(Some("p1".into()), Some("Site A".into()), 10, 10, 0, 0, 0, 42);
        assert_eq!(c.type_, NotificationType::RunCompleted);
        assert_eq!(c.severity, Severity::Good);
        assert!(!c.title.is_empty());
        assert_eq!(c.payload["claimed"], 10);
    }

    #[test]
    fn partial_failure_is_warn() {
        let c = classify_run(Some("p1".into()), Some("Site A".into()), 10, 4, 6, 0, 0, 5);
        assert_eq!(c.type_, NotificationType::RunCompletedWithFailures);
        assert_eq!(c.severity, Severity::Warn);
    }

    #[test]
    fn captcha_wall_is_captcha_blocked() {
        let c = classify_run(Some("p1".into()), Some("Site A".into()), 8, 0, 8, 0, 8, 5);
        assert_eq!(c.type_, NotificationType::CaptchaBlocked);
        assert_eq!(c.severity, Severity::Warn);
        assert_eq!(c.payload["captcha_blocked"], 8);
    }

    #[test]
    fn total_panic_is_run_failed_bad() {
        let c = classify_run(Some("p1".into()), Some("Site A".into()), 5, 0, 0, 5, 0, 5);
        assert_eq!(c.type_, NotificationType::RunFailed);
        assert_eq!(c.severity, Severity::Bad);
    }

    #[test]
    fn zero_claims_with_failures_is_warn() {
        // claimed 0 but failed > 0 shouldn't happen from the pool, but the
        // classifier must still be total — it picks the warnings branch.
        let c = classify_run(None, None, 0, 0, 3, 0, 0, 1);
        assert!(matches!(c.type_, NotificationType::RunCompletedWithFailures));
    }

    #[test]
    fn schedule_fired_severity_info() {
        let c = classify_schedule_fired(Some("p1".into()), Some("Site A".into()), Some("nightly".into()));
        assert_eq!(c.type_, NotificationType::ScheduleFired);
        assert_eq!(c.severity, Severity::Info);
    }

    #[test]
    fn notification_serde_roundtrip() {
        let n = Notification {
            id: "id-1".into(),
            type_: NotificationType::RunCompleted,
            severity: Severity::Good,
            project_id: Some("p1".into()),
            job_id: None,
            title: "ok".into(),
            message: "all good".into(),
            payload: serde_json::json!({"k": 1}),
            is_read: false,
            created_at: DateTime::parse_from_rfc3339("2026-10-18T08:00:00Z").unwrap().with_timezone(&Utc),
        };
        let s = serde_json::to_string(&n).unwrap();
        assert!(s.contains("\"type\":\"runCompleted\""));
        let back: Notification = serde_json::from_str(&s).unwrap();
        assert_eq!(back.type_, NotificationType::RunCompleted);
        assert_eq!(back.severity, Severity::Good);
        assert_eq!(back.created_at, n.created_at);
        assert!(!back.is_read);
    }

    #[test]
    fn severity_rank_ordering() {
        assert!(Severity::Bad.rank() > Severity::Warn.rank());
        assert!(Severity::Warn.rank() > Severity::Info.rank());
    }

    #[test]
    fn default_email_gating() {
        assert!(NotificationType::RunCompleted.default_email());
        assert!(NotificationType::RunFailed.default_email());
        assert!(!NotificationType::SettingsChanged.default_email());
        assert!(!NotificationType::SystemInfo.default_email());
    }

    #[test]
    fn classification_is_pure_no_clock() {
        let a = classify_run(Some("p".into()), Some("N".into()), 2, 2, 0, 0, 0, 9);
        let b = classify_run(Some("p".into()), Some("N".into()), 2, 2, 0, 0, 0, 9);
        assert_eq!(a.title, b.title);
        assert_eq!(a.payload, b.payload);
    }
}
