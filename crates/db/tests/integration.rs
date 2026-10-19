//! Integration tests against the live PostgreSQL container ("ser-replica-db"
//! from docker-compose.yml, Windows Docker Desktop).
//!
//! Skipped — not failed — when `SEER_DATABASE_URL` is unreachable, so the
//! suite runs on a box without the container (with a printed hint).

use ser_db::{JobStatus, ResultKind, Repo};
use engine::enginespec::parse_engine_file;

const GSA_AARDVARK: &str =
    r"C:\Users\kingsly\Downloads\Compressed\toCrack\ser_setup\install\Engines\Aardvark Topsites.ini";

async fn pool_or_skip() -> Option<sqlx::PgPool> {
    match ser_db::connect(None).await {
        Ok(p) => Some(p),
        Err(e) => {
            eprintln!(
                "SKIP db integration tests: cannot reach Postgres ({e}).\n\
                 Start it with: docker compose up -d  (from the repo root)"
            );
            None
        }
    }
}

fn stamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let s = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    format!("{:012x}-{:08x}", s.as_secs(), s.subsec_nanos())
}

#[tokio::test]
async fn full_pipeline_project_engines_keywords_jobs_results_history() {
    let Some(pool) = pool_or_skip().await else { return };
    let repo = Repo::new(pool.clone());

    // 1) project
    let name = format!("pipeline test {}", stamp());
    let project = repo.create_project(&name, Some("integration test")).await.unwrap();
    assert_eq!(project.name, name);

    // 2) engines — from the REAL GSA .ini, parsed by the engine crate
    let spec =
        parse_engine_file(std::path::Path::new(GSA_AARDVARK).into()).expect("Aardvark parses");
    let saved = repo
        .upsert_engine(
            project.id,
            &spec.name,
            &spec.engine_type.to_string(),
            spec.url.trim(),
            "Engines/Aardvark Topsites.ini",
            spec.steps.len() as i32,
        )
        .await
        .unwrap();
    assert_eq!(saved.step_count, spec.steps.len() as i32);
    assert!(saved.step_count >= 5);

    // upsert again — same identity, no duplicate row
    let again = repo
        .upsert_engine(
            project.id,
            &spec.name,
            "Directory",
            spec.url.trim(),
            "Engines/Aardvark Topsites.ini",
            spec.steps.len() as i32,
        )
        .await
        .unwrap();
    assert_eq!(again.id, saved.id, "upsert must not create a second row");

    // 3) keywords
    let kw = repo.add_keyword(project.id, "running shoes").await.unwrap();
    let kw2 = repo.add_keyword(project.id, "running shoes").await.unwrap();
    assert_eq!(kw.id, kw2.id, "duplicate keyword must upsert");

    // 4) job lifecycle: create -> claim -> record results -> finish
    let job = repo
        .create_job(project.id, Some(saved.id), Some(kw.id))
        .await
        .unwrap();
    assert_eq!(job.status, "pending");
    assert!(!job.is_terminal());

    let claimed = repo.claim_next_pending(project.id).await.unwrap().expect("pending job exists");
    assert_eq!(claimed.id, job.id);
    assert_eq!(claimed.status, "running");
    assert_eq!(claimed.attempts, 1);
    assert!(claimed.started_at.is_some());

    repo.record_result(
        job.id,
        ResultKind::Success,
        Some("https://aardvark-topsites.com/entry/12345"),
        Some("buy running shoes online"),
        Some("submission confirmed"),
        Some(serde_json::json!({"http": 200})), false)
    .await
    .unwrap();
    let results = repo.results_for_job(job.id).await.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].kind, "success");
    assert_eq!(results[0].payload.as_ref().unwrap()["http"], 200);

    // 5) history / dedup: first sight = true, repeat = false
    let first = repo
        .record_url(project.id, "https://aardvark-topsites.com/entry/12345", Some(&spec.name))
        .await
        .unwrap();
    let second = repo
        .record_url(project.id, "https://aardvark-topsites.com/entry/12345", None)
        .await
        .unwrap();
    assert!(first, "first sight must be flagged new");
    assert!(!second, "repeat sight must not be flagged new");
    assert_eq!(repo.history_len(project.id).await.unwrap(), 1);

    // 6) finish + read back
    repo.finish_job(job.id, JobStatus::Done, None).await.unwrap();
    let done = repo.job_by_id(job.id).await.unwrap().unwrap();
    assert_eq!(done.status, "done");
    assert!(done.is_terminal());
    assert!(done.finished_at.is_some());

    // failed-job path too
    let job2 = repo.create_job(project.id, None, None).await.unwrap();
    repo.claim_next_pending(project.id).await.unwrap().expect("claims job2");
    repo
        .finish_job(job2.id, JobStatus::Failed, Some("time out after 30s"))
        .await
        .unwrap();
    let failed_list = repo
        .list_jobs(project.id, Some(JobStatus::Failed))
        .await
        .unwrap();
    assert_eq!(failed_list.len(), 1);
    assert_eq!(failed_list[0].last_error.as_deref(), Some("time out after 30s"));

    // status filter for Done excludes the failed one
    let done_list = repo.list_jobs(project.id, Some(JobStatus::Done)).await.unwrap();
    assert_eq!(done_list.len(), 1);
    assert_eq!(done_list[0].id, job.id);

    // engines/keywords listings reflect our inserts
    assert_eq!(repo.list_engines(project.id).await.unwrap().len(), 1);
    assert_eq!(repo.list_keywords(project.id).await.unwrap().len(), 1);

    // project lookup round-trip
    let found = repo.project_by_name(&name).await.unwrap().unwrap();
    assert_eq!(found.id, project.id);
}

