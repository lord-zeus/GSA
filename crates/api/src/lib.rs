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

/// F#17 — background scheduler loop (ticks + fires due schedules).
pub mod scheduler;
/// F#18 — in-process real-time alert bus (SSE fan-out).
pub mod livebus;
/// F#18 — the single emission funnel (persist → broadcast → email).
pub mod notifier;
/// T6 — live run feed (job_events rows + SSE run bus + runner sink).
pub mod runfeed;
/// T10 — background multi-project run tracking (active runs + last summary).
pub mod runtrack;

use axum::routing::{delete, get, patch, post, put};

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

    /// F#18 — real-time alert bus (shared by every SSE client).
    pub bus: crate::livebus::LiveBus,

    /// T6 — live run-event bus (the project console streams from here).
    pub run_bus: crate::runfeed::RunBus,
    /// T10 — background run registry (one active per project + last summary).
    pub runs: crate::runtrack::RunTracker,
}



pub async fn app_state(dsn: Option<&str>) -> anyhow::Result<Arc<AppState>> {

    let pool = ser_db::connect(dsn).await?;

    Ok(Arc::new(AppState {
        pool,
        bus: crate::livebus::LiveBus::new(),
        run_bus: crate::runfeed::RunBus::new(),
        runs: {
            let cap: usize = std::env::var("SEER_MAX_ACTIVE_RUNS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(8);
            crate::runtrack::RunTracker::new(cap)
        },
    }))

}



fn error_response(e: anyhow::Error) -> Response {

    (StatusCode::INTERNAL_SERVER_ERROR,

     Json(serde_json::json!({ "error": e.to_string() }))).into_response()

}



/// True if `e` is the Postgres unique-constraint violation that

/// `POST /api/proxies` raises on a re-entered `host:port`.  Lets the handler

/// map a collision to 409 "already exists" instead of leaking a 500 with the

/// raw constraint string.  Matches on both the human-readable message and the

/// specific constraint name — neither changes with sqlx version.

fn is_unique_violation(e: &anyhow::Error) -> bool {

    let msg = e.to_string();

    msg.contains("duplicate key value violates unique constraint")

        || msg.contains("uq_proxies_host_port")

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

    let list = Repo::new(state.pool.clone()).list_projects().await;

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

    match Repo::new(state.pool.clone()).create_project(&body.name, body.description.as_deref()).await {

        Ok(p) => (StatusCode::CREATED, Json(p)).into_response(),

        Err(e) => error_response(e),

    }

}



async fn list_engines(Path(project_id): Path<Uuid>, state: State<Arc<AppState>>) -> Response {

    let result = Repo::new(state.pool.clone()).list_engines_all(project_id).await;

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

    let result = Repo::new(state.pool.clone()).add_keyword(project_id, &body.text).await;

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

    let result = Repo::new(state.pool.clone()).add_keywords(project_id, &body.keywords).await;

    match result {

        Ok((stored, _)) => {

            let list = Repo::new(state.pool.clone()).list_keywords(project_id).await;

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

    let result = Repo::new(state.pool.clone()).list_keywords(project_id).await;

    match result {

        Ok(list) => Json(list).into_response(),

        Err(e) => error_response(e),

    }

}



async fn get_keyword(

    Path((project_id, id)): Path<(Uuid, Uuid)>,

    state: State<Arc<AppState>>,

) -> Response {

    let result = Repo::new(state.pool.clone()).keyword_by_id(id).await;

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

    let repo = Repo::new(state.pool.clone());

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

    let result = Repo::new(state.pool.clone()).create_job(project_id, body.engine_id, body.keyword_id).await;

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

    let result = Repo::new(state.pool.clone()).list_jobs(project_id, status).await;

    match result {

        Ok(list) => Json(list).into_response(),

        Err(e) => error_response(e),

    }

}



async fn get_job(Path(id): Path<Uuid>, state: State<Arc<AppState>>) -> Response {

    let result = Repo::new(state.pool.clone()).job_by_id(id).await;

    match result {

        Ok(Some(j)) => Json(j).into_response(),

        Ok(None) => (StatusCode::NOT_FOUND, Json(serde_json::json!({"error":"not found"}))).into_response(),

        Err(e) => error_response(e),

    }

}



async fn job_results(Path(id): Path<Uuid>, state: State<Arc<AppState>>) -> Response {

    let result = Repo::new(state.pool.clone()).results_for_job(id).await;

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

    let result = Repo::new(state.pool.clone()).project_results(project_id, q.kind.as_deref(), limit).await;

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

    let result = Repo::new(state.pool.clone()).result_stats(project_id).await;

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

    let result = Repo::new(state.pool.clone())

        .update_project(id, &body.name, body.description.as_deref())

        .await;

    match result {

        Ok(project) => Json(project).into_response(),

        Err(e) => error_response(e),

    }

}



/// PUT /api/projects/{id}/url — set the project's base target URL
/// (GSA Data-tab "URL (spintax)"). `None`/empty body 400s; a project
/// with an empty URL is left at the engine-spec default.
#[derive(Deserialize)]

pub struct SetProjectUrlBody {

    url: String,

}

async fn set_project_url_api(

    Path(id): Path<Uuid>,

    state: State<Arc<AppState>>,

    body: Json<SetProjectUrlBody>,

) -> Response {

    let url = body.url.trim().to_string();

    if url.is_empty() {

        return StatusCode::BAD_REQUEST

            .into_response();

    }

    match Repo::new(state.pool.clone())

        .set_project_url(id, &url)

        .await

    {

        Ok(p) => Json(p).into_response(),

        Err(e) => error_response(e),

    }

}



/// F#30 — PUT /api/projects/{id}/article — set the project's Article body
/// (the GSA "Article content" block). An empty body clears it, so the
/// runner auto-generates a short article from the keyword + target URL.
#[derive(Deserialize)]
pub struct SetProjectArticleBody {
    #[serde(default)]
    article: String,
}

async fn set_project_article_api(
    Path(id): Path<Uuid>,
    state: State<Arc<AppState>>,
    body: Json<SetProjectArticleBody>,
) -> Response {
    let article = body.article.clone();
    match Repo::new(state.pool.clone())
        .set_project_article(id, article)
        .await
    {
        Ok(p) => Json(p).into_response(),
        Err(e) => error_response(e),
    }
}

/// PUT /api/projects/{id}/comment — the GSA "Comment text": the project's
/// comment body for the comment / guestbook / blog-comment engines
/// (`%blog_comment%` / `%guestbook_comment%` / `%comment%`).
#[derive(Deserialize)]
pub struct SetProjectCommentBody {
    #[serde(default)]
    comment: String,
}

async fn set_project_comment_api(
    Path(id): Path<Uuid>,
    state: State<Arc<AppState>>,
    body: Json<SetProjectCommentBody>,
) -> Response {
    let comment = body.comment.clone();
    match Repo::new(state.pool.clone())
        .set_project_comment(id, comment)
        .await
    {
        Ok(p) => Json(p).into_response(),
        Err(e) => error_response(e),
    }
}

/// PUT /api/projects/{id}/anchor-options — the GSA "Anchor Text (below the
/// keyword)" tab: anchor mix JSON, URL randomization/variation, root-URL
/// percentage, linkwheel (upstream project ids), and auto-export.  Every
/// field is optional; the repo `COALESCE`s so omitted fields are left
/// untouched.
#[derive(Deserialize)]

pub struct AnchorOptionsBody {

    #[serde(default)]

    anchor_mix: Option<String>,

    #[serde(default)]

    use_random_url: Option<bool>,

    #[serde(default)]

    url_variations: Option<i32>,

    #[serde(default)]

    root_url: Option<i32>,

    #[serde(default)]

    linkwheel: Option<Vec<Uuid>>,

    #[serde(default)]

    auto_export: Option<bool>,

}

async fn set_project_anchor_options_api(

    Path(id): Path<Uuid>,

    state: State<Arc<AppState>>,

    body: Json<AnchorOptionsBody>,

) -> Response {

    let result = Repo::new(state.pool.clone())

        .set_project_options(

            id,

            body.anchor_mix.as_deref(),

            body.use_random_url,

            body.url_variations,

            body.root_url,

            body.linkwheel.as_deref(),

            body.auto_export,

        )

        .await;

    match result {

        Ok(p) => Json(p).into_response(),

        Err(e) => error_response(e),

    }

}



/// GET /api/projects/{id}/verified — GSA "link list" feed: the project's
/// verified (successful) submission URLs, most recent first.  Accepts an
/// optional `?limit=N` query param (default 500, max 5000).
#[derive(Deserialize)]

pub struct VerifiedQuery {

    #[serde(default)]

    limit: Option<i64>,

}

async fn list_verified_urls_api(

    Path(id): Path<Uuid>,

    Query(q): Query<VerifiedQuery>,

    state: State<Arc<AppState>>,

) -> Response {

    let limit = q.limit.unwrap_or(500);

    match Repo::new(state.pool.clone())

        .list_verified_urls(id, limit)

        .await

    {

        Ok(rows) => Json(rows).into_response(),

        Err(e) => error_response(e),

    }

}



/// GET /api/projects/{id} — read one project (404 if unknown).

async fn get_project(

    Path(id): Path<Uuid>,

    state: State<Arc<AppState>>,

) -> Response {

    match Repo::new(state.pool.clone()).project_by_id(id).await {

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

    match Repo::new(state.pool.clone()).delete_project(id).await {

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

    let repo = Repo::new(state.pool.clone());

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

    let repo = Repo::new(state.pool.clone());

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

    let repo = Repo::new(state.pool.clone());

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

    match Repo::new(state.pool.clone()).cancel_job(id).await {

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

    let result = Repo::new(state.pool.clone()).claim_next_pending(q.project_id).await;

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

    let repo = Repo::new(state.pool.clone());
    match repo
        .upsert_engine(
            project_id,
            &body.engine_name,
            body.engine_type.as_deref().unwrap_or("directory"),
            body.url.as_deref().unwrap_or(""),
            &body.file_path,
            body.step_count,
        )
        .await
    {
        Ok(mut e) => {
            // Honor a GSA ";down" engine (INI `enabled=0`) on manual add too.
            if !body.file_path.trim().is_empty()
                && !engine::ini_is_enabled(&resolve_engine_path(&body.file_path))
            {
                match repo.set_engine_enabled(e.id, false).await {
                    Ok(dis) => e = dis,
                    Err(err) => return error_response(err),
                }
            }
            Json(e).into_response()
        }
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

    let result = Repo::new(state.pool.clone())

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

    let repo = Repo::new(state.pool.clone());

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

    let repo = Repo::new(state.pool.clone());

    if let Err(e) = repo.set_setting("email.address", &body.address).await {

        return error_response(e);

    }

    if let Err(e) = repo.set_setting("email.password", &body.password).await {

        return error_response(e);

    }

    Json(serde_json::json!({ "status": "ok" })).into_response()

}



// ---------------------------------------------------------------------------
// E2 — Mailbox creation (GSA SERE flow).  GSA's answer to the register →
// email-verify → login wall: create the disposable mailbox FIRST via the
// email engines (anonmails.de register chain / hidebox.de visit flow),
// persist it as the operator mailbox, so registration-family target engines
// resolve their `%your e-mail%`/`%login%` tokens and the F#12 verification
// inbox is real.
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct CreateEmailBody {
    /// Optional engine name to pin (e.g. "anonmails.de"); omitted =
    /// provider auto-select (verifiable register flow first).
    #[serde(default)]
    engine: String,
}

/// POST /api/email/create — create a mailbox via a GSA email engine and
/// persist it as the operator mailbox. Response: the full creation report
/// (`engine`, `address`, `password`, `domain`, `steps: [{kind,url,ok,detail}]`);
/// `ok`/`verified` distinguish "confirmed" from "HTTP 200 only".
async fn create_email(
    State(state): State<Arc<AppState>>,
    body: Option<Json<CreateEmailBody>>,
) -> Response {
    let repo = Repo::new(state.pool.clone());

    let in_use_addr = repo.email_mailbox().await.unwrap_or_default().0;

    let gsa_root = std::env::var("GSA_INSTALL_ROOT")
        .unwrap_or_else(|_| r"C:\Users\kingsly\GSA_SER_portable".into());
    let root = std::path::PathBuf::from(gsa_root);
    let engines = match tokio::task::spawn_blocking(move || {
        engine::email_create::load_email_engines(&root)
    })
    .await
    {
        Ok(e) => e,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"ok": false, "error": e.to_string()})),
            )
                .into_response()
        }
    };
    if engines.is_empty() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "ok": false,
                "error": "no email engines found under GSA_INSTALL_ROOT/Email_Engines"
            })),
        )
            .into_response();
    }

    let pin = body.as_ref().map(|b| b.engine.clone()).unwrap_or_default();
    let in_use: Vec<String> = (!in_use_addr.trim().is_empty())
        .then(|| in_use_addr.clone())
        .into_iter()
        .collect();

    let driver: std::sync::Arc<dyn engine::Driver> = std::sync::Arc::new(engine::HttpDriver::new());
    let created = match tokio::time::timeout(
        std::time::Duration::from_secs(90),
        tokio::task::spawn_blocking(move || {
            engine::email_create::create_mailbox(&engines, &pin, &in_use, None, None, &driver)
        }),
    )
    .await
    {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"ok": false, "error": e.to_string()})),
            )
                .into_response()
        }
        Err(_) => {
            return (
                StatusCode::GATEWAY_TIMEOUT,
                Json(serde_json::json!({"ok": false, "error": "mailbox creation timed out (90s)"})),
            )
                .into_response()
        }
    };

    if created.ok && !created.address.is_empty() {
        if let Err(e) = repo.set_setting("email.address", &created.address).await {
            return error_response(e);
        }
        if let Err(e) = repo.set_setting("email.password", &created.password).await {
            return error_response(e);
        }
    }
    (
        if created.ok { StatusCode::OK } else { StatusCode::UNPROCESSABLE_ENTITY },
        Json(created),
    )
        .into_response()
}


/// POST /api/catengine/import — import the live GSA engine directories.

