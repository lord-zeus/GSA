//! # Feature #3 — engine catalog (registry import)
//!
//! Imports the REAL GSA engine corpus (825 web + 217 email .ini files)
//! into Postgres so the dashboard/executor can pick from every engine GSA
//! ships, without any per-project re-registration.  Import is idempotent
//! (upsert on relative file path) and batched in a single transaction.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{FromRow, Row};

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct CatalogEngine {
    pub id: i64,
    pub file_path: String,
    pub engine_name: String,
    pub engine_type: String,
    pub url: String,
    pub step_count: i32,
    pub dofollow: bool,
    pub register: bool,
    pub category: String,
    pub imported_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImportFailure {
    pub file: String,
    pub error: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImportSummary {
    pub directory: String,
    pub category: String,
    pub scanned: u32,
    pub imported: u32,
    pub failed: u32,
    pub failures: Vec<ImportFailure>,
}

/// Registry of the global engine catalog.
pub struct Registry<'a> {
    pool: &'a sqlx::PgPool,
}

impl<'a> Registry<'a> {
    pub fn new(pool: &'a sqlx::PgPool) -> Self {
        Self { pool }
    }

    /// Borrow the underlying pool (read-only).
    pub fn pool(&self) -> &'a sqlx::PgPool {
        self.pool
    }

    /// Row count for one category (web / email).
    pub async fn count_by_category(&self, category: &str) -> anyhow::Result<i64> {
        let v: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM engine_catalog WHERE category = $1",
        )
        .bind(category)
        .fetch_one(self.pool)
        .await?;
        Ok(v)
    }

    /// Import one engine directory (all .ini files), upserting each parsed
    /// engine into engine_catalog.  One round-trip transaction total —
    /// the whole dir commits atomically or not at all.
    pub async fn import_dir(
        &self,
        dir: &str,
        category: &str,
    ) -> anyhow::Result<ImportSummary> {
        let path = std::path::Path::new(dir);
        let files: Vec<std::path::PathBuf> = match std::fs::read_dir(path) {
            Ok(rd) => rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.extension()
                        .map(|x| x.to_string_lossy().eq_ignore_ascii_case("ini"))
                        .unwrap_or(false)
                })
                .collect(),
            Err(e) => anyhow::bail!("cannot read engine dir {dir}: {e}"),
        };

        let mut parsed: Vec<(String, engine::Engine)> = Vec::with_capacity(files.len());
        let mut failures: Vec<ImportFailure> = Vec::new();
        let mut scanned = 0u32;
        for f in files {
            scanned += 1;
            let parent = f.parent().and_then(|d| d.file_name())
                .map(|d| d.to_string_lossy().into_owned())
                .unwrap_or_else(|| category.to_string());
            let fname = f.file_name().map(|x| x.to_string_lossy().into_owned()).unwrap_or_default();
            let rel = format!("{parent}/{fname}");
            match engine::parse_engine_file(&f) {
                Ok(spec) => parsed.push((rel, spec)),
                Err(e) => failures.push(ImportFailure {
                    file: f.file_name().map(|x| x.to_string_lossy().into_owned()).unwrap_or_default(),
                    error: e.to_string(),
                }),
            }
        }

        let mut tx = self.pool.begin().await?;
        for (rel, spec) in &parsed {
            sqlx::query(
                "INSERT INTO engine_catalog
                     (file_path, engine_name, engine_type, url, step_count, dofollow, register, category)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                 ON CONFLICT (file_path) DO UPDATE SET
                     engine_name  = EXCLUDED.engine_name,
                     engine_type = EXCLUDED.engine_type,
                     url         = EXCLUDED.url,
                     step_count  = EXCLUDED.step_count,
                     dofollow    = EXCLUDED.dofollow,
                     register    = EXCLUDED.register,
                     category    = EXCLUDED.category,
                     imported_at = now()",
            )
            .bind(rel)
            .bind(&spec.name)
            .bind(spec.engine_type.to_string())
            .bind(spec.url.trim())
            .bind(spec.steps.len() as i32)
            .bind(spec.dofollow)
            .bind(spec.register)
            .bind(category)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;

        Ok(ImportSummary {
            directory: dir.to_string(),
            category: category.to_string(),
            scanned,
            imported: parsed.len() as u32,
            failed: failures.len() as u32,
            failures: failures.into_iter().take(10).collect(),
        })
    }

    /// Import both Engines/ (web) and Email_Engines/ (email) under a GSA root.
    pub async fn import_gsa_root(
        &self,
        root: &str,
    ) -> anyhow::Result<Vec<ImportSummary>> {
        let mut out = Vec::new();
        for (sub, cat) in [("Engines", "web"), ("Email_Engines", "email")] {
            let d = std::path::Path::new(root).join(sub);
            if d.is_dir() {
                out.push(self.import_dir(d.to_str().unwrap(), cat).await?);
            }
        }
        Ok(out)
    }

    pub async fn count(&self) -> anyhow::Result<i64> {
        Ok(sqlx::query_scalar("SELECT count(*) FROM engine_catalog")
            .fetch_one(self.pool)
            .await?)
    }

    pub async fn get(&self, id: i64) -> anyhow::Result<Option<CatalogEngine>> {
        let row = sqlx::query(
            "SELECT id, file_path, engine_name, engine_type, url, step_count,
                    dofollow, register, category, imported_at
             FROM engine_catalog WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(self.pool)
        .await?;
        match row {
            Some(r) => Ok(Some(CatalogEngine::from_row(&r)?)),
            None => Ok(None),
        }
    }

    pub async fn list(
        &self,
        engine_type: Option<&str>,
        search: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> anyhow::Result<Vec<CatalogEngine>> {
        let rows = sqlx::query(
            "SELECT id, file_path, engine_name, engine_type, url, step_count,
                    dofollow, register, category, imported_at
             FROM engine_catalog
             WHERE ($1::TEXT IS NULL OR engine_type ILIKE '%' || $1 || '%')
               AND ($2::TEXT IS NULL OR engine_name ILIKE '%' || $2 || '%' OR url ILIKE '%' || $2 || '%')
             ORDER BY category, engine_name
             LIMIT $3 OFFSET $4",
        )
        .bind(engine_type)
        .bind(search)
        .bind(limit.min(10000).max(1))
        .bind(offset)
        .fetch_all(self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| CatalogEngine::from_row(&r))
            .collect::<Result<_, _>>()?)
    }

    /// Distinct type counts for the dashboard overview.
    pub async fn type_breakdown(&self) -> anyhow::Result<Vec<(String, i64)>> {
        let rows = sqlx::query(
            "SELECT engine_type, count(*) FROM engine_catalog
             GROUP BY engine_type ORDER BY count DESC, engine_type",
        )
        .fetch_all(self.pool)
        .await?;
        let mut out: Vec<(String, i64)> = Vec::new();
        for r in rows.iter() {
            let t: String = r.try_get(0)?;
            let n: i64 = r.try_get(1)?;
            out.push((t, n));
        }
        Ok(out)
    }
}