#[tokio::test]
async fn job_claim_is_atomic_under_concurrency() {
    let Some(pool) = pool_or_skip().await else { return };
    let repo = Repo::new(pool.clone());
    let project = repo
        .create_project(&format!("racing {}", stamp()), None)
        .await
        .unwrap();

    // seed 10 pending jobs
    for _ in 0..10 {
        repo.create_job(project.id, None, None).await.unwrap();
    }

    // 16 "workers" race to claim; exactly 10 jobs may be claimed, each once.
    let mut handles = Vec::new();
    for _ in 0..16 {
        let pool = pool.clone();
        handles.push(tokio::spawn(async move {
            Repo::new(pool.clone()).claim_next_pending(project.id).await
        }));
    }
    let mut claimed_ids = Vec::new();
    for h in handles {
        match h.await.unwrap() {
            Ok(Some(j)) => claimed_ids.push(j.id),
            Ok(None) => {}
            Err(e) => panic!("claim failed: {e}"),
        }
    }
    assert_eq!(claimed_ids.len(), 10, "every pending job claimed exactly once");
    let set: std::collections::HashSet<_> = claimed_ids.iter().collect();
    assert_eq!(set.len(), 10, "no job claimed twice");
}

// ── F#9 keyword management ─────────────────────────────────────────────
#[tokio::test]
async fn f9_keyword_bulk_dedupe_delete() {
    let Some(pool) = pool_or_skip().await else { return };
    let repo = Repo::new(pool.clone());
    let project = repo
        .create_project(&format!("f9 kw {}:", stamp()), None)
        .await
        .unwrap();

    // Plain keyword added twice → ONE row (dedupe on normalized text).
    let a = repo.add_keyword(project.id, "  seo  audit  services ").await.unwrap();
    let b = repo.add_keyword(project.id, "seo audit services").await.unwrap();
    assert_eq!(a.id, b.id, "whitespace-variant keyword must dedupe");
    assert_eq!(a.text, "seo audit services", "whitespace must normalize");
    // Case-variant must ALSO dedupe (GSA keyword lists are case-insensitive).
    let c = repo.add_keyword(project.id, "SEO Audit Services").await.unwrap();
    assert_eq!(a.id, c.id, "case-variant keyword must dedupe");

    // Spintax keyword fans out to concrete variants.
    let (stored, _) = repo
        .add_keywords(project.id, &["cheap {shoes | boots}".to_string()])
        .await
        .unwrap();
    assert!(stored >= 2, "spintax keyword should fan out, got {stored}");
    let kws = repo.list_keywords(project.id).await.unwrap();
    let has = |s: &str| kws.iter().any(|k| k.text == s);
    assert!(has("cheap shoes"), "got: {:?}", kws.iter().map(|k| &k.text).collect::<Vec<_>>());
    assert!(has("cheap boots"));

    // Bulk: existing + new + a spintax in one pass.
    let (stored2, _) = repo
        .add_keywords(
            project.id,
            &[
                "seo audit services".to_string(),   // already exists
                "brand new keyword".to_string(),     // new
                "{a | b | c}".to_string(),           // fans out
            ],
        )
        .await
        .unwrap();
    assert!(stored2 >= 4, "expected >=4 distinct (existing + new + a/b/c), got {stored2}");

    let all = repo.list_keywords(project.id).await.unwrap();
    assert!(all.iter().any(|k| k.text == "brand new keyword"));

    // keyword_by_id round-trip.
    let target = all.iter().find(|k| k.text == "brand new keyword").unwrap().clone();
    let got = repo.keyword_by_id(target.id).await.unwrap();
    assert!(got.is_some());
    assert_eq!(got.unwrap().text, "brand new keyword");

    // delete_keyword: keyword gone, but a job bound to it keeps its id (FK SET NULL semantics).
    let eng = parse_engine_file(std::path::Path::new(GSA_AARDVARK).into()).expect("parse Aardvark");
    let erow = repo
        .upsert_engine(
            project.id,
            &eng.name,
            &eng.engine_type.to_string(),
            eng.url.trim(),
            GSA_AARDVARK,
            eng.steps.len() as i32,
        )
        .await
        .unwrap();
    let job = repo.create_job(project.id, Some(erow.id), Some(target.id)).await.unwrap();
    assert_eq!(job.keyword_id, Some(target.id));
    let deleted = repo.delete_keyword(target.id).await.unwrap();
    assert!(deleted);
    let after = repo.keyword_by_id(target.id).await.unwrap();
    assert!(after.is_none());
    let job_after = repo.job_by_id(job.id).await.unwrap().unwrap();
    assert_eq!(job_after.keyword_id, None, "job keyword must be SET NULL after keyword delete");
}