async fn catalog_import(

    State(state): State<Arc<AppState>>,

) -> Response {

    let gsa_root = std::env::var("GSA_INSTALL_ROOT").unwrap_or_else(|_| {

        r"C:\Users\kingsly\GSA_SER_portable".into()

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

// F#8b — CDP helpers (browser pool).  See the F#8b block in
// [`run_project_pool`] / [`test_engine_pool`].

/// GET `{base}/json` (CDP target list) and return the first `page`
/// target's `webSocketDebuggerUrl`.
use engine::Driver as _;  // F#8b — CDP/HTTP verb calls (navigate)

async fn probe_cdp_pages(base: &str) -> Option<String> {
    let http = engine::HttpDriver::new();
    let resp = http.navigate(format!("{base}/json")).await;
    if !resp.ok {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(&resp.html).ok()?;
    let arr = v.as_array()?;
    for t in arr {
        if t.get("type").and_then(|x| x.as_str()) == Some("page") {
            if let Some(ws) = t.get("webSocketDebuggerUrl").and_then(|x| x.as_str()) {
                return Some(ws.to_string());
            }
        }
    }
    None
}

/// Resolve a usable CDP `webSocketDebuggerUrl` from `base`:
/// probe `{base}/json`; if the endpoint isn't up and `base` is one of the
/// default 127.0.0.1:9222 endpoints, spawn a headless Edge with a dedicated
/// profile and poll for up to ~15 s.
async fn browser_ws_endpoint(base: &str) -> Option<String> {
    let base = base.trim_end_matches('/');
    if !(base.starts_with("http://") || base.starts_with("https://")) {
        return None;
    }
    if let Some(ws) = probe_cdp_pages(base).await {
        return Some(ws);
    }
    let is_default = matches!(base, "http://127.0.0.1:9222" | "http://localhost:9222");
    if !is_default {
        return None;
    }
    // Spawn headless Edge (dedicated profile so the user's session is untouched).
    let edge = std::env::var("SER_EDGE_PATH")
        .or_else(|_| {
            std::env::var("PROGRAMFILES(X86)")
                .map(|pf| format!("{pf}\\Microsoft\\Edge\\Application\\msedge.exe"))
        })
        .or_else(|_| {
            std::env::var("PROGRAMFILES")
                .map(|pf| format!("{pf}\\Microsoft\\Edge\\Application\\msedge.exe"))
        })
        .unwrap_or_else(|_| {
            r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe".to_string()
        });
    let profile = std::env::temp_dir().join("ser-replica-edge-profile");
    let _ = std::fs::create_dir_all(&profile);
    if std::path::Path::new(&edge).exists() {
        let _child = std::process::Command::new(&edge)
            .args([
                "--headless=new",
                "--remote-debugging-port=9222",
                "--remote-debugging-address=127.0.0.1",
                "--no-first-run",
                "--no-default-browser-check",
                "--disable-gpu",
            ])
            .arg(format!("--user-data-dir={}", profile.to_string_lossy()))
            .spawn();
    }
    for _ in 0..30 {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        if let Some(ws) = probe_cdp_pages(base).await {
            return Some(ws);
        }
    }
    None
}


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

/// Shared execution path between the HTTP `POST /api/projects/{id}/run`

/// handler and the F#17 scheduler loop: build the pool, fire the project

/// once, return the summary (no HTTP types — the handler wraps it in a

/// JSON response, the scheduler just logs + records it).

/// T6 — `pause_after_override` wins over `projects.pause_after` when
/// `Some` (the auto-run endpoint uses `Some(links_target)` so the first
/// winning engine soft-stops the rest of the queue).
pub async fn run_project_pool(
    project_id: Uuid,
    state: &Arc<AppState>,
    pause_after_override: Option<usize>,
    // T9 — fair-share controller for this run. `None` = legacy first-come-first-
    // served pool (the previous behavior, and the path used by manual / test
    // runs). `Some(..)` = work-conserving fair dispatch with per-engine rate
    // caps, 429/403 backoff, and a global target gate.
    fair: Option<std::sync::Arc<std::sync::Mutex<runner::FairState>>>,
    // T10c — lazy queue-refill provider (auto-runs only). `Some(..)` = when
    // the queue drains before the fair target is met, provision more jobs
    // and keep going instead of stopping.
    replenish: Option<runner::ReplenishFn>,
) -> anyhow::Result<serde_json::Value> {

    // GSA install root — same default the import path uses; override with

    // GSA_INSTALL_ROOT.

    let root = std::env::var("GSA_INSTALL_ROOT")

        .unwrap_or_else(|_| r"C:\Users\kingsly\GSA_SER_portable".into());

    let roots = vec![std::path::PathBuf::from(root)];



    // Operator mailbox — authoritative for F#12 mail runs.

    let (email_address, email_password) = Repo::new(state.pool.clone())

        .email_mailbox()

        .await

        .unwrap_or_default();



    // F#15 — the "Global Settings" drive the pool.

    let global_settings = Repo::new(state.pool.clone()).global_settings().await.unwrap_or_default();

    let mut cfg = runner::RunnerConfig::default();

    cfg.concurrent_workers = global_settings.threads.unwrap_or(4).clamp(1, 64) as usize;

    cfg.per_job_timeout = std::time::Duration::from_secs(global_settings.max_submit_seconds.unwrap_or(90).clamp(1, 3600) as u64);

    cfg.leader_idle_timeout = std::time::Duration::from_millis(800);

    cfg.max_retries = 0;



    // F#13 — proxy routing (when `use_proxies`).

    let use_proxies = Repo::new(state.pool.clone())

        .project_use_proxies(project_id)

        .await

        .ok()

        .flatten()

        .unwrap_or(false);

    let proxies: Vec<engine::proxy::ProxySpec> = if use_proxies {

        Repo::new(state.pool.clone())

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



    // F#19 / F#24-lite — fetch the project's own config row once and
    // thread anchor mix, URL options, linkwheel, auto-export, and the
    // GSA Data-tab URL (F#19 base target) into the submission context.
    let project_row = Repo::new(state.pool.clone())
        .project_by_id(project_id)
        .await
        .ok()
        .flatten();

    let ctx = runner::SubmissionContext {
        email_address,
        email_password,
        proxies,
        settings: std::sync::Arc::new(global_settings.clamped_ref()),

        project_id,
        project_url: project_row.as_ref().map(|p| p.url.clone()).unwrap_or_default(),
        // F#30 — the project's own Article body (GSA "Article content");
        // empty → the runner auto-generates a short article from the
        // keyword + target URL.
        article_text: project_row
            .as_ref()
            .map(|p| p.article_text.clone())
            .unwrap_or_default(),
        // F#31 — the project's own Comment body (GSA "Comment text");
        // empty → the runner auto-generates a comment from the comments.dat pool.
        comment_text: project_row
            .as_ref()
            .map(|p| p.comment_text.clone())
            .unwrap_or_default(),
        anchor: project_row
            .as_ref()
            .and_then(|p| p.anchor_mix.as_deref())
            .and_then(|s| serde_json::from_str::<engine::anchor::PoolConfig>(s).ok())
            .filter(|c| !c.is_empty()),
        url_options: engine::anchor::UrlOptions {
            use_random_url: project_row.as_ref().map(|p| p.use_random_url).unwrap_or(false),
            url_variations: u32::try_from(project_row.as_ref().map(|p| p.url_variations).unwrap_or(0))
                .unwrap_or(0),
            root_url: u32::try_from(project_row.as_ref().map(|p| p.root_url).unwrap_or(0))
                .unwrap_or(0),
        },
        linkwheel_project_ids: project_row.as_ref().map(|p| p.linkwheel.clone()).unwrap_or_default(),
        auto_export: project_row.as_ref().map(|p| p.auto_export).unwrap_or(false),
        // T5 — searched site list: the base page for discovery engines.
        project_sites: Repo::new(state.pool.clone())
            .list_project_sites(project_id)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|r| r.url)
            .collect(),
        // T6 — live run feed: every runner event lands in `job_events`
        // (DB) and on the SSE bus (live console) at the moment it happens.
        events: Some(crate::runfeed::RunEventSink::new(
            project_id,
            state.pool.clone(),
            state.run_bus.sender(),
        )),

        ..Default::default()
    };



    let mut drivers = runner::DriverRegistry::new()

        .with_http(engine::HttpDriver::new())

        .with_email(engine::HttpDriver::new());

    // F#8b — CDP browser pool (GSA's "browser-submit" half): JS-heavy engines
    // (set unknown variable / page must have / recaptcha / …) route to
    // `DriverKind::Browser`.  Connect a `WebDriver` to a headless Edge on
    // `SER_EDGE_CDP` (default http://127.0.0.1:9222), spawning Edge if the
    // endpoint isn't up.  `SER_EDGE_CDP=0` disables the pool → the previous
    // clean "driver not registered for kind Browser" behaviour.
    //
    // T14 — TWO LANES: the bound WebDriver still owns ONE live page, so
    // browser-routed jobs serialize against `DriverRegistry::browser_gate`
    // (held per job, full job duration).  What changed: we no longer collapse
    // the WHOLE run to `concurrent_workers = 1` when the queue contains a
    // browser engine — the Http/Email fleet (the overwhelming majority) keeps
    // its full worker width while browser jobs flow behind the gate.
    //
    // T10c/queue check — only connect CDP lazily when this run's pending
    // queue actually contains browser-routed engines (an all-HTTP queue pays
    // zero browser cost even while Edge is up).
    {
        let base = std::env::var("SER_EDGE_CDP")
            .unwrap_or_else(|_| "http://127.0.0.1:9222".to_string());
        let needs_browser = queue_needs_browser(project_id, &state.pool).await;
        if base != "0" && needs_browser {
            match browser_ws_endpoint(&base).await {
                Some(ws) => match engine::WebDriver::connect(ws).await {
                    Ok(wd) => {
                        drivers = drivers.with_browser(wd);
                        eprintln!(
                            "[browser] CDP driver attached — browser jobs serialize on the single-flight gate; Http/Email keep full worker width"
                        );
                    }
                    Err(e) => eprintln!("[browser] CDP ws connect failed: {e}"),
                },
                None => eprintln!("[browser] no CDP endpoint at {base} (15s probe) — browser-routed engines will report a clean failure"),
            }
        } else if needs_browser {
            eprintln!("[browser] queue has browser-routed engines but SER_EDGE_CDP=0 — they will fail with a clean driver error");
        }
    }


    // T6 — a handler may override the project-level pause ceiling
    // (auto-run: stop as soon as the target link count is reached).
    let pause_after = pause_after_override
        .unwrap_or_else(|| project_row.as_ref().map(|p| p.pause_after as usize).unwrap_or(0));
    let pool = runner::pool_for_disk(state.pool.clone(), cfg, roots, drivers)
        .with_context(ctx)
        .with_pause_after(pause_after);
    // T9 — attach the fair-share controller when this run uses fair share.
    // (Moves the Arc in; the pool clones it per worker.)
    let pool = match fair {
        Some(f) => pool.with_fair(f),
        None => pool,
    };
    // T10c — attach the lazy queue-refill provider (auto-runs only).
    let pool = match replenish {
        Some(rp) => pool.with_replenish(rp),
        None => pool,
    };


    // F#18 — time the run so the alert can carry a duration.
    let started = std::time::Instant::now();
    let summary = pool.run(project_id).await;
    let duration_secs = started.elapsed().as_secs();

    // F#18 — one notification per run outcome (good, bad, or mixed).
    let project_name = Repo::new(state.pool.clone())
        .project_by_id(project_id)
        .await
        .ok()
        .flatten()
        .map(|p| p.name)
        .unwrap_or_else(|| project_id.to_string());
    if let Err(e) = notifier::notify_run(
        state,
        project_id,
        &project_name,
        summary.claimed,
        summary.done,
        summary.failed,
        summary.panics,
        0, // captcha_blocked: not separately surfaced by the pool yet
        duration_secs,
    )
    .await
    {
        eprintln!("[notify] run-outcome emit failed: {e}");
    }


    Ok(serde_json::json!({

        "status": "ok",

        "project_id": project_id.to_string(),

        "claimed": summary.claimed,

        "done": summary.done,

        "failed": summary.failed,

        "panics": summary.panics,

    }))

}



/// T10c — does this project's *pending* queue contain any browser-routed
/// engine? A bound CDP WebDriver owns one live page target, so only runs
/// that actually touch browser engines serialize to a single worker; the
/// HTTP/email fleet (the vast majority) keeps full worker width. Reads each
/// queued engine's INI and applies the same routing rule GSA uses (browser
/// markers → `DriverKind::Browser`). Fails conservative: any read it can't
/// answer (unknown queue, engine-less job, missing spec) treats the run as
/// browser-needing — the pre-T10c behaviour.
async fn queue_needs_browser(project_id: Uuid, pool: &sqlx::PgPool) -> bool {
    let repo = ser_db::Repo::new(pool.clone());
    let pending = match repo.list_jobs(project_id, Some(ser_db::JobStatus::Pending)).await {
        Ok(j) => j,
        Err(_) => return true,
    };
    if pending.is_empty() {
        return false;
    }
    let mut ids: std::collections::BTreeSet<Uuid> = Default::default();
    for j in &pending {
        match j.engine_id {
            Some(id) => {
                ids.insert(id);
            }
            None => return true,
        }
    }
    let engines = match repo.list_engines_all(project_id).await {
        Ok(e) => e,
        Err(_) => return true,
    };
    let root = std::env::var("GSA_INSTALL_ROOT")
        .unwrap_or_else(|_| r"C:\Users\kingsly\GSA_SER_portable".into());
    for id in &ids {
        let Some(path) = engines.iter().find(|e| e.id == *id).map(|e| e.file_path.clone())
        else {
            return true;
        };
        let raw = match std::fs::read_to_string(std::path::Path::new(&root).join(&path)) {
            Ok(t) => t,
            Err(_) => return true, // unresolvable spec — be conservative
        };
        if engine::routing::required_driver_kind_from_text(&raw)
            == engine::routing::DriverKind::Browser
        {
            return true;
        }
    }
    false
}

/// T10 — background multi-project run dispatcher.
///
/// Spawns the pool as a detached tokio task (survives the HTTP request
/// finishing and the browser navigating away), registers it in the run
/// tracker (which enforces *one active run per project* and a *global cap*),
/// and immediately returns the run id so the client gets 202 and polls
/// `/run-status` + the SSE feed for progress.  Multiple projects dispatch in
/// parallel; the process-wide cap is the only cross-project throttle.
async fn dispatch_background(
    state: &Arc<AppState>,
    project_id: Uuid,
    links_target: usize,
    engines_count: usize,
    auto: bool,
    pause_override: Option<usize>,
    fair: Option<Arc<std::sync::Mutex<runner::FairState>>>,
    enqueued_jobs: usize,
    order_names: Vec<String>,
    replenish: Option<runner::ReplenishFn>,
) -> Result<Uuid, crate::runtrack::BeginErr> {
    let run_id = Uuid::new_v4();
    let st = Arc::clone(state);
    let fair_task = fair.clone();
    let replen_task = replenish.clone();
    let names = order_names;
    let enq = enqueued_jobs;

    let task = tokio::spawn(async move {
        let outcome = run_project_pool(
            project_id,
            &st,
            pause_override,
            fair_task.clone(),
            replen_task.clone(),
        )
        .await;
        let (status, payload) = match outcome {
            Ok(mut s) => {
                if fair_task.is_some() {
                    let r = fair_task.as_ref().unwrap().lock().unwrap();
                    let engines_json: Vec<serde_json::Value> = r
                        .snapshot(std::time::Instant::now())
                        .into_iter()
                        .map(|ss| {
                            serde_json::json!({
                                "engine": ss.engine,
                                "links": ss.links,
                                "admitted": ss.admitted,
                                "rate_hits": ss.rate_hits,
                                "disabled": ss.disabled,
                                "cool_remaining_secs": ss.cool_remaining.map(|d| d.as_secs()),
                            })
                        })
                        .collect();
                    let (rj_target, rj_cool, rj_inflight, rj_quota, rj_disabled) = r.rejections();
                    let created = r.total_links();
                    let target = r.policy().target;
                    if let Some(obj) = s.as_object_mut() {
                        obj.insert("auto_run".to_string(), serde_json::json!(true));
                        obj.insert("fair_share".to_string(), serde_json::json!(true));
                        obj.insert("engines_tried_order".to_string(), serde_json::json!(names));
                        obj.insert("links_target".to_string(), serde_json::json!(target));
                        obj.insert("enqueued_jobs".to_string(), serde_json::json!(enq));
                        obj.insert(
                            "links_created".to_string(),
                            serde_json::json!(created),
                        );
                        if created < 1 {
                            obj.insert(
                                "links_created_note".to_string(),
                                serde_json::json!("created counts finished engine jobs; slow engines add more as they complete"),
                            );
                        }
                        obj.insert(
                            "fair".to_string(),
                            serde_json::json!({
                                "target": target,
                                "links_created": created,
                                "reached_target": created >= target,
                                "cancelled": r.force_stop(),
                                "stop_reason": r.stop_reason().map(|s| s.to_string()),
                                "engines_blacklisted": r.blacklisted_count(),
                                "engines": engines_json,
                                "rejections": {
                                    "target": rj_target,
                                    "cooling": rj_cool,
                                    "in_flight_cap": rj_inflight,
                                    "quota": rj_quota,
                                    "disabled": rj_disabled
                                }
                            }),
                        );
                    }
                }
                ("ok".to_string(), s)
            }
            Err(e) => {
                let msg = e.to_string();
                use runner::EventSink as _;
                let sink = crate::runfeed::RunEventSink::new(
                    project_id,
                    st.pool.clone(),
                    st.run_bus.sender(),
                );
                sink.record(runner::RunEvent {
                    job_id: None,
                    engine: None,
                    level: "error".into(),
                    message: format!("run started in background but failed to initialise: {msg}"),
                });
                ("error".to_string(), serde_json::json!({ "error": msg }))
            }
        };
        st.runs.finalize(
            project_id,
            run_id,
            crate::runtrack::RunSummary {
                run_id,
                finished_at: chrono::DateTime::from(chrono::Utc::now()),
                status,
                payload,
            },
        );
    });

    let ar = crate::runtrack::ActiveRun {
        run_id,
        project_id,
        started_at: chrono::DateTime::from(chrono::Utc::now()),
        links_target,
        engines: engines_count,
        auto,
        fair,
        task,
    };
    state
        .runs
        .begin(ar)
        .map(|()| run_id)
}

/// T10 — map a [`crate::runtrack::BeginErr`] to an HTTP response
/// (409 same-project already running; 429 global cap reached).
fn begin_err_response(
    beg: crate::runtrack::BeginErr,
    state: &Arc<AppState>,
    project_id: Uuid,
) -> Response {
    match beg {
        crate::runtrack::BeginErr::AlreadyRunning => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "a run is already in progress for this project",
                "project_id": project_id.to_string(),
                "active": state.runs.active_status(project_id),
                "hint": "poll GET /api/projects/{id}/run-status, or POST …/run-cancel to stop it",
            })),
        )
            .into_response(),
        crate::runtrack::BeginErr::CapReached => (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({
                "error": format!("process-wide cap of {} active runs reached; wait for one to finish", state.runs.capacity()),
                "capacity": state.runs.capacity(),
                "active_runs": state.runs.all_active(),
            })),
        )
            .into_response(),
    }
}


/// T10 — run a project's pending job queue in the BACKGROUND (202 + run id).
/// Progress: `GET …/run-status`; logs: `GET …/events/live` (SSE).  A second
/// start of the same project is rejected while one is active (409).
async fn run_project(State(state): State<Arc<AppState>>, Path(project_id): Path<Uuid>) -> Response {
    match dispatch_background(&state, project_id, 0, 0, false, None, None, 0, Vec::new(), None).await {
        Ok(run_id) => (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({
                "run_id": run_id,
                "status": "running",
                "background": true,
                "project_id": project_id.to_string(),
                "links_target": 0,
                "active_runs": state.runs.active_count(),
                "capacity": state.runs.capacity(),
            })),
        )
            .into_response(),
        Err(beg) => begin_err_response(beg, &state, project_id),
    }
}


// ─────────────────────────────────────────────────────────────
// Stage B — multi-family best-engine selection + mixed run.
// A SER run posts the BEST backlink of each KIND (article / comment /
// link) rather than one engine on a hunch: classify every enabled engine
// into a family, rank the strongest per family (dofollow-first, live,
// low-friction), and drive each family's best engine(s) against the target.
// ─────────────────────────────────────────────────────────────

/// One engine the selector is pointing at (its DB id, for enqueue + display).
#[derive(serde::Serialize, Clone)]
struct PickedEngine {
    id: Uuid,
    name: String,
    engine_type: String,
    live: bool,
}

/// Resolve a stored engine `file_path` (usually relative, e.g. `Engines` + name)
/// against the GSA install root so `parse_engine_file` finds the file.
fn resolve_engine_path(file_path: &str) -> std::path::PathBuf {
    let root = std::env::var("GSA_INSTALL_ROOT")
        .unwrap_or_else(|_| r"C:\Users\kingsly\GSA_SER_portable".to_string());
    let p = std::path::Path::new(file_path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::path::PathBuf::from(root).join(file_path)
    }
}

