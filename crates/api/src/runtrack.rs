//! T10 — background multi-project run tracking.
//!
//! `ser-api` is a single process, so an in-memory registry is sufficient to
//! know *which projects are running right now* and *the last finished
//! result* per project.  All durable work (jobs, links, log lines) already
//! lives in Postgres (`jobs`, `job_events`); this tracker only carries the
//! live "in flight" marker + the shared fair-share controller so the API can
//! report progress and accept a cancel at any time.
//!
//! Guarantees:
//!   * at most ONE active background run per project (a second start →
//!     `BeginErr::AlreadyRunning`);
//!   * at most `max_active` concurrent runs in the whole process
//!     (`SEER_MAX_ACTIVE_RUNS`, default 8) — a start beyond the cap →
//!     `BeginErr::CapReached`;
//!   * a background run that is told to finish removes itself from `active`
//!     and records its `last` summary, so a *new* run may start immediately.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// A live background run (one per project at a time).
pub struct ActiveRun {
    pub run_id: Uuid,
    pub project_id: Uuid,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub links_target: usize,
    pub engines: usize,
    /// True when this is a fair-share auto-run (has a live fair controller).
    pub auto: bool,
    /// Shared fair controller (run-auto). Cancel → set `force_stop`;
    /// status → read `total_links`. `None` for plain (F#12) runs.
    pub fair: Option<Arc<Mutex<runner::FairState>>>,
    /// Detached task handle. Used for liveness (`is_finished`) and aborted
    /// when [`RunTracker::begin`] rejects a spawn that already raced in.
    pub task: tokio::task::JoinHandle<()>,
}

