//! REST API for the ser-replica dashboard (axum over the SQLx pool).
//!
//! Routes:
//!   GET  /healthz                    liveness + db check
//!   GET  /api/projects               list projects
//!   POST /api/projects               create project {name, description?}
//!   GET  /api/projects/:id/engines   engines registered for a project
//!   GET  /api/projects/:id/keywords keywords of a project
//!   POST /api/projects/:id/keywords add keyword {text}
//!   POST /api/projects/:id/jobs      enqueue job {engine_id?, keyword_id?}
//!   GET  /api/projects/:id/jobs      jobs for a project (?status=)
//!   GET  /api/jobs/:id               one job
//!   GET  /api/jobs/:id/results       results of a job
//!   POST /api/jobs/claim             worker pickup (atomic)

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use ser_db::JobStatus;
use ser_db::PgPool;
use ser_db::Registry;
use ser_db::Repo;
use std::sync::Arc;
use uuid::Uuid;

/// Application state shared by all handlers (a connection pool).
pub struct AppState {
    pub pool: PgPool,
}

pub async fn app_state(dsn: Option<&str>) -> anyhow::Result<Arc<AppState>> {
    let pool = ser_db::connect(dsn).await?;
    Ok(Arc::new(AppState { pool }))
}

fn error_response(e: anyhow::Error) -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR,
     Json(serde_json::json!({ "error": e.to_string() }))).into_response()
}

pub async fn healthz(state: State<Arc<AppState>>) -> Response {
    let up = ser_db::ping(&state.pool).await;
    if up {
        (StatusCode::OK, Json(serde_json::json!({ "status": "ok", "db": "up" }))).into_response()
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "status": "degraded", "db": "down" })),
        ).into_response()
    }
}

async fn list_projects(state: State<Arc<AppState>>) -> Response {
    let list = Repo::new(&state.pool).list_projects().await;
    match list {
        Ok(list) => Json(list).into_response(),
        Err(e) => error_response(e),
    }
}

#[derive(Deserialize)]
pub struct CreateProject {
    name: String,
    #[serde(default)]
    description: Option<String>,
}

async fn create_project(state: State<Arc<AppState>>, body: Json<CreateProject>) -> Response {
    match Repo::new(&state.pool).create_project(&body.name, body.description.as_deref()).await {
        Ok(p) => (StatusCode::CREATED, Json(p)).into_response(),
        Err(e) => error_response(e),
    }
}

async fn list_engines(Path(project_id): Path<Uuid>, state: State<Arc<AppState>>) -> Response {
    let result = Repo::new(&state.pool).list_engines_all(project_id).await;
    match result {
        Ok(list) => Json(list).into_response(),
        Err(e) => error_response(e),
    }
}

#[derive(Deserialize)]
pub struct AddKeyword {
    text: String,
}

async fn create_keyword(
    Path(project_id): Path<Uuid>,
    state: State<Arc<AppState>>,
    body: Json<AddKeyword>,
) -> Response {
    let result = Repo::new(&state.pool).add_keyword(project_id, &body.text).await;
    match result {
        Ok(k) => (StatusCode::CREATED, Json(k)).into_response(),
        Err(e) => error_response(e),
    }
}

#[derive(Deserialize)]
pub struct AddKeywords {
    /// Raw lines — one entry or one spintax keyword per line.
    keywords: Vec<String>,
}

async fn create_keywords_bulk(
    Path(project_id): Path<Uuid>,
    state: State<Arc<AppState>>,
    body: Json<AddKeywords>,
) -> Response {
    let result = Repo::new(&state.pool).add_keywords(project_id, &body.keywords).await;
    match result {
        Ok((stored, _)) => {
            let list = Repo::new(&state.pool).list_keywords(project_id).await;
            match list {
                Ok(list) => (
                    StatusCode::CREATED,
                    Json(serde_json::json!({ "stored": stored, "keywords": list })),
                )
                    .into_response(),
                Err(e) => error_response(e),
            }
        }
        Err(e) => error_response(e),
    }
}