/// Liveness check: does this host answer an HTTPS request through the proxy?
/// Used so the family selector PREFERs live sites over dead ones (the "best
/// engine for the article" is one that actually responds, not just one with
/// the fewest fields).
fn probe_live(host: &str, proxy: &str) -> bool {
    if host.is_empty() || host.contains('%') {
        return false;
    }
    let mut cmd = std::process::Command::new("curl");
    cmd.args(["-s", "-o", "/dev/null", "-m", "6", "-w", "%{http_code}", "-A", "Mozilla/5.0"]);
    if !proxy.is_empty() {
        cmd.args(["--proxy", &format!("http://{}", proxy)]);
    }
    cmd.arg(format!("https://{}/", host));
    match cmd.output() {
        Ok(out) => {
            let code = std::str::from_utf8(&out.stdout).unwrap_or("").trim().to_string();
            matches!(code.parse::<u16>(), Ok(c) if (200..400).contains(&c))
        }
        Err(_) => false,
    }
}

/// For each enabled project engine, read its INI to recover dofollow /
/// just-download / field count, then rank the candidates per family.  Returns
/// the top-N per family (indices into `picked`) and the parallel `picked`
/// list so a caller can map index → DB engine id.
fn build_family_selection(
    rows: Vec<ser_db::Engine>,
    per_family: usize,
    proxy: &str,
) -> (
    std::collections::BTreeMap<engine::Family, Vec<usize>>,
    Vec<PickedEngine>,
) {
    use engine::EngineType;
    let mut cands: Vec<engine::CandidateEngine> = Vec::new();
    let mut picked: Vec<PickedEngine> = Vec::new();
    for row in rows.into_iter().filter(|r| r.enabled) {
        let (dofollow, just_dl, fields, etype, host, live) =
            match engine::parse_engine_file(resolve_engine_path(&row.file_path).as_path()) {
                Ok(eng) => {
                    let host = eng
                        .base_url()
                        .split("://")
                        .nth(1)
                        .map(|h| h.split('/').next().unwrap_or(h))
                        .unwrap_or("")
                        .to_string();
                    let fields = eng.steps.iter().map(|s| s.fields.len()).sum::<usize>();
                    let (df, jd, et) = (eng.dofollow, eng.fixed_url.is_some(), eng.engine_type);
                    (df, jd, fields, et, host.clone(), probe_live(&host, proxy))
                }
                Err(_) => (false, false, 0_usize, EngineType::Unknown, String::new(), false),
            };
        cands.push(engine::CandidateEngine {
            engine_name: row.engine_name.clone(),
            engine_type: etype,
            dofollow,
            field_count: fields,
            just_download: just_dl,
            is_enabled: true,
            is_live: Some(live),
        });
        picked.push(PickedEngine {
            id: row.id,
            name: row.engine_name,
            engine_type: row.engine_type,
            live,
        });
    }
    let sel = engine::best_per_family(&cands, per_family);
    (sel, picked)
}

/// Preferred proxy for liveness probes: the first enabled project proxy
/// (host:port), or empty for direct access.
async fn first_proxy(repo: &Repo) -> String {
    repo.list_enabled_proxies()
        .await
        .ok()
        .into_iter()
        .flatten()
        .next()
        .map(|p| format!("{}:{}", p.host, p.port))
        .unwrap_or_default()
}

/// Optional `?per=N` cap on how many engines per family (default 2).
#[derive(Deserialize)]
struct PerFamilyQ {
    #[serde(default)]
    per: Option<usize>,
}

fn per_from_query(q: &PerFamilyQ) -> usize {
    q.per.map(|n| n.clamp(1, 10)).unwrap_or(2)
}

/// DRY — which engines WOULD be picked per family for this project (best-
/// engine-per-family), without enqueueing or running anything.
async fn family_plan(
    Path(project_id): Path<Uuid>,
    Query(q): Query<PerFamilyQ>,
    state: State<Arc<AppState>>,
) -> Response {
    let repo = Repo::new(state.pool.clone());
    match repo.list_engines_all(project_id).await {
        Ok(rows) => {
            let per = per_from_query(&q);
            let (sel, picked) = build_family_selection(rows, per, &first_proxy(&repo).await);
            let families: std::collections::BTreeMap<String, Vec<PickedEngine>> = sel
                .into_iter()
                .map(|(fam, idxs)| {
                    let engines: Vec<PickedEngine> = idxs
                        .iter()
                        .filter_map(|&i| picked.get(i).cloned())
                        .collect();
                    (fam.as_str().to_string(), engines)
                })
                .collect();
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "per_family": per,
                    "families": families,
                    "total_selected": families.values().map(Vec::len).sum::<usize>(),
                })),
            )
                .into_response()
        }
        Err(e) => error_response(e),
    }
}

/// Stage B run — enqueue the best engine of EACH family (article / comment /
/// link) crossed with every project keyword, then drive the pool.  This is
/// the "post the best backlink of each kind" flow the project actually wants.
async fn run_mixed(
    Path(project_id): Path<Uuid>,
    Query(q): Query<PerFamilyQ>,
    state: State<Arc<AppState>>,
) -> Response {
    let repo = Repo::new(state.pool.clone());
    let (rows, kws) = match (
        repo.list_engines_all(project_id).await,
        repo.list_keywords(project_id).await,
    ) {
        (Ok(r), Ok(k)) => (r, k),
        (Err(e), _) | (_, Err(e)) => return error_response(e),
    };

    let per = per_from_query(&q);
    let (sel, picked) = build_family_selection(rows, per, &first_proxy(&repo).await);
    if sel.is_empty() {
        return (
            StatusCode::OK,
            Json(serde_json::json!({
                "status": "nothing-to-run",
                "reason": "no enabled engine maps to a posting family (article/comment/link)",
            })),
        )
            .into_response();
    }

    let mut enqueued = 0usize;
    for idxs in sel.values() {
        for &i in idxs {
            if let Some(ep) = picked.get(i) {
                for kw in &kws {
                    if repo
                        .create_job(project_id, Some(ep.id), Some(kw.id))
                        .await
                        .is_ok()
                    {
                        enqueued += 1;
                    }
                }
            }
        }
    }

    match run_project_pool(project_id, &state, None, None, None).await {
        Ok(v) => {
            let mut v = v;
            if let Some(obj) = v.as_object_mut() {
                obj.insert("multi_family".to_string(), serde_json::json!(true));
                obj.insert("per_family".to_string(), serde_json::json!(per));
                obj.insert("enqueued_jobs".to_string(), serde_json::json!(enqueued));
            }
            (StatusCode::OK, Json(v)).into_response()
        }
        Err(e) => error_response(e),
    }
}


// ─────────────────────────────────────────────────────────────
// T6 — Auto-run: the SYSTEM picks and orders the engines.
//
// The user names the site + link keywords; we rank the project's
// engines by health (real successes first, captcha walls last,
// least-used next so a fresh engine gets its shot) and queue ONE
// job per engine in worst→best order.  The pool dispatches in
// `created_at DESC` order, so the best engines are dispatched
// first, in parallel waves, and the first success soft-stops the
// rest (pause-after override).  That is GSA's "try engine, on
// failure try the next" — the user never picks an engine.
// ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct RunAutoBody {
    /// How many top-ranked engines to fan out.  Omit = **all enabled engines**
    /// (default).  An explicit value is clamped to `1..=500`.
    #[serde(default)]
    engines: Option<usize>,
    /// Successful links to create before soft-stopping the rest of the
    /// queue (default: one per keyword — "try & error until we win").
    #[serde(default)]
    links: Option<usize>,
}

/// T10 — fair-share auto-run in the BACKGROUND (202 + run id).
async fn run_auto(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<Uuid>,
    Json(body): Json<RunAutoBody>,
) -> Response {
    let pre = match run_auto_prepare(project_id, &state, body).await {
        Ok(p) => p,
        Err(e) => return error_response(e),
    };
    match dispatch_background(
        &state,
        project_id,
        pre.links_target,
        pre.engines,
        true,
        Some(0),
        Some(pre.fair),
        pre.enqueued,
        pre.order_names.clone(),
        Some(pre.replenish),
    )
    .await
    {
        Ok(run_id) => (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({
                "run_id": run_id,
                "status": "running",
                "background": true,
                "project_id": project_id.to_string(),
                "links_target": pre.links_target,
                "engines": pre.engines,
                "enqueued_jobs": pre.enqueued,
                "active_runs": state.runs.active_count(),
                "capacity": state.runs.capacity(),
            })),
        )
            .into_response(),
        Err(beg) => begin_err_response(beg, &state, project_id),
    }
}

/// T10 — live run status for a project (the dashboard's source of truth).
/// `active`  = the in-flight run (live fair progress + started_at), if any.
/// `jobs`    = per-status job counts straight from the DB (always current).
/// `last_run` = the most recent finished run summary (persisted alongside).
async fn project_run_status(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<Uuid>,
) -> Response {
    let repo = Repo::new(state.pool.clone());
    if repo.project_by_id(project_id).await.ok().flatten().is_none() {
        return (StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "not found" })))
            .into_response();
    }
    let counts = repo
        .job_status_counts_for_project(project_id)
        .await
        .unwrap_or_default();
    let mut jobs = serde_json::Map::new();
    for (st, n) in counts {
        jobs.insert(st, serde_json::json!(n));
    }
    let active = state.runs.active_status(project_id);
    let last = state
        .runs
        .latest(project_id)
        .map(|s| {
            serde_json::json!({
                "run_id": s.run_id,
                "finished_at": s.finished_at.to_rfc3339(),
                "status": s.status,
                "summary": s.payload,
            })
        });
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "project_id": project_id.to_string(),
            "active": active,
            "jobs": jobs,
            "last_run": last,
            "active_runs_global": state.runs.active_count(),
            "capacity": state.runs.capacity(),
        })),
    )
        .into_response()
}

/// T10 — cancel a project's active background run (soft).
/// Stops admitting new jobs (fair `force_stop`) and flips the still-queued
/// `pending` jobs to `cancelled`; already-claimed jobs finish their attempt.
async fn project_run_cancel(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<Uuid>,
) -> Response {
    let repo = Repo::new(state.pool.clone());
    let had_fair = if let Some(fair) = state.runs.fair_of(project_id) {
        fair.lock().unwrap().set_force_stop(true);
        true
    } else {
        false
    };
    let cancelled = repo.cancel_pending_jobs(project_id).await.unwrap_or(0);
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "project_id": project_id.to_string(),
            "force_stop_requested": had_fair,
            "pending_cancelled": cancelled,
            "active": state.runs.active_status(project_id),
        })),
    )
        .into_response()
}

/// T10 — the global "what's running now" view across ALL projects (the
/// operator's cross-project dashboard). Lists every in-flight run live.
/// T11 — each run carries its FULL live state (fair-share breakdown,
/// in-flight workers, rejections, blacklisted engines) via active_status.
async fn global_runs(State(state): State<Arc<AppState>>) -> Response {
    let active = state.runs.all_active();
    let mut runs = Vec::with_capacity(active.len());
    for base in active {
        let mut v = base;
        // Attach the live fair-share state when the run registered one.
        let pid = v
            .get("project_id")
            .and_then(|s| s.as_str())
            .and_then(|s| Uuid::parse_str(s).ok());
        let fair = match pid {
            Some(p) => state.runs.fair_of(p),
            None => None,
        };
        if let Some(fair) = fair {
            let r = fair.lock().unwrap();
            let engines_json: Vec<serde_json::Value> = r
                .snapshot(std::time::Instant::now())
                .into_iter()
                .map(|ss| {
                    serde_json::json!({
                        "engine": ss.engine,
                        "links": ss.links,
                        "admitted": ss.admitted,
                        "rate_hits": ss.rate_hits,
                        "disabled": ss.disabled,
                        "cool_remaining_secs": ss.cool_remaining.map(|d| d.as_secs()),
                    })
                })
                .collect();
            let (rj_target, rj_cool, rj_inflight, rj_quota, rj_disabled) = r.rejections();
            let target = r.policy().target;
            let created = r.total_links();
            if let Some(obj) = v.as_object_mut() {
                obj.insert(
                    "fair".to_string(),
                    serde_json::json!({
                        "target": target,
                        "links_created": created,
                        "reached_target": created >= target,
                        "cancelled": r.force_stop(),
                        "stop_reason": r.stop_reason().map(|s| s.to_string()),
                        "engines_blacklisted": r.blacklisted_count(),
                        "engines": engines_json,
                        "rejections": {
                            "target": rj_target,
                            "cooling": rj_cool,
                            "in_flight_cap": rj_inflight,
                            "quota": rj_quota,
                            "disabled": rj_disabled
                        }
                    }),
                );
            }
        }
        runs.push(v);
    }
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "active_runs": runs.len(),
            "capacity": state.runs.capacity(),
            "runs": runs,
        })),
    )
        .into_response()
}

struct RunAutoReady {
    links_target: usize,
    engines: usize,
    enqueued: usize,
    order_names: Vec<String>,
    fair: std::sync::Arc<std::sync::Mutex<runner::FairState>>,
    /// T10c — lazy queue-refill provider for the pool (budget-capped).
    replenish: runner::ReplenishFn,
}

/// T10 — validate, rank engines, banner, provision the job queue, and build
/// the fair-share controller — but do NOT execute.  The handler then hands
/// this to `dispatch_background`, which runs it detached.  The execution +
/// fair summary now happen inside that background task.
async fn run_auto_prepare(
    project_id: Uuid,
    state: &Arc<AppState>,
    body: RunAutoBody,
) -> anyhow::Result<RunAutoReady> {
    let repo = Repo::new(state.pool.clone());

    // 1) Sanity: there must be engines + keywords to drive.
    let project = match repo.project_by_id(project_id).await? {
        Some(p) => p,
        None => anyhow::bail!("project not found"),
    };
    let keywords = repo.list_keywords(project_id).await?;
    if keywords.is_empty() {
        anyhow::bail!("no keywords on this project — add at least one link keyword first");
    }
    let mut engines = repo.list_engines_all(project_id).await?;
    engines.retain(|e| e.enabled && !e.file_path.trim().is_empty());
    if engines.is_empty() {
        anyhow::bail!("no usable engines on this project (engines need an INI file path)");
    }

    // 2) Rank by health.  Successes are the only hard signal; then
    // captcha walls (they eat a whole job), then least-used (fresh
    // engines deserve a fair shot before re-tired ones).
    engines.sort_by(|a, b| {
        b.successes.cmp(&a.successes)
            .then(b.last_success_at.cmp(&a.last_success_at))
            .then(a.captcha_failures.cmp(&b.captcha_failures))
            .then(a.attempts.cmp(&b.attempts))
            .then_with(|| a.created_at.cmp(&b.created_at))
    });
    // Fan out **all enabled engines** by default — every working engine
    // gets dispatched per run.  Only an explicit `engines:` request narrows
    // the pool (clamped to a sane 500 ceiling).
    let top_n = match body.engines {
        Some(n) => n.clamp(1, 500),
        None => engines.len().max(1),
    };
    // Head of the sorted list = best health first.
    let picked: Vec<ser_db::Engine> = engines.into_iter().take(top_n).collect();
    let _ = &project;
    let order_names: Vec<String> = picked.iter().map(|e| e.engine_name.clone()).collect();
    let links_target = body.links.unwrap_or(keywords.len()).max(1);

    // 3) Banner line into the live feed (level "auto") — the console's
    // first non-user line of every run, stating what the system chose.
    {
        use runner::EventSink as _;
        let sink = crate::runfeed::RunEventSink::new(
            project_id,
            state.pool.clone(),
            state.run_bus.sender(),
        );
        sink.record(runner::RunEvent {
            job_id: None,
            engine: None,
            level: "auto".into(),
            message: format!(
                "AUTO-RUN — fair-share across {} engines (rate-capped, 429/403 auto-backoff), target {} links: {}",
                picked.len(),
                links_target,
                order_names.join(" → ")
            ),
        });
    }

    // 4) T9 — provision the job queue. Over-provision (2× target, capped at
    //    5000) so the full target stays reachable even if one engine goes hot
    //    or down: fair-share absorbs the failed engine's share onto the healthy
    //    ones. Round-robin across engines (fair); cycle keywords for variety.
    let n_eng = picked.len().max(1);
    let total_jobs = (links_target * 2).min(5000).max(1);
    let mut enqueued = 0usize;
    for slot in 0..total_jobs {
        let e = &picked[slot % n_eng];
        let kw = &keywords[(slot / n_eng) % keywords.len()];
        if repo.create_job(project_id, Some(e.id), Some(kw.id)).await.is_ok() {
            enqueued += 1;
        }
    }

    // 5) T9 — build the fair-share controller and execute. `pause_after` is 0
    //    (fair owns the stopping): the leader stops admitting jobs once
    //    `links_target` links are created; a 429/403/captcha puts an engine on
    //    exponential cooldown so the others absorb the load (work-conserving).
    let policy = runner::FairPolicy {
        target: links_target,
        max_inflight_per_engine: 2,
        max_per_engine: 0,
        base_backoff: std::time::Duration::from_secs(20),
        max_backoff: std::time::Duration::from_secs(300),
    };
    let fair = std::sync::Arc::new(std::sync::Mutex::new(
        runner::FairState::new(policy, picked.iter().map(|e| e.id)),
    ));
    // 6) T10c — lazy refill. When the provisioned queue drains before the
    //    target is met (a 1,000-link run burns through its 4,000 pre-queued
    //    jobs quickly), the pool leader calls this: it re-provisions jobs
    //    round-robin on the same engines/keywords and returns how many it
    //    made. Hard per-run budget (5×target, min 5,000) guarantees the run
    //    has a termination condition even for a fully dead fleet — the fair
    //    blacklist + `engines_exhausted()` is the real stop signal, the
    //    budget the guard against unbounded refills. A `0` return = stop.
    let links_budget = (links_target * 5).max(5000);
    let enq_counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(enqueued));
    let eng_ids: Vec<Uuid> = picked.iter().map(|e| e.id).collect();
    let kw_ids: Vec<Uuid> = keywords.iter().map(|k| k.id).collect();
    let state_rc = std::sync::Arc::clone(state);
    let replenish: runner::ReplenishFn = std::sync::Arc::new(move |need: usize| {
        let budget = links_budget;
        let already = enq_counter.load(std::sync::atomic::Ordering::Acquire);
        let count = need.min(budget.saturating_sub(already));
        let eng_ids = eng_ids.clone();
        let kw_ids = kw_ids.clone();
        let db = state_rc.pool.clone();
        let bus = state_rc.run_bus.sender();
        let enq_counter = std::sync::Arc::clone(&enq_counter);
        let project = project_id;
        Box::pin(async move {
            if count == 0 {
                return Ok(0usize);
            }
            let repo = Repo::new(db.clone());
            let mut made = 0usize;
            for slot in 0..count {
                let eng = eng_ids[slot % eng_ids.len()];
                let kw = kw_ids[slot % kw_ids.len()];
                if repo.create_job(project, Some(eng), Some(kw)).await.is_ok() {
                    made += 1;
                }
            }
            enq_counter.fetch_add(made, std::sync::atomic::Ordering::Release);
            if made > 0 {
                use runner::EventSink as _;
                let sink = crate::runfeed::RunEventSink::new(project, db, bus);
                let _ = sink.record(runner::RunEvent {
                    job_id: None,
                    engine: None,
                    level: "auto".into(),
                    message: format!("queue drained before target — replenished {made} more jobs (per-run cap {budget}) — continuing"),
                });
            }
            Ok(made)
        })
    });
    Ok(RunAutoReady {
        links_target,
        engines: picked.len(),
        enqueued,
        order_names,
        fair,
        replenish,
    })
}

