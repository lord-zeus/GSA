//! T6 — live run feed: the runner's voice, delivered two ways.
//!
//! * **DB (`job_events`)** — the source of truth; survives restarts, tailable
//!   from the UI at any time via `GET /api/projects/{id}/events`.
//! * **SSE (`RunBus`)** — the live push channel: every event is also
//!   broadcast here so the open console updates in real time. `broadcast`
//!   drops the oldest frame for a lagging receiver instead of blocking the
//!   runner, and the UI re-polls the DB on reconnect to close any gap.
//!
//! [`RunEventSink`] is the `runner::EventSink` impl the pool builder
//! hangs on the `SubmissionContext`: the runner stays transport-agnostic,
//! this module is where a `RunEvent` becomes a row + a frame.

use std::sync::Arc;

use serde::Serialize;
use sqlx::PgPool;
use tokio::sync::broadcast;
use uuid::Uuid;

use ser_db::Repo;

const RUN_BUS_CAPACITY: usize = 512;

/// One event on the live run stream. `id` is the `job_events` row id —
/// the UI dedupes events that arrive on BOTH the live frame and the
/// catch-up poll with it.
#[derive(Debug, Clone, Serialize)]
pub struct RunMsg {
    pub id: i64,
    pub project_id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    pub level: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ts: Option<chrono::DateTime<chrono::Utc>>,
}

pub struct RunBus {
    tx: broadcast::Sender<RunMsg>,
}

impl RunBus {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(RUN_BUS_CAPACITY);
        Self { tx }
    }

    pub fn sender(&self) -> broadcast::Sender<RunMsg> {
        self.tx.clone()
    }

    pub fn send(&self, msg: RunMsg) {
        self.tx.send(msg).unwrap_or(0);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<RunMsg> {
        self.tx.subscribe()
    }
}

impl std::fmt::Debug for RunBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunBus").finish_non_exhaustive()
    }
}

impl Default for RunBus {
    fn default() -> Self {
        Self::new()
    }
}

/// The runner-side sink: persist to `job_events`, then broadcast.
///
/// Fire-and-forget (`record` is sync on the trait): the DB insert runs in
/// its own task; a slow or dead DB never stalls a worker, and a dropped
/// SSE send is fine — the DB row is authoritative and the UI re-polls.
#[derive(Debug)]
pub struct RunEventSink {
    project_id: Uuid,
    pool: PgPool,
    tx: broadcast::Sender<RunMsg>,
}

impl RunEventSink {
    pub fn new(project_id: Uuid, pool: PgPool, tx: broadcast::Sender<RunMsg>) -> Arc<Self> {
        Arc::new(Self { project_id, pool, tx })
    }
}

impl runner::EventSink for RunEventSink {
    fn record(&self, ev: runner::RunEvent) {
        let project_id = self.project_id;
        let pool = self.pool.clone();
        let tx = self.tx.clone();
        let msg = RunMsg {
            id: 0,
            project_id,
            job_id: ev.job_id,
            engine: ev.engine.clone(),
            level: ev.level.clone(),
            message: ev.message.clone(),
            ts: Some(chrono::Utc::now()),
        };
        tokio::spawn(async move {
            match Repo::new(pool)
                .insert_job_event(
                    project_id,
                    msg.job_id,
                    msg.engine.as_deref(),
                    &msg.level,
                    &msg.message,
                )
                .await
            {
                Ok(id) => {
                    let _ = tx.send(RunMsg { id, ..msg });
                }
                Err(e) => eprintln!("[runfeed] job_events insert failed: {e} (event dropped, bus only)"),
            }
        });
    }
}
