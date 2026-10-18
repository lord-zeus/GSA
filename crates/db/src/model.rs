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