// ─────────────────────────────────────────────────────────────
// T6 — Live run feed: DB-tail + SSE.
// ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct EventsQ {
    /// Continue after this `job_events.id` (default 0 = from the start).
    #[serde(default)]
    after: i64,
    #[serde(default = "events_default_limit")]
    limit: i64,
}

fn events_default_limit() -> i64 {
    200
}

/// T6 — tail of the project's live run log (source of truth).
/// The UI fetches this on open / reconnect and then follows the
/// SSE stream; both carry the same `id`, so the console dedupes.
async fn project_events(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<Uuid>,
    Query(q): Query<EventsQ>,
) -> Response {
    let repo = Repo::new(state.pool.clone());
    match repo.list_job_events(project_id, q.after, q.limit).await {
        Ok(rows) => (
            StatusCode::OK,
            Json(serde_json::json!({ "events": rows })),
        )
            .into_response(),
        Err(e) => error_response(e),
    }
}

/// T6 — live run log (SSE). Every `job_events` row is re-broadcast
/// here the moment the runner emits it. Frames for OTHER projects are
/// dropped server-side; a lagging client just misses a frame and
/// recovers through `project_events` on reconnect.
async fn project_events_live(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<Uuid>,
) -> Response {
    use axum::response::sse::{Event, KeepAlive, Sse};
    use futures_util::StreamExt;
    use tokio_stream::wrappers::BroadcastStream;

    let rx = state.run_bus.subscribe();
    let pid = project_id;
    let hello_time = chrono::Utc::now().to_rfc3339();

    type SseFrame = Result<Event, std::convert::Infallible>;
    let stream: futures_util::stream::BoxStream<'static, SseFrame> =
        futures_util::stream::once(std::future::ready(Ok::<Event, std::convert::Infallible>(
            Event::default()
                .event("hello")
                .data(
                    serde_json::to_string(&serde_json::json!({
                        "serverTime": hello_time,
                        "project_id": pid.to_string(),
                    }))
                    .unwrap_or_default(),
                ),
        )))
        .chain(
            BroadcastStream::new(rx).filter_map(move |r| {
                let fut = async move {
                    match r {
                        Ok(msg) if msg.project_id == pid => Some(Ok::<Event, std::convert::Infallible>(
                            Event::default()
                                .event("run")
                                .data(serde_json::to_string(&msg).unwrap_or_default()),
                        )),
                        // Other projects / lag frames: drop. The DB tail
                        // endpoint is the recovery path.
                        _ => None,
                    }
                };
                fut
            }),
        )
        .boxed();

    Sse::new(stream)
        .keep_alive(
            KeepAlive::default().interval(std::time::Duration::from_secs(15)),
        )
        .into_response()
}

// ─────────────────────────────────────────────────────────────
// Phase 1 — Auto-pick: the system fills a project with working
// engines on its own.  The user's job ends at URL + keyword +
// link count.
//
// Over-automation guardrail:
//   * static scoring over the whole catalog is FREE (no network —
//     no liveness probing: dofollow + friction only);
//   * live dry-runs run in parallel over the SAMPLE — `per_family`
//     (default all-in-family, ceil 1000) and `max_total` (default
//     5000, ceil 5000).  The old 12 / 60 ceilings are gone: a pick
//     can span the whole posting catalog, because the run pool must
//     be as wide as possible ("run as many as possible");
//   * failed dry runs are AUDITED, not removed — by default they stay
//     in the pool (`keep_unverified: true`); set keep_unverified:false
//     for the stricter "verified only" mode (they are disabled, visible).
// ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct AutoPickBody {
    /// How many engines to sample per family for live verification
    /// (default 64, clamped 1..=1000 — high enough to take every posting
    /// engine in a family).
    #[serde(default)]
    per_family: Option<usize>,
    /// Hard cap on the total number of engines sampled (default 1000,
    /// clamped 1..=5000 — high enough that the whole catalog fits).
    #[serde(default)]
    max_total: Option<usize>,
    /// Live-test each sampled engine (default true; false = add without
    /// testing).
    #[serde(default)]
    test: bool,
    /// Keep engines that FAIL the live test in the run pool (default true —
    /// "run as many as possible"). Set false to disable failed engines and
    /// keep only the verified ones (previous behaviour).
    #[serde(default = "default_true")]
    keep_unverified: bool,
}

/// Phase 1 — "Auto-pick" endpoint: sample the whole engine catalog,
/// rank it statically per family, dry-run the (bounded) sample, and
/// leave the project holding only verified engines, ready for
/// `run-auto`.  Auditable: every consideration is reported back.
async fn auto_pick(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<Uuid>,
    Json(body): Json<AutoPickBody>,
) -> Response {
    match auto_pick_pool(project_id, &state, body).await {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => error_response(e),
    }
}

async fn auto_pick_pool(
    project_id: Uuid,
    state: &Arc<AppState>,
    body: AutoPickBody,
) -> anyhow::Result<serde_json::Value> {
    let repo = Repo::new(state.pool.clone());

    // 0) Sanity: a keyword (dry-runs need anchor text) and the project.
    if repo.project_by_id(project_id).await?.is_none() {
        anyhow::bail!("project not found");
    }
    let keywords = repo.list_keywords(project_id).await?;
    if keywords.is_empty() {
        anyhow::bail!("no keywords on this project — add at least one link keyword first");
    }
    let kw = keywords.first().map(|k| k.id);
    let have: std::collections::BTreeSet<String> = repo
        .list_engines_all(project_id)
        .await?
        .into_iter()
        .map(|e| e.file_path)
        .collect();

    // 1) Whole catalog, posting families only.  Free: local INI parse +
    //    stored metadata, no network (liveness is left to the dry run).
    let cat = Registry::new(&state.pool).list(None, None, 10000, 0).await?;
    let mut rows: Vec<ser_db::CatalogEngine> = Vec::new();
    let mut cands: Vec<engine::CandidateEngine> = Vec::new();
    for ce in cat {
        if have.contains(&ce.file_path) {
            continue; // already on the project — never re-considered
        }
        let et = std::str::FromStr::from_str(&ce.engine_type)
            .unwrap_or(engine::EngineType::Unknown);
        if engine::Family::for_type(&et).is_none() {
            continue; // email / video / unknown — not a posting family
        }
        let (field_count, just_download) =
            match engine::parse_engine_file(resolve_engine_path(&ce.file_path).as_path()) {
                Ok(eng) => (
                    eng.steps.iter().map(|s| s.fields.len()).sum::<usize>(),
                    eng.fixed_url.is_some(),
                ),
                Err(_) => (0_usize, false),
            };
        cands.push(engine::CandidateEngine {
            engine_name: ce.engine_name.clone(),
            engine_type: et,
            dofollow: ce.dofollow,
            field_count,
            just_download,
            is_enabled: true,
            is_live: None, // liveness decided by the dry-run, not a probe
        });
        rows.push(ce);
    }
    if cands.is_empty() {
        anyhow::bail!(
            "nothing new to consider — the catalog is empty or every engine is already on the project"
        );
    }

    // 2) Bounded sample: top-N per family, then an absolute cap.  Both
    //    ceilings and DEFAULTS are high on purpose (per-family up to 1000,
    //    total up to 5000) so a pick can span the whole posting catalog —
    //    omitting both picks essentially every posting engine.
    let per = body.per_family.unwrap_or(1000).clamp(1, 1000);
    let max_total = body.max_total.unwrap_or(5000).clamp(1, 5000);
    let sel = engine::best_per_family(&cands, per);
    let mut picked: Vec<usize> = Vec::new();
    for (_fam, idxs) in sel {
        picked.extend(idxs);
    }
    picked.truncate(max_total);

    // 3) Add the sampled engines to the project.
    let mut added: Vec<ser_db::Engine> = Vec::new();
    for i in &picked {
        let ce = &rows[*i];
        let mut row = repo
            .upsert_engine(
                project_id,
                &ce.engine_name,
                &ce.engine_type,
                &ce.url,
                &ce.file_path,
                ce.step_count,
            )
            .await?;
        // Honor a GSA ";down" engine (INI `enabled=0`): keep the row in the
        // project for audit, but flip it out of the dispatch pool.
        if !ce.file_path.trim().is_empty() && !engine::ini_is_enabled(&resolve_engine_path(&ce.file_path)) {
            row = repo.set_engine_enabled(row.id, false).await?;
        }
        added.push(row);
    }

    // 4) Banner into the live feed — the console shows the decision.
    {
        use runner::EventSink as _;
        let sink = crate::runfeed::RunEventSink::new(
            project_id,
            state.pool.clone(),
            state.run_bus.sender(),
        );
        sink.record(runner::RunEvent {
            job_id: None,
            engine: None,
            level: "auto".into(),
            message: format!(
                "AUTO-PICK — considered {} catalog engines, sampled {} (per-family cap {}, total cap {}) — {}",
                cands.len(),
                added.len(),
                per,
                max_total,
                if body.test {
                    "live-testing each, only verified ones stay enabled"
                } else {
                    "no live test (added as-is)"
                },
            ),
        });
    }

    // 5) Live dry runs (if requested): success → verified.  Failure →
    //    *stays in the pool* when `keep_unverified` (the default — the
    //    intent is "run as many as possible"); it is only disabled when the
    //    caller explicitly asks for the stricter verified-only mode.  Runs
    //    in parallel (12 in flight) so hundreds finish in reasonable time.
    let mut verified: Vec<serde_json::Value> = Vec::new();
    let mut failed: Vec<serde_json::Value> = Vec::new();

    if !body.test {
        for row in &added {
            let fam = engine::Family::for_type(
                &std::str::FromStr::from_str(&row.engine_type)
                    .unwrap_or(engine::EngineType::Unknown),
            )
            .map(|f| f.as_str().to_string())
            .unwrap_or_else(|| row.engine_type.clone());
            verified.push(serde_json::json!({
                "engine": row.engine_name,
                "family": fam,
                "url": row.url,
                "verified": null
            }));
        }
    } else {
        use futures_util::StreamExt as _;
        let keep_unverified = body.keep_unverified;
        let repo_ref = &repo; // capture the reference (Copy) into each task
        let mut stream = futures_util::stream::iter(added.clone())
            .map(|row| async move {
                let repo_t = repo_ref.clone();
                let fam = engine::Family::for_type(
                    &std::str::FromStr::from_str(&row.engine_type)
                        .unwrap_or(engine::EngineType::Unknown),
                )
                .map(|f| f.as_str().to_string())
                .unwrap_or_else(|| row.engine_type.clone());
                let outcome = test_engine_pool(
                    project_id,
                    TestEngineBody { engine_id: row.id, keyword_id: kw },
                    state,
                )
                .await;
                match outcome {
                    Ok(v) => {
                        let successful = v
                            .pointer("/report/successful")
                            .and_then(|x| x.as_u64())
                            .unwrap_or(0);
                        if successful > 0 {
                            (
                                true,
                                serde_json::json!({
                                    "engine": row.engine_name, "family": fam, "url": row.url,
                                    "verified": true
                                }),
                                None,
                            )
                        } else {
                            let reason = v
                                .get("report")
                                .and_then(|r| r.get("errors"))
                                .and_then(|e| e.as_array())
                                .map(|arr| {
                                    arr.iter()
                                        .filter_map(|x| x.as_str())
                                        .take(2)
                                        .collect::<Vec<_>>()
                                        .join("; ")
                                })
                                .or_else(|| {
                                    v.get("error").and_then(|e| e.as_str()).map(|s| s.to_string())
                                })
                                .unwrap_or_else(|| "dry run created no link".into());
                            if !keep_unverified {
                                let _ = repo_t.set_engine_enabled(row.id, false).await;
                            }
                            (
                                false,
                                serde_json::json!({
                                    "engine": row.engine_name, "family": fam, "url": row.url,
                                    "verified": false
                                }),
                                Some(reason),
                            )
                        }
                    }
                    Err(e) => {
                        if !keep_unverified {
                            let _ = repo_t.set_engine_enabled(row.id, false).await;
                        }
                        (
                            false,
                            serde_json::json!({
                                "engine": row.engine_name, "family": fam, "url": row.url,
                                "verified": false
                            }),
                            Some(e.to_string()),
                        )
                    }
                }
            })
            .buffer_unordered(12);
        while let Some((ok, mut value, reason)) = stream.next().await {
            if let Some(r) = reason {
                value["reason"] = serde_json::json!(r);
            }
            if ok {
                verified.push(value);
            } else {
                failed.push(value);
            }
        }
    }

    Ok(serde_json::json!({
        "considered": cands.len(),
        "sampled": added.len(),
        "tested": if body.test { added.len() } else { 0 },
        "verified": verified,
        "failed": failed,
        "note": if body.test {
            if body.keep_unverified {
                "every picked engine STAYS in the pool — verified ones above, rejected ones audited (re-enable/disabled only if asked) — hit run-auto to create links"
            } else {
                "only engines that passed the dry run stay enabled — hit run-auto to create links"
            }
        } else {
            "live verification off — every picked engine stays in the pool — hit run-auto to create links"
        },
    }))
}

// ─────────────────────────────────────────────────────────────
// T6 — Test-all engines + captcha solver health.
// ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct TestAllBody {
    /// Max engines to test (default 12). Each test is a full
    /// end-to-end dry run, so this is deliberately bounded.
    #[serde(default)]
    limit: Option<usize>,
}

/// T6 — "test all those engines" (user request): dry-run the project's
/// engines one by one (best-health first — previous test runs already
/// demote broken ones) and report which ones actually create a link.
/// Each attempt goes through `test_engine_pool` so results are
/// `results`-free, and every attempt is visible in the live feed.
async fn test_all_engines(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<Uuid>,
    Json(body): Json<TestAllBody>,
) -> Response {
    let max = body.limit.unwrap_or(12).clamp(1, 40);
    let repo = Repo::new(state.pool.clone());
    let engines = match repo.list_engines_all(project_id).await {
        Ok(rows) => rows,
        Err(e) => return error_response(e),
    };
    let mut engines: Vec<_> = engines
        .into_iter()
        .filter(|e| e.enabled && !e.file_path.trim().is_empty())
        .collect();
    engines.sort_by(|a, b| {
        b.successes.cmp(&a.successes)
            .then(b.last_success_at.cmp(&a.last_success_at))
            .then(a.captcha_failures.cmp(&b.captcha_failures))
            .then(a.attempts.cmp(&b.attempts))
    });
    let engines = engines.into_iter().take(max).collect::<Vec<_>>();
    if engines.is_empty() {
        return (
            StatusCode::OK,
            Json(serde_json::json!({ "tested": 0, "ok": 0, "results": [] })),
        )
            .into_response();
    }

    // One keyword for the whole batch (the first one — a test needs
    // SOME anchor text, but it is not the point).
    let kw = repo
        .list_keywords(project_id)
        .await
        .ok()
        .and_then(|k| k.first().map(|k| k.id));

    let mut results: Vec<serde_json::Value> = Vec::with_capacity(engines.len());
    let mut ok = 0usize;
    for e in &engines {
        match test_engine_pool(
            project_id,
            TestEngineBody {
                engine_id: e.id,
                keyword_id: kw,
            },
            &state,
        )
        .await
        {
            Ok(mut v) => {
                let successful = v
                    .pointer("/report/successful")
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0);
                if successful > 0 {
                    ok += 1;
                }
                if let Some(o) = v.as_object_mut() {
                    o.insert("engine".to_string(), serde_json::json!(e.engine_name));
                    o.insert("ok".to_string(), serde_json::json!(successful > 0));
                }
                results.push(v);
            }
            Err(err) => results.push(serde_json::json!({
                "engine": e.engine_name,
                "ok": false,
                "successful": 0,
                "error": err.to_string(),
            })),
        }
    }
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "tested": results.len(),
            "ok": ok,
            "results": results,
        })),
    )
        .into_response()
}