async fn list_keywords(Path(project_id): Path<Uuid>, state: State<Arc<AppState>>) -> Response {
    let result = Repo::new(&state.pool).list_keywords(project_id).await;
    match result {
        Ok(list) => Json(list).into_response(),
        Err(e) => error_response(e),
    }
}

async fn get_keyword(
    Path((project_id, id)): Path<(Uuid, Uuid)>,
    state: State<Arc<AppState>>,
) -> Response {
    let result = Repo::new(&state.pool).keyword_by_id(id).await;
    match result {
        Ok(Some(k)) if k.project_id == project_id => Json(k).into_response(),
        Ok(_) => (StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "not found" }))).into_response(),
        Err(e) => error_response(e),
    }
}

async fn delete_keyword(
    Path((project_id, id)): Path<(Uuid, Uuid)>,
    state: State<Arc<AppState>>,
) -> Response {
    let repo = Repo::new(&state.pool);
    match repo.keyword_by_id(id).await {
        Ok(Some(k)) if k.project_id == project_id => {}
        Ok(_) => {
            return (StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "not found" }))).into_response()
        }
        Err(e) => return error_response(e),
    }
    match repo.delete_keyword(id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "not found" }))).into_response(),
        Err(e) => error_response(e),
    }
}

#[derive(Deserialize)]
pub struct CreateJob {
    #[serde(default)]
    engine_id: Option<Uuid>,
    #[serde(default)]
    keyword_id: Option<Uuid>,
}

async fn create_job(Path(project_id): Path<Uuid>, state: State<Arc<AppState>>, body: Json<CreateJob>) -> Response {
    let result = Repo::new(&state.pool).create_job(project_id, body.engine_id, body.keyword_id).await;
    match result {
        Ok(j) => (StatusCode::CREATED, Json(j)).into_response(),
        Err(e) => error_response(e),
    }
}

#[derive(Deserialize)]
struct JobFilter {
    status: Option<String>,
}

async fn list_jobs(
    Path(project_id): Path<Uuid>,
    Query(q): Query<JobFilter>,
    state: State<Arc<AppState>>,
) -> Response {
    let status = q.status.and_then(|s| JobStatus::parse(&s));
    let result = Repo::new(&state.pool).list_jobs(project_id, status).await;
    match result {
        Ok(list) => Json(list).into_response(),
        Err(e) => error_response(e),
    }
}

async fn get_job(Path(id): Path<Uuid>, state: State<Arc<AppState>>) -> Response {
    let result = Repo::new(&state.pool).job_by_id(id).await;
    match result {
        Ok(Some(j)) => Json(j).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, Json(serde_json::json!({"error":"not found"}))).into_response(),
        Err(e) => error_response(e),
    }
}

async fn job_results(Path(id): Path<Uuid>, state: State<Arc<AppState>>) -> Response {
    let result = Repo::new(&state.pool).results_for_job(id).await;
    match result {
        Ok(list) => Json(list).into_response(),
        Err(e) => error_response(e),
    }
}

// ── F#10 Results engine ───────────────────────────────────────────────────
#[derive(Deserialize)]
struct ResultQ {
    kind: Option<String>,
    #[serde(default)]
    limit: Option<i64>,
}

/// All results for a project, joined to engine + keyword, newest first.
async fn project_results(
    Path(project_id): Path<Uuid>,
    Query(q): Query<ResultQ>,
    state: State<Arc<AppState>>,
) -> Response {
    let limit = q.limit.unwrap_or(200);
    let result = Repo::new(&state.pool).project_results(project_id, q.kind.as_deref(), limit).await;
    match result {
        Ok(list) => Json(list).into_response(),
        Err(e) => error_response(e),
    }
}

/// Summary card for a project: per-kind counts, jobs run, unique successes.
async fn project_stats(
    Path(project_id): Path<Uuid>,
    state: State<Arc<AppState>>,
) -> Response {
    let result = Repo::new(&state.pool).result_stats(project_id).await;
    match result {
        Ok(list) => Json(list).into_response(),
        Err(e) => error_response(e),
    }
}