// ── F#10 Results engine: project-level listings, filters, stats ───────────
#[tokio::test]
async fn f10_project_results_and_stats() {
    let Some(pool) = pool_or_skip().await else { return };
    let repo = Repo::new(pool.clone());

    let project = repo
        .create_project(&format!("f10 results {}", stamp()), None)
        .await
        .unwrap();

    // engine + keyword so results have something to join onto
    let eng = parse_engine_file(std::path::Path::new(GSA_AARDVARK).into()).expect("parse Aardvark");
    let erow = repo
        .upsert_engine(project.id, &eng.name, &eng.engine_type.to_string(), eng.url.trim(),
            GSA_AARDVARK, eng.steps.len() as i32)
        .await
        .unwrap();
    let kw = repo.add_keyword(project.id, "f10 backlink keyword").await.unwrap();

    // Job A — bound to engine + keyword; 2 successes (one URL dup, one new), 1 error.
    let a = repo.create_job(project.id, Some(erow.id), Some(kw.id)).await.unwrap();
    repo.claim_next_pending(project.id).await.unwrap().expect("claim a");
    repo.record_result(a.id, ResultKind::Success, Some("https://ex.com/entry/1"),
        Some("buy f10 backlink keyword"), Some("ok"), None, false).await.unwrap();
    repo.record_result(a.id, ResultKind::Success, Some("https://ex.com/entry/2"),
        Some("f10 backlink keyword"), Some("ok"), None, false).await.unwrap();
    repo.record_result(a.id, ResultKind::Error, None, None,
        Some("captcha wall"), None, false).await.unwrap();
    repo.finish_job(a.id, JobStatus::Done, None).await.unwrap();

    // Job B — no engine/keyword, 1 rejection + 1 info, and a DUPLICATE success URL
    // (same entry/1) to prove unique_success_urls de-duplicates.
    let b = repo.create_job(project.id, None, None).await.unwrap();
    repo.claim_next_pending(project.id).await.unwrap().expect("claim b");
    repo.record_result(b.id, ResultKind::Success, Some("https://ex.com/entry/1"),
        Some("dup url"), None, None, false).await.unwrap();
    repo.record_result(b.id, ResultKind::Rejected, None, None, Some("already listed"), None, false).await.unwrap();
    repo.record_result(b.id, ResultKind::Info, None, None, Some("rate-limited, will retry"), None, false).await.unwrap();
    repo.finish_job(b.id, JobStatus::Done, None).await.unwrap();

    // 1) project_results: all 7 rows present, sorted newest-first, joined to
    //    engine + keyword where the owning job had them.
    let all = repo.project_results(project.id, None, 500).await.unwrap();
    assert_eq!(all.len(), 6, "expected 6 result rows (3+3), got {}", all.len());
    assert!(all[0].created_at >= all[5].created_at, "must be newest-first");

    // Job A rows carry engine_name + keyword; Job B rows don't.
    let a_rows: Vec<_> = all.iter().filter(|r| r.job_id == a.id).collect();
    let b_rows: Vec<_> = all.iter().filter(|r| r.job_id == b.id).collect();
    assert_eq!(a_rows.len(), 3);
    assert_eq!(b_rows.len(), 3);
    for r in &a_rows {
        assert_eq!(r.engine_name.as_deref(), Some(eng.name.as_str()));
        assert_eq!(r.keyword.as_deref(), Some("f10 backlink keyword"));
    }
    for r in &b_rows {
        assert!(r.engine_name.is_none(), "job B has no engine");
        assert!(r.keyword.is_none(), "job B has no keyword");
    }

    // 2) kind filter returns only that kind, in order.
    let successes = repo.project_results(project.id, Some("success"), 500).await.unwrap();
    assert_eq!(successes.len(), 3, "2 unique jobs, 3 success rows total");
    assert!(successes.iter().all(|r| r.kind == "success"));

    let rejects = repo.project_results(project.id, Some("rejected"), 500).await.unwrap();
    assert_eq!(rejects.len(), 1);
    assert_eq!(rejects[0].message.as_deref(), Some("already listed"));

    // Nonexistent kind → empty, not an error.
    let none = repo.project_results(project.id, Some("not_a_kind"), 500).await.unwrap();
    assert!(none.is_empty());

    // 3) limit clamps and never returns more than asked.
    let capped = repo.project_results(project.id, None, 2).await.unwrap();
    assert_eq!(capped.len(), 2, "limit must cap the result set");
    let overcap = repo.project_results(project.id, None, 999999).await.unwrap();
    assert_eq!(overcap.len(), 6, "limit above cardinality returns everything");

    // 4) result_stats: per-kind counts + total + unique success URLs.
    let st = repo.result_stats(project.id).await.unwrap();
    assert_eq!(st.total, 6, "total results");
    assert_eq!(st.success, 3);
    assert_eq!(st.error, 1);
    assert_eq!(st.info, 1);
    assert_eq!(st.rejected, 1);
    assert_eq!(st.total_jobs, 2, "both jobs counted");
    // entry/1 appears in BOTH jobs → only 2 distinct URLs (entry/1, entry/2).
    assert_eq!(st.unique_success_urls, 2, "duplicate URL must count once");

    // 5) cross-project isolation: a different project sees none of these.
    let other = repo.create_project(&format!("f10 other {}", stamp()), None).await.unwrap();
    assert!(repo.project_results(other.id, None, 500).await.unwrap().is_empty());
    let st2 = repo.result_stats(other.id).await.unwrap();
    assert_eq!(st2.total, 0);
    assert_eq!(st2.unique_success_urls, 0);
}