/// T6 — one probe of a captcha provider account: hit its key-info
/// endpoint with the configured API key and report ok / balance /
/// the provider's own error text.
async fn captcha_probe(
    name: &str,
    url: String,
) -> serde_json::Value {
    use engine::Driver as _;
    let d = engine::HttpDriver::new();
    let r = d.navigate(url).await;
    if !r.ok {
        return serde_json::json!({
            "target": name,
            "ok": false,
            "detail": r.error.clone().unwrap_or_else(|| "request failed".into()),
        });
    }
    let body = r.html.trim().to_string();
    let (ok, detail) = if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
        match v.get("balance") {
            Some(b) => (true, format!("balance = {b}")),
            None if v.get("success").and_then(|s| s.as_bool()) == Some(true) => {
                (true, "valid key (no balance field)".to_string())
            }
            _ => (false, body.chars().take(160).collect()),
        }
    } else if body.chars().next().map_or(false, |c| c.is_ascii_digit()) {
        (true, format!("balance = {body}"))
    } else {
        (false, body.chars().take(160).collect())
    };
    serde_json::json!({ "target": name, "ok": ok, "detail": detail })
}

/// F#21 `POST /api/captcha/test` — probe the paid captcha key(s) the global
/// settings would use and report exactly which solvers are armed, without
/// spending a run slot on an engine that will hit a captcha wall.
/// `provider=auto` walks the F#29 free-first chain and reports the
/// first account that accepts the key.
async fn captcha_test(State(state): State<Arc<AppState>>) -> Response {
    let repo = Repo::new(state.pool.clone());
    let globals = repo.global_settings().await.unwrap_or_default();
    let provider = globals
        .captcha_provider
        .clone()
        .unwrap_or_else(|| "auto".to_string());
    let key = globals.captcha_api_key.clone().unwrap_or_default();

    if provider == "local" {
        return (
            StatusCode::OK,
            Json(serde_json::json!({
                "provider": "local",
                "armed": true,
                "detail": "local captcha solver active (offline, covers in-page captchas without an account)",
                "probes": [],
            })),
        )
            .into_response();
    }
    if key.trim().is_empty() {
        return (
            StatusCode::OK,
            Json(serde_json::json!({
                "provider": provider,
                "armed": false,
                "detail": format!("no captcha API key configured (provider = {provider}) — engines behind a captcha wall will fail until one is set"),
                "probes": [],
            })),
        )
            .into_response();
    }

    let k = key.trim();
    let mut candidates: Vec<(&str, String)> = Vec::new();
    match provider.as_str() {
        "2captcha" => candidates.push((
            "2captcha",
            format!("https://api.2captcha.com/res.php?action=getbalance&key={k}"),
        )),
        "capmonster" => candidates.push((
            "capmonster",
            format!("https://capmonster.cloud/getbalance?key={k}"),
        )),
        "anticaptcha" => candidates.push((
            "anticaptcha",
            format!("https://api.anticaptcha.com/antipal/checkkey.php?key={k}"),
        )),
        _ => {
            // `auto` — the free-first chain, probed in solve order.
            candidates.push((
                "2captcha",
                format!("https://api.2captcha.com/res.php?action=getbalance&key={k}"),
            ));
            candidates.push((
                "anticaptcha",
                format!("https://api.anticaptcha.com/antipal/checkkey.php?key={k}"),
            ));
            candidates.push((
                "capmonster",
                format!("https://capmonster.cloud/getbalance?key={k}"),
            ));
        }
    }

    let mut probes: Vec<serde_json::Value> = Vec::new();
    let mut armed = false;
    for (name, url) in candidates {
        if armed {
            break;
        }
        let probe = captcha_probe(name, url).await;
        if probe.get("ok").and_then(|b| b.as_bool()) == Some(true) {
            armed = true;
        }
        probes.push(probe);
    }
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "provider": provider,
            "armed": armed,
            "detail": if armed {
                "at least one solver account accepted the key"
            } else {
                "no solver account accepted this key — engine captcha walls will block submissions"
            },
            "probes": probes,
        })),
    )
        .into_response()
}


// ─────────────────────────────────────────────────────────────
// F#26 — Polish: Notes / Color / Pause-after-N + Test dry-run.
// ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct TestEngineBody {
    engine_id: Uuid,
    /// Optional: which keyword to submit for the dry run. `None` = the
    /// project's first keyword (if any).
    #[serde(default)]
    keyword_id: Option<Uuid>,
}

/// F#26 — Test Button (GSA "Test" per engine): drive ONE engine against
/// the live target end-to-end (spec → plan → executor → captcha) WITHOUT
/// persisting any `results` row, job status, or auto-export CSV append.
///
/// The API creates a transient job (the runner needs a real row to read
/// its file path + keyword id), runs it in `dry_run` mode via
/// `WorkerPool::run_one_job`, then marks it `cancelled` so the pool does
/// NOT pick it up on the next run. Returns the execution report as JSON
/// so the UI can show `{success, error?, steps, captcha_failed, ...}`.
///
/// This is the GSA "Test" button — it tells you *whether the engine
/// works* before you burn real run slots on it.
async fn test_engine(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<Uuid>,
    Json(body): Json<TestEngineBody>,
) -> Response {
    match test_engine_pool(project_id, body, &state).await {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => error_response(e),
    }
}

async fn test_engine_pool(
    project_id: Uuid,
    body: TestEngineBody,
    state: &Arc<AppState>,
) -> anyhow::Result<serde_json::Value> {
    let repo = Repo::new(state.pool.clone());

    // 1) Create the transient job. If there's no engine for this project
    //    yet, `create_job` still works — the runner errors out on the
    //    "spec missing" path and the outcome reflects that.
    let job = repo.create_job(project_id, Some(body.engine_id), body.keyword_id).await?;

    // 2) Build a context mirroring `run_project_pool`, with `dry_run=true`
    //    and `auto_export=false` (a test should not append to the CSV).
    let (email_address, email_password) = repo.email_mailbox().await.unwrap_or_default();
    let global_settings = repo.global_settings().await.unwrap_or_default();
    let mut cfg = runner::RunnerConfig::default();
    cfg.concurrent_workers = 1; // one worker — we're testing one engine
    cfg.per_job_timeout = std::time::Duration::from_secs(
        global_settings.max_submit_seconds.unwrap_or(90).clamp(1, 3600) as u64,
    );
    cfg.leader_idle_timeout = std::time::Duration::from_millis(800);

    let project_row = repo.project_by_id(project_id).await.ok().flatten();
    let use_proxies = repo
        .project_use_proxies(project_id)
        .await
        .ok()
        .flatten()
        .unwrap_or(false);
    let proxies: Vec<engine::proxy::ProxySpec> = if use_proxies {
        repo.list_enabled_proxies()
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
        settings: std::sync::Arc::new(global_settings.clamped_ref()),
        project_id,
        project_url: project_row.as_ref().map(|p| p.url.clone()).unwrap_or_default(),
        // F#30 — the project's own Article body (GSA "Article content");
        // empty → the runner auto-generates a short article from the
        // keyword + target URL.
        article_text: project_row
            .as_ref()
            .map(|p| p.article_text.clone())
            .unwrap_or_default(),
        // F#31 — the project's own Comment body (GSA "Comment text");
        // empty → the runner auto-generates a comment from the comments.dat pool.
        comment_text: project_row
            .as_ref()
            .map(|p| p.comment_text.clone())
            .unwrap_or_default(),
        anchor: project_row
            .as_ref()
            .and_then(|p| p.anchor_mix.as_deref())
            .and_then(|s| serde_json::from_str::<engine::anchor::PoolConfig>(s).ok())
            .filter(|c| !c.is_empty()),
        url_options: engine::anchor::UrlOptions {
            use_random_url: project_row.as_ref().map(|p| p.use_random_url).unwrap_or(false),
            url_variations: u32::try_from(project_row.as_ref().map(|p| p.url_variations).unwrap_or(0))
                .unwrap_or(0),
            root_url: u32::try_from(project_row.as_ref().map(|p| p.root_url).unwrap_or(0))
                .unwrap_or(0),
        },
        linkwheel_project_ids: project_row.as_ref().map(|p| p.linkwheel.clone()).unwrap_or_default(),
        // T5 — searched site list for discovery engines (test dry-runs too).
        project_sites: {
            let pid = project_id;
            Repo::new(state.pool.clone())
                .list_project_sites(pid)
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|r| r.url)
                .collect::<Vec<_>>()
        },
        // F#26 — test run should NOT pollute the export feed, regardless
        // of the project's setting.
        auto_export: false,
        // F#26 — THE critical flag: no job status / result row / CSV
        // append — just drive the engine and report back.
        dry_run: true,
        // T6 — live run feed (the test console shows what's happening now).
        events: Some(crate::runfeed::RunEventSink::new(
            project_id,
            state.pool.clone(),
            state.run_bus.sender(),
        )),

        ..Default::default()
    };

    let root = std::env::var("GSA_INSTALL_ROOT")
        .unwrap_or_else(|_| r"C:\Users\kingsly\GSA_SER_portable".into());
    let roots = vec![std::path::PathBuf::from(root)];
    let mut drivers = runner::DriverRegistry::new()
        .with_http(engine::HttpDriver::new())
        .with_email(engine::HttpDriver::new());

    // F#8b — CDP browser pool (GSA's "browser-submit" half): JS-heavy engines
    // (set unknown variable / page must have / recaptcha / …) route to
    // `DriverKind::Browser`.  Connect a `WebDriver` to a headless Edge on
    // `SER_EDGE_CDP` (default http://127.0.0.1:9222), spawning Edge if the
    // endpoint isn't up.  `SER_EDGE_CDP=0` disables the pool → the previous
    // clean "driver not registered for kind Browser" behaviour.
    //
    // A bound WebDriver owns ONE live page target, so with the browser
    // driver registered the run serializes (`concurrent_workers = 1`):
    // interleaving two multi-step engine runs on one page cross-talks.
    {
        let base = std::env::var("SER_EDGE_CDP")
            .unwrap_or_else(|_| "http://127.0.0.1:9222".to_string());
        if base != "0" {
            match browser_ws_endpoint(&base).await {
                Some(ws) => match engine::WebDriver::connect(ws).await {
                    Ok(wd) => {
                        drivers = drivers.with_browser(wd);
                        cfg.concurrent_workers = 1;
                    }
                    Err(e) => eprintln!("[browser] CDP ws connect failed: {e}"),
                },
                None => eprintln!("[browser] no CDP endpoint at {base} ({0}s probe) — browser-routed engines will report a clean failure", 15),
            }
        }
    }

    let pool = runner::pool_for_disk(state.pool.clone(), cfg, roots, drivers).with_context(ctx);
    let outcome = pool.run_one_job(job.id).await;

    // 3) Ensure the transient job is in a terminal state so a subsequent
    //    `run` doesn't pick it up. In dry_run the runner's own
    //    `finish_job` path already set a terminal status — this is a
    //    belt-and-suspenders.
    let _ = repo
        .finish_job(job.id, ser_db::JobStatus::Cancelled, Some("test-engine dry-run (F#26)"))
        .await;

    // 4) Build the response: either the outcome (success path) or an
    //    error report (failure path).
    let (outcome_json, err_json) = match outcome {
        Ok(o) => (
            serde_json::json!({
                "successful": o.report.successful,
                "content_success": o.report.content_success,
                "failed": o.report.failed,
                "retried": o.report.retried,
                "captcha_failed": o.report.captcha_failed,
                "errors": o.report.errors,
                "steps": o.report.steps,
                "submitted_url": o.report.submitted_url,
                "submitted_url_source": o.report.submitted_url_source,
            }),
            serde_json::Value::Null,
        ),
        Err(e) => (serde_json::json!({}), serde_json::json!({ "error": e.to_string() })),
    };

    Ok(serde_json::json!({
        "project_id": project_id.to_string(),
        "engine_id": body.engine_id.to_string(),
        "job_id": job.id.to_string(),
        "report": outcome_json,
        "error": err_json,
    }))
}

#[derive(Deserialize)]
struct PauseAfterBody {
    pause_after: i32,
}

/// F#26 — Pause after N: set the project's pause-after ceiling. `0`
/// means "run to completion" (the default). `None` in the body = "don't
/// change this field" (for the `set_project_polish` path).
async fn set_project_pause_after(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<Uuid>,
    Json(body): Json<PauseAfterBody>,
) -> Response {
    match Repo::new(state.pool.clone())
        .set_project_polish(project_id, None, None, Some(body.pause_after.max(0)))
        .await
    {
        Ok(p) => Json(p).into_response(),
        Err(e) => error_response(e),
    }
}

/// F#26 — Polish: save per-project notes, color tag, and/or the
/// pause-after-N ceiling. Any field omitted from the body is left
/// untouched (server-side COALESCE semantics in `set_project_polish`).
#[derive(Deserialize)]
struct SetPolishBody {
    /// Free-form notes for the project (GSA "New Project Data" notes tab).
    notes: Option<String>,
    /// Color tag, e.g. `#E11D48` (GSA project color for quick scanning).
    color: Option<String>,
    /// `0` = run to completion (default); `n` = pause after n submissions.
    pause_after: Option<i32>,
}

async fn set_project_polish(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<Uuid>,
    Json(body): Json<SetPolishBody>,
) -> Response {
    match Repo::new(state.pool.clone())
        .set_project_polish(project_id, body.notes.clone(), body.color.clone(), body.pause_after)
        .await
    {
        Ok(p) => Json(p).into_response(),
        Err(e) => error_response(e),
    }
}

/// F#26 — Resume: clear the project's `paused_at` flag. The run is
/// resumable by calling `POST /api/projects/{id}/run` again — the pool
/// reads pending jobs and picks up where it left off.
async fn resume_project(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<Uuid>,
) -> Response {
    match Repo::new(state.pool.clone()).set_project_paused(project_id, false).await {
        Ok(p) => Json(p).into_response(),
        Err(e) => error_response(e),
    }
}

// ─────────────────────────────────────────────────────────────
// F#17 — per-project scheduled runs (schedule CRUD + run-now).
// Handlers mirror the existing CRUD style; the scheduler loop lives
// in `api/src/scheduler.rs` and fires due schedules via
// `Repo::claim_schedule` + `run_project_pool`.
// ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct CreateSchedule {
    pub name: String,
    /// "once" | "recurring".
    #[serde(default = "default_mode")]
    pub mode: String,
    /// Required — the first fire time (naive UTC, "YYYY-MM-DD HH:MM:SS"
    /// or ISO-8601).  Recurring: base of the cadence grid.
    pub start_at: String,
    /// Optional hard stop at this datetime (naive UTC).
    #[serde(default)]
    pub stop_at: Option<String>,
    /// Required for `recurring`, 1..=525600 minutes. Ignored for `once`.
    #[serde(default)]
    pub repeat_every_minutes: Option<i32>,
    /// Optional ISO weekday filter (1 = Mon … 7 = Sun).
    #[serde(default)]
    pub repeat_weekdays: Option<Vec<i32>>,
    /// Optional UTC wall-clock window, "HH:MM:SS". A wrap (start > end) is
    /// allowed and means `t >= window_start || t <= window_end`.
    #[serde(default)]
    pub window_start: Option<String>,
    #[serde(default)]
    pub window_end: Option<String>,
    /// Optional total-fire cap (positive). `None` / `null` = no cap.
    #[serde(default)]
    pub max_runs: Option<i64>,
    /// Optional enabled flag (defaults to true on create).
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_mode() -> String {
    "recurring".into()
}
fn default_true() -> bool {
    true
}

/// Convert the API input shape → the pure engine spec. Validation is
/// enforced here (returns 422) so a row can never be written with an
/// impossible combination — the engine's `validate()` does the same
/// checks but we surface the errors directly at the boundary.
fn spec_from_create(
    body: &CreateSchedule,
) -> Result<(engine::schedule::ScheduleSpec, chrono::NaiveDateTime, Option<chrono::NaiveDateTime>, Option<chrono::NaiveTime>, Option<chrono::NaiveTime>), String> {
    use engine::schedule::{ScheduleMode, ScheduleSpec, WeekdayMask};

    let mode = match body.mode.to_ascii_lowercase().as_str() {
        "once" => ScheduleMode::Once,
        "recurring" => ScheduleMode::Recurring,
        other => {
            return Err(format!("mode must be 'once' or 'recurring' (got '{}')", other));
        }
    };
    let start_at = parse_dt(&body.start_at)?;
    let stop_at = body.stop_at.as_ref().map(|s| parse_dt(s)).transpose()?;
    let window_start = body.window_start.as_ref().map(|s| parse_time(s)).transpose()?;
    let window_end = body.window_end.as_ref().map(|s| parse_time(s)).transpose()?;
    let repeat_weekdays = body
        .repeat_weekdays
        .as_ref()
        .filter(|v| !v.is_empty())
        .map(|v| WeekdayMask::from(v.iter().map(|&x| x as i64).collect::<Vec<i64>>()));

    let spec = ScheduleSpec {
        name: body.name.clone(),
        mode,
        start_at,
        stop_at,
        repeat_every_minutes: body.repeat_every_minutes.map(|x| x.max(1) as u32),
        repeat_weekdays,
        window_start,
        window_end,
        max_runs: body.max_runs,
        run_count: 0,
    };
    Ok((spec, start_at, stop_at, window_start, window_end))
}