// ── F#11 Projects UI polish: full CRUD + job control ─────────────────────
#[derive(Deserialize)]
pub struct UpdateProject {
    name: String,
    #[serde(default)]
    description: Option<String>,
}

/// PUT /api/projects/{id} — rename / re-describe a project.
async fn update_project(
    Path(id): Path<Uuid>,
    state: State<Arc<AppState>>,
    body: Json<UpdateProject>,
) -> Response {
    let result = Repo::new(&state.pool)
        .update_project(id, &body.name, body.description.as_deref())
        .await;
    match result {
        Ok(project) => Json(project).into_response(),
        Err(e) => error_response(e),
    }
}

/// GET /api/projects/{id} — read one project (404 if unknown).
async fn get_project(
    Path(id): Path<Uuid>,
    state: State<Arc<AppState>>,
) -> Response {
    match Repo::new(&state.pool).project_by_id(id).await {
        Ok(Some(p)) => Json(p).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => error_response(e),
    }
}

/// DELETE /api/projects/{id} — remove a project (cascades engines/keywords/jobs).
async fn delete_project(
    Path(id): Path<Uuid>,
    state: State<Arc<AppState>>,
) -> Response {
    match Repo::new(&state.pool).delete_project(id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => error_response(e),
    }
}

/// Prefer the incoming value when it is a non-empty string, else fall back.
fn pick_str(p: Option<&str>, fallback: &str) -> String {
    match p {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => fallback.to_string(),
    }
}

#[derive(Deserialize)]
pub struct UpdateEngine {
    engine_name: String,
    #[serde(default)]
    engine_type: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    step_count: Option<i32>,
}

/// PATCH /api/projects/{id}/engines/{kid} — update mutable fields on an engine.
async fn update_engine(
    Path((project_id, id)): Path<(Uuid, Uuid)>,
    state: State<Arc<AppState>>,
    body: Json<UpdateEngine>,
) -> Response {
    let repo = Repo::new(&state.pool);
    let Some(existing) = repo.engine_by_id(id).await.ok().flatten() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if existing.project_id != project_id {
        return StatusCode::NOT_FOUND.into_response();
    }
    let name = pick_str(if body.engine_name.trim().is_empty() { None } else { Some(body.engine_name.as_str()) }, &existing.engine_name);
    let et = pick_str(body.engine_type.as_deref(), &existing.engine_type);
    let url = pick_str(body.url.as_deref(), &existing.url);
    let steps = body.step_count.unwrap_or(existing.step_count);
    match repo.update_engine(id, &name, &et, &url, steps).await {
        Ok(row) => Json(row).into_response(),
        Err(e) => error_response(e),
    }
}

/// PUT /api/projects/{id}/engines/{kid}/enabled — toggle enabled.
async fn set_engine_enabled(
    Path((project_id, id)): Path<(Uuid, Uuid)>,
    state: State<Arc<AppState>>,
    body: Json<serde_json::Value>,
) -> Response {
    let enabled = body.0.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
    let repo = Repo::new(&state.pool);
    let Some(existing) = repo.engine_by_id(id).await.ok().flatten() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if existing.project_id != project_id {
        return StatusCode::NOT_FOUND.into_response();
    }
    match repo.set_engine_enabled(id, enabled).await {
        Ok(row) => Json(row).into_response(),
        Err(e) => error_response(e),
    }
}

/// DELETE /api/projects/{id}/engines/{kid}.
async fn delete_engine(
    Path((project_id, id)): Path<(Uuid, Uuid)>,
    state: State<Arc<AppState>>,
) -> Response {
    let repo = Repo::new(&state.pool);
    let Some(existing) = repo.engine_by_id(id).await.ok().flatten() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if existing.project_id != project_id {
        return StatusCode::NOT_FOUND.into_response();
    }
    match repo.delete_engine(id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => error_response(e),
    }
}

