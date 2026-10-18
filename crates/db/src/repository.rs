use sqlx::{FromRow, PgPool, Row};
use uuid::Uuid;

use crate::model::{
    Engine, Job, JobStatus, Keyword, Project, ProjectResult, Proxy, ResultKind, ResultStats,
    SubmissionResult,
};

/// CRUD + pipeline helpers over the core tables.
///
/// `Repo` **owns** its `PgPool` (a cheap `Arc` clone of the shared pool), not
/// a borrowed `&'a PgPool`.  Owning means a `Repo` can be captured into a
/// `'static` future (e.g. `tokio::spawn` / a boxed async block) without the
/// borrow-outlives-frame problem — the pattern the F#14 bulk geo verifier
/// needs.  `PgPool` is connection-pooled behind an `Arc`, so the clone is
/// cheap and shares the same pool; callers pass `state.pool.clone()`.
#[derive(Clone)]
pub struct Repo {
    pool: PgPool,
}

impl Repo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }


    // ─────────────────────────────────────────────────────────────
    // F#16 — stats & analytics queries (raw per-bucket counts; shaped by
    // ser_engine::stats into the dashboard model).
    // ─────────────────────────────────────────────────────────────

    /// `status -> count` across ALL jobs (global stats page).
    pub async fn job_status_counts(&self) -> anyhow::Result<Vec<(String, i64)>> {
        let rows =
            sqlx::query("SELECT status, COUNT(*)::bigint FROM jobs GROUP BY status")
                .fetch_all(&self.pool)
                .await?;
        let out = rows
            .into_iter()
            .map(|r| -> anyhow::Result<(String, i64)> {
                Ok((r.try_get::<String, _>(0)?, r.try_get::<i64, _>(1)?))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(out)
    }

    /// `kind -> count` buckets across the WHOLE results table (global dashboard).
    pub async fn result_kind_buckets(&self) -> anyhow::Result<Vec<(String, i64)>> {
        let rows =
            sqlx::query("SELECT kind, COUNT(*)::bigint FROM results GROUP BY kind")
                .fetch_all(&self.pool)
                .await?;
        let out = rows
            .into_iter()
            .map(|r| -> anyhow::Result<(String, i64)> {
                Ok((r.try_get::<String, _>(0)?, r.try_get::<i64, _>(1)?))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(out)
    }

    /// How many outcomes were captcha walls (the GSA-distinct metric).
    pub async fn captcha_count(&self) -> anyhow::Result<i64> {
        let row = sqlx::query(
            "SELECT COUNT(*)::bigint FROM results WHERE captchas = TRUE",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(row.try_get::<i64, _>(0)?)
    }

    /// Distinct created links across all successes (GSA "unique links").
    pub async fn unique_link_count(&self) -> anyhow::Result<i64> {
        let row = sqlx::query(
            "SELECT COUNT(DISTINCT url)::bigint FROM results
             WHERE kind = 'success' AND url IS NOT NULL",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(row.try_get::<i64, _>(0)?)
    }

    /// Per-day `(date, links, captchas, other)` for a timeline window (`since`).
    pub async fn stats_daily(
        &self,
        since: chrono::NaiveDateTime,
    ) -> anyhow::Result<Vec<(chrono::NaiveDate, i64, i64, i64)>> {
        let rows = sqlx::query(
            "SELECT created_at::date AS d,
                    COUNT(*) FILTER (WHERE kind = 'success' AND url IS NOT NULL)::bigint AS links,
                    COUNT(*) FILTER (WHERE captchas = TRUE)::bigint AS captchas,
                    COUNT(*) FILTER (WHERE kind <> 'success')::bigint AS other
             FROM results
             WHERE created_at >= $1
             GROUP BY d
             ORDER BY d",
        )
        .bind(since)
        .fetch_all(&self.pool)
        .await?;
        let out = rows
            .into_iter()
            .map(|r| -> anyhow::Result<(chrono::NaiveDate, i64, i64, i64)> {
                Ok((
                    r.try_get::<chrono::NaiveDate, _>(0)?,
                    r.try_get::<i64, _>(1)?,
                    r.try_get::<i64, _>(2)?,
                    r.try_get::<i64, _>(3)?,
                ))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(out)
    }

    /// Per-engine rollup: `(id, name, jobs, successes, failures, captchas, unique_links)`.
    pub async fn stats_per_engine(
        &self,
    ) -> anyhow::Result<Vec<(String, String, i64, i64, i64, i64, i64)>> {
        let rows = sqlx::query(
            "SELECT e.id::text, e.engine_name AS name,
                (SELECT COUNT(*)::bigint FROM jobs j WHERE j.engine_id = e.id) AS jobs,
                (SELECT COUNT(*)::bigint FROM results r JOIN jobs j ON j.id = r.job_id
                     WHERE j.engine_id = e.id AND r.kind = 'success' AND r.url IS NOT NULL) AS successes,
                (SELECT COUNT(*)::bigint FROM results r JOIN jobs j ON j.id = r.job_id
                     WHERE j.engine_id = e.id AND r.kind IN ('error','rejected')) AS failures,
                (SELECT COUNT(*)::bigint FROM results r JOIN jobs j ON j.id = r.job_id
                     WHERE j.engine_id = e.id AND r.captchas = TRUE) AS captchas,
                (SELECT COUNT(DISTINCT r.url)::bigint FROM results r JOIN jobs j ON j.id = r.job_id
                     WHERE j.engine_id = e.id AND r.kind = 'success' AND r.url IS NOT NULL) AS unique_links
             FROM engines e
             WHERE EXISTS (SELECT 1 FROM jobs j WHERE j.engine_id = e.id)
             ORDER BY successes DESC, jobs DESC, e.engine_name",
        )
        .fetch_all(&self.pool)
        .await?;
        let out = rows
            .into_iter()
            .map(|r| -> anyhow::Result<(String, String, i64, i64, i64, i64, i64)> {
                Ok((
                    r.try_get::<String, _>(0)?,
                    r.try_get::<String, _>(1)?,
                    r.try_get::<i64, _>(2)?,
                    r.try_get::<i64, _>(3)?,
                    r.try_get::<i64, _>(4)?,
                    r.try_get::<i64, _>(5)?,
                    r.try_get::<i64, _>(6)?,
                ))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(out)
    }

    /// Per-project rollup: same shape, joined through `jobs.project_id`.
    pub async fn stats_per_project(
        &self,
    ) -> anyhow::Result<Vec<(String, String, i64, i64, i64, i64, i64)>> {
        let rows = sqlx::query(
            "SELECT p.id::text, p.name,
                (SELECT COUNT(*)::bigint FROM jobs j WHERE j.project_id = p.id) AS jobs,
                (SELECT COUNT(*)::bigint FROM results r JOIN jobs j ON j.id = r.job_id
                     WHERE j.project_id = p.id AND r.kind = 'success' AND r.url IS NOT NULL) AS successes,
                (SELECT COUNT(*)::bigint FROM results r JOIN jobs j ON j.id = r.job_id
                     WHERE j.project_id = p.id AND r.kind IN ('error','rejected')) AS failures,
                (SELECT COUNT(*)::bigint FROM results r JOIN jobs j ON j.id = r.job_id
                     WHERE j.project_id = p.id AND r.captchas = TRUE) AS captchas,
                (SELECT COUNT(DISTINCT r.url)::bigint FROM results r JOIN jobs j ON j.id = r.job_id
                     WHERE j.project_id = p.id AND r.kind = 'success' AND r.url IS NOT NULL) AS unique_links
             FROM projects p
             WHERE EXISTS (SELECT 1 FROM jobs j WHERE j.project_id = p.id)
             ORDER BY successes DESC, jobs DESC, p.name",
        )
        .fetch_all(&self.pool)
        .await?;
        let out = rows
            .into_iter()
            .map(|r| -> anyhow::Result<(String, String, i64, i64, i64, i64, i64)> {
                Ok((
                    r.try_get::<String, _>(0)?,
                    r.try_get::<String, _>(1)?,
                    r.try_get::<i64, _>(2)?,
                    r.try_get::<i64, _>(3)?,
                    r.try_get::<i64, _>(4)?,
                    r.try_get::<i64, _>(5)?,
                    r.try_get::<i64, _>(6)?,
                ))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(out)
    }

    // ── projects ──────────────────────────────────────────────────────────
    pub async fn create_project(&self, name: &str, description: Option<&str>) -> anyhow::Result<Project> {
        let row = sqlx::query(
            "INSERT INTO projects (name, description) VALUES ($1, $2)
             RETURNING id, name, description, created_at, updated_at, use_proxies",
        )
        .bind(name)
        .bind(description)
        .fetch_one(&self.pool)
        .await?;
        Ok(FromRow::from_row(&row)?)
    }

    /// Update name/description of an existing project.
    pub async fn update_project(
        &self,
        id: Uuid,
        name: &str,
        description: Option<&str>,
    ) -> anyhow::Result<Project> {
        let row = sqlx::query(
            "UPDATE projects SET name = $1, description = $2 WHERE id = $3
             RETURNING id, name, description, created_at, updated_at, use_proxies",
        )
        .bind(name)
        .bind(description)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| anyhow::anyhow!("project {id} not found"))?;
        Ok(FromRow::from_row(&row)?)
    }

    /// Delete a project. Cascades to engines/keywords/jobs/results via DB FKs.
    pub async fn delete_project(&self, id: Uuid) -> anyhow::Result<bool> {
        let n = sqlx::query("DELETE FROM projects WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(n.rows_affected() > 0)
    }

    pub async fn list_projects(&self) -> anyhow::Result<Vec<Project>> {
        let rows = sqlx::query(
            "SELECT id, name, description, created_at, updated_at, use_proxies
             FROM projects ORDER BY created_at",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|r| Project::from_row(&r)).collect::<Result<_, _>>()?)
    }

    pub async fn project_by_name(&self, name: &str) -> anyhow::Result<Option<Project>> {
        let row = sqlx::query(
            "SELECT id, name, description, created_at, updated_at, use_proxies
             FROM projects WHERE name = $1",
        )
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| Project::from_row(&r)).transpose()?)
    }

    /// Fetch one project by id (404-friendly: None when absent).
    pub async fn project_by_id(&self, id: Uuid) -> anyhow::Result<Option<Project>> {
        let row = sqlx::query(
            "SELECT id, name, description, created_at, updated_at, use_proxies
             FROM projects WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| Project::from_row(&r)).transpose()?)
    }

    // ── engines ───────────────────────────────────────────────────────────
    pub async fn upsert_engine(
        &self,
        project_id: Uuid,
        name: &str,
        engine_type: &str,
        url: &str,
        file_path: &str,
        step_count: i32,
    ) -> anyhow::Result<Engine> {
        let row = sqlx::query(
            "INSERT INTO engines (project_id, engine_name, engine_type, url, file_path, step_count)
             VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (project_id, file_path) DO UPDATE SET
                engine_name = EXCLUDED.engine_name,
                engine_type = EXCLUDED.engine_type,
                url         = EXCLUDED.url,
                step_count  = EXCLUDED.step_count
             RETURNING id, project_id, engine_name, engine_type, url, file_path, step_count, enabled, created_at",
        )
        .bind(project_id)
        .bind(name)
        .bind(engine_type)
        .bind(url)
        .bind(file_path)
        .bind(step_count)
        .fetch_one(&self.pool)
        .await?;
        Ok(FromRow::from_row(&row)?)
    }

    /// Update mutable fields of an engine (identifying columns unchanged).
    pub async fn update_engine(
        &self,
        id: Uuid,
        name: &str,
        engine_type: &str,
        url: &str,
        step_count: i32,
    ) -> anyhow::Result<Engine> {
        let row = sqlx::query(
            "UPDATE engines SET engine_name = $1, engine_type = $2, url = $3, step_count = $4
             WHERE id = $5
             RETURNING id, project_id, engine_name, engine_type, url, file_path, step_count, enabled, created_at",
        )
        .bind(name)
        .bind(engine_type)
        .bind(url)
        .bind(step_count)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| anyhow::anyhow!("engine {id} not found"))?;
        Ok(FromRow::from_row(&row)?)
    }

    /// Toggle enabled/disabled on an engine.
    pub async fn set_engine_enabled(&self, id: Uuid, enabled: bool) -> anyhow::Result<Engine> {
        let row = sqlx::query(
            "UPDATE engines SET enabled = $1 WHERE id = $2
             RETURNING id, project_id, engine_name, engine_type, url, file_path, step_count, enabled, created_at",
        )
        .bind(enabled)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| anyhow::anyhow!("engine {id} not found"))?;
        Ok(FromRow::from_row(&row)?)
    }

    /// Delete one engine row (history rows stay; jobs referencing it are unaffected
    /// because jobs store the engine id, not an FK to a unique engine set).
    pub async fn delete_engine(&self, id: Uuid) -> anyhow::Result<bool> {
        let n = sqlx::query("DELETE FROM engines WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(n.rows_affected() > 0)
    }

    /// Same as `list_engines` but includes disabled rows (UI needs to see the
    /// enabled toggle state). Ordered by name for stable display.
    pub async fn list_engines_all(&self, project_id: Uuid) -> anyhow::Result<Vec<Engine>> {
        let rows = sqlx::query(
            "SELECT id, project_id, engine_name, engine_type, url, file_path, step_count, enabled, created_at
             FROM engines WHERE project_id = $1 ORDER BY engine_name",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|r| Engine::from_row(&r)).collect::<Result<_, _>>()?)
    }

    pub async fn list_engines(&self, project_id: Uuid) -> anyhow::Result<Vec<Engine>> {
        let rows = sqlx::query(
            "SELECT id, project_id, engine_name, engine_type, url, file_path, step_count, enabled, created_at
             FROM engines WHERE project_id = $1 AND enabled ORDER BY engine_name",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|r| Engine::from_row(&r)).collect::<Result<_, _>>()?)
    }

    pub async fn engine_by_file(&self, project_id: Uuid, file_path: &str) -> anyhow::Result<Option<Engine>> {
        let row = sqlx::query(
            "SELECT id, project_id, engine_name, engine_type, url, file_path, step_count, enabled, created_at
             FROM engines WHERE project_id = $1 AND file_path = $2",
        )
        .bind(project_id)
        .bind(file_path)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| Engine::from_row(&r)).transpose()?)
    }

    /// Look up an engine by primary key (any project).  Used by the F#8
    /// worker pool to resolve a job's `engine_id` to its `.ini` file path.
    pub async fn engine_by_id(&self, engine_id: Uuid) -> anyhow::Result<Option<Engine>> {
        let row = sqlx::query(
            "SELECT id, project_id, engine_name, engine_type, url, file_path, step_count, enabled, created_at
             FROM engines WHERE id = $1",
        )
        .bind(engine_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| Engine::from_row(&r)).transpose()?)
    }

    /// Atomically claim a *specific* job: flip it from `pending` to
    /// `running` only if no other worker beat us to it.  Safe under
    /// concurrency — a second claimant's UPDATE matches zero rows and
    /// returns `None`.  This is the F#8 seam: the pool feeds job ids over
    /// a channel, and each worker calls this to take ownership.
    pub async fn claim_by_id(&self, job_id: Uuid) -> anyhow::Result<Option<Job>> {
        let token = format!("{}-{}", uuid::Uuid::new_v4(), std::process::id());
        let row = sqlx::query(
            "UPDATE jobs
             SET status = 'running', attempts = attempts + 1, started_at = now(),
                 claimed_by = $2
             WHERE id = $1
               AND status = 'pending'
               AND claimed_by IS NULL
             RETURNING id, project_id, engine_id, keyword_id, status, attempts, last_error, started_at, finished_at, created_at",
        )
        .bind(job_id)
        .bind(&token)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| Job::from_row(&r)).transpose()?)
    }

    // ── keywords ──────────────────────────────────────────────────────────
    /// Add (or return, on case-insensitive match) a keyword.  Text is
    /// normalized (trim + collapse whitespace) so near-duplicates share
    /// one row; matching on `lower(text)` makes "SEO" and "seo" the same
    /// keyword while preserving the original casing of the first entry.
    pub async fn add_keyword(&self, project_id: Uuid, text: &str) -> anyhow::Result<Keyword> {
        let text = engine::normalize_keyword(text);
        if let Some(row) = sqlx::query(
            "SELECT id, project_id, text, added_at
             FROM keywords
             WHERE project_id = $1 AND lower(text) = lower($2)
             LIMIT 1",
        )
        .bind(project_id)
        .bind(&text)
        .fetch_optional(&self.pool)
        .await?
        {
            return Ok(FromRow::from_row(&row)?);
        }
        let row = sqlx::query(
            "INSERT INTO keywords (project_id, text) VALUES ($1, $2)
             ON CONFLICT (project_id, text) DO UPDATE SET text = EXCLUDED.text
             RETURNING id, project_id, text, added_at",
        )
        .bind(project_id)
        .bind(&text)
        .fetch_one(&self.pool)
        .await?;
        Ok(FromRow::from_row(&row)?)
    }

    /// Bulk-add keywords (e.g. paste a keyword list).  Each line is
    /// normalized and individually spintax-expanded to distinct concrete
    /// keywords (bounded), matching GSA's spintax keyword lists.
    /// Returns the distinct new count + total stored.
    pub async fn add_keywords(&self, project_id: Uuid, raw_lines: &[String]) -> anyhow::Result<(usize, usize)> {
        let max_variants = 20;
        let mut expanded: Vec<String> = Vec::new();
        for line in raw_lines {
            for v in engine::expand_keyword(line, max_variants) {
                if !expanded.iter().any(|x| x.eq_ignore_ascii_case(&v)) {
                    expanded.push(v);
                }
            }
        }
        // Distinct stored rows (case-insensitive) — matches the actual number
        // of keyword rows the project ends up with from this batch.
        let stored = expanded.len();
        for v in &expanded {
            self.add_keyword(project_id, v).await?;
        }
        Ok((stored, stored))
    }

    pub async fn keyword_by_id(&self, id: Uuid) -> anyhow::Result<Option<Keyword>> {
        let row = sqlx::query(
            "SELECT id, project_id, text, added_at FROM keywords WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| Keyword::from_row(&r)).transpose()?)
    }

    /// Delete a keyword.  Jobs referencing it are kept (keyword_id SET NULL
    /// by FK) so past run history stays intact.
    pub async fn delete_keyword(&self, id: Uuid) -> anyhow::Result<bool> {
        let res = sqlx::query("DELETE FROM keywords WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    pub async fn list_keywords(&self, project_id: Uuid) -> anyhow::Result<Vec<Keyword>> {
        let rows = sqlx::query(
            "SELECT id, project_id, text, added_at FROM keywords WHERE project_id = $1 ORDER BY added_at",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|r| Keyword::from_row(&r)).collect::<Result<_, _>>()?)
    }

    // ── jobs ──────────────────────────────────────────────────────────────
    pub async fn create_job(
        &self,
        project_id: Uuid,
        engine_id: Option<Uuid>,
        keyword_id: Option<Uuid>,
    ) -> anyhow::Result<Job> {
        let row = sqlx::query(
            "INSERT INTO jobs (project_id, engine_id, keyword_id) VALUES ($1, $2, $3)
             RETURNING id, project_id, engine_id, keyword_id, status, attempts, last_error, started_at, finished_at, created_at",
        )
        .bind(project_id)
        .bind(engine_id)
        .bind(keyword_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(FromRow::from_row(&row)?)
    }

    /// Atomically claim the oldest pending job for a project (worker pickup).
    ///
    /// `pg_advisory_xact_lock` on a per-project key serialises claim attempts
    /// so that under concurrency every pending job is claimed exactly once
    /// and no worker double-claims the same row: the lock is acquired at the
    /// start of the transaction, the UPDATE runs with a `status='pending' AND
    /// claimed_by IS NULL` guard, and both the UPDATE and the lock release
    /// on COMMIT. A losing worker's UPDATE matches zero rows and returns
    /// `None` — the test `job_claim_is_atomic_under_concurrency` asserts this
    /// by requiring exactly N claims of N distinct jobs under N concurrent
    /// claimants.
    pub async fn claim_next_pending(&self, project_id: Uuid) -> anyhow::Result<Option<Job>> {
        let mut tx = self.pool.begin().await?;
        // Per-project advisory lock key: hash the UUID bytes into an i64 so
        // distinct projects have distinct locks, and the lock is released on
        // COMMIT.
        let mut h: i64 = 0x53455231_69706c69i64; // "SER-replica"
        for b in project_id.as_bytes() {
            h = h.wrapping_mul(31).wrapping_add(*b as i64);
        }
        sqlx::query("SELECT pg_advisory_xact_lock($1::bigint)")
            .bind(h)
            .execute(&mut *tx)
            .await?;
        let token = format!("{}-{}", uuid::Uuid::new_v4(), std::process::id());
        let row = sqlx::query(
            "UPDATE jobs
             SET status = 'running', attempts = attempts + 1, started_at = now(),
                 claimed_by = $2
             WHERE id = (
                SELECT id FROM jobs WHERE project_id = $1 AND status = 'pending'
                  AND claimed_by IS NULL
                ORDER BY created_at LIMIT 1
             )
               AND status = 'pending'
               AND claimed_by IS NULL
             RETURNING id, project_id, engine_id, keyword_id, status, attempts, last_error, started_at, finished_at, created_at",
        )
        .bind(project_id)
        .bind(&token)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(row.map(|r| Job::from_row(&r)).transpose()?)
    }

    pub async fn finish_job(
        &self,
        job_id: Uuid,
        status: JobStatus,
        last_error: Option<&str>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE jobs SET status = $2, last_error = $3, finished_at = now() WHERE id = $1",
        )
        .bind(job_id)
        .bind(status.as_str())
        .bind(last_error)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Cancel a job. Allowed from Pending or Running; idempotent if already terminal.
    /// Returns the updated row, or None if it did not exist OR was already terminal.
    pub async fn cancel_job(&self, id: Uuid) -> anyhow::Result<Option<Job>> {
        let row = sqlx::query(
            "UPDATE jobs SET status = 'cancelled',
                     last_error = COALESCE(last_error, 'cancelled by user'),
                     finished_at = COALESCE(finished_at, now())
             WHERE id = $1 AND status IN ('pending', 'running')
             RETURNING id, project_id, engine_id, keyword_id, status, attempts, last_error,
                       started_at, finished_at, created_at",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| Ok::<Job, anyhow::Error>(FromRow::from_row(&r)?)).transpose()?)
    }

    pub async fn job_by_id(&self, job_id: Uuid) -> anyhow::Result<Option<Job>> {
        let row = sqlx::query(
            "SELECT id, project_id, engine_id, keyword_id, status, attempts, last_error, started_at, finished_at, created_at
             FROM jobs WHERE id = $1",
        )
        .bind(job_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| Job::from_row(&r)).transpose()?)
    }

    pub async fn list_jobs(
        &self,
        project_id: Uuid,
        filter: Option<JobStatus>,
    ) -> anyhow::Result<Vec<Job>> {
        let rows = if let Some(st) = filter {
            sqlx::query(
                "SELECT id, project_id, engine_id, keyword_id, status, attempts, last_error, started_at, finished_at, created_at
                 FROM jobs WHERE project_id = $1 AND status = $2 ORDER BY created_at DESC",
            )
            .bind(project_id)
            .bind(st.as_str())
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query(
                "SELECT id, project_id, engine_id, keyword_id, status, attempts, last_error, started_at, finished_at, created_at
                 FROM jobs WHERE project_id = $1 ORDER BY created_at DESC",
            )
            .bind(project_id)
            .fetch_all(&self.pool)
            .await?
        };
        Ok(rows.into_iter().map(|r| Job::from_row(&r)).collect::<Result<_, _>>()?)
    }

    // ── results ───────────────────────────────────────────────────────────
    pub async fn record_result(
        &self,
        job_id: Uuid,
        kind: ResultKind,
        url: Option<&str>,
        anchor: Option<&str>,
        message: Option<&str>,
        payload: Option<serde_json::Value>,
        // F#16: captcha wall flag (GSA tracks this as its own metric, not a failure).
        captchas: bool,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO results (job_id, kind, url, anchor, message, payload, captchas)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(job_id)
        .bind(kind.as_str())
        .bind(url)
        .bind(anchor)
        .bind(message)
        .bind(payload)
        .bind(captchas)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn results_for_job(&self, job_id: Uuid) -> anyhow::Result<Vec<SubmissionResult>> {
        let rows = sqlx::query(
            "SELECT id, job_id, kind, url, anchor, message, payload, created_at
             FROM results WHERE job_id = $1 ORDER BY created_at",
        )
        .bind(job_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|r| SubmissionResult::from_row(&r)).collect::<Result<_, _>>()?)
    }

    /// All submission results for a project, newest first, joined to the
    /// engine name and keyword (anchor) each one belonged to. Optional
    /// `kind` filter + `limit` cap.
    pub async fn project_results(
        &self,
        project_id: Uuid,
        kind: Option<&str>,
        limit: i64,
    ) -> anyhow::Result<Vec<ProjectResult>> {
        let limit = limit.clamp(1, 500);
        let (rows, params) = match kind {
            Some(k) => {
                (
                    sqlx::query(
                        "SELECT r.id, r.job_id, r.kind, r.url, r.anchor, r.message,
                                e.engine_name, kw.text AS keyword, r.created_at
                         FROM results r
                         JOIN jobs j ON j.id = r.job_id
                         LEFT JOIN engines e ON e.id = j.engine_id
                         LEFT JOIN keywords kw ON kw.id = j.keyword_id
                         WHERE j.project_id = $1 AND r.kind = $2
                         ORDER BY r.created_at DESC LIMIT $3",
                    )
                    .bind(project_id)
                    .bind(k)
                    .bind(limit),
                    3,
                )
            }
            None => (
                sqlx::query(
                    "SELECT r.id, r.job_id, r.kind, r.url, r.anchor, r.message,
                            e.engine_name, kw.text AS keyword, r.created_at
                     FROM results r
                     JOIN jobs j ON j.id = r.job_id
                     LEFT JOIN engines e ON e.id = j.engine_id
                     LEFT JOIN keywords kw ON kw.id = j.keyword_id
                     WHERE j.project_id = $1
                     ORDER BY r.created_at DESC LIMIT $2",
                )
                .bind(project_id)
                .bind(limit),
                2,
            ),
        };
        let _ = params;
        let rows = rows.fetch_all(&self.pool).await?;
        Ok(rows.into_iter().map(|r| ProjectResult::from_row(&r)).collect::<Result<_, _>>()?)
    }

    /// Aggregate health for the project Results summary card.
    pub async fn result_stats(&self, project_id: Uuid) -> anyhow::Result<ResultStats> {
        let mut stats = ResultStats { total_jobs: /* filled below */ 0, ..Default::default() };
        let counts: (i64, i64, i64, i64, i64) = sqlx::query_as(
            "SELECT
                COALESCE(count(*) FILTER (WHERE r.kind = 'success'), 0),
                COALESCE(count(*) FILTER (WHERE r.kind = 'info'), 0),
                COALESCE(count(*) FILTER (WHERE r.kind = 'error'), 0),
                COALESCE(count(*) FILTER (WHERE r.kind = 'rejected'), 0),
                COALESCE(count(*), 0)
             FROM results r JOIN jobs j ON j.id = r.job_id
             WHERE j.project_id = $1",
        )
        .bind(project_id)
        .fetch_one(&self.pool)
        .await?;
        stats.success = counts.0;
        stats.info = counts.1;
        stats.error = counts.2;
        stats.rejected = counts.3;
        stats.total = counts.4;
        stats.total_jobs = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE project_id = $1")
            .bind(project_id)
            .fetch_one(&self.pool)
            .await?;
        stats.unique_success_urls = sqlx::query_scalar(
            "SELECT count(DISTINCT r.url) FROM results r 
             JOIN jobs j ON j.id = r.job_id WHERE j.project_id = $1 
             AND r.kind = 'success' AND r.url IS NOT NULL",
        )
        .bind(project_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(stats)
    }

    // ── history (dedup) ─────────────────────────────────────────────────────────────────────────────────────────────────
    /// Upsert a submitted URL into project history; returns true if it was a
    /// first sight (i.e. the caller should treat it as a fresh success).
    pub async fn record_url(
        &self,
        project_id: Uuid,
        url: &str,
        engine_name: Option<&str>,
    ) -> anyhow::Result<bool> {
        let row = sqlx::query(
            "INSERT INTO history (project_id, url, engine_name)
             VALUES ($1, $2, $3)
             ON CONFLICT (project_id, url) DO UPDATE SET
                last_seen = now(),
                engine_name = COALESCE(EXCLUDED.engine_name, history.engine_name)
             RETURNING first_seen = last_seen",
        )
        .bind(project_id)
        .bind(url)
        .bind(engine_name)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.get::<bool, _>(0))
    }

    pub async fn history_len(&self, project_id: Uuid) -> anyhow::Result<i64> {
        Ok(sqlx::query_scalar(
            "SELECT count(*) FROM history WHERE project_id = $1",
        )
        .bind(project_id)
        .fetch_one(&self.pool)
        .await?)
    }

    // ── F#12: operator-level email mailbox (GSA operator config) ────────
    pub async fn get_setting(&self, key: &str) -> anyhow::Result<Option<String>> {
        Ok(sqlx::query_scalar("SELECT value FROM settings WHERE key = $1")
            .bind(key)
            .fetch_optional(&self.pool)
            .await?
            .map(|v: String| v))
    }

    pub async fn set_setting(&self, key: &str, value: &str) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO settings (key, value) VALUES ($1, $2)
             ON CONFLICT (key) DO UPDATE SET value = $2, updated_at = now()",
        )
        .bind(key)
        .bind(value)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The email engine operator mailbox, as (address, password). Empty strings
    /// when not configured.
    pub async fn email_mailbox(&self) -> anyhow::Result<(String, String)> {
        let addr = self.get_setting("email.address").await?.unwrap_or_default();
        let pass = self.get_setting("email.password").await?.unwrap_or_default();
        Ok((addr, pass))
    }

    // ── F#13: proxies ─────────────────────────────────────────────────────
    const PROXY_COLS: &'static str =
        "id, host, port, proxy_kind, login, password, enabled,\n        country, ip, ip_public, geo_verified_at, created_at";

    /// Add a proxy to the list. `kind` must be "http" or "socks5"
    /// (normalized: case-insensitive, defaults to "http").  `country` is the
    /// optional F#14 target-country (GSA "Country" column).
    pub async fn add_proxy(
        &self,
        host: &str,
        port: i32,
        kind: &str,
        login: &str,
        password: &str,
        enabled: bool,
        country: Option<&str>,
    ) -> anyhow::Result<Proxy> {
        let kind = if kind.eq_ignore_ascii_case("socks5") || kind.eq_ignore_ascii_case("socks") {
            "socks5"
        } else {
            "http"
        };
        let row = sqlx::query(
            "INSERT INTO proxies (host, port, proxy_kind, login, password, enabled, country)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             RETURNING id, host, port, proxy_kind, login, password, enabled,
                       country, ip, ip_public, geo_verified_at, created_at",
        )
        .bind(host)
        .bind(port)
        .bind(kind)
        .bind(login)
        .bind(password)
        .bind(enabled)
        .bind(country)
        .fetch_one(&self.pool)
        .await?;
        Ok(FromRow::from_row(&row)?)
    }

    pub async fn list_proxies(&self) -> anyhow::Result<Vec<Proxy>> {
        let rows = sqlx::query(&format!("SELECT {} FROM proxies ORDER BY created_at, id", Self::PROXY_COLS))
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.into_iter().map(|r| Proxy::from_row(&r)).collect::<Result<_, _>>()?)
    }

    /// Only enabled proxies — the list the runner rotates over.
    pub async fn list_enabled_proxies(&self) -> anyhow::Result<Vec<Proxy>> {
        let rows = sqlx::query(&format!("SELECT {} FROM proxies WHERE enabled ORDER BY created_at, id", Self::PROXY_COLS))
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.into_iter().map(|r| Proxy::from_row(&r)).collect::<Result<_, _>>()?)
    }

    pub async fn proxy_by_id(&self, id: Uuid) -> anyhow::Result<Option<Proxy>> {
        let row = sqlx::query(&format!("SELECT {} FROM proxies WHERE id = $1", Self::PROXY_COLS))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| Proxy::from_row(&r)).transpose()?)
    }

    pub async fn set_proxy_enabled(&self, id: Uuid, enabled: bool) -> anyhow::Result<Proxy> {
        let row = sqlx::query(&format!("UPDATE proxies SET enabled = $1 WHERE id = $2 RETURNING {}", Self::PROXY_COLS))
            .bind(enabled)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| anyhow::anyhow!("proxy {id} not found"))?;
        Ok(FromRow::from_row(&row)?)
    }

    pub async fn delete_proxy(&self, id: Uuid) -> anyhow::Result<bool> {
        let n = sqlx::query("DELETE FROM proxies WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(n.rows_affected() > 0)
    }

    // ── F#14: geo / IP ─────────────────────────────────────────────────────
    /// Set or clear a proxy's target country (the GSA "Country" column).
    /// Passing `None` clears it to `NULL`.
    pub async fn set_proxy_country(&self, id: Uuid, country: Option<&str>) -> anyhow::Result<Proxy> {
        let row = sqlx::query(&format!(
            "UPDATE proxies SET country = $1 WHERE id = $2 RETURNING {}",
            Self::PROXY_COLS
        ))
        .bind(country)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| anyhow::anyhow!("proxy {id} not found"))?;
        Ok(FromRow::from_row(&row)?)
    }

    /// Persist a geo/IP verification result on a proxy (auto-populated by
    /// `POST /api/proxies/{id}/verify-geo`).
    pub async fn store_geo_verification(
        &self,
        id: Uuid,
        ip: Option<&str>,
        ip_public: bool,
    ) -> anyhow::Result<Proxy> {
        let row = sqlx::query(&format!(
            "UPDATE proxies SET ip = $1, ip_public = $2, geo_verified_at = now()
             WHERE id = $3 RETURNING {}",
            Self::PROXY_COLS
        ))
        .bind(ip)
        .bind(ip_public)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| anyhow::anyhow!("proxy {id} not found"))?;
        Ok(FromRow::from_row(&row)?)
    }

    /// Enabled proxies, optionally filtered to a **target country** (GSA
    /// country-targeting).  `Some(country)` → only proxies whose stored
    /// `country` equals `country` (case-insensitive); `None` → *all* enabled
    /// proxies (no country filter — the "any country" job).
    pub async fn list_enabled_proxies_by_country(
        &self,
        country: Option<&str>,
    ) -> anyhow::Result<Vec<Proxy>> {
        let rows = if let Some(c) = country {
            sqlx::query(&format!(
                "SELECT {} FROM proxies WHERE enabled AND lower(trim(country)) = lower(trim($1)) ORDER BY created_at, id",
                Self::PROXY_COLS
            ))
            .bind(c)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query(&format!("SELECT {} FROM proxies WHERE enabled ORDER BY created_at, id", Self::PROXY_COLS))
                .fetch_all(&self.pool)
                .await?
        };
        Ok(rows.into_iter().map(|r| Proxy::from_row(&r)).collect::<Result<_, _>>()?)
    }

    /// All proxies (regardless of enabled state) optionally filtered to a
    /// target country — the **UI** country filter.  `None` → all proxies;
    /// `Some(c)` → only proxies whose `country` equals `c` (case-insensitive).
    /// Distinct from [`Self::list_enabled_proxies_by_country`], which is the
    /// runner-facing *rotation* set (enabled only).
    pub async fn list_all_proxies_by_country(
        &self,
        country: Option<&str>,
    ) -> anyhow::Result<Vec<Proxy>> {
        let rows = if let Some(c) = country {
            sqlx::query(&format!(
                "SELECT {} FROM proxies WHERE lower(trim(country)) = lower(trim($1)) ORDER BY created_at, id",
                Self::PROXY_COLS
            ))
            .bind(c)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query(&format!("SELECT {} FROM proxies ORDER BY created_at, id", Self::PROXY_COLS))
                .fetch_all(&self.pool)
                .await?
        };
        Ok(rows.into_iter().map(|r| Proxy::from_row(&r)).collect::<Result<_, _>>()?)
    }

    // ── F#13: project proxy gate ──────────────────────────────────────────
    pub async fn set_project_use_proxies(&self, id: Uuid, use_proxies: bool) -> anyhow::Result<Project> {
        let row = sqlx::query(
            "UPDATE projects SET use_proxies = $1, updated_at = now() WHERE id = $2
             RETURNING id, name, description, created_at, updated_at, use_proxies",
        )
        .bind(use_proxies)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| anyhow::anyhow!("project {id} not found"))?;
        Ok(FromRow::from_row(&row)?)
    }

    pub async fn project_use_proxies(&self, id: Uuid) -> anyhow::Result<Option<bool>> {
        Ok(sqlx::query_scalar("SELECT use_proxies FROM projects WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?)
    }

    // ── F#15: global settings + engine-override settings ─────────────────
    /// Read the stored global settings blob. A missing row or empty `{}`
    /// yields the out-of-the-box GSA defaults; a stored blob is clamped on
    /// the way out.
    pub async fn global_settings(&self) -> anyhow::Result<engine::settings::Settings> {
        let raw = self.get_setting("global.settings").await?;
        match raw {
            Some(s) if !s.trim().is_empty() => Ok(
                serde_json::from_str::<engine::settings::Settings>(&s)
                    .map_err(|e| anyhow::anyhow!("stored settings blob is corrupt: {e}"))?
                    .clamped(),
            ),
            _ => Ok(engine::settings::Settings::gsa_default()),
        }
    }

    /// Persist the global settings blob (the JSON document GSA keeps in its
    /// `GSA_SER_Settings.zip` profile).
    pub async fn set_global_settings(&self, global: &engine::settings::Settings) -> anyhow::Result<()> {
        let json = serde_json::to_string(&global.clamped_ref())
            .map_err(|e| anyhow::anyhow!("global settings not serializable: {e}"))?;
        self.set_setting("global.settings", &json).await
    }

    /// All per-engine override rows, grouped into one `SettingsOverride` per
    /// engine (sparse `field → value` map, exactly as the GSA engine-settings
    /// panel edits them).
    pub async fn list_engine_overrides(&self) -> anyhow::Result<Vec<engine::settings::SettingsOverride>> {
        let rows = sqlx::query("SELECT engine_name, field, value FROM engine_settings")
            .fetch_all(&self.pool)
            .await?;
        let mut by_engine: std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>> =
            std::collections::BTreeMap::new();
        for row in rows {
            let name: String = row.try_get("engine_name")?;
            let field: String = row.try_get("field")?;
            let value: String = row.try_get("value")?;
            by_engine.entry(name).or_default().insert(field, value);
        }
        Ok(by_engine
            .into_iter()
            .map(|(name, fields)| engine::settings::SettingsOverride {
                engine_name: name,
                fields,
            })
            .collect())
    }

    /// Upsert one field of one engine's override. An empty/blank value
    /// DELETES that override (the UI "clear back to global" button).
    pub async fn set_engine_override(
        &self,
        engine_name: &str,
        field: &str,
        value: Option<&str>,
    ) -> anyhow::Result<()> {
        let field = field.trim();
        if field.is_empty() {
            anyhow::bail!("override field name is empty");
        }
        if let Some(v) = value.map(str::trim).filter(|v| !v.is_empty()) {
            sqlx::query(
                "INSERT INTO engine_settings (engine_name, field, value) VALUES ($1, $2, $3)
                 ON CONFLICT (engine_name, field) DO UPDATE SET value = $3, updated_at = now()",
            )
            .bind(engine_name)
            .bind(field)
            .bind(v)
            .execute(&self.pool)
            .await?;
        } else {
            sqlx::query("DELETE FROM engine_settings WHERE engine_name = $1 AND field = $2")
                .bind(engine_name)
                .bind(field)
                .execute(&self.pool)
                .await?;
        }
        Ok(())
    }

    /// Remove every stored override for one engine.
    pub async fn delete_engine_overrides(&self, engine_name: &str) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM engine_settings WHERE engine_name = $1")
            .bind(engine_name)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Resolve the effective settings for a single engine: the stored global
    /// values with that engine's sparse overrides applied on top (pure logic
    /// lives in `ser_engine::settings::effective`).
    pub async fn effective_settings_for_engine(&self, engine_name: &str) -> anyhow::Result<engine::settings::Settings> {
        let global = self.global_settings().await?;
        let overrides = self.list_engine_overrides().await?;
        Ok(engine::settings::effective(&global, Some(engine_name), &overrides))
    }

    /// Bundle export (the GSA_SER_Settings payload): global settings + every
    /// engine override + the proxy list snapshot.
    pub async fn export_settings_bundle(&self) -> anyhow::Result<engine::settings::SettingsBundle> {
        let global = self.global_settings().await?;
        let engine_overrides = self
            .list_engine_overrides()
            .await?
            .into_iter()
            .map(|o| (o.engine_name.clone(), o))
            .collect();
        let proxies_sql = sqlx::query(
            "SELECT host, port, proxy_kind, login, password, enabled, country
             FROM proxies ORDER BY host, port",
        )
        .fetch_all(&self.pool)
        .await?;
        let proxies = proxies_sql
            .iter()
            .map(|row| {
                let host: String = row.try_get("host")?;
                let port: i32 = row.try_get("port")?;
                let proxy_kind: String = row.try_get("proxy_kind")?;
                let login: String = row.try_get("login")?;
                let password: String = row.try_get("password")?;
                let enabled: bool = row.try_get("enabled")?;
                let country: Option<String> = row.try_get("country")?;
                Ok::<_, sqlx::Error>(engine::settings::ProxySnapshot {
                    host,
                    port: port as u16,
                    proxy_kind,
                    login,
                    password,
                    enabled,
                    country,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(engine::settings::SettingsBundle {
            global,
            engine_overrides,
            proxies,
            ..Default::default()
        })
    }

    /// Bundle import (reverse of `export_settings_bundle`): replaces the
    /// stored global settings, replaces engine overrides for the engines the
    /// bundle touched (wholesale), and upserts the bundle's proxies by
    /// (host, port). Returns the number of proxy rows upserted.
    /// Everything runs in one transaction so a mid-import crash leaves the
    /// previous state intact.
    pub async fn import_settings_bundle(&self, bundle: &engine::settings::SettingsBundle) -> anyhow::Result<usize> {
        let mut tx = self.pool.begin().await?;
        let global_json = serde_json::to_string(&bundle.global.clamped_ref())
            .map_err(|e| anyhow::anyhow!("global settings not serializable: {e}"))?;
        sqlx::query(
            "INSERT INTO settings (key, value) VALUES ('global.settings', $1)
             ON CONFLICT (key) DO UPDATE SET value = $1, updated_at = now()",
        )
        .bind(global_json)
        .execute(&mut *tx)
        .await?;

        let bundle_engines: std::collections::BTreeSet<String> =
            bundle.engine_overrides.keys().cloned().collect();
        for name in &bundle_engines {
            sqlx::query("DELETE FROM engine_settings WHERE engine_name = $1")
                .bind(name)
                .execute(&mut *tx)
                .await?;
        }
        for (name, o) in &bundle.engine_overrides {
            for (field, value) in &o.fields {
                let v = value.trim();
                if v.is_empty() {
                    continue;
                }
                sqlx::query(
                    "INSERT INTO engine_settings (engine_name, field, value) VALUES ($1, $2, $3)
                     ON CONFLICT (engine_name, field) DO UPDATE SET value = $3, updated_at = now()",
                )
                .bind(name)
                .bind(field)
                .bind(v)
                .execute(&mut *tx)
                .await?;
            }
        }
        let mut upserted = 0usize;
        for p in &bundle.proxies {
            sqlx::query(
                "INSERT INTO proxies (host, port, proxy_kind, login, password, enabled, country)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)
                 ON CONFLICT (host, port) DO UPDATE SET
                     proxy_kind = EXCLUDED.proxy_kind,
                     login = EXCLUDED.login,
                     password = EXCLUDED.password,
                     enabled = EXCLUDED.enabled,
                     country = EXCLUDED.country",
            )
            .bind(&p.host)
            .bind(p.port as i32)
            .bind(&p.proxy_kind)
            .bind(&p.login)
            .bind(&p.password)
            .bind(p.enabled)
            .bind(&p.country)
            .execute(&mut *tx)
            .await?;
            upserted += 1;
        }
        tx.commit().await?;
        Ok(upserted)
    }
}