fn parse_dt(s: &str) -> Result<chrono::NaiveDateTime, String> {
    // Accept ISO-8601 and "YYYY-MM-DD HH:MM:SS".
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S")
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S"))
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f"))
        .map_err(|_| format!("start_at / stop_at format 'YYYY-MM-DD HH:MM:SS' — got '{}'", s))
}
fn parse_time(s: &str) -> Result<chrono::NaiveTime, String> {
    chrono::NaiveTime::parse_from_str(s, "%H:%M:%S").map_err(|_| format!("HH:MM:SS — got '{}'", s))
        .or_else(|_| chrono::NaiveTime::parse_from_str(s, "%H:%M").map_err(|_| format!("HH:MM:SS — got '{}'", s)))
}



async fn create_schedule_handler(
    Path(project_id): Path<Uuid>,
    State(state): State<Arc<AppState>>,
    body: Json<CreateSchedule>,
) -> Response {
    let (spec, start_at, stop_at, ws, we) = match spec_from_create(&body.0) {
        Ok(v) => v,
        Err(msg) => {
            return (StatusCode::UNPROCESSABLE_ENTITY, Json(serde_json::json!({ "error": msg }))).into_response();
        }
    };
    if let Err(e) = spec.validate() {
        return (StatusCode::UNPROCESSABLE_ENTITY, Json(serde_json::json!({ "error": e.to_string() }))).into_response();
    }
    // First next_run_at: use the pure core's `next_run` at "now" — this
    // means a freshly created schedule fires on the very next eligible
    // slot after creation, regardless of the anchor time.
    let now = chrono::Utc::now().naive_utc();
    let next_at = spec.next_run(now);
    let repo = Repo::new(state.pool.clone());
    match repo
        .create_schedule(
            project_id,
            &body.name,
            &body.mode.to_ascii_lowercase(),
            chrono::DateTime::from_naive_utc_and_offset(start_at, chrono::Utc),
            stop_at.map(|d| chrono::DateTime::from_naive_utc_and_offset(d, chrono::Utc)),
            body.repeat_every_minutes,
            body.repeat_weekdays.clone(),
            ws,
            we,
            body.max_runs,
            body.enabled,
        )
        .await
    {
        Ok(s) => {
            let _ = repo.set_next_run_at(
                s.id,
                next_at.map(|n| chrono::DateTime::from_naive_utc_and_offset(n, chrono::Utc)),
            ).await;
            (
                StatusCode::CREATED,
                Json(serde_json::json!({ "schedule": s, "next_run_at": next_at })),
            )
                .into_response()
        }
        Err(e) => error_response(e),
    }
}

async fn list_schedules_handler(
    Path(project_id): Path<Uuid>,
    State(state): State<Arc<AppState>>,
) -> Response {
    match Repo::new(state.pool.clone()).list_schedules(Some(project_id)).await {
        Ok(list) => Json(list).into_response(),
        Err(e) => error_response(e),
    }
}

async fn list_schedules_all(State(state): State<Arc<AppState>>) -> Response {
    match Repo::new(state.pool.clone()).list_schedules(None).await {
        Ok(list) => Json(list).into_response(),
        Err(e) => error_response(e),
    }
}

async fn get_schedule(Path(id): Path<Uuid>, State(state): State<Arc<AppState>>) -> Response {
    match Repo::new(state.pool.clone()).schedule_by_id(id).await {
        Ok(Some(s)) => Json(s).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "schedule not found" }))).into_response(),
        Err(e) => error_response(e),
    }
}

#[derive(Deserialize)]
pub struct SetScheduleEnabled {
    enabled: bool,
}
async fn set_schedule_enabled(
    Path(id): Path<Uuid>,
    State(state): State<Arc<AppState>>,
    body: Json<SetScheduleEnabled>,
) -> Response {
    match Repo::new(state.pool.clone()).set_schedule_enabled(id, body.enabled).await {
        Ok(s) => Json(s).into_response(),
        Err(e) => error_response(e),
    }
}

async fn delete_schedule(Path(id): Path<Uuid>, State(state): State<Arc<AppState>>) -> Response {
    match Repo::new(state.pool.clone()).delete_schedule(id).await {
        Ok(true) => (StatusCode::NO_CONTENT, "").into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "schedule not found" }))).into_response(),
        Err(e) => error_response(e),
    }
}

/// `POST /api/schedules/{id}/run-now` — manually fire a schedule right
/// now (same path the scheduler loop takes: claim + `run_project_pool`).
/// If the schedule is already consumed (once / cap hit) return `{status:
/// "skipped"}` rather than an error, so a UI "Run now" button never
/// 5xx's on a finished schedule.
async fn run_now_schedule(
    Path(id): Path<Uuid>,
    State(state): State<Arc<AppState>>,
) -> Response {
    let repo = Repo::new(state.pool.clone());
    let sched = match repo.schedule_by_id(id).await {
        Ok(Some(s)) => s,
        Ok(None) => return (StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "schedule not found" }))).into_response(),
        Err(e) => return error_response(e),
    };
    let now = chrono::Utc::now().naive_utc();
    let spec = spec_from_row(&sched);
    let next = spec.next_run(now);
    let now_dt = chrono::Utc::now(); // deadline used by claim_schedule
    let next_after = next.map(|n| chrono::DateTime::from_naive_utc_and_offset(n, chrono::Utc)); // None = consumed
    let claimed = match repo.claim_schedule(id, now_dt, next_after).await {
        Ok(Some(_)) => true,
        Ok(None) => false,
        Err(e) => return error_response(e),
    };
    if !claimed {
        return Json(serde_json::json!({ "status": "skipped", "reason": "next_run_at is not due / schedule is consumed" })).into_response();
    }
    let summary = match run_project_pool(sched.project_id, &state, None, None, None).await {
        Ok(v) => v,
        Err(e) => return error_response(e),
    };
    Json(summary).into_response()
}

/// Rebuild a `ScheduleSpec` from the DB row — the inverse of
/// `spec_from_create`. Used by the scheduler loop, `run-now`, and
/// any UI that wants the "next fire time".
pub fn spec_from_row(r: &ser_db::Schedule) -> engine::schedule::ScheduleSpec {
    use engine::schedule::{ScheduleMode, WeekdayMask};
    let mode = match r.mode.as_str() {
        "once" => ScheduleMode::Once,
        _ => ScheduleMode::Recurring,
    };
    let start_at = r.start_at.naive_utc();
    let stop_at = r.stop_at.map(|d| d.naive_utc());
    let repeat_weekdays = r.repeat_weekdays.as_ref().filter(|v| !v.is_empty())
        .map(|v| WeekdayMask::from(v.iter().map(|&x| x as i64).collect::<Vec<i64>>()));
    engine::schedule::ScheduleSpec {
        name: r.name.clone(),
        mode,
        start_at,
        stop_at,
        repeat_every_minutes: r.repeat_every_minutes.map(|x| (x as i64).max(1) as u32),
        repeat_weekdays,
        window_start: r.window_start,
        window_end: r.window_end,
        max_runs: r.max_runs,
        run_count: r.run_count as i64,
    }
}
// ---------------------------------------------------------------------------
// F#23 — Site finding (backlink candidate discovery) — the GSA `se.dat` 2,525-
// site engine catalog.  `GET /api/sites` browses the catalog; `POST
// /api/sites/find` runs the finder against it using a real `HttpDriver` and
// returns `FoundSite` candidates deduped across sources.
// ---------------------------------------------------------------------------

/// GET /api/sites?limit=&offset=&q= — browse the `se.dat` site catalog.
async fn site_catalog(Query(q): Query<CatalogQuery>) -> Response {
    let sites = match engine::site::load_site_catalog() {
        Ok(sites) => sites,
        Err(e) => return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e })),
        ).into_response(),
    };
    let total = sites.len() as i64;
    let limit = q.limit.unwrap_or(100).max(1).min(1000);
    let offset = q.offset.unwrap_or(0).max(0);
    let needle = q.q.map(|s| s.to_ascii_lowercase()).filter(|s| !s.is_empty());
    let page: Vec<serde_json::Value> = sites
        .into_iter()
        .skip(offset as usize)
        .map(|s| serde_json::json!({
            "name": s.name,
            "country": s.country,
            "languages": s.languages,
            "enabled": s.enabled,
            "url": s.url,
        }))
        .filter(|v| needle.as_ref().is_none_or(|n| {
            v.get("name").and_then(|x| x.as_str()).map(|x| x.to_ascii_lowercase().contains(n)).unwrap_or(false)
                || v.get("country").and_then(|x| x.as_str()).map(|x| x.to_ascii_lowercase().contains(n)).unwrap_or(false)
        }))
        .take(limit as usize)
        .collect();
    Json(serde_json::json!({ "total": total, "limit": limit, "offset": offset, "sites": page }))
        .into_response()
}

/// POST /api/sites/find — run the site finder.
/// Body: {keyword, domain?, mode?, quotes?, stop_words_pct?, limit?, pages?}.
async fn site_find(body: Json<serde_json::Value>) -> Response {
    let v = &body.0;
    let keyword = v.get("keyword").and_then(|x| x.as_str()).unwrap_or("").to_string();
    if keyword.trim().is_empty() {
        return (StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "keyword is required" }))).into_response();
    }
    let domain = v.get("domain").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let quotes = v.get("quotes").and_then(|x| x.as_bool()).unwrap_or(false);
    let limit = v.get("limit").and_then(|x| x.as_u64()).unwrap_or(20).min(200) as usize;
    let pages = v.get("pages").and_then(|x| x.as_u64()).unwrap_or(2).min(5) as u32;
    let max_sites = v.get("max_sites").and_then(|x| x.as_u64()).unwrap_or(48).min(400) as usize;
    // MVP default 0 — clean keyword query.  GSA's "add stop words with N%"
    // (multi-language word sampling) is opt-in via the request body.
    let stop_words_pct = v.get("stop_words_pct").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
    // T5 — optional engine whitelist (e.g. ["Bing"]); empty = whole catalog.
    let engine_names: Vec<String> = v
        .get("engines")
        .and_then(|x| x.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    let mode = match v.get("mode").and_then(|x| x.as_str()) {        Some("site") => engine::site::QueryMode::Site,
        Some("intitle") => engine::site::QueryMode::Intitle,
        Some("inurl") => engine::site::QueryMode::Inurl,
        Some("plain") => engine::site::QueryMode::Plain,
        _ => engine::site::QueryMode::Plain,
    };

    let all_sites = match engine::site::load_site_catalog() {
        Ok(sites) => sites,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e }))).into_response(),
    };
    // T5 — honor the engine whitelist (substring, case-insensitive).
    let sites_refs = engine::site::filter_engines(&all_sites, &engine_names);
    let sites: Vec<engine::site::SiteSpec> = if sites_refs.is_empty() {
        if !engine_names.is_empty() {
            return (StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": format!("no catalog engine matches {:?} (see GET /api/sites)", engine_names),
                }))).into_response();
        }
        Vec::new()
    } else {
        sites_refs.into_iter().cloned().collect()
    };

    let cfg = engine::site::SiteFindConfig {
        keyword: keyword.clone(),
        domain,
        mode,
        quotes,
        stop_words_pct,
        max_results: limit,
        max_sites,
        max_pages: pages.max(1),
        min_keywords_present: 0,
        skip_countries: Vec::new(),
        skip_languages: Vec::new(),
        block_domains: Vec::new(),
        seen_ips: std::collections::HashSet::new(),
        seed: 0xC0FFEE,
    };

    let driver: std::sync::Arc<dyn engine::driver::Driver> =
        std::sync::Arc::new(engine::CurlDriver::new()); // curl TLS fingerprint passes Brave

    let started = std::time::Instant::now();
    let attempted_engines = sites.len();
    let found = engine::site::find_sites(cfg, driver, sites).await;
    let elapsed_ms = started.elapsed().as_millis() as u64;

    let sites_out: Vec<serde_json::Value> = found
        .into_iter()
        .take(limit)
        .map(|f| serde_json::json!({
            "url": f.url,
            "title": f.title,
            "source_site": f.source_site,
            "source_page": f.source_page,
        }))
        .collect();

    Json(serde_json::json!({
        "status": "ok",
        "keyword": keyword,
        "elapsed_ms": elapsed_ms,
        "count": sites_out.len(),
        "attempted_engines": attempted_engines,
        "engines": engine_names,
        "sites": sites_out,
    })).into_response()
}


// ─────────────────────────────────────────────────────────────
// T5 — project site list: where live search goes, where the runner
// submits to.  GSA flow: search (se.dat catalog) → found sites → the
// discovery engines post the backlink there.
// ─────────────────────────────────────────────────────────────

/// GET /api/projects/{id}/sites — the project's saved found-site list.
async fn project_sites_list(Path(project_id): Path<Uuid>, state: State<Arc<AppState>>) -> Response {
    match Repo::new(state.pool.clone()).list_project_sites(project_id).await {
        Ok(rows) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "project_id": project_id.to_string(),
                "count": rows.len(),
                "sites": rows,
            })),
        )
            .into_response(),
        Err(e) => error_response(e),
    }
}

#[derive(Deserialize)]
struct AddSitesBody {
    /// Site URLs (array is preferred).
    #[serde(default)]
    urls: Vec<String>,
    /// Single-URL convenience form.
    #[serde(default)]
    url: Option<String>,
    /// Where the URLs came from: "search" (site finder) or "manual".
    #[serde(default = "default_site_origin")]
    origin: String,
}
fn default_site_origin() -> String {
    "manual".to_string()
}

/// POST /api/projects/{id}/sites — save found site URLs (upsert, unique
/// per project).
async fn project_sites_add(
    Path(project_id): Path<Uuid>,
    state: State<Arc<AppState>>,
    body: Json<AddSitesBody>,
) -> Response {
    let b = &body.0;
    let mut urls = b.urls.clone();
    if let Some(u) = b.url.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        urls.push(u.to_string());
    }
    if urls.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "urls (array) or url (string) is required" })),
        )
            .into_response();
    }
    let repo = Repo::new(state.pool.clone());
    if let Err(e) = repo.add_project_sites(project_id, &urls, &b.origin).await {
        return error_response(e);
    }
    match repo.list_project_sites(project_id).await {
        Ok(rows) => (
            StatusCode::OK,
            Json(serde_json::json!({ "saved": urls.len(), "total": rows.len(), "sites": rows })),
        )
            .into_response(),
        Err(e) => error_response(e),
    }
}

/// DELETE /api/projects/{id}/sites — clear the site list (fresh search).
async fn project_sites_clear(Path(project_id): Path<Uuid>, state: State<Arc<AppState>>) -> Response {
    match Repo::new(state.pool.clone()).clear_project_sites(project_id).await {
        Ok(removed) => (StatusCode::OK, Json(serde_json::json!({ "removed": removed }))).into_response(),
        Err(e) => error_response(e),
    }
}

/// POST /api/projects/{id}/sites/find — run the live site finder (same
/// engine as `POST /api/sites/find` — keyword / mode / `engines` whitelist /
/// limit / pages) and save the found sites into the project's site list.
/// This is the GSA "find sites → submit to them" MVP loop.
async fn project_sites_find(
    Path(project_id): Path<Uuid>,
    state: State<Arc<AppState>>,
    body: Json<serde_json::Value>,
) -> Response {
    let v = &body.0;
    let keyword = v.get("keyword").and_then(|x| x.as_str()).unwrap_or("").to_string();
    if keyword.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "keyword is required" })),
        )
            .into_response();
    }
    let domain = v.get("domain").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let quotes = v.get("quotes").and_then(|x| x.as_bool()).unwrap_or(false);
    let limit = v.get("limit").and_then(|x| x.as_u64()).unwrap_or(20).min(200) as usize;
    let pages = v.get("pages").and_then(|x| x.as_u64()).unwrap_or(1).min(5) as u32;
    let engine_names: Vec<String> = v
        .get("engines")
        .and_then(|x| x.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    let mode = match v.get("mode").and_then(|x| x.as_str()) {
        Some("site") => engine::site::QueryMode::Site,
        Some("intitle") => engine::site::QueryMode::Intitle,
        Some("inurl") => engine::site::QueryMode::Inurl,
        Some("plain") => engine::site::QueryMode::Plain,
        _ => engine::site::QueryMode::Plain,
    };

    let all_sites = match engine::site::load_site_catalog() {
        Ok(s) => s,
        Err(e) => return error_response(anyhow::anyhow!(e)),
    };
    let refs = engine::site::filter_engines(&all_sites, &engine_names);
    let sites: Vec<engine::site::SiteSpec> = refs.into_iter().cloned().collect();
    if sites.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": format!("no catalog engine matches {:?} (see GET /api/sites)", engine_names),
            })),
        )
            .into_response();
    }

    let cfg = engine::site::SiteFindConfig {
        keyword: keyword.clone(),
        domain,
        mode,
        quotes,
        stop_words_pct: 0,
        max_results: limit,
        // 48 engine fetches: budget big enough to walk past the ~36 dead
        // "brave" catalog entries and reach the working scrapers, while
        // dead-service skipping keeps actual fetch count small.
        max_sites: 48,
        max_pages: pages.max(1),
        min_keywords_present: 0,
        skip_countries: Vec::new(),
        skip_languages: Vec::new(),
        block_domains: Vec::new(),
        seen_ips: std::collections::HashSet::new(),
        seed: 0xC0FFEE,
    };
    let driver: std::sync::Arc<dyn engine::driver::Driver> = std::sync::Arc::new(engine::CurlDriver::new());
    let started = std::time::Instant::now();
    let attempted_engines = sites.len();
    let found = engine::site::find_sites(cfg, driver, sites).await;
    let elapsed_ms = started.elapsed().as_millis() as u64;

    let urls: Vec<String> = found.iter().map(|f| f.url.clone()).collect();
    let repo = Repo::new(state.pool.clone());
    let saved = match repo.add_project_sites(project_id, &urls, "search").await {
        Ok(n) => n,
        Err(e) => return error_response(e),
    };
    let rows = repo.list_project_sites(project_id).await.unwrap_or_default();
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "ok",
            "keyword": keyword,
            "elapsed_ms": elapsed_ms,
            "attempted_engines": attempted_engines,
            "found": urls.len(),
            "saved_new": saved,
            "total_sites": rows.len(),
            "sites": rows,
        })),
    )
        .into_response()
}

