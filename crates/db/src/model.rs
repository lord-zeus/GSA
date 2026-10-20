use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Project {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// F#13: when true, this project's jobs route through the enabled proxy
    /// list (round-robin, down proxies skipped). Default false (direct).
    #[serde(default)]
    pub use_proxies: bool,
    /// F#19: GSA "Anchor Text" mix — JSON blob. `None` = the main keyword is
    /// used as the anchor for every submission (previous behavior); `Some`
    /// = a weighted pool the runner draws from per job.
    #[serde(default)]
    pub anchor_mix: Option<String>,
    /// F#19: GSA "Use a random URL from above on every submission (else in
    /// sequence)". Drives multi-URL (linkwheel) target selection at F#22.
    #[serde(default)]
    pub use_random_url: bool,
    /// F#19: GSA "Use URL variations with N%" — emit scheme/www/case variants
    /// of the target URL with this probability (0..=100).
    #[serde(default)]
    pub url_variations: i32,
    /// F#19: GSA "Use the root/main URL with N%" — submit the bare
    /// `scheme://host/` (no path) with this probability (0..=100).
    #[serde(default)]
    pub root_url: i32,
    /// F#24-lite: GSA "Use verified URLs of another project (Multi-Tier)" —
    /// the upstream project ids whose verified URLs this project submits.
    #[serde(default)]
    pub linkwheel: Vec<Uuid>,
    /// F#24-lite: GSA "Automatically export verified URLs" — append each
    /// success to `<data>/verified/<project-name>.csv` as it happens.
    #[serde(default)]
    pub auto_export: bool,
    /// GSA Data tab "URL (spintax)": the base URL this project's backlinks
    /// point at. Empty = engine-spec default (`%url%` left to the INI).
    /// The runner applies F#19 URL options (root_url / url_variations)
    /// and F#24-lite linkwheel (upstream verified URLs) to it per job.
    #[serde(default)]
    pub url: String,
    /// F#30 — GSA "Article content": project-level article body used by
    /// article-family engines as `%article%`. Spintax allowed. Empty = the
    /// runner auto-generates a short article from the keyword + target URL.
    #[serde(default)]
    pub article_text: String,
    /// F#26 — GSA "Notes" free text (shown on the project card / export header).
    #[serde(default)]
    pub notes: String,
    /// F#26 — GSA "Custom Project Color" (a CSS hex like `#4f9d5d`; UI only).
    #[serde(default)]
    pub color: String,
    /// F#26 — GSA "Pause after N submissions": soft-stop a single pool run once
    /// this many successful links have been submitted (0 / None = no pause).
    #[serde(default)]
    pub pause_after: i32,
    /// F#26 — set by the pool the moment it hits the pause_after ceiling so the
    /// UI can show "paused after N" and the remaining pending jobs stay queued.
    #[serde(default)]
    pub paused_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Engine {
    pub id: Uuid,
    pub project_id: Uuid,
    pub engine_name: String,
    pub engine_type: String,
    pub url: String,
    pub file_path: String,
    pub step_count: i32,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Keyword {
    pub id: Uuid,
    pub project_id: Uuid,
    pub text: String,
    pub added_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobStatus {
    Pending,
    Running,
    Done,
    Failed,
    Rejected,
    Cancelled,
}

impl JobStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Rejected => "rejected",
            Self::Cancelled => "cancelled",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "pending" => Self::Pending,
            "running" => Self::Running,
            "done" => Self::Done,
            "failed" => Self::Failed,
            "rejected" => Self::Rejected,
            "cancelled" => Self::Cancelled,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Job {
    pub id: Uuid,
    pub project_id: Uuid,
    pub engine_id: Option<Uuid>,
    pub keyword_id: Option<Uuid>,
    pub status: String,
    pub attempts: i32,
    pub last_error: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl Job {
    pub fn status_value(&self) -> Option<JobStatus> {
        JobStatus::parse(&self.status)
    }
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.status_value(),
            Some(JobStatus::Done)
                | Some(JobStatus::Failed)
                | Some(JobStatus::Rejected)
                | Some(JobStatus::Cancelled)
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultKind {
    Success,
    Info,
    Error,
    Rejected,
}

impl ResultKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Info => "info",
            Self::Error => "error",
            Self::Rejected => "rejected",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "success" => Self::Success,
            "info" => Self::Info,
            "error" => Self::Error,
            "rejected" => Self::Rejected,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct SubmissionResult {
    pub id: Uuid,
    pub job_id: Uuid,
    pub kind: String,
    pub url: Option<String>,
    pub anchor: Option<String>,
    pub message: Option<String>,
    pub payload: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
}

impl SubmissionResult {
    pub fn kind_value(&self) -> Option<ResultKind> {
        ResultKind::parse(&self.kind)
    }
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct HistoryEntry {
    pub project_id: Uuid,
    pub url: String,
    pub engine_name: Option<String>,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
}

/// One row for the dashboard's project Results view: the stored
/// submission result joined to the engine and keyword it belonged to.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct ProjectResult {
    pub id: Uuid,
    pub job_id: Uuid,
    pub kind: String,
    pub url: Option<String>,
    pub anchor: Option<String>,
    pub message: Option<String>,
    pub engine_name: Option<String>,
    pub keyword: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Aggregate health of a project's submissions, for the Results summary card.
#[derive(Debug, Clone, Serialize, Default)]
pub struct ResultStats {
    pub success: i64,
    pub info: i64,
    pub error: i64,
    pub rejected: i64,
    pub total: i64,
    pub total_jobs: i64,
    pub unique_success_urls: i64,
}

// ── F#13: proxies ─────────────────────────────────────────────────────────
/// One proxy server in the GSA-style proxy list.
/// `proxy_kind`: "http" or "socks5" (the original's `proxy type=` vocabulary).
///
/// NOTE: derives `Serialize` for the UI but NOT `Deserialize` — the inbound
/// API shape is a dedicated request struct (with `#[serde(default)]` for the
/// optional login/password/kind), so the stored row shape stays explicit.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Proxy {
    pub id: Uuid,
    pub host: String,
    pub port: i32,
    pub proxy_kind: String,
    pub login: String,
    pub password: String,
    pub enabled: bool,
    /// F#14 — target country the proxy serves (GSA "Country" column).  `NULL`
    /// = untargeted (used for any-country jobs).
    #[serde(default)]
    pub country: Option<String>,
    /// F#14 — verified egress IP of the proxy (auto-populated by `verify-geo`).
    #[serde(default)]
    pub ip: Option<String>,
    /// F#14 — whether the verified IP is public (not RFC1918/loopback/link-local).
    #[serde(default)]
    pub ip_public: bool,
    /// F#14 — when the IP was last verified.
    #[serde(default)]
    pub geo_verified_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// Inbound shape for `POST /api/proxies` — `host` + `port` required, the
/// rest optional with GSA defaults (http, no auth, enabled).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProxyInput {
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub port: i32,
    #[serde(default)]
    pub proxy_kind: Option<String>,
    #[serde(default)]
    pub login: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    /// F#14 — optional target country set at creation time.
    #[serde(default)]
    pub country: Option<String>,
}

/// Outcome of one proxy connectivity test (`POST /api/proxies/{id}/test`):
/// mirrors GSA's `proxy test` / `proxy verify` — we attempt a real request
/// THROUGH the proxy and report whether a TCP connect (and, for HTTP
/// proxies, a response) completed, plus latency.
#[derive(Debug, Clone, Serialize)]
pub struct ProxyTest {
    pub id: Uuid,
    pub ok: bool,
    pub latency_ms: i64,
    pub detail: String,
}

// ─────────────────────────────────────────────────────────────
// F#17 — per-project scheduled runs (GSA "scheduler" / scheduled posting).
// ─────────────────────────────────────────────────────────────

/// A project schedule row. The `start_at`/`repeat_*/window_*/stop_at/
/// max_runs` fields are the declarative calendar; `run_count`/`enabled`/
/// `next_run_at` are the runtime state the API scheduler mutates.
///
/// `repeat_weekdays` is ISO 1 (Mon) … 7 (Sun); `None` = any weekday. The
/// `NaiveTime` window columns are a UTC wall-clock window.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Schedule {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    /// `"once"` or `"recurring"`.
    pub mode: String,
    pub start_at: DateTime<Utc>,
    pub stop_at: Option<DateTime<Utc>>,
    /// Cadence in minutes (1..=525600). `None` for `once`, required for
    /// `recurring`.
    pub repeat_every_minutes: Option<i32>,
    /// ISO weekday set (1=Mon..7=Sun) or `None` = any day.
    pub repeat_weekdays: Option<Vec<i32>>,
    pub window_start: Option<chrono::NaiveTime>,
    pub window_end: Option<chrono::NaiveTime>,
    pub max_runs: Option<i64>,
    pub run_count: i32,
    pub enabled: bool,
    /// The last-fired slot (`timestamptz`); `NULL` = never fired. The
    /// scheduler compares against `now` via `Repo::claim_schedule`.
    pub next_run_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Inbound shape for `POST /api/projects/{id}/schedules` — mirrors the
/// `ScheduleSpec` fields the operator picks in the UI. `start_at` is naive
/// UTC (`%Y-%m-%d %H:%M:%S`); the API validates it against
/// `ScheduleSpec::validate()` before insert.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ScheduleInput {
    pub name: String,
    #[serde(default)]
    pub mode: String, // "once" | "recurring"
    pub start_at: String,
    #[serde(default)]
    pub stop_at: Option<String>,
    #[serde(default)]
    pub repeat_every_minutes: Option<i32>,
    #[serde(default)]
    pub repeat_weekdays: Option<Vec<i32>>,
    #[serde(default)]
    pub window_start: Option<String>, // "HH:MM:SS"
    #[serde(default)]
    pub window_end: Option<String>,
    #[serde(default)]
    pub max_runs: Option<i64>,
    #[serde(default)]
    pub enabled: Option<bool>,
}


// ─────────────────────────────────────────────────────────────
// F#18 — alert log (Notification Center).
// ─────────────────────────────────────────────────────────────

/// One alert log row. `type`/`severity` are the STRING spellings defined
/// by `engine::notify::NotificationType::as_str()` / `Severity::as_str()`
/// (camelCase / lowercase respectively), so the API can write the row
/// from either shape directly. `created_at` is a `TIMESTAMPTZ`.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct NotificationRow {
    pub id: Uuid,
    pub r#type: String,
    pub severity: String,
    pub project_id: Option<Uuid>,
    pub job_id: Option<String>,
    pub title: String,
    pub message: String,
    pub payload: serde_json::Value,
    pub is_read: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Inbound shape for `POST /api/notifications/emit` (the API's
/// `Notifier` also emits directly via `repo.insert_notification`, so
/// this struct exists for external / test emitters — the UI's
/// "test my email config" button uses it too).
#[derive(Debug, Clone, Deserialize)]
pub struct NotifyEmit {
    #[serde(rename = "type")]
    pub type_: String,
    pub severity: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub job_id: Option<String>,
    pub title: String,
    pub message: String,
    #[serde(default)]
    pub payload: serde_json::Value,
}

/// Settings-page shape for the "alert channels" block (GSA "email me on
/// X" toggles + SMTP config). Lives as `settings.notify_config` (JSON)
/// via `repo::get_setting`/`set_setting`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NotifySettings {
    #[serde(rename = "emailEnabled")]
    pub email_enabled: bool,
    pub smtpHost: String,
    pub smtpPort: u16,
    /// `starttls` (587) or `direct-tls` (465). `log` = render-only, no
    /// socket — used by the test button.
    pub smtpMode: String,
    pub authUser: String,
    pub authPassword: String,
    pub senderName: String,
    pub recipient: String,
    #[serde(default = "default_connect_timeout")]
    pub connectTimeoutSecs: u64,
    /// Per-type `emailEnabled` toggle map (camelCase type → bool). Missing
    /// keys fall back to `NotificationType::default_email()`.
    #[serde(rename = "perTypeEmail")]
    pub per_type_email: std::collections::BTreeMap<String, bool>,
}
fn default_connect_timeout() -> u64 { 15 }
