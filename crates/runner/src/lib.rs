//! # Ser-runner — job queue + concurrent worker pool (Feature #8).
//!
//! Wires together:
//!
//! * **Queue** — the `jobs` table in Postgres, claimed atomically via
//!   [`ser_db::Repo::claim_next_pending`] (advisory-lock guarded).
//! * **Driver** — `engine::executor::execute_plan` driven by the
//!   [`engine::driver::Driver`] impl, either F#6's `HttpDriver` or
//!   F#7's `WebDriver`; both are async + Send so workers can share them.
//! * **Pool** — N concurrent `tokio::task`s each running one job
//!   end-to-end (claim → resolve → execute → persist), with panic
//!   isolation so one bad worker never tears down the others.
//!
//! Tests live in `crates/runner/tests/pool_integration.rs` and are gated
//! on the Postgres container (same pattern as `crates/db/tests`).

pub mod config;
pub mod execute;
pub mod pool;
pub mod specprovider;

pub use config::RunnerConfig;
pub use execute::{DriverRegistry, JobOutcome, JobRunner, SubmissionContext};
pub use pool::{PoolSummary, WorkerPool};
pub use specprovider::{SpecRegistry, SpecSource};

/// Convenience: build a pool for an inline test registry.
pub fn pool_for(
    db: sqlx::PgPool,
    cfg: RunnerConfig,
    registry: SpecRegistry,
    drivers: DriverRegistry,
) -> WorkerPool {
    WorkerPool::new(db, cfg, SpecSource::Inline(registry), drivers)
}

/// Convenience: build a pool that reads engine `.ini` files from disk.
pub fn pool_for_disk(
    db: sqlx::PgPool,
    cfg: RunnerConfig,
    gsa_roots: Vec<std::path::PathBuf>,
    drivers: DriverRegistry,
) -> WorkerPool {
    let src = SpecSource::FromDisk(specprovider::DiskSpecSource::new(gsa_roots));
    WorkerPool::new(db, cfg, src, drivers)
}