// POST /api/sites/find_on — single-page live check of F#23 extraction on a
// real URL (no engine lookup).
#[derive(Deserialize)]
struct FindOnBody {
    url: String,
    #[serde(default = "default_anchor")]
    anchor_text: String,
    #[serde(default)]
    #[serde(rename = "ignore")]
    ignore: Vec<String>,
}

fn default_anchor() -> String { String::new() }

async fn site_find_on(
    Json(body): Json<FindOnBody>,
) -> Response {
    let driver: std::sync::Arc<dyn engine::driver::Driver> = std::sync::Arc::new(engine::CurlDriver::new());
    let kw = if body.anchor_text.is_empty() { String::from("test") } else { body.anchor_text.clone() };
    let site = engine::site::SiteSpec {
        name: "test".into(),
        url: body.url.clone(),
        ..Default::default()
    };
    let cfg = engine::site::SiteFindConfig {
        keyword: kw,
        max_results: 50,
        block_domains: body.ignore.clone(),
        ..Default::default()
    };
    let res = engine::site::find_sites_on_site(&site, &cfg, Arc::clone(&driver)).await;
    let hits: Vec<serde_json::Value> = res
        .hits
        .into_iter()
        .map(|f| serde_json::json!({ "url": f.url }))
        .collect();
    (StatusCode::OK,
        Json(serde_json::json!({
            "anchor_on": body.url,
            "anchor_text": body.anchor_text,
            "count": hits.len(),
            "sites": hits,
            "status": "ok"
        }))).into_response()
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
        // F#19 — GSA "Anchor Text (below the keyword)" tab + URL options +
        // linkwheel + auto-export.  Separate PUTs so the UI can save them
        // independently (each is `None`-tolerant in the repo).
        .route("/api/projects/{id}/url", put(set_project_url_api))
        // F#30 — set the project's Article body (the GSA "Article content"
        // block). Empty body clears it → the runner auto-generates from
        // the project's keyword + target URL.
        .route("/api/projects/{id}/article", put(set_project_article_api))
        .route("/api/projects/{id}/comment", put(set_project_comment_api))
        .route(
            "/api/projects/{id}/anchor-options",
            put(set_project_anchor_options_api),
        )
        // F#24-lite — auto-export of the project's verified URLs (the
        // GSA "link list" feed for downstream tiers).
        .route("/api/projects/{id}/verified", get(list_verified_urls_api))

        .route("/api/jobs/claim", post(claim_job))

        .route("/api/jobs/{id}/finish", post(finish_job))

        .route("/api/jobs/{id}/cancel", post(cancel_job))

        .route("/api/jobs/{id}", get(get_job))

        .route("/api/projects/{id}/run", post(run_project))

        // F#26 — Polish: per-project Notes/Color/Pause-after-N, resume from
        // pause, and the per-engine "Test" dry-run (drive one engine, no DB
        // side effects).
        .route("/api/projects/{id}/test-engine", post(test_engine))
        // Stage B — multi-family best-engine selection (dry) + mixed run.
        .route("/api/projects/{id}/family-plan", get(family_plan))
        .route("/api/projects/{id}/run-mixed", post(run_mixed))
        .route("/api/projects/{id}/pause-after", put(set_project_pause_after))
        .route("/api/projects/{id}/polish", patch(set_project_polish))
        .route("/api/projects/{id}/resume", post(resume_project))

        // T6 — the auto UX: the system picks engines (try & fallback),
        // live run feed (DB tail + SSE), test-all engines, captcha
        // solver health.
        .route("/api/projects/{id}/run-auto", post(run_auto))
        .route("/api/projects/{id}/auto-pick", post(auto_pick))
        .route("/api/projects/{id}/run-status", get(project_run_status))
        .route("/api/projects/{id}/run-cancel", post(project_run_cancel))
        .route("/api/projects/{id}/events", get(project_events))
        .route("/api/projects/{id}/events/live", get(project_events_live))
        .route("/api/projects/{id}/test-all", post(test_all_engines))
        .route("/api/captcha/test", post(captcha_test))

        .route("/api/proxies", get(list_proxies).post(create_proxy))

        .route("/api/proxies/{id}", delete(delete_proxy))

        .route("/api/proxies/{id}/enabled", put(set_proxy_enabled))

        .route("/api/proxies/{id}/test", post(proxy_test))

        .route("/api/projects/{id}/proxies", put(set_project_proxies))

        .route("/api/proxies/{id}/country", put(set_proxy_country))

        .route("/api/proxies/{id}/verify-geo", post(verify_proxy_geo))

        .route("/api/proxies/verify-all", post(verify_all_proxies_geo))
        .route("/api/proxies/sources", get(proxy_sources))
        .route("/api/proxies/scrape", post(proxy_scrape))

        .route("/api/settings/email", get(get_email_setting).put(put_email_setting))
        .route("/api/email/create", post(create_email))

        .route("/api/settings/global", get(get_global_settings).put(put_global_settings))

        .route("/api/settings/engine/overrides", get(list_engine_overrides).post(set_engine_override).delete(delete_engine_overrides))

        .route("/api/settings/engine/{name}/effective", get(effective_engine_settings))
        // F#18 — alert channels.
        .route("/api/settings/notify", get(get_notify_settings).put(put_notify_settings).post(test_notify_email))
        .route("/api/notifications", get(list_notifications_api))
        .route("/api/notifications/unread", get(count_unread_api))
        .route("/api/notifications/{id}/read", put(mark_notification_read_api).post(mark_notification_read_api))
        .route("/api/notifications/read-all", put(mark_all_read_api).post(mark_all_read_api))
        .route("/api/notifications/live", get(notifications_sse))
        .route("/api/runs", get(global_runs))

        .route("/api/settings/export", get(export_settings_bundle))

        .route("/api/settings/import", post(import_settings_bundle))

        .route("/api/stats", get(stats_dashboard))

        .route("/api/catengine/stats", get(catalog_stats))

        .route("/api/catengine", get(catalog_list))

        .route("/api/catengine/import", post(catalog_import))

        // F#23 — site finding (the se.dat 2,525-site catalog + finder).
        .route("/api/sites", get(site_catalog))
        .route("/api/sites/find", post(site_find))
.route("/api/sites/find_on", post(site_find_on))
        // T5 — project site list (search results the runner submits to)
        .route("/api/projects/{id}/sites", get(project_sites_list).post(project_sites_add).delete(project_sites_clear))
        .route("/api/projects/{id}/sites/find", post(project_sites_find))

        // F#17 — per-project scheduled runs (schedule CRUD + run-now).
        .route(
            "/api/projects/{id}/schedules",
            get(list_schedules_handler).post(create_schedule_handler),
        )
        .route("/api/schedules", get(list_schedules_all))
        .route("/api/schedules/{id}", get(get_schedule).delete(delete_schedule))
        .route(
            "/api/schedules/{id}/enabled",
            put(set_schedule_enabled),
        )
        .route("/api/schedules/{id}/run-now", post(run_now_schedule))

        .with_state(state)

}



// ─── F#16: stats & analytics dashboard ──────────────────────────────────────



/// `GET /api/stats?days=30` — the GSA-style global dashboard.

///

/// Returns the whole model in one round-trip (one small `Repo` per query is

/// fine: these are aggregate reads, not hot paths).  `days` bounds the

/// timeline window (clamped to [1, 365], default 30).

#[derive(Deserialize)]

struct StatsQ {

    #[serde(default)]

    days: Option<u32>,

}



async fn stats_dashboard(Query(q): Query<StatsQ>, state: State<Arc<AppState>>) -> Response {

    let days = q.days.unwrap_or(30).clamp(1, 365);

    let repo = Repo::new(state.pool.clone());



    // 1. Global job-status buckets.

    let status_buckets = match repo.job_status_counts().await {

        Ok(rows) => rows

            .into_iter()

            .map(|(status, count)| engine::stats::StatusBucket { status, count })

            .collect::<Vec<_>>(),

        Err(e) => return error_response(e),

    };



    // 2. Global result-kind buckets (from the same results table the per-day

    //    query counts; one extra cheap aggregate keeps `summarize` honest).

    let kind_buckets = match global_kind_buckets(&repo).await {

        Ok(k) => k,

        Err(e) => return error_response(e),

    };



    let captchas_total = match repo.captcha_count().await {

        Ok(c) => c,

        Err(e) => return error_response(e),

    };

    let unique_links = match repo.unique_link_count().await {

        Ok(u) => u,

        Err(e) => return error_response(e),

    };



    let summary = engine::stats::summarize(&status_buckets, &kind_buckets, captchas_total, unique_links);



    // 3. Timeline over the requested window.

    let today = chrono::Utc::now().date_naive();

    let window_start = (today - chrono::Duration::days((days as i64) - 1))

        .and_hms_opt(0,0,0).unwrap();

    let daily_rows = match repo.stats_daily(window_start).await {

        Ok(r) => r,

        Err(e) => return error_response(e),

    };

    let days_series = engine::stats::day_series(&daily_rows, days, today);



    // 4. Per-engine rollup.

    let engines = match repo.stats_per_engine().await {

        Ok(rows) => rows

            .into_iter()

            .map(|(id, name, j, s, f, c, u)| engine::stats::engine_stat(&id, &name, j, s, f, c, u))

            .collect::<Vec<_>>(),

        Err(e) => return error_response(e),

    };



    // 5. Per-project rollup.

    let projects = match repo.stats_per_project().await {

        Ok(rows) => rows

            .into_iter()

            .map(|(id, name, j, s, f, c, u)| engine::stats::project_stat(&id, &name, j, s, f, c, u))

            .collect::<Vec<_>>(),

        Err(e) => return error_response(e),

    };



    let dash = engine::stats::Dashboard {

        summary,

        days: days_series,

        engines,

        projects,

    };



    match serde_json::to_value(&dash) {

        Ok(v) => (StatusCode::OK, Json(v)).into_response(),

        Err(e) => error_response(anyhow::anyhow!("serialize: {e}")),

    }

}



/// Result-kind buckets across the whole results table (global).

async fn global_kind_buckets(

    repo: &Repo,

) -> anyhow::Result<Vec<engine::stats::KindBucket>> {

    // Delegate to the db layer — this is the "summarize" kind counts

    // aggregated over *all* results (not per-project).

    let rows = repo.result_kind_buckets().await?;

    Ok(rows

        .into_iter()

        .map(|(kind, count)| engine::stats::KindBucket { kind, count })

        .collect())

}





async fn list_proxies(state: State<Arc<AppState>>, query: axum::extract::Query<std::collections::HashMap<String, String>>) -> Response {

    let country = query.0.get("country").map(|s| s.trim()).filter(|s| !s.is_empty());

    let repo = Repo::new(state.pool.clone());

    let p = repo.list_all_proxies_by_country(country.as_deref()).await;

    match p {

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

    /// F#14 — optional target country (GSA "Country" column).

    #[serde(default)]

    country: Option<String>,

}



fn default_port() -> u16 { 8080 }

fn default_kind() -> String { "http".into() }



async fn create_proxy(state: State<Arc<AppState>>, body: Json<AddProxyReq>) -> Response {

    if body.kind != "http" && body.kind != "socks5" {

        return (StatusCode::BAD_REQUEST,

            Json(serde_json::json!({ "error": "kind must be 'http' or 'socks5'" }))).into_response();

    }

    match Repo::new(state.pool.clone())

        .add_proxy(

            &body.host,

            body.port as i32,

            &body.kind,

            body.login.as_deref().unwrap_or_default(),

            body.password.as_deref().unwrap_or_default(),

            true,

            body.country.as_deref(),

        )

        .await

    {

        Ok(p) => (StatusCode::CREATED, Json(serde_json::to_value(&p).unwrap())).into_response(),

        Err(e) => {

            if is_unique_violation(&e) {

                (StatusCode::CONFLICT,

                 Json(serde_json::json!({ "error": "a proxy with this host:port already exists" }))).into_response()

            } else {

                error_response(e)

            }

        }

    }

}




// — F#24: proxy list scraper (GSA "add free proxies from list") —
/// GET /api/proxies/sources — the built-in free-proxy source catalog.
async fn proxy_sources() -> Response {
    let list: Vec<serde_json::Value> = engine::proxy_scraper::known_sources()
        .iter()
        .map(|s| serde_json::json!({
            "name": s.name, "url": s.url, "proto": s.proto, "format": s.format,
        }))
        .collect();
    Json(serde_json::json!({ "count": list.len(), "sources": list })).into_response()
}

/// Inbound for `POST /api/proxies/scrape` — a free-proxy publisher `url` to
/// fetch + import (parsed rows become DISABLED proxies ready for F#13
/// `/test` promotion).  `format` is "html" (default for table sites) or
/// "text" (one endpoint per line).
#[derive(serde::Deserialize)]
struct ScrapeBody {
    url: String,
    #[serde(default)]
    format: Option<String>,
}

/// POST /api/proxies/scrape — fetch a free-proxy source, parse
/// `host:port` rows, import each as a DISABLED proxy row (uq_proxies_host_port
/// on conflict = counted as duplicate).  GSA's "add free proxies from list":
/// scraped rows enter the list disabled; the user tests (F#13) + geo-verifies
/// (F#14) to promote.
async fn proxy_scrape(state: State<Arc<AppState>>, Json(body): Json<ScrapeBody>) -> Response {
    let url = body.url.trim().to_string();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return (StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "url must start with http:// or https://"}))).into_response();
    }
    let fmt = body.format.as_deref().unwrap_or("text").to_string().to_lowercase();
    let fmt = if fmt == "html" { "html" } else { "text" };

    let parsed = match engine::proxy_scraper::scrape(&url, fmt).await {
        Ok(p) => p,
        Err(e) => {
            return (StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({ "error": format!("scrape failed: {e}") }))).into_response();
        }
    };

    let repo = Repo::new(state.pool.clone());
    let mut imported = 0usize;
    let mut duplicates = 0usize;
    let mut failed = Vec::new();
    for p in parsed {
        match repo.add_proxy(&p.host, p.port as i32, "http", "", "", false, None).await {
            Ok(_) => imported += 1,
            Err(e) if is_unique_violation(&e) => duplicates += 1,
            Err(e) => failed.push(format!("{}:{} — {}", p.host, p.port, e)),
        }
    }
    (StatusCode::OK, Json(serde_json::json!({
        "url": url,
        "format": fmt,
        "parsed": imported + duplicates + failed.len(),
        "imported": imported,
        "duplicates": duplicates,
        "failed": failed,
    }))).into_response()
}

