//! Concurrent worker pool.
//!
//! ```text
//! leader ──┬──▶ worker 0  (claim -> run_one -> persist)
//!          ├──▶ worker 1
//!          └──▶ worker N
//!   (leader stops when queue drains; pool waits for all in-flight)
//! ```
//!
//! Each worker gets its own `mpsc` channel — `Receiver::recv` borrows
//! `&mut self`, so a single receiver can't be shared across tasks. The
//! leader dispatches job ids round-robin. Each worker claims the job
//! atomically (via [`ser_db::Repo::claim_by_id`]) and runs it
//! end-to-end. Panics in a worker are captured by the `JoinSet`,
//! counted, and do NOT tear down the other workers or the leader.

use std::sync::Arc;

use tokio::sync::mpsc;
use tokio::task::JoinSet;
use uuid::Uuid;

use crate::config::RunnerConfig;
use crate::execute::{DriverRegistry, JobOutcome, JobRunner};
use crate::specprovider::SpecSource;

/// Aggregate of a single `run()` call.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PoolSummary {
    /// Jobs that were claimed and executed (success or failure).
    pub claimed: usize,
    /// Jobs that ended `done`.
    pub done: usize,
    /// Jobs that ended `failed` (driver errors, spec missing, etc.).
    pub failed: usize,
    /// Worker tasks that panicked (crash isolation caught them).
    pub panics: usize,
    /// Wall-clock runtime of the whole pool run.
    pub duration: std::time::Duration,
}

/// One project's worth of queued jobs, executed by `concurrent_workers`
/// tokio tasks. `Clone`-safe: the pool is the configuration record; the
/// `run()` method is the only thing that touches the DB.
#[derive(Clone)]
pub struct WorkerPool {
    db: sqlx::PgPool,
    cfg: RunnerConfig,
    spec: SpecSource,
    drivers: Arc<DriverRegistry>,
    ctx: crate::execute::SubmissionContext,
}

impl WorkerPool {
    /// Build a pool.
    pub fn new(
        db: sqlx::PgPool,
        cfg: RunnerConfig,
        spec: SpecSource,
        drivers: DriverRegistry,
    ) -> Self {
        Self {
            db,
            cfg,
            spec,
            drivers: Arc::new(drivers),
            ctx: Default::default(),
        }
    }

    /// Attach the project-level context (target URL, keyword, files, seed).
    pub fn with_context(mut self, ctx: crate::execute::SubmissionContext) -> Self {
        self.ctx = ctx;
        self
    }

    /// Run until the project's `jobs` table has no `pending` rows.
    pub async fn run(&self, project_id: Uuid) -> PoolSummary {
        let started = std::time::Instant::now();
        let workers = self.cfg.concurrent_workers.max(1);

        // One channel PER worker — `Receiver::recv` needs `&mut self`, so
        // each worker takes ownership of exactly one receiver.  The senders
        // stay in `senders`; we drop them at the end of the leader loop,
        // which closes every channel and unblocks the workers' recv().
        let mut pairs: Vec<(mpsc::Sender<Uuid>, mpsc::Receiver<Uuid>)> = Vec::with_capacity(workers);
        for _ in 0..workers {
            pairs.push(mpsc::channel::<Uuid>(workers));
        }
        let mut senders: Vec<mpsc::Sender<Uuid>> = Vec::with_capacity(workers);
        let mut receivers: Vec<mpsc::Receiver<Uuid>> = Vec::with_capacity(workers);
        for (t, r) in pairs.drain(..) {
            senders.push(t);
            receivers.push(r);
        }

        let summary = Arc::new(std::sync::Mutex::new(PoolSummary::default()));
        let mut set = JoinSet::new();
        for rx in receivers {
            set.spawn(worker_loop(self.clone(), Arc::clone(&summary), rx));
        }

        // Leader: pull pending ids, dispatch round-robin, until the
        // queue drains.  After the loop, drop every sender so each
        // worker's `recv()` returns None and the worker exits.
        {
            let repo = ser_db::Repo::new(self.db.clone());
            let mut next = 0usize;
            loop {
                let jobs = match repo
                    .list_jobs(project_id, Some(ser_db::JobStatus::Pending))
                    .await
                {
                    Ok(jobs) => jobs,
                    Err(e) => {
                        eprintln!("runner: list_jobs({project_id}) failed: {e} — closing leader");
                        break;
                    }
                };
                if jobs.is_empty() {
                    break; // drained
                }
                for j in jobs {
                    let tx = &senders[next % senders.len()];
                    next += 1;
                    if tx.send(j.id).await.is_err() {
                        break; // that worker is gone; the rest will be
                        // unblocked by the drop below anyway.
                    }
                }
                tokio::time::sleep(self.cfg.leader_idle_timeout).await;
            }
        }
        // CLOSE THE CHANNELS: drop every sender so each worker's `recv()`
        // returns None and its loop ends.
        drop(senders);

        // Await every worker; count crashes (panics) separately.
        let mut panics = 0usize;
        let mut cancelled = 0usize;
        while let Some(res) = set.join_next().await {
            if let Err(e) = res {
                if e.is_panic() {
                    panics += 1;
                } else {
                    cancelled += 1;
                }
            }
        }
        if panics + cancelled > 0 {
            eprintln!(
                "runner: {panics} panic(s), {cancelled} cancel(s) — isolation held, pool continues"
            );
        }

        let mut s = summary.lock().unwrap();
        s.panics = panics;
        s.duration = started.elapsed();
        *s
    }
}

/// One worker's loop: pull `job_id` over the channel, claim atomically,
/// run the job, persist, continue.
async fn worker_loop(pool: WorkerPool, summary: Arc<std::sync::Mutex<PoolSummary>>, mut rx: mpsc::Receiver<Uuid>) {
    let cfg = pool.cfg.clone();
    let spec = pool.spec.clone();
    let drivers = Arc::clone(&pool.drivers);
    let ctx = pool.ctx.clone();
    let db = pool.db.clone();

    while let Some(job_id) = rx.recv().await {
        let repo = ser_db::Repo::new(db.clone());

        // Claim atomically. If someone beat us to it, the guard clause
        // matches zero rows and we get `None` — skip silently.
        let claimed = match repo.claim_by_id(job_id).await {
            Ok(j) => j,
            Err(e) => {
                let mut s = summary.lock().unwrap();
                s.failed += 1;
                eprintln!("runner: claim_by_id({job_id}) errored: {e}");
                continue;
            }
        };
        let Some(job) = claimed else {
            continue; // already claimed elsewhere (or already terminal)
        };

        // Build the runner locally (all inputs cloned from `pool`).
        let runner = JobRunner::new(cfg.clone(), spec.clone(), drivers.clone(), ctx.clone());
        let timeout = cfg.per_job_timeout;

        let outcome: JobOutcome =
            match tokio::time::timeout(timeout, runner.run_one(&repo, &job)).await {
                Ok(o) => o,
                Err(_) => {
                    // Timed out — the inner future was cancelled; the
                    // job is still `running`. Mark it failed explicitly.
                    let _ = repo
                        .finish_job(
                            job.id,
                            ser_db::JobStatus::Failed,
                            Some("worker-side timeout (outer)"),
                        )
                        .await;
                    let mut s = summary.lock().unwrap();
                    s.claimed += 1;
                    s.failed += 1;
                    continue;
                }
            };

        let mut s = summary.lock().unwrap();
        s.claimed += 1;
        if outcome.is_done() {
            s.done += 1;
        } else {
            s.failed += 1;
        }
    }
}