/// POST /api/jobs/{id}/cancel — cancel a pending or running job.
async fn cancel_job(Path(id): Path<Uuid>, state: State<Arc<AppState>>) -> Response {
    match Repo::new(&state.pool).cancel_job(id).await {
        Ok(Some(j)) => Json(j).into_response(),
        Ok(None) => StatusCode::CONFLICT.into_response(),  // already terminal
        Err(e) => error_response(e),
    }
}


#[derive(Deserialize)]
struct ClaimQuery {
    project_id: Uuid,
}

async fn claim_job(
    Query(q): Query<ClaimQuery>,
    state: State<Arc<AppState>>,
) -> Response {
    let result = Repo::new(&state.pool).claim_next_pending(q.project_id).await;
    match result {
        Ok(Some(j)) => Json(j).into_response(),
        Ok(None) => Json(serde_json::json!({ "claimed": null })).into_response(),
        Err(e) => error_response(e),
    }
}

#[derive(Deserialize)]
pub struct AddEngine {
    engine_name: String,
    #[serde(default)]
    engine_type: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    file_path: String,
    #[serde(default)]
    step_count: i32,
}

async fn upsert_engine(
    Path(project_id): Path<Uuid>,
    state: State<Arc<AppState>>,
    body: Json<AddEngine>,
) -> Response {
    let result = Repo::new(&state.pool)
        .upsert_engine(
            project_id,
            &body.engine_name,
            body.engine_type.as_deref().unwrap_or("directory"),
            body.url.as_deref().unwrap_or(""),
            &body.file_path,
            body.step_count,
        )
        .await;
    match result {
        Ok(e) => Json(e).into_response(),
        Err(e) => error_response(e),
    }
}

#[derive(Deserialize)]
pub struct FinishJob {
    status: String,
    #[serde(default)]
    last_error: Option<String>,
}

async fn finish_job(
    Path(id): Path<Uuid>,
    state: State<Arc<AppState>>,
    body: Json<FinishJob>,
) -> Response {
    let status = JobStatus::parse(&body.status).unwrap_or(JobStatus::Done);
    let result = Repo::new(&state.pool)
        .finish_job(id, status, body.last_error.as_deref())
        .await;
    match result {
        Ok(()) => Json(serde_json::json!({ "status": "ok", "id": id })).into_response(),
        Err(e) => error_response(e),
    }
}

/// Query params for the catalog listing endpoint.
#[derive(Debug, Deserialize)]
pub struct CatalogQuery {
    #[serde(default)]
    pub kind: Option<String>,   // filter by engine type substring
    #[serde(default)]
    pub q: Option<String>,      // free-text search (name/url)
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub offset: Option<i64>,
}

/// GET /api/catengine — browse the imported GSA engine catalog.
async fn catalog_list(
    State(state): State<Arc<AppState>>,
    Query(qq): Query<CatalogQuery>,
) -> Response {
    let limit = qq.limit.unwrap_or(100);
    let offset = qq.offset.unwrap_or(0);
    let result: anyhow::Result<Vec<serde_json::Value>> = async move {
        let regs = Registry::new(&state.pool);
        let engines = regs
            .list(qq.kind.as_deref(), qq.q.as_deref(), limit, offset)
            .await?;
        Ok(engines
            .into_iter()
            .map(|e| serde_json::json!({
                "id": e.id,
                "kind": e.engine_type,
                "name": e.engine_name,
                "url": e.url,
                "step_count": e.step_count,
                "category": e.category,
                "file_path": e.file_path,
            }))
            .collect())
    }.await;
    match result {
        Ok(v) => Json(v).into_response(),
        Err(e) => error_response(e),
    }
}

/// GET /api/settings/email — the operator email mailbox (GSA operator config).
/// Returned shape: {"address": "...", "password": "..."}.
async fn get_email_setting(State(state): State<Arc<AppState>>) -> Response {
    let repo = Repo::new(&state.pool);
    let (a, p) = match repo.email_mailbox().await {
        Ok(v) => v,
        Err(e) => return error_response(e),
    };
    Json(serde_json::json!({ "address": a, "password": p })).into_response()
}

