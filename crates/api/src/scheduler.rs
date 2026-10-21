//! F#17 — background scheduler loop.
//!
//! Ticks every `SEER_SCHEDULER_TICK_MS` ms (default 30 000 = 30 s). On each
//! tick it lists enabled schedules that have a `next_run_at` at or before
//! "now", and for each one it:
//!
//!   1. Re-reads the row.
//!   2. Calls the pure core `engine::schedule::ScheduleSpec::next_run(now)`
//!      to compute where the row should advance to (None when the schedule
//!      is already consumed — the scheduler then just records the claim as
//!      a no-op).
//!   3. Atomically claims the slot via `Repo::claim_schedule`
//!      (rowcount 1 = win; 0 = lose to another tick/scheduler).
//!   4. Fires the project via `run_project_pool` (the exact same in-process
//!      pool path the HTTP `POST /api/projects/{id}/run` handler uses —
//!      shared config, proxy list, workers, timeouts).
//!
//! The loop is spawned from `main()` once at startup and runs for the
//! lifetime of the API process. A tick with no due schedules is trivial
//! (one index query against `idx_schedules_enabled_next`).

use crate::{run_project_pool, spec_from_row, AppState};
use ser_db::Repo;
use std::sync::Arc;
use sqlx::Row;
use uuid::Uuid;

const TICK_MS_DEFAULT: u64 = 30_000;

/// One pass of the scheduler. Returns the count of schedules claimed and
/// fired in this tick (0 = nothing due).
pub async fn tick(state: Arc<AppState>) -> u32 {
    // List enabled + due rows. A single index query (no row holding):
    let rows = sqlx::query("SELECT id FROM schedules \
                            WHERE enabled = TRUE AND next_run_at IS NOT NULL AND next_run_at <= now() \
                            ORDER BY next_run_at ASC LIMIT 64")
        .fetch_all(&state.pool)
        .await;

    let due: Vec<Uuid> = match rows {
        Err(e) => {
            eprintln!("[scheduler] due list failed: {e}");
            return 0;
        }
        Ok(rows) => rows
            .iter()
            .filter_map(|r| r.try_get::<Uuid, _>("id").ok())
            .collect(),
    };

    if due.is_empty() {
        return 0;
    }

    let fired = due.len() as u32;
    for id in due {
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(e) = fire(state, id).await {
                eprintln!("[scheduler] fire {id} failed: {e}");
            }
        });
    }
    fired
}

async fn fire(state: Arc<AppState>, id: Uuid) -> anyhow::Result<()> {
    let repo = Repo::new(state.pool.clone());
    let row = match repo.schedule_by_id(id).await? {
        Some(r) => r,
        None => return Ok(()), // deleted between ticks — skip
    };

    let now = chrono::Utc::now().naive_utc();
    let spec = spec_from_row(&row);
    let next_after = spec
        .next_run(now)
        .map(|n| chrono::DateTime::from_naive_utc_and_offset(n, chrono::Utc));
    let now_dt = chrono::Utc::now();
    let claimed = match repo.claim_schedule(id, now_dt, next_after).await? {
        Some(_) => true,
        None => false, // another tick won this slot; not ours
    };
    if !claimed {
        return Ok(());
    }
    eprintln!("[scheduler] firing project {} (schedule {})", row.project_id, id);
    // Run in-process; the pool's per-job timeout bounds the call. A failure
    // is logged but does NOT stop the scheduler loop — the tick continues.
    let summary = match run_project_pool(row.project_id, &state, None).await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[scheduler] run project {} failed: {e}", row.project_id);
            // F#18 — a schedule slot was claimed but the run blew up:
            // alert the operator (the run-outcome notifier only covers
            // the in-pool path, so this error case is the one to emit).
            if let Err(ne) = crate::notifier::notify(
                &state,
                &engine::notify::classify_schedule_failed(
                    Some(row.project_id.to_string()),
                    None,
                    Some(row.name.clone()),
                    Some(e.to_string()),
                ),
            )
            .await
            {
                eprintln!("[scheduler] schedule-failed notify failed: {ne}");
            }
            return Ok(());
        }
    };
    eprintln!(
        "[scheduler] done: claimed={} done={} failed={} panics={}",
        summary.get("claimed").and_then(|v| v.as_u64()).unwrap_or(0),
        summary.get("done").and_then(|v| v.as_u64()).unwrap_or(0),
        summary.get("failed").and_then(|v| v.as_u64()).unwrap_or(0),
        summary.get("panics").and_then(|v| v.as_u64()).unwrap_or(0)
    );
    Ok(())
}

/// Spawn the scheduler loop. Returns a JoinHandle the caller can hold
/// to await a clean shutdown (the API process currently never exits, so
/// the handle is dropped after spawn — the task keeps running under the
/// tokio runtime).
pub fn spawn(state: Arc<AppState>) -> tokio::task::JoinHandle<()> {
    let tick_ms: u64 = std::env::var("SEER_SCHEDULER_TICK_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(TICK_MS_DEFAULT)
        .max(1_000);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(tick_ms));
        // First tick after half an interval (gives operators a moment to
        // toggle / edit schedules before the loop starts firing).
        tokio::time::sleep(std::time::Duration::from_millis(tick_ms / 2)).await;
        loop {
            interval.tick().await;
            match tick(state.clone()).await {
                n if n > 0 => eprintln!("[scheduler] tick fired {n} schedule(s)"),
                _ => {} // quiet tick
            }
        }
    })
}
