//! Runner configuration.

#[derive(Debug, Clone)]
pub struct RunnerConfig {
    /// How many workers pull concurrently. 1 for deterministic tests,
    /// N for production fanout (GSA typically runs 25-50 in parallel).
    pub concurrent_workers: usize,
    /// How long the leader waits for one more claim before deciding the
    /// queue is drained, then signals the pool to finish.
    pub leader_idle_timeout: std::time::Duration,
    /// Per-job timeout (driver I/O).
    pub per_job_timeout: std::time::Duration,
    /// How many times to requeue a job that panicked or errored.
    pub max_retries: u32,
}

impl Default for RunnerConfig {
    fn default() -> Self {
        Self {
            concurrent_workers: 4,
            leader_idle_timeout: std::time::Duration::from_millis(150),
            per_job_timeout: std::time::Duration::from_secs(60),
            max_retries: 1,
        }
    }
}
