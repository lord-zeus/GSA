//! F#18 — the single emission funnel.
//!
//! Every "good or bad thing that happens" flows through `notify()`:
//!   1. Persist the alert as a `notifications` row (the source of truth,
//!      queried by the Notification Center and by SSE reconnect-catch-up).
//!   2. Broadcast it on the `LiveBus` so open SSE clients update live.
//!   3. If the email channel is on for this type, dispatch SMTP in a
//!      `spawn_blocking` task (synchronous socket, see `engine::smtp`).
//!
//! Step 3 is best-effort: an SMTP failure never fails the emit. We
//! re-classify the failure as a `SystemError` (`classify_system_error`)
//! and route it through the SAME funnel (so the bell gets "SMTP down"),
//! EXCEPT we suppress email on that follow-up to avoid an infinite loop
//! (a "can't send email" alert can't itself be emailed).

use std::sync::Arc;

use engine::notify::{self as nt, Classified, Notification};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use crate::AppState;

/// The operator's alert-channel + per-type toggle settings. Persisted as
/// `settings.notify_config` (JSON) — a single key so the GET/PUT endpoints
/// and the settings page's export/import both treat it as one blob. All
/// fields have defaults so a partially-saved row deserializes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NotifyConfig {
    /// Master switch: even if a per-type `email` is true, master off = no email.
    pub email_enabled: bool,
    /// Where the alerts are addressed. Empty = email off regardless.
    pub recipient: String,
    pub smtp_host: String,
    pub smtp_port: u16,
    /// `starttls` (587) or `direct-tls` (465).
    pub smtp_mode: String,
    pub smtp_user: String,
    pub smtp_password: String,
    /// "From" display name.
    pub sender_name: String,
    /// Per-type email override map: camelCase type name -> bool. Missing
    /// keys fall back to `NotificationType::default_email()`.
    /// (The Vue store builds this from the same camelCase enum the engine
    /// uses, so the two never drift.)
    #[serde(rename = "perType")]
    pub per_type: std::collections::BTreeMap<String, bool>,
}

impl Default for NotifyConfig {
    fn default() -> Self {
        NotifyConfig {
            email_enabled: false,
            recipient: String::new(),
            smtp_host: String::new(),
            smtp_port: 587,
            smtp_mode: "starttls".into(),
            smtp_user: String::new(),
            smtp_password: String::new(),
            sender_name: String::new(),
            per_type: std::collections::BTreeMap::new(),
        }
    }
}

impl NotifyConfig {
    /// Load from the `settings` table (key `notify_config`). Missing row or
    /// parse error -> `Default` (email off; the UI still shows the form).
    pub async fn load(pool: &sqlx::PgPool) -> Self {
        match sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = 'notify_config'")
            .fetch_optional(pool)
            .await
            .ok()
            .flatten()
        {
            Some(raw) => serde_json::from_str(&raw).unwrap_or_default(),
            None => NotifyConfig::default(),
        }
    }

    /// Persist. `Upsert`-style: one `INSERT ... ON CONFLICT (key) DO UPDATE`.
    pub async fn save(&self, pool: &sqlx::PgPool) -> anyhow::Result<()> {
        let raw = serde_json::to_string(self)?;
        sqlx::query(
            "INSERT INTO settings (key, value, updated_at) VALUES ('notify_config', $1, now()) \
             ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = now()",
        )
        .bind(&raw)
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Whether a given type should be emailed, given the master switch and
    /// per-type override map. Mirrors `NotificationType::default_email()`
    /// for types the operator hasn't explicitly toggled.
    pub fn email_for(&self, t: nt::NotificationType) -> bool {
        if !self.email_enabled {
            return false;
        }
        if self.recipient.trim().is_empty() {
            return false;
        }
        self.per_type
            .get(t.as_str())
            .copied()
            .unwrap_or_else(|| t.default_email())
    }

    /// The `engine::smtp::SmtpConfig` this maps to (only meaningful when
    /// `email_for(...)` is true).
    pub fn to_smtp(&self) -> engine::smtp::SmtpConfig {
        use engine::smtp::{SmtpConfig, SmtpMode};
        SmtpConfig {
            host: self.smtp_host.clone(),
            port: self.smtp_port,
            auth_user: self.smtp_user.clone(),
            auth_password: self.smtp_password.clone(),
            mode: match self.smtp_mode.as_str() {
                "direct-tls" => SmtpMode::DirectTls,
                "log" => SmtpMode::Log,
                _ => SmtpMode::StartTls,
            },
            sender_name: self.sender_name.clone(),
            recipient: self.recipient.clone(),
            connect_timeout_secs: 15,
        }
    }
}

/// The result of an emit — for the SSE/REST responses and for the UI to
/// show a toast. The notification is already persisted + broadcast.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmitOutcome {
    pub notification: Notification,
    /// Whether an email was dispatched (or skipped).
    pub email_sent: bool,
    pub email_skipped_reason: Option<String>,
}