/// PUT /api/settings/email — set the operator email mailbox.
/// Body: {"address": "...", "password": "..."}.
#[derive(Deserialize)]
pub struct SetEmailSetting {
    address: String,
    password: String,
}
async fn put_email_setting(
    State(state): State<Arc<AppState>>,
    body: Json<SetEmailSetting>,
) -> Response {
    let repo = Repo::new(&state.pool);
    if let Err(e) = repo.set_setting("email.address", &body.address).await {
        return error_response(e);
    }
    if let Err(e) = repo.set_setting("email.password", &body.password).await {
        return error_response(e);
    }
    Json(serde_json::json!({ "status": "ok" })).into_response()
}

/// POST /api/catengine/import — import the live GSA engine directories.
async fn catalog_import(
    State(state): State<Arc<AppState>>,
) -> Response {
    let gsa_root = std::env::var("GSA_INSTALL_ROOT").unwrap_or_else(|_| {
        r"C:\Users\kingsly\Downloads\GSA_SER_v18.20_FINAL\GSA_SER_v18.20_FINAL".into()
    });
    let result = async {
        let regs = Registry::new(&state.pool);
        regs.import_gsa_root(&gsa_root).await
    }
    .await;
    match result {
        Ok(sums) => {
            let total: u32 = sums.iter().map(|s| s.imported).sum();
            let failed: u32 = sums.iter().map(|s| s.failed).sum();
            Json(serde_json::json!({
                "status": "ok",
                "imported": total,
                "failed": failed,
                "categories": sums,
                "source": gsa_root,
            }))
            .into_response()
        }
        Err(e) => error_response(e),
    }
}


/// GET /api/catengine/stats — counts + type breakdown for the dashboard.
async fn catalog_stats(State(state): State<Arc<AppState>>) -> Response {
    let result = async {
        let regs = Registry::new(&state.pool);
        let total = regs.count().await?;
        let breakdown = regs
            .type_breakdown()
            .await?
            .into_iter()
            .map(|(t, n)| serde_json::json!({ "engine_type": t, "n": n }))
            .collect::<Vec<_>>();
        let web = regs.count_by_category("web").await?;
        let email = regs.count_by_category("email").await?;
        Ok(serde_json::json!({ "total": total, "web": web, "email": email, "types": breakdown }))
    }
    .await;
    match result {
        Ok(v) => Json(v).into_response(),
        Err(e) => error_response(e),
    }
}

