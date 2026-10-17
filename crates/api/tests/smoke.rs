//! Live HTTP smoke tests against a real Postgres.
//!
//! Runs the axum router in-process through `tower::ServiceExt::oneshot` so
//! we don't need to bind a port. Skipped (not failed) when Postgres is
//! unreachable so the suite still runs on a box without the container.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use ser_api::{app_state, router};
use tower::ServiceExt;

const GSA_AARDVARK: &str =
    r"C:\Users\kingsly\Downloads\GSA_SER_v18.20_FINAL\GSA_SER_v18.20_FINAL\Engines\Aardvark Topsites.ini";

async fn body_to_json(response: axum::response::Response) -> (StatusCode, serde_json::Value) {
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = if bytes.is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| serde_json::json!({"raw": String::from_utf8_lossy(&bytes)}))
    };
    (status, json)
}

#[tokio::test]
async fn healthz_ok() {
    let Ok(state) = app_state(None).await else {
        eprintln!("SKIP healthz: no Postgres (docker compose up -d)");
        return;
    };
    let app = router(state);
    let request = Request::builder()
        .method("GET")
        .uri("/healthz")
        .body(Body::empty())
        .unwrap();
    let (status, body) = body_to_json(app.clone().oneshot(request).await.unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn full_api_smoke_project_engines_keywords_jobs() {
    let Ok(state) = app_state(None).await else {
        eprintln!("SKIP full smoke: no Postgres (docker compose up -d)");
        return;
    };
    let app = router(state);

    // 1) create project
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let name = format!("smoke {stamp}");
    let request = Request::builder()
        .method("POST")
        .uri("/api/projects")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({ "name": name, "description": "smoke" }))
                .unwrap(),
        ))
        .unwrap();
    let (status, project) = body_to_json(app.clone().oneshot(request).await.unwrap()).await;
    assert_eq!(status, StatusCode::CREATED);
    let proj_id = project["id"].as_str().expect("id").to_string();

    // 2) list keywords (empty)
    let request = Request::builder()
        .method("GET")
        .uri(&format!("/api/projects/{proj_id}/keywords"))
        .body(Body::empty())
        .unwrap();
    let (status, keywords) = body_to_json(app.clone().oneshot(request).await.unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(keywords.as_array().map(|a| a.len()).unwrap_or(0), 0);

    // 3) upsert engine parsed from the real GSA .ini
    let spec =
        engine::enginespec::parse_engine_file(std::path::Path::new(GSA_AARDVARK).into())
            .expect("Aardvark parses");
    let request = Request::builder()
        .method("POST")
        .uri(&format!("/api/projects/{proj_id}/engines"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "engine_name": spec.name,
                "engine_type": spec.engine_type.to_string(),
                "url": spec.url,
                "file_path": GSA_AARDVARK,
                "step_count": spec.steps.len() as i32,
            }))
            .unwrap(),
        ))
        .unwrap();
    let (status, engine) = body_to_json(app.clone().oneshot(request).await.unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    let engine_id = engine["id"].as_str().expect("engine id").to_string();
    assert_eq!(engine["engine_name"].as_str(), Some(spec.name.as_str()));
    assert_eq!(engine["step_count"].as_i64(), Some(spec.steps.len() as i64));

    // 4) add two keywords, remember the first for job binding
    let kw1 = "best pizza near me";
    let kw2 = "where to buy coffee in tokyo";
    let mut kw1_id = None;
    for kw in [kw1, kw2] {
        let request = Request::builder()
            .method("POST")
            .uri(&format!("/api/projects/{proj_id}/keywords"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({ "text": kw })).unwrap(),
            ))
            .unwrap();
        let (status, body) = body_to_json(app.clone().oneshot(request).await.unwrap()).await;
        assert_eq!(status, StatusCode::CREATED);
        if kw1_id.is_none() {
            kw1_id = Some(body["id"].as_str().expect("kw id").to_string());
        }
    }
    let kw1_id = kw1_id.expect("kw1 uuid");

    // list should now return 2
    let request = Request::builder()
        .method("GET")
        .uri(&format!("/api/projects/{proj_id}/keywords"))
        .body(Body::empty())
        .unwrap();
    let (status, keywords) = body_to_json(app.clone().oneshot(request).await.unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    let kws = keywords.as_array().expect("keywords list");
    assert_eq!(kws.len(), 2);
    assert!(kws.iter().any(|k| k["text"].as_str() == Some(kw1)));
    assert!(kws.iter().any(|k| k["text"].as_str() == Some(kw2)));

    // 5) create a job bound to the engine + kw1
    let request = Request::builder()
        .method("POST")
        .uri(&format!("/api/projects/{proj_id}/jobs"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "engine_id": engine_id,
                "keyword_id": kw1_id,
            }))
            .unwrap(),
        ))
        .unwrap();
    let (status, job) = body_to_json(app.clone().oneshot(request).await.unwrap()).await;
    assert_eq!(status, StatusCode::CREATED);
    let job_id = job["id"].as_str().expect("job id").to_string();
    assert_eq!(job["status"].as_str(), Some("pending"));
    assert_eq!(job["engine_id"].as_str(), Some(engine_id.as_str()));
    assert_eq!(job["keyword_id"].as_str(), Some(kw1_id.as_str()));

    // 6) list jobs - exactly ours
    let request = Request::builder()
        .method("GET")
        .uri(&format!("/api/projects/{proj_id}/jobs"))
        .body(Body::empty())
        .unwrap();
    let (status, jobs) = body_to_json(app.clone().oneshot(request).await.unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(jobs.as_array().map(|a| a.len()).unwrap_or(0), 1);

    // 7) atomic claim - first returns the job, second returns null
    let request = Request::builder()
        .method("POST")
        .uri(&format!("/api/jobs/claim?project_id={proj_id}"))
        .body(Body::empty())
        .unwrap();
    let (status, claimed) = body_to_json(app.clone().oneshot(request).await.unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(claimed["id"].as_str(), Some(job_id.as_str()));
    assert_eq!(claimed["status"].as_str(), Some("running"));
    assert_eq!(claimed["attempts"].as_i64(), Some(1));

    let request = Request::builder()
        .method("POST")
        .uri(&format!("/api/jobs/claim?project_id={proj_id}"))
        .body(Body::empty())
        .unwrap();
    let (status, claimed2) = body_to_json(app.clone().oneshot(request).await.unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(claimed2.get("claimed").is_some(), "second claim must be null");

    // 8) finish the job as `finished`
    let request = Request::builder()
        .method("POST")
        .uri(&format!("/api/jobs/{job_id}/finish"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({ "status": "done" })).unwrap(),
        ))
        .unwrap();
    let (status, _body) = body_to_json(app.clone().oneshot(request).await.unwrap()).await;
    assert_eq!(status, StatusCode::OK);

    // job now reports `finished` status
    let request = Request::builder()
        .method("GET")
        .uri(&format!("/api/jobs/{job_id}"))
        .body(Body::empty())
        .unwrap();
    let (status, job) = body_to_json(app.clone().oneshot(request).await.unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(job["status"].as_str(), Some("done"));
    assert!(job["finished_at"].is_string());

    // 9) results endpoint - no rows yet but must return a list
    let request = Request::builder()
        .method("GET")
        .uri(&format!("/api/jobs/{job_id}/results"))
        .body(Body::empty())
        .unwrap();
    let (status, results) = body_to_json(app.clone().oneshot(request).await.unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(results.is_array());

    // 10) healthz still healthy
    let request = Request::builder()
        .method("GET")
        .uri("/healthz")
        .body(Body::empty())
        .unwrap();
    let (status, body) = body_to_json(app.clone().oneshot(request).await.unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
}

// \u2500\u2500 F#9 keyword management routes \u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500
#[tokio::test]
async fn f9_keyword_api_routes_dedupe_bulk_get_delete() {
    let Ok(state) = app_state(None).await else {
        eprintln!("SKIP f9 keyword api: no Postgres (docker compose up -d)");
        return;
    };
    let app = router(state);

    // project
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let (status, p) = body_to_json(
        app.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/projects")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(
                        &serde_json::json!({ "name": format!("f9 api kw {stamp}") }),
                    )
                    .unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let proj_id = p["id"].as_str().unwrap().to_string();

    // 1) single add x2 with whitespace variants -> dedupe (same id)
    let (st1, b1) = body_to_json(
        app.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(&format!("/api/projects/{proj_id}/keywords"))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(
                        &serde_json::json!({ "text": "  SEO audit  services " }),
                    )
                    .unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    let (st2, b2) = body_to_json(
        app.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(&format!("/api/projects/{proj_id}/keywords"))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(
                        &serde_json::json!({ "text": "seo audit services" }),
                    )
                    .unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(st1, StatusCode::CREATED);
    assert_eq!(st2, StatusCode::CREATED);
    assert_eq!(b1["id"], b2["id"], "case/whitespace-variant keyword must dedupe");
    assert_eq!(b1["text"].as_str(), Some("SEO audit services"), "first entry's canonical text wins");

    // 2) bulk: mixed plain + spintax + duplicate -> distinct stored
    let (st3, b3) = body_to_json(
        app.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(&format!("/api/projects/{proj_id}/keywords/bulk"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "keywords": [
                                "seo audit services",      // duplicate
                                "backlink outreach",        // new
                                "cheap {shoes | boots}",    // spintax -> 2
                            ]
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(st3, StatusCode::CREATED);
    assert!(
        b3["stored"].as_u64().unwrap() >= 3,
        "bulk should store >=3 distinct (new + 2 spintax), got {b3:?}"
    );
    let list: Vec<String> = b3["keywords"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k["text"].as_str().unwrap().to_string())
        .collect();
    assert!(list.iter().any(|t| t == "backlink outreach"), "list: {list:?}");
    assert!(list.iter().any(|t| t == "cheap shoes"), "spintax fan-out missing: {list:?}");
    assert!(list.iter().any(|t| t == "cheap boots"), "spintax fan-out missing: {list:?}");

    // 3) list endpoint
    let (st4, list_body) = body_to_json(
        app.clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(&format!("/api/projects/{proj_id}/keywords"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(st4, StatusCode::OK);
    let n = list_body.as_array().unwrap().len();
    assert!(n >= 4, "expected >=4 rows, got {n}");

    // 4) GET by id
    let kw_id = b3["keywords"]
        .as_array()
        .unwrap()
        .iter()
        .find(|k| k["text"] == "backlink outreach")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (st5, b5) = body_to_json(
        app.clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(&format!("/api/projects/{proj_id}/keywords/{kw_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(st5, StatusCode::OK);
    assert_eq!(b5["text"].as_str(), Some("backlink outreach"));

    // 5) DELETE by id -> 204, then GET -> 404
    let (st6, _) = body_to_json(
        app.clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(&format!("/api/projects/{proj_id}/keywords/{kw_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(st6, StatusCode::NO_CONTENT);
    let (st7, _) = body_to_json(
        app.clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(&format!("/api/projects/{proj_id}/keywords/{kw_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(st7, StatusCode::NOT_FOUND);

    // 6) job bound to a keyword survives keyword deletion (SET NULL)
    let (st8, b8) = body_to_json(
        app.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(&format!("/api/projects/{proj_id}/keywords"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({ "text": "doomed kw" })).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(st8, StatusCode::CREATED);
    let doomed = b8["id"].as_str().unwrap().to_string();
    let (st9, job) = body_to_json(
        app.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(&format!("/api/projects/{proj_id}/jobs"))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&serde_json::json!({
                        "keyword_id": doomed
                    }))
                    .unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(st9, StatusCode::CREATED);
    let job_id = job["id"].as_str().unwrap().to_string();
    let (st10, _) =
        body_to_json(
            app.clone()
                .oneshot(
                    Request::builder()
                        .method("DELETE")
                        .uri(&format!("/api/projects/{proj_id}/keywords/{doomed}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap(),
        )
        .await;
    assert_eq!(st10, StatusCode::NO_CONTENT);
    let (st11, b11) = body_to_json(
        app.clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(&format!("/api/jobs/{job_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(st11, StatusCode::OK);
    assert_eq!(b11["keyword_id"], serde_json::Value::Null, "job keyword must be SET NULL");
}

// ── F#10 Results engine: project-level results + stats endpoints ──────────
#[tokio::test]
async fn f10_project_results_and_stats_api() {
    let Ok(state) = app_state(None).await else {
        eprintln!("SKIP f10 api results: no Postgres (docker compose up -d)");
        return;
    };
    use ser_db::{JobStatus, ResultKind, Repo};
    let app = router(state.clone());
    let repo = Repo::new(&state.pool);

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();

    // 1) new project via POST
    let (status, p) = body_to_json(
        app.clone().oneshot(
            Request::builder().method("POST").uri("/api/projects")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&serde_json::json!({
                    "name": format!("f10 api results {stamp}") })).unwrap()))
            .unwrap(),
        ).await.unwrap()
    ).await;
    assert_eq!(status, StatusCode::CREATED);
    let proj_id: String = p["id"].as_str().unwrap().to_string();

    // 2) seed results by running jobs through the SAME API the runner uses
    //    (create -> claim -> finish records nothing; so seed via Repo directly
    //    to exercise the join/stats SQL path deterministically).
    let eng = engine::enginespec::parse_engine_file(
        std::path::Path::new(GSA_AARDVARK).into()).expect("parse Aardvark");
    let kwid = repo.add_keyword(uuid::Uuid::parse_str(&proj_id).unwrap(), "f10 api kw")
        .await.unwrap().id;
    let erow = repo.upsert_engine(uuid::Uuid::parse_str(&proj_id).unwrap(), &eng.name,
            &eng.engine_type.to_string(), eng.url.trim(), GSA_AARDVARK,
            eng.steps.len() as i32).await.unwrap();

    let jid = repo.create_job(uuid::Uuid::parse_str(&proj_id).unwrap(),
        Some(erow.id), Some(kwid)).await.unwrap().id;
    repo.claim_next_pending(uuid::Uuid::parse_str(&proj_id).unwrap()).await.unwrap().unwrap();
    repo.record_result(jid, ResultKind::Success, Some("https://ex.com/r1"),
        Some("f10 api kw"), Some("ok"), None).await.unwrap();
    repo.record_result(jid, ResultKind::Success, Some("https://ex.com/r2"),
        Some("f10 api kw"), Some("ok"), None).await.unwrap();
    repo.record_result(jid, ResultKind::Error, None, None, Some("captcha"), None).await.unwrap();
    repo.record_result(jid, ResultKind::Success, Some("https://ex.com/r1"),
        Some("dup"), None, None).await.unwrap(); // duplicate URL
    repo.finish_job(jid, JobStatus::Done, None).await.unwrap();
    let _ = proj_id; // keep

    // 3) GET /api/projects/{id}/results — list comes back, joined fields present
    let (st3, b3) = body_to_json(app.clone().oneshot(
        Request::builder().method("GET")
            .uri(format!("/api/projects/{proj_id}/results"))
            .body(Body::empty()).unwrap()
    ).await.unwrap()).await;
    assert_eq!(st3, StatusCode::OK);
    let arr = b3.as_array().expect("results must be an array");
    assert_eq!(arr.len(), 4, "4 seeded result rows");
    // At least one row should carry engine + keyword (the one bound to a job)
    let with_join = arr.iter().filter(|r| r["engine_name"].is_string() && r["keyword"].is_string()).count();
    assert!(with_join >= 2, "job rows expose engine_name + keyword, got {with_join}");
    // All rows must carry id/job_id/kind/created_at fields (schema contract)
    for r in arr {
        assert!(r["id"].as_str().is_some());
        assert!(r["job_id"].as_str().is_some());
        assert!(r["kind"].as_str().is_some());
        assert!(r["created_at"].as_str().is_some());
    }

    // 4) ?kind=success filter returns only success, no others
    let (st4, b4) = body_to_json(app.clone().oneshot(
        Request::builder().method("GET")
            .uri(format!("/api/projects/{proj_id}/results?kind=success"))
            .body(Body::empty()).unwrap()
    ).await.unwrap()).await;
    assert_eq!(st4, StatusCode::OK);
    let ok_arr = b4.as_array().unwrap();
    assert_eq!(ok_arr.len(), 3, "3 success rows");
    assert!(ok_arr.iter().all(|r| r["kind"].as_str() == Some("success")));

    // 5) ?limit=1 caps the list
    let (st5, b5) = body_to_json(app.clone().oneshot(
        Request::builder().method("GET")
            .uri(format!("/api/projects/{proj_id}/results?limit=1"))
            .body(Body::empty()).unwrap()
    ).await.unwrap()).await;
    assert_eq!(st5, StatusCode::OK);
    assert_eq!(b5.as_array().unwrap().len(), 1);

    // 6) GET /api/projects/{id}/results/stats — summary numbers consistent
    let (st6, s6) = body_to_json(app.clone().oneshot(
        Request::builder().method("GET")
            .uri(format!("/api/projects/{proj_id}/results/stats"))
            .body(Body::empty()).unwrap()
    ).await.unwrap()).await;
    assert_eq!(st6, StatusCode::OK);
    let as_i64 = |k: &str| s6[k].as_i64().unwrap_or_else(|| panic!("missing {k}"));
    assert_eq!(as_i64("total"), 4);
    assert_eq!(as_i64("success"), 3);
    assert_eq!(as_i64("error"), 1);
    assert_eq!(as_i64("info"), 0);
    assert_eq!(as_i64("rejected"), 0);
    assert_eq!(as_i64("total_jobs"), 1);
    // r1 + r2 = 2 distinct success URLs even though r1 appears twice.
    assert_eq!(as_i64("unique_success_urls"), 2);
}

#[tokio::test]
async fn f11_projects_ui_full_crud_api() {
    let Ok(state) = app_state(None).await else {
        eprintln!("SKIP f11 api: no Postgres (docker compose up -d)");
        return;
    };
    let app = router(state);

    // 1) create project
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let name = format!("f11 {stamp}");
    let (status, project) = body_to_json(
        app.clone().oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/projects")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({"name": name, "description": "v1"})).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let proj = project["id"].as_str().unwrap().to_string();

    // 2) PUT rename + new description
    let (status, renamed) = body_to_json(
        app.clone().oneshot(
            Request::builder()
                .method("PUT")
                .uri(&format!("/api/projects/{proj}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({"name": name.clone() + "-renamed", "description": "v2"})).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(renamed["description"], "v2");

    // 3) add a real engine from GSA Aardvark
    let spec = engine::enginespec::parse_engine_file(
        std::path::Path::new(GSA_AARDVARK).into(),
    )
    .expect("Aardvark");
    let (status, engine) = body_to_json(
        app.clone().oneshot(
            Request::builder()
                .method("POST")
                .uri(&format!("/api/projects/{proj}/engines"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "engine_name": spec.name,
                        "engine_type": spec.engine_type.to_string(),
                        "url": spec.url.trim(),
                        "file_path": GSA_AARDVARK,
                        "step_count": spec.steps.len() as i32
                    })).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK); // POST /engines (upsert) returns 200
    let engine_id = engine["id"].as_str().unwrap().to_string();
    assert_eq!(engine["enabled"], true);

    // 4) PATCH (we wired PUT here to match route) — update name/type/url/step_count
    let (status, updated) = body_to_json(
        app.clone().oneshot(
            Request::builder()
                .method("PUT")
                .uri(&format!("/api/projects/{proj}/engines/{engine_id}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "engine_name": spec.name.clone() + " (edit)",
                        "engine_type": spec.engine_type.to_string(),
                        "url": "https://edited.example.org",
                        "step_count": 99
                    })).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updated["step_count"], 99);

    // 5) toggle enabled off, then on again (PUT /enabled)
    let (status, off) = body_to_json(
        app.clone().oneshot(
            Request::builder()
                .method("PUT")
                .uri(&format!("/api/projects/{proj}/engines/{engine_id}/enabled"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&serde_json::json!({"enabled": false})).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(off["enabled"], false);
    let (_, on) = body_to_json(
        app.clone().oneshot(
            Request::builder()
                .method("PUT")
                .uri(&format!("/api/projects/{proj}/engines/{engine_id}/enabled"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&serde_json::json!({"enabled": true})).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(on["enabled"], true);

    // 6) keyword + job create
    let (status, kw) = body_to_json(
        app.clone().oneshot(
            Request::builder()
                .method("POST")
                .uri(&format!("/api/projects/{proj}/keywords"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&serde_json::json!({"text": "f11 job keyword"})).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let keyword_id = kw["id"].as_str().unwrap().to_string();

    let (status, job) = body_to_json(
        app.clone().oneshot(
            Request::builder()
                .method("POST")
                .uri(&format!("/api/projects/{proj}/jobs"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&serde_json::json!({
                    "engine_id": engine_id.clone(),
                    "keyword_id": keyword_id.clone()
                })).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let job_id = job["id"].as_str().unwrap().to_string();
    assert_eq!(job["status"], "pending");

    // 7) claim + cancel (F#11 cancel endpoint)
    let claim_uri = format!("/api/jobs/claim?project_id={proj}");
    let (_, claimed) = body_to_json(app.clone().oneshot(
        Request::builder().method("POST").uri(&claim_uri).body(Body::empty()).unwrap()
    ).await.unwrap()).await;
    assert_eq!(claimed["id"], job_id, "the pending job gets claimed");

    let (status, cancelled) = body_to_json(
        app.clone().oneshot(
            Request::builder()
                .method("POST")
                .uri(&format!("/api/jobs/{job_id}/cancel"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cancelled["status"], "cancelled");

    // second cancel → 409 conflict
    let (status, _) = body_to_json(
        app.clone().oneshot(
            Request::builder().method("POST").uri(&format!("/api/jobs/{job_id}/cancel")).body(Body::empty()).unwrap()
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "terminal job cancel is a no-op (409)");

    // 8) results stats on this project (0 captured yet)
    let (status, stats) = body_to_json(
        app.clone().oneshot(Request::builder().method("GET").uri(&format!("/api/projects/{proj}/results/stats")).body(Body::empty()).unwrap())
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(stats["total"], 0);

    // 9) delete engine → 204; delete again → 404
    let (status, _) = body_to_json(
        app.clone().oneshot(Request::builder().method("DELETE").uri(&format!("/api/projects/{proj}/engines/{engine_id}")).body(Body::empty()).unwrap())
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = body_to_json(
        app.clone().oneshot(Request::builder().method("DELETE").uri(&format!("/api/projects/{proj}/engines/{engine_id}")).body(Body::empty()).unwrap())
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // 10) delete project → 204; project gone
    let (status, _) = body_to_json(
        app.clone().oneshot(Request::builder().method("DELETE").uri(&format!("/api/projects/{proj}")).body(Body::empty()).unwrap())
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = body_to_json(
        app.clone().oneshot(Request::builder().method("GET").uri(&format!("/api/projects/{proj}")).body(Body::empty()).unwrap())
        .await
        .unwrap(),
    )
    .await;
    assert!(matches!(status, StatusCode::NOT_FOUND) || status.is_client_error());
    let _ = claim_uri; // (referenced above)
}
