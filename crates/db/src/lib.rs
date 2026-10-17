//! Database layer for ser-replica: connection pool, migrations, and typed
//! repositories against PostgreSQL (running in the Windows Docker container
//! declared in `docker-compose.yml`).
//!
//! All SQL is checked against a live database by the `tests/` suite, which
//! is skipped (not failed) when `SEER_DATABASE_URL` is unreachable.

mod model;
mod registry;
mod repository;

pub use model::*;
pub use registry::{CatalogEngine, ImportSummary, Registry};
pub use repository::Repo;
pub use sqlx::PgPool;

/// Re-export so API consumers use the same error type the repositories return.
pub use anyhow;

use sqlx::postgres::PgPoolOptions;

/// Connect to the database and apply migrations.
///
/// DSN resolution order: explicit argument, then the `SEER_DATABASE_URL`
/// environment variable, then a `.env` file discovered from the current
/// working directory or the executable's location, and finally the local
/// compose container defaults (host port 5433).
pub async fn connect(dsn: Option<&str>) -> anyhow::Result<PgPool> {
    let dsn = dsn
        .map(|s| s.to_string())
        .or_else(|| std::env::var("SEER_DATABASE_URL").ok())
        .or_else(read_env_file_dsn)
        .unwrap_or_else(|| "postgres://ser:***@localhost:5433/ser".to_string());
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .acquire_timeout(std::time::Duration::from_secs(3))
        .connect(&dsn)
        .await?;
    migrate(&pool).await?;
    Ok(pool)
}

fn parse_env_dsn(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let line = line.trim();
        line.strip_prefix("SEER_DATABASE_URL=")
            .map(|v| v.trim().trim_matches('"').trim_matches('\'').to_string())
            .filter(|v| !v.is_empty())
    })
}

/// Look for a `.env` file with `SEER_DATABASE_URL`, walking upward from
/// both the current working directory and the executable's own directory.
/// This makes `ser-desktop.exe` work when double-clicked from anywhere:
/// the binary lives in `<repo>/target/debug`, `.env` at the repo root.
fn read_env_file_dsn() -> Option<String> {
    let mut roots: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            roots.push(dir.to_path_buf());
        }
    }
    let mut seen = std::collections::HashSet::new();
    for root in roots {
        let mut dir: Option<std::path::PathBuf> = Some(root);
        while let Some(d) = dir {
            if seen.insert(d.clone()) {
                if let Ok(text) = std::fs::read_to_string(d.join(".env")) {
                    if let Some(v) = parse_env_dsn(&text) {
                        return Some(v);
                    }
                }
            }
            dir = d.parent().map(|p| p.to_path_buf());
        }
    }
    None
}

/// Apply all SQL migrations in `migrations/` (tracked in `schema_migrations`).
pub async fn migrate(pool: &PgPool) -> anyhow::Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS schema_migrations (\
             version TEXT PRIMARY KEY, \
             applied_at TIMESTAMPTZ NOT NULL DEFAULT now())",
    )
    .execute(pool)
    .await?;

    for file in std::fs::read_dir(migrations_dir())? {
        let file = file?;
        let name = file.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".sql") {
            continue;
        }
        let applied: Option<String> = sqlx::query_scalar(
            "SELECT version FROM schema_migrations WHERE version = $1",
        )
        .bind(&name)
        .fetch_optional(pool)
        .await?;
        if applied.is_some() {
            continue;
        }
        let sql = std::fs::read_to_string(file.path())?;
        sqlx::raw_sql(&sql).execute(pool).await?;
        sqlx::query("INSERT INTO schema_migrations (version) VALUES ($1)")
            .bind(&name)
            .execute(pool)
            .await?;
    }
    Ok(())
}

fn migrations_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations")
}

/// Live liveness check for the dashboard's `/healthz`.
pub async fn ping(pool: &PgPool) -> bool {
    match pool.acquire().await {
        Ok(_) => true,
        Err(_) => false,
    }
}