/// The most recent finished run summary (kept alongside the DB rows).
#[derive(Clone)]
pub struct RunSummary {
    pub run_id: Uuid,
    pub finished_at: chrono::DateTime<chrono::Utc>,
    /// "ok" | "error" | "cancelled"
    pub status: String,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BeginErr {
    /// This project already has an active background run.
    AlreadyRunning,
    /// More than `max_active` runs are in flight process-wide.
    CapReached,
}

struct Inner {
    active: HashMap<Uuid, ActiveRun>,
    last: HashMap<Uuid, RunSummary>,
    max_active: usize,
}

pub struct RunTracker {
    inner: Mutex<Inner>,
}

impl RunTracker {
    /// `max_active` is the process-wide ceiling (0 = unlimited).
    pub fn new(max_active: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                active: HashMap::new(),
                last: HashMap::new(),
                max_active,
            }),
        }
    }

    pub fn capacity(&self) -> usize {
        self.inner.lock().unwrap().max_active
    }

    pub fn active_count(&self) -> usize {
        self.inner.lock().unwrap().active.len()
    }

    /// Register a freshly spawned run. On rejection the spawned task is
    /// aborted so a loser in a race leaves no orphan work (its already-
    /// enqueued jobs simply sit back in the `pending` queue).
    pub fn begin(&self, ar: ActiveRun) -> Result<(), BeginErr> {
        let mut g = self.inner.lock().unwrap();
        if g.active.contains_key(&ar.project_id) {
            ar.task.abort();
            return Err(BeginErr::AlreadyRunning);
        }
        if g.max_active > 0 && g.active.len() >= g.max_active {
            ar.task.abort();
            return Err(BeginErr::CapReached);
        }
        g.active.insert(ar.project_id, ar);
        Ok(())
    }

    /// Called by a background task when its run finishes. Removes the active
    /// entry (only if it is still this run — a newer run may have replaced it)
    /// and records the `last` summary.
    pub fn finalize(&self, project_id: Uuid, run_id: Uuid, summary: RunSummary) {
        let mut g = self.inner.lock().unwrap();
        if g.active.get(&project_id).map(|a| a.run_id) == Some(run_id) {
            g.active.remove(&project_id);
        }
        g.last.insert(project_id, summary);
    }

    /// The live fair controller for a project's active run (cancel/status).
    pub fn fair_of(&self, project_id: Uuid) -> Option<Arc<Mutex<runner::FairState>>> {
        self.inner
            .lock()
            .unwrap()
            .active
            .get(&project_id)
            .and_then(|a| a.fair.clone())
    }

    fn live_progress(a: &ActiveRun) -> (usize, bool) {
        a.fair
            .as_ref()
            .map(|f| {
                let s = f.lock().unwrap();
                let created = s.total_links();
                let target = s.policy().target;
                (created, target == 0 || created >= target)
            })
            .unwrap_or((0, a.links_target == 0))
    }

    /// Live status of a project's active run, or `None` when idle.
    pub fn active_status(&self, project_id: Uuid) -> Option<serde_json::Value> {
        let g = self.inner.lock().unwrap();
        let a = g.active.get(&project_id)?;
        let (links_created, reached) = Self::live_progress(a);
        Some(serde_json::json!({
            "run_id": a.run_id,
            "project_id": a.project_id.to_string(),
            "status": if a.task.is_finished() { "finishing" } else { "running" },
            "started_at": a.started_at.to_rfc3339(),
            "links_target": a.links_target,
            "engines": a.engines,
            "auto": a.auto,
            "links_created": links_created,
            "reached_target": reached,
        }))
    }

    pub fn latest(&self, project_id: Uuid) -> Option<RunSummary> {
        self.inner.lock().unwrap().last.get(&project_id).cloned()
    }

    /// Every active run (the global "what's running now" view).
    pub fn all_active(&self) -> Vec<serde_json::Value> {
        let g = self.inner.lock().unwrap();
        g.active
            .values()
            .map(|a| {
                let (links_created, reached) = Self::live_progress(a);
                serde_json::json!({
                    "run_id": a.run_id,
                    "project_id": a.project_id.to_string(),
                    "status": if a.task.is_finished() { "finishing" } else { "running" },
                    "started_at": a.started_at.to_rfc3339(),
                    "links_target": a.links_target,
                    "links_created": links_created,
                    "reached_target": reached,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_task() -> tokio::task::JoinHandle<()> {
        tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        })
    }

    fn mk_ar(_tr: &RunTracker, project: Uuid, run: Uuid) -> ActiveRun {
        ActiveRun {
            run_id: run,
            project_id: project,
            started_at: chrono::DateTime::from(chrono::Utc::now()),
            links_target: 10,
            engines: 3,
            auto: true,
            fair: None,
            task: dummy_task(),
        }
    }

    #[tokio::test]
    async fn second_begin_same_project_conflicts() {
        let tr = RunTracker::new(8);
        let p = Uuid::new_v4();
        tr.begin(mk_ar(&tr, p, Uuid::new_v4())).unwrap();
        let err = tr.begin(mk_ar(&tr, p, Uuid::new_v4())).unwrap_err();
        assert_eq!(err, BeginErr::AlreadyRunning);
        assert_eq!(tr.active_count(), 1);
    }

    #[tokio::test]
    async fn global_cap_rejects_excess_projects() {
        let tr = RunTracker::new(1); // cap 1
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        tr.begin(mk_ar(&tr, a, Uuid::new_v4())).unwrap();
        let err = tr.begin(mk_ar(&tr, b, Uuid::new_v4())).unwrap_err();
        assert_eq!(err, BeginErr::CapReached);
        assert_eq!(tr.active_count(), 1);
    }

    #[tokio::test]
    async fn finalize_releases_project_for_rerun() {
        let tr = RunTracker::new(8);
        let p = Uuid::new_v4();
        let run1 = Uuid::new_v4();
        tr.begin(mk_ar(&tr, p, run1)).unwrap();
        assert!(tr.active_status(p).is_some());
        tr.finalize(
            p,
            run1,
            RunSummary {
                run_id: run1,
                finished_at: chrono::DateTime::from(chrono::Utc::now()),
                status: "ok".into(),
                payload: serde_json::json!({ "links_created": 7 }),
            },
        );
        assert!(tr.active_status(p).is_none());
        assert!(tr.latest(p).is_some());
        // a fresh run on the same project is now allowed
        tr.begin(mk_ar(&tr, p, Uuid::new_v4())).unwrap();
        assert_eq!(tr.active_count(), 1);
    }
}