async fn delete_proxy(state: State<Arc<AppState>>, Path(id): Path<Uuid>) -> Response {

    match Repo::new(state.pool.clone()).delete_proxy(id).await {

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

    match Repo::new(state.pool.clone()).set_proxy_enabled(id, enabled).await {

        Ok(p) => (StatusCode::OK, Json(serde_json::to_value(&p).unwrap())).into_response(),

        Err(e) => error_response(e),

    }

}



/// F#13 `POST /api/proxies/{id}/test` — runs the real-socket connectivity

/// probe (GSA `proxy test`).  Returns `{ok, detail, latency_ms}`.

async fn proxy_test(state: State<Arc<AppState>>, Path(id): Path<Uuid>) -> Response {

    let repo = Repo::new(state.pool.clone());

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

    match Repo::new(state.pool.clone()).set_project_use_proxies(id, use_proxies).await {

        Ok(p) => (StatusCode::OK, Json(serde_json::to_value(&p).unwrap())).into_response(),

        Err(e) => error_response(e),

    }

}



// ─── F#14: geo / IP handling ──────────────────────────────────────────────────

/// F#14 `PUT /api/proxies/{id}/country` — set (or clear with `country: null`)

/// the proxy's target country (GSA's "Country" column).  The pure matching

/// policy lives in `engine::geo::normalize_country` for testability.

async fn set_proxy_country(

    state: State<Arc<AppState>>,

    Path(id): Path<Uuid>,

    body: Json<serde_json::Value>,

) -> Response {

    let raw = body.0.get("country").cloned().unwrap_or(serde_json::Value::Null);

    let country: Option<String> = match raw {

        serde_json::Value::String(s) if !s.trim().is_empty() => {

            engine::geo::normalize_country(&s).filter(|c| !c.is_empty())

        }

        serde_json::Value::Null => None,

        serde_json::Value::String(_) => None, // blank string → clear

        other => {

            return (StatusCode::BAD_REQUEST,

                Json(serde_json::json!({ "error": format!("country must be a string or null, got {other}") }))

            ).into_response()

        }

    };

    match Repo::new(state.pool.clone()).set_proxy_country(id, country.as_deref()).await {

        Ok(p) => (StatusCode::OK, Json(serde_json::to_value(&p).unwrap())).into_response(),

        Err(e) => error_response(e),

    }

}



#[derive(Deserialize, Default)]

struct VerifyGeoReq {

    /// Optional override for the egress-IP endpoint (default: GSA's own

    /// `https://api.ipify.org/?format=raw`).

    #[serde(default)]

    egress_url: Option<String>,

    /// Optional timeout-seconds for the single hop (default: 6s).

    #[serde(default)]

    timeout_secs: Option<u64>,

}



/// F#14 `POST /api/proxies/{id}/verify-geo` — run an egress-IP probe

/// *through* the proxy and persist the result on the row.  The IP-verification

/// contract: the returned address is the proxy's **egress** (a `127.0.0.1`

/// / `10.x` back means the proxy did not route).  Optional country resolution

/// is pluggable (today IP-only, the faithful GSA default).

async fn verify_proxy_geo(

    state: State<Arc<AppState>>,

    Path(id): Path<Uuid>,

    body: Option<Json<VerifyGeoReq>>,

) -> Response {

    let req = body.map(|Json(v)| v).unwrap_or_default();

    let repo = Repo::new(state.pool.clone());

    let proxy = match repo.proxy_by_id(id).await {

        Ok(Some(p)) => p,

        Ok(None) => return (StatusCode::NOT_FOUND,

            Json(serde_json::json!({ "error": "not found" }))).into_response(),

        Err(e) => return error_response(e),

    };

    let spec = engine::proxy::ProxySpec {

        host: proxy.host.clone(),

        port: proxy.port as u16,

        proxy_kind: proxy.proxy_kind,

        login: proxy.login,

        password: proxy.password,

    };

    let egress_url = req

        .egress_url

        .unwrap_or_else(|| engine::geo_probe::DEFAULT_EGRESS_URL.to_string());

    let timeout = std::time::Duration::from_secs(req.timeout_secs.unwrap_or(6));

    let result = engine::geo_probe::resolve_ip(&spec, &egress_url, timeout, None).await;

    let stored = repo

        .store_geo_verification(

            id,

            if result.ip.is_empty() { None } else { Some(result.ip.as_str()) },

            result.public,

        )

        .await;

    let p = match stored {

        Ok(p) => p,

        Err(e) => return error_response(e),

    };

    (StatusCode::OK, Json(serde_json::json!({

        "proxy_id": id,

        "ip": result.ip,

        "public": result.public,

        "speed_ms": result.speed_ms,

        "country": result.country,

        "geo_verified_at": p.geo_verified_at,

    }))).into_response()

}



/// F#14 `POST /api/proxies/verify-all` — bulk egress verification of every

/// **enabled** proxy.  Returns per-proxy results (GSA's "verify all proxies"

/// workflow).  Concurrency is bounded to 8 to not saturate the egress host.

async fn verify_all_proxies_geo(

    state: State<Arc<AppState>>,

    body: Option<Json<VerifyGeoReq>>,

) -> Response {

    let req = body.map(|Json(v)| v).unwrap_or_default();

    let repo = Repo::new(state.pool.clone());

    let proxies = match repo.list_enabled_proxies().await {

        Ok(p) => p,

        Err(e) => return error_response(e),

    };

    let egress_url = req

        .egress_url

        .unwrap_or_else(|| engine::geo_probe::DEFAULT_EGRESS_URL.to_string());

    let timeout = std::time::Duration::from_secs(req.timeout_secs.unwrap_or(6));

    // Clone the SQL pool once up front — the `'static` spawned futures need

    // an owned reference; `state` is a by-value axum extractor that dies with

    // this frame.

    let pool = state.pool.clone();



    use futures_util::StreamExt;

    type BoxedFuture = std::pin::Pin<Box<dyn std::future::Future<Output = serde_json::Value> + Send + 'static>>;

    let results: Vec<BoxedFuture> = proxies

        .into_iter()

        .map(|p| -> BoxedFuture {

            // `Repo` owns its pool, so a simple clone is enough — the future

            // becomes `'static` and captures the Arc pool by value.

            let repo = Repo::new(pool.clone());

            let spec = engine::proxy::ProxySpec {

                host: p.host,

                port: p.port as u16,

                proxy_kind: p.proxy_kind,

                login: p.login,

                password: p.password,

            };

            let id = p.id;

            let egress_url = egress_url.clone();

            Box::pin(async move {

                let r = engine::geo_probe::resolve_ip(&spec, &egress_url, timeout, None).await;

                let stored = repo

                    .store_geo_verification(

                        id,

                        if r.ip.is_empty() { None } else { Some(r.ip.as_str()) },

                        r.public,

                    )

                    .await;

                let ok = stored.is_ok();

                serde_json::json!({

                    "proxy_id": id,

                    "ip": r.ip,

                    "public": r.public,

                    "speed_ms": r.speed_ms,

                    "ok": ok,

                })

            })

        })

        .collect::<Vec<_>>();

    let mut out = Vec::with_capacity(results.len());

    let mut stream = futures_util::stream::iter(results).buffer_unordered(8);

    while let Some(v) = stream.next().await {

        out.push(v);

    }

    (StatusCode::OK, Json(serde_json::json!({ "results": out }))).into_response()

}



// ─── F#13: project proxy gate ────────────────────────────────────────────────



// ─── F#15: settings & profiles (global + engine-override + import/export) ──



/// GET /api/settings/global — read the stored global settings.

async fn get_global_settings(state: State<Arc<AppState>>) -> Response {

    match Repo::new(state.pool.clone()).global_settings().await {

        Ok(s) => (StatusCode::OK, Json(serde_json::to_value(&s).unwrap())).into_response(),

        Err(e) => error_response(e),

    }

}



/// PUT /api/settings/global — replace the whole stored global settings.

async fn put_global_settings(state: State<Arc<AppState>>, body: Json<engine::settings::Settings>) -> Response {

    match Repo::new(state.pool.clone()).set_global_settings(&body.0).await {

        Ok(()) => Json(serde_json::json!({ "status": "ok" })).into_response(),

        Err(e) => error_response(e),

    }

}



/// GET /api/settings/engine/overrides — every per-engine override row.

async fn list_engine_overrides(state: State<Arc<AppState>>) -> Response {

    match Repo::new(state.pool.clone()).list_engine_overrides().await {

        Ok(list) => (StatusCode::OK, Json(serde_json::to_value(&list).unwrap())).into_response(),

        Err(e) => error_response(e),

    }

}



#[derive(Deserialize)]

struct SetEngineOverrideReq {

    engine_name: String,

    field: String,

    /// Omitted / null / empty-string = "clear this field back to global".

    value: Option<String>,

}



/// POST /api/settings/engine/overrides — set (or clear, with `value: null`)

/// one field of one engine's override.

async fn set_engine_override(state: State<Arc<AppState>>, body: Json<SetEngineOverrideReq>) -> Response {

    let value = body.0.value.as_deref().map(str::trim).filter(|v| !v.is_empty());

    match Repo::new(state.pool.clone())

        .set_engine_override(&body.0.engine_name, &body.0.field, value)

        .await

    {

        Ok(()) => Json(serde_json::json!({ "status": "ok" })).into_response(),

        Err(e) => error_response(e),

    }

}



#[derive(Deserialize)]

struct DeleteEngineOverridesReq {

    engine_name: String,

}



/// DELETE /api/settings/engine/overrides?engine_name=<name> — remove every

/// stored override for one engine. (query-param form, so the URL stays a

/// single route for every engine.)

async fn delete_engine_overrides(

    state: State<Arc<AppState>>,

    q: axum::extract::Query<DeleteEngineOverridesReq>,

) -> Response {

    match Repo::new(state.pool.clone()).delete_engine_overrides(&q.0.engine_name).await {

        Ok(()) => StatusCode::NO_CONTENT.into_response(),

        Err(e) => error_response(e),

    }

}



/// GET /api/settings/engine/{name}/effective — the *effective* settings for

/// one engine (global + that engine's overrides, clamped, ready to feed a

/// runner).

async fn effective_engine_settings(

    state: State<Arc<AppState>>,

    Path(name): Path<String>,

) -> Response {

    match Repo::new(state.pool.clone()).effective_settings_for_engine(&name).await {

        Ok(s) => (StatusCode::OK, Json(serde_json::to_value(&s).unwrap())).into_response(),

        Err(e) => error_response(e),

    }

}



/// GET /api/settings/export — the GSA_SER_Settings bundle as JSON (global +

/// engine overrides + proxy snapshot). The UI wraps this with a download.

async fn export_settings_bundle(state: State<Arc<AppState>>) -> Response {

    match Repo::new(state.pool.clone()).export_settings_bundle().await {

        Ok(b) => (StatusCode::OK, Json(serde_json::to_value(&b).unwrap())).into_response(),

        Err(e) => error_response(e),

    }

}



#[derive(Deserialize)]

struct ImportBundleReq {

    bundle: engine::settings::SettingsBundle,

}



/// POST /api/settings/import — apply a bundle (global + engine overrides +

/// proxies, in one transaction). Returns the number of proxy rows upserted.

async fn import_settings_bundle(state: State<Arc<AppState>>, body: Json<ImportBundleReq>) -> Response {

    match Repo::new(state.pool.clone()).import_settings_bundle(&body.0.bundle).await {

        Ok(n) => (StatusCode::OK, Json(serde_json::json!({ "status": "ok", "proxies_upserted": n }))).into_response(),

        Err(e) => error_response(e),

    }

}


// ─────────────────────────────────────────────────────────────────────────────
// F#18 — alert channels + real-time notifications.
// ─────────────────────────────────────────────────────────────────────────────

/// GET /api/settings/notify — the operator's alert-channel config
/// (SMTP + per-type email toggles). Password comes back as-is (this is a
/// local operator tool, not a public API).
async fn get_notify_settings(state: State<Arc<AppState>>) -> Response {
    let cfg = crate::notifier::NotifyConfig::load(&state.pool).await;
    (StatusCode::OK, Json(serde_json::to_value(&cfg).unwrap())).into_response()
}

/// PUT /api/settings/notify — save the config, then emit a
/// `settingsChanged` alert (the operator just changed how they're alerted,
/// which is itself "something that happened").
async fn put_notify_settings(state: State<Arc<AppState>>, body: Json<crate::notifier::NotifyConfig>) -> Response {
    match body.0.save(&state.pool).await {
        Ok(()) => {
            let email_on = body.0.email_enabled && !body.0.recipient.trim().is_empty();
            let _ = crate::notifier::notify_settings_saved(
                &state,
                if email_on {
                    format!("email alerts ON to {}", body.0.recipient)
                } else {
                    "email alerts OFF".to_string()
                },
            )
            .await;
            (StatusCode::OK, Json(serde_json::json!({ "status": "ok", "email_enabled": email_on }))).into_response()
        }
        Err(e) => error_response(e.into()),
    }
}

/// POST /api/settings/notify — "test my email config": send one
/// `systemInfo` test alert through the saved SMTP settings (mode `log`
/// renders without a socket, so this doubles as a dry-run). The outcome is
/// returned as JSON AND a `systemError`/`settingsChanged` alert is emitted
/// so the bell tells the story either way.
async fn test_notify_email(state: State<Arc<AppState>>) -> Response {
    let cfg = crate::notifier::NotifyConfig::load(&state.pool).await;
    let smtp_cfg = cfg.to_smtp();
    let probe = engine::notify::Notification {
        id: "test".into(),
        type_: engine::notify::NotificationType::SystemInfo,
        severity: engine::notify::Severity::Info,
        project_id: None,
        job_id: None,
        title: "SER test alert".into(),
        message: "If you got this email (or saw it in the logs), your alert channel works.".into(),
        payload: serde_json::json!({ "source": "test-notify" }),
        is_read: false,
        created_at: chrono::Utc::now(),
    };
    let out = match tokio::task::spawn_blocking(move || engine::smtp::send(&smtp_cfg, &probe)).await {
        Ok(Ok(r)) => {
            let _ = crate::notifier::notify_settings_saved(&state, "email test delivered".to_string()).await;
            serde_json::json!({ "status": "ok", "delivered": r.delivered,
                                 "message_bytes": r.message_size,
                                 "mode": r.mode.as_kebab(),
                                 "rendered": r.rendered })
        }
        Ok(Err(e)) => {
            let _ = crate::notifier::emit_system_error(&state, "email test failed", Some(e.clone())).await;
            serde_json::json!({ "status": "error", "error": e })
        }
        Err(je) => {
            let _ = crate::notifier::emit_system_error(&state, "email test task panicked", Some(je.to_string())).await;
            serde_json::json!({ "status": "error", "error": je.to_string() })
        }
    };
    (StatusCode::OK, Json(out)).into_response()
}

#[derive(Deserialize)]
struct ListQueries {
    #[serde(default)]
    limit: Option<i64>,
    #[serde(default)]
    offset: Option<i64>,
    #[serde(default)]
    project_id: Option<Uuid>,
    #[serde(default)]
    unread: Option<bool>,
}

/// GET /api/notifications — Notification Center list (newest first).
async fn list_notifications_api(state: State<Arc<AppState>>, Query(q): Query<ListQueries>) -> Response {
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let offset = q.offset.unwrap_or(0).max(0);
    match Repo::new(state.pool.clone())
        .list_notifications(limit, offset, q.project_id, q.unread.unwrap_or(false))
        .await
    {
        Ok(list) => (StatusCode::OK, Json(serde_json::to_value(&list).unwrap())).into_response(),
        Err(e) => error_response(e),
    }
}

/// GET /api/notifications/unread — badge count for the bell.
async fn count_unread_api(state: State<Arc<AppState>>) -> Response {
    match Repo::new(state.pool.clone()).count_unread_notifications().await {
        Ok(n) => (StatusCode::OK, Json(serde_json::json!({ "unread": n }))).into_response(),
        Err(e) => error_response(e),
    }
}

/// PUT|POST /api/notifications/{id}/read — mark one read.
async fn mark_notification_read_api(state: State<Arc<AppState>>, Path(id): Path<Uuid>) -> Response {
    match Repo::new(state.pool.clone()).mark_notification_read(id).await {
        Ok(true) => (StatusCode::OK, Json(serde_json::json!({ "status": "ok" }))).into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "not found" }))).into_response(),
        Err(e) => error_response(e),
    }
}

/// PUT|POST /api/notifications/read-all — wipe the badge.
async fn mark_all_read_api(state: State<Arc<AppState>>) -> Response {
    match Repo::new(state.pool.clone()).mark_all_notifications_read().await {
        Ok(n) => (StatusCode::OK, Json(serde_json::json!({ "status": "ok", "marked": n }))).into_response(),
        Err(e) => error_response(e),
    }
}

/// GET /api/notifications/live — the real-time channel (SSE).
///
/// `Server-Sent Events` over the in-process `tokio::sync::broadcast` bus:
/// each connected client gets its own receiver, and `emit` never blocks on
/// a slow client (broadcast drops for lagging receivers; the Vue store
/// re-fetches the DB list on the `EventSource` (re)connect, so no alert is
/// ever lost). Every frame is one of:
///   event: hello     {"serverTime": "..."}              (immediately)
///   event: notify    <Notification>                      (each alert)
///   event: error     {"error": "..."}                    (stream died mid-flight)
/// The `text/event-stream` content type makes `EventSource` auto-reconnect
/// with `Last-Event-Id` semantics; we don't read the header (the DB list is
/// the source of truth — a reconnect is just a `GET /api/notifications`).
async fn notifications_sse(state: State<Arc<AppState>>) -> Response {
    use axum::response::sse::{Event, KeepAlive, Sse};
    use futures_util::StreamExt;
    use tokio_stream::wrappers::BroadcastStream;

    let mut rx = state.bus.subscribe();
    let hello_time = chrono::Utc::now().to_rfc3339();

    // A stream of `Event`s: `hello` first, then one frame per bus event.
    // `Lagged` frames (a slow client) are dropped — the UI refetches on
    // reconnect, and the DB already has every alert.
    type SseFrame = Result<Event, std::convert::Infallible>;
    let stream: futures_util::stream::BoxStream<'static, SseFrame> = futures_util::stream::once(std::future::ready(Ok::<Event, std::convert::Infallible>(Event::default()
            .event("hello")
            .data(serde_json::to_string(&serde_json::json!({ "serverTime": hello_time })).unwrap()))))
    .chain(
        BroadcastStream::new(rx).filter_map(|r| {
            let fut = async move { match r {
                Ok(ev) => Some(Ok::<Event, std::convert::Infallible>(Event::default().event("notify").data(serde_json::to_string(&ev.notification).unwrap_or_default()))),
                // `Lagged` (slow client) and `Empty` (channel closed): drop
                // the frame. The UI refetches on EventSource reconnect and
                // the DB already holds every alert, so no data is lost.
                Err(_) => None,
            }};
            fut
        }),
    )
    .boxed();

    Sse::new(stream)
        .keep_alive(
            KeepAlive::default().interval(std::time::Duration::from_secs(15)),
        )
        .into_response()
}

/// Helper: `SmtpMode` -> kebab-case string for the test button's JSON.
trait Kebab {
    fn as_kebab(&self) -> &'static str;
}
impl Kebab for engine::smtp::SmtpMode {
    fn as_kebab(&self) -> &'static str {
        match self {
            engine::smtp::SmtpMode::Log => "log",
            engine::smtp::SmtpMode::StartTls => "starttls",
            engine::smtp::SmtpMode::DirectTls => "direct-tls",
        }
    }
}