/// Emit one classified event. This is THE function every "something
/// happened" site in the codebase calls (`run_project_pool`, the
/// scheduler, proxy-failure paths, the settings page, etc.).
///
/// Guarantees: always persists a row, always broadcasts; email is
/// best-effort and never panics the caller.
pub async fn notify(
    state: &Arc<AppState>,
    c: &Classified,
) -> anyhow::Result<EmitOutcome> {
    let row = crate::Repo::new(state.pool.clone())
        .insert_notification(
            c.type_.as_str(),
            c.severity.as_str(),
            c.project_id.as_deref().and_then(|s| Uuid::parse_str(s).ok()),
            c.job_id.as_deref(),
            &c.title,
            &c.message,
            &c.payload,
        )
        .await?;

    // Build the wire shape (id/created_at come back from Postgres in `row`).
    let notification = Notification {
        id: row.id.to_string(),
        type_: c.type_,
        severity: c.severity,
        project_id: row.project_id.map(|u| u.to_string()),
        job_id: row.job_id.clone(),
        title: row.title.clone(),
        message: row.message.clone(),
        payload: row.payload.clone(),
        is_read: false,
        created_at: row.created_at,
    };

    // Live broadcast — never blocks on a slow client (broadcast semantics).
    state.bus.send(notification.clone());

    // Email dispatch — best-effort, spawn_blocking (synchronous socket).
    let cfg = NotifyConfig::load(&state.pool).await;
    let email_for = cfg.email_for(notification.type_);
    let (email_sent, email_skipped_reason) = if !email_for {
        (false, Some(if cfg.email_enabled && cfg.recipient.trim().is_empty() {
            "no recipient configured".to_string()
        } else {
            "email disabled for this type by settings".to_string()
        }))
    } else {
        let smtp_cfg = cfg.to_smtp();
        let n = notification.clone();
        match tokio::task::spawn_blocking(move || engine::smtp::send(&smtp_cfg, &n)).await {
            Ok(Ok(r)) => (r.delivered, None),
            Ok(Err(e)) => {
                // A failed send → a follow-up SystemError, re-emitted through
                // the SAME funnel (bell says "SMTP down") but with email
                // suppressed so it can't loop. `emit_system` below does that.
                let _ = emit_system_error(state, "smtp send failed", Some(e.clone())).await;
                (false, Some(e))
            }
            Err(je) => {
                let _ = emit_system_error(state, "smtp task panic", Some(je.to_string())).await;
                (false, Some(je.to_string()))
            }
        }
    };

    Ok(EmitOutcome { notification, email_sent, email_skipped_reason })
}

/// Convenience: classify a run-outcome snapshot and emit it (used by
/// `run_project_pool`). Wraps `nt::classify_run` so the call site stays
/// a one-liner.
pub async fn notify_run(
    state: &Arc<AppState>,
    project_id: Uuid,
    project_name: &str,
    claimed: usize,
    done: usize,
    failed: usize,
    panics: usize,
    captcha_blocked: usize,
    duration_secs: u64,
) -> anyhow::Result<EmitOutcome> {
    let c = nt::classify_run(
        Some(project_id.to_string()),
        Some(project_name.to_string()),
        claimed, done, failed, panics, captcha_blocked, duration_secs,
    );
    notify(state, &c).await
}

/// A settings-level "config saved" event (used by `PUT /api/settings/notify`).
pub async fn notify_settings_saved(state: &Arc<AppState>, summary: String) -> anyhow::Result<EmitOutcome> {
    let c = nt::classify_settings_changed("notify config", Some(summary.into()));
    notify(state, &c).await
}

/// An internal notifier failure (e.g. a failed SMTP send). Mirrors
/// `nt::classify_system_error` but suppresses email (the "couldn't email
/// about a mailing failure" case).
pub async fn emit_system_error(
    state: &Arc<AppState>,
    what: &str,
    detail: Option<String>,
) -> anyhow::Result<EmitOutcome> {
    // Emit directly: persist + broadcast, but SKIP email (avoid the loop).
    let c = nt::classify_system_error(what, detail.clone());
    let proj_u: Option<Uuid> = c.project_id.as_deref().and_then(|s| s.parse().ok());
    let row = crate::Repo::new(state.pool.clone())
        .insert_notification(
            c.type_.as_str(),
            c.severity.as_str(),
            proj_u,
            c.job_id.as_deref(),
            &c.title,
            &c.message,
            &c.payload,
        )
        .await?;
    let notification = Notification {
        id: row.id.to_string(),
        type_: c.type_,
        severity: c.severity,
        project_id: row.project_id.map(|u| u.to_string()),
        job_id: row.job_id.clone(),
        title: row.title.clone(),
        message: row.message.clone(),
        payload: row.payload.clone(),
        is_read: false,
        created_at: row.created_at,
    };
    state.bus.send(notification.clone());
    Ok(EmitOutcome {
        notification,
        email_sent: false,
        email_skipped_reason: Some("suppressed (system error — avoid email loop)".into()),
    })
}

// Re-export the severity so call sites don't need the engine path.
pub use nt::Severity as NotifSeverity;