// — F#11 Projects UI polish: full CRUD (project rename/delete, engine update/
// enable-toggle/delete, job cancel) + cascade-delete of children.
#[tokio::test]
async fn f11_project_crud_and_lifecycle() {
    let Some(pool) = pool_or_skip().await else { return };
    let repo = Repo::new(pool.clone());

    // 1) create + rename + re-describe
    let p = repo
        .create_project(&format!("f11 crud {}", stamp()), Some("before rename"))
        .await
        .unwrap();
    let renamed = repo
        .update_project(p.id, &format!("f11 crud renamed {}", stamp()), Some("after rename"))
        .await
        .unwrap();
    assert_eq!(renamed.name.len() > 0, true);
    assert_eq!(renamed.description.as_deref(), Some("after rename"));
    let again = repo.project_by_name(&renamed.name).await.unwrap().unwrap();
    assert_eq!(again.id, p.id);

    // 2) engine: upsert (defaults enabled=true), edit, toggle off/on, delete
    let spec = parse_engine_file(std::path::Path::new(GSA_AARDVARK).into()).expect("Aardvark");
    let e = repo
        .upsert_engine(p.id, &spec.name, &spec.engine_type.to_string(), spec.url.trim(), GSA_AARDVARK, spec.steps.len() as i32)
        .await
        .unwrap();
    assert!(e.enabled, "upserted engine defaults to enabled");

    let edited = repo
        .update_engine(e.id, &format!("{} (edited)", spec.name), &spec.engine_type.to_string(), "https://edited.example.org", 99)
        .await
        .unwrap();
    assert_eq!(edited.step_count, 99);
    assert_eq!(edited.url, "https://edited.example.org");

    let off = repo.set_engine_enabled(e.id, false).await.unwrap();
    assert!(!off.enabled);
    let back = repo.set_engine_enabled(e.id, true).await.unwrap();
    assert!(back.enabled);
    // disabled engines are excluded from list_engines
    let _ = repo.set_engine_enabled(e.id, false).await.unwrap();
    assert!(repo.list_engines(p.id).await.unwrap().iter().all(|x| x.id != e.id), "disabled engine hidden from list");
    let _ = repo.set_engine_enabled(e.id, true).await.unwrap();

    // 3) job: create -> claim -> cancel (F#11 cancel path), status flips to cancelled
    let kw = repo.add_keyword(p.id, "f11 cancel keyword").await.unwrap();
    let j = repo.create_job(p.id, Some(e.id), Some(kw.id)).await.unwrap();
    assert_eq!(j.status, "pending");
    let claimed = repo.claim_next_pending(p.id).await.unwrap().expect("claims the job");
    assert_eq!(claimed.id, j.id);
    match repo.cancel_job(j.id).await {
        Ok(Some(cancelled)) => {
            assert_eq!(cancelled.status, "cancelled");
            assert!(cancelled.finished_at.is_some());
        }
        Ok(None) => panic!("cancel must succeed on a running job"),
        Err(err) => panic!("cancel: {err}"),
    }
    // cancelling an already-terminal job is a NO-OP (None), not an error
    let second = repo.cancel_job(j.id).await.unwrap();
    assert!(second.is_none(), "second cancel must be a no-op");

    // 4) engine delete succeeds
    let gone = repo.delete_engine(e.id).await.unwrap();
    assert!(gone);
    let ghost = repo.delete_engine(e.id).await.unwrap();
    assert!(!ghost, "second delete must report not-found");

    // 5) project delete cascades: keywords/jobs/results all row-gone
    let rjob = repo.create_job(p.id, None, None).await.unwrap();
    // claim+finish so results can exist
    let _ = repo.claim_next_pending(p.id).await.unwrap();
    repo.record_result(rjob.id, ResultKind::Success, Some("https://cascade.example/x"), None, None, None, false).await.unwrap();
    repo.finish_job(rjob.id, JobStatus::Done, None).await.unwrap();
    assert_eq!(repo.results_for_job(rjob.id).await.unwrap().len(), 1);
    assert!(repo.keyword_by_id(kw.id).await.unwrap().is_some());

    let deleted = repo.delete_project(p.id).await.unwrap();
    assert!(deleted);
    assert!(repo.project_by_name(&renamed.name).await.unwrap().is_none());
    assert!(repo.keyword_by_id(kw.id).await.unwrap().is_none(), "keyword must cascade");
    let gone_job = repo.job_by_id(rjob.id).await.unwrap();
    assert!(gone_job.is_none(), "job must cascade");
    let ghost = repo.delete_project(p.id).await.unwrap();
    assert!(!ghost, "second delete reports not-found");
}