// ── F#12 glue: in-process job execution ────────────────────────────────────
/// POST /api/projects/{id}/run — execute this project's queued `pending`
/// jobs now, in-process, and report how many completed.
///
/// This is the missing F#12 integration seam: the `ser-runner` worker pool
/// was a library with no caller, so jobs created from the dashboard sat
/// `pending` forever. Here we instantiate the pool against the live DB +
/// on-disk GSA specs and run it to completion within the request.
///
/// Driver wiring (deliberate scope decision):
///   * `HttpDriver` for plain-form **web** engines (the 825).
///   * `HttpDriver` for the **F#12 WEB-inbox** email engines
///     (`pop3 server=WEB`, 142) — both their inbox fetch and the
///     verification hop are HTTP.
///   * The 75 real-POP3 mailbox engines run their own socket transport
///     (no driver needed for the fetch; the driver covers the visit).
///   * Browser-only engines report a clean "driver not registered" failure
///     rather than hanging (F#3/F#7 browser pool is a later stage).
///
/// The operator mailbox is read from `/api/settings/email` and injected as
/// the mail-context fallback, so email engines run end-to-end without any
/// in-band credentials. The pool's `leader_idle_timeout` bounds how long we
/// wait after the last claim for in-flight work to settle, so the response
/// returns once the batch has settled (not just when the queue drained).
async fn run_project(
    Path(project_id): Path<Uuid>,
    State(state): State<Arc<AppState>>,
) -> Response {
    // GSA install root — same default the import path uses; override with
    // GSA_INSTALL_ROOT.
    let root = std::env::var("GSA_INSTALL_ROOT")
        .unwrap_or_else(|_| {
            r"C:\Users\kingsly\Downloads\GSA_SER_v18.20_FINAL\GSA_SER_v18.20_FINAL".into()
        });
    let roots = vec![std::path::PathBuf::from(root)];

    // Operator mailbox — authoritative for F#12 mail runs (the runner falls
    // back to this when the pool context is empty).
    let (email_address, email_password) = Repo::new(&state.pool)
        .email_mailbox()
        .await
        .unwrap_or_default();

    let mut cfg = runner::RunnerConfig::default();
    cfg.concurrent_workers = 4; // GSA runs 25-50; 4 is a sane local default.
    cfg.per_job_timeout = std::time::Duration::from_secs(90);
    cfg.leader_idle_timeout = std::time::Duration::from_millis(800);
    cfg.max_retries = 0;

    // F#13 — if the project has `use_proxies`, route its jobs through the
    // enabled proxy list.  Otherwise the context stays empty and the runner
    // keeps using a direct connection.
    let use_proxies = Repo::new(&state.pool)
        .project_use_proxies(project_id)
        .await
        .ok()
        .flatten()
        .unwrap_or(false);
    let proxies: Vec<engine::proxy::ProxySpec> = if use_proxies {
        Repo::new(&state.pool)
            .list_enabled_proxies()
            .await
            .map(|list| {
                list.into_iter()
                    .map(|p| engine::proxy::ProxySpec {
                        host: p.host,
                        port: p.port as u16,
                        proxy_kind: p.proxy_kind,
                        login: p.login,
                        password: p.password,
                    })
                    .collect()
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    let ctx = runner::SubmissionContext {
        email_address,
        email_password,
        proxies,
        ..Default::default()
    };

    let drivers = runner::DriverRegistry::new()
        .with_http(engine::HttpDriver::new())
        .with_email(engine::HttpDriver::new());

    let pool = runner::pool_for_disk(state.pool.clone(), cfg, roots, drivers).with_context(ctx);

    let summary = pool.run(project_id).await;

    Json(serde_json::json!({
        "status": "ok",
        "claimed": summary.claimed,
        "done": summary.done,
        "failed": summary.failed,
        "panics": summary.panics,
    }))
    .into_response()
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/api/projects", get(list_projects).post(create_project))
        .route("/api/projects/{id}", get(get_project).put(update_project).delete(delete_project))
        .route("/api/projects/{id}/engines", get(list_engines).post(upsert_engine))
        .route(
            "/api/projects/{id}/engines/{kid}",
            put(update_engine).delete(delete_engine),
        )
        .route("/api/projects/{id}/engines/{kid}/enabled", put(set_engine_enabled))
        .route("/api/projects/{id}/keywords", get(list_keywords).post(create_keyword))
        .route("/api/projects/{id}/keywords/bulk", post(create_keywords_bulk))
        .route(
            "/api/projects/{id}/keywords/{kid}",
            get(get_keyword).delete(delete_keyword),
        )
        .route("/api/projects/{id}/jobs", get(list_jobs).post(create_job))
        .route("/api/jobs/{id}/results", get(job_results))
        .route("/api/projects/{id}/results", get(project_results))
        .route("/api/projects/{id}/results/stats", get(project_stats))
        .route("/api/jobs/claim", post(claim_job))
        .route("/api/jobs/{id}/finish", post(finish_job))
        .route("/api/jobs/{id}/cancel", post(cancel_job))
        .route("/api/jobs/{id}", get(get_job))
        .route("/api/projects/{id}/run", post(run_project))
        .route("/api/proxies", get(list_proxies).post(create_proxy))
        .route("/api/proxies/{id}", delete(delete_proxy))
        .route("/api/proxies/{id}/enabled", put(set_proxy_enabled))
        .route("/api/proxies/{id}/test", post(proxy_test))
        .route("/api/projects/{id}/proxies", put(set_project_proxies))
        .route("/api/settings/email", get(get_email_setting).put(put_email_setting))
        .route("/api/catengine/stats", get(catalog_stats))
        .route("/api/catengine", get(catalog_list))
        .route("/api/catengine/import", post(catalog_import))
        .with_state(state)
}

// ─── F#13: proxy management ────────────────────────────────────────────────────

async fn list_proxies(state: State<Arc<AppState>>) -> Response {
    match Repo::new(&state.pool).list_proxies().await {
        Ok(p) => (StatusCode::OK, Json(serde_json::to_value(&p).unwrap())).into_response(),
        Err(e) => error_response(e),
    }
}

#[derive(Deserialize)]
struct AddProxyReq {
    host: String,
    #[serde(default = "default_port")]
    port: u16,
    #[serde(default = "default_kind")]
    kind: String,
    #[serde(default)]
    login: Option<String>,
    #[serde(default)]
    password: Option<String>,
}

fn default_port() -> u16 { 8080 }
fn default_kind() -> String { "http".into() }

async fn create_proxy(state: State<Arc<AppState>>, body: Json<AddProxyReq>) -> Response {
    if body.kind != "http" && body.kind != "socks5" {
        return (StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "kind must be 'http' or 'socks5'" }))).into_response();
    }
    match Repo::new(&state.pool)
        .add_proxy(
            &body.host,
            body.port as i32,
            &body.kind,
            body.login.as_deref().unwrap_or_default(),
            body.password.as_deref().unwrap_or_default(),
                        true,
                    )
        .await
    {
        Ok(p) => (StatusCode::CREATED, Json(serde_json::to_value(&p).unwrap())).into_response(),
        Err(e) => error_response(e),
    }
}

async fn delete_proxy(state: State<Arc<AppState>>, Path(id): Path<Uuid>) -> Response {
    match Repo::new(&state.pool).delete_proxy(id).await {
        Ok(true) => (StatusCode::NO_CONTENT, "").into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "not found" }))).into_response(),
        Err(e) => error_response(e),
    }
}

async fn set_proxy_enabled(
    state: State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    body: Json<serde_json::Value>,
) -> Response {
    let enabled = body
        .as_object()
        .and_then(|o| o.get("enabled"))
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    match Repo::new(&state.pool).set_proxy_enabled(id, enabled).await {
        Ok(p) => (StatusCode::OK, Json(serde_json::to_value(&p).unwrap())).into_response(),
        Err(e) => error_response(e),
    }
}

/// F#13 `POST /api/proxies/{id}/test` — runs the real-socket connectivity
/// probe (GSA `proxy test`).  Returns `{ok, detail, latency_ms}`.
async fn proxy_test(state: State<Arc<AppState>>, Path(id): Path<Uuid>) -> Response {
    let repo = Repo::new(&state.pool);
    let proxy = match repo.proxy_by_id(id).await {
        Ok(Some(p)) => p,
        Ok(None) => {
            return (StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "not found" }))).into_response()
        }
        Err(e) => return error_response(e),
    };
    let spec = engine::proxy::ProxySpec {
        host: proxy.host,
        port: proxy.port as u16,
        proxy_kind: proxy.proxy_kind,
        login: proxy.login,
        password: proxy.password,
    };
    let result = engine::proxy_test::test_proxy(&spec).await;
    (StatusCode::OK, Json(serde_json::json!({
        "proxy_id": id,
        "ok": result.ok,
        "detail": result.detail,
        "latency_ms": result.latency_ms,
    }))).into_response()
}

async fn set_project_proxies(
    state: State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    body: Json<serde_json::Value>,
) -> Response {
    let use_proxies = body.as_object().and_then(|o| o.get("use_proxies")).and_then(|v| v.as_bool())
        .unwrap_or(false);
    match Repo::new(&state.pool).set_project_use_proxies(id, use_proxies).await {
        Ok(p) => (StatusCode::OK, Json(serde_json::to_value(&p).unwrap())).into_response(),
        Err(e) => error_response(e),
    }
}
