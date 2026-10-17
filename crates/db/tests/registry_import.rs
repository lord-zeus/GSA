//! Feature #3 — engine catalog import, verified against the REAL GSA corpus.
//!
//! Skipped (not failed) when Postgres or the GSA install is absent.

use ser_db::{Registry, CatalogEngine};

const GSA_ROOT: &str =
    r"C:\Users\kingsly\Downloads\GSA_SER_v18.20_FINAL\GSA_SER_v18.20_FINAL";
const WEB_COUNT: u32 = 825;
const EMAIL_COUNT: u32 = 217;

async fn pool_or_skip() -> Option<sqlx::PgPool> {
    match ser_db::connect(None).await {
        Ok(p) => Some(p),
        Err(e) => {
            eprintln!("SKIP registry tests: cannot reach Postgres ({e})");
            None
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn import_full_gsa_corpus_idempotent() {
    let Some(pool) = pool_or_skip().await else { return };
    let reg = Registry::new(&pool);
    if !std::path::Path::new(GSA_ROOT).is_dir() {
        eprintln!("SKIP: GSA install not found at {GSA_ROOT}");
        return;
    }

    // 1) import the full real corpus (1,042 engines)
    let s1 = reg.import_gsa_root(GSA_ROOT).await.unwrap();
    assert_eq!(s1.len(), 2, "web + email imports");
    let web = s1.iter().find(|s| s.category == "web").unwrap();
    let mail = s1.iter().find(|s| s.category == "email").unwrap();
    assert_eq!((web.scanned, web.imported, web.failed), (WEB_COUNT, WEB_COUNT, 0));
    assert_eq!((mail.scanned, mail.imported, mail.failed), (EMAIL_COUNT, EMAIL_COUNT, 0));

    let total = reg.count().await.unwrap();
    assert_eq!(total, 1042, "full corpus in catalog");

    // 2) import AGAIN — idempotent: same rows, no duplicates, no drift
    let s2 = reg.import_gsa_root(GSA_ROOT).await.unwrap();
    let web2 = s2.iter().find(|s| s.category == "web").unwrap();
    assert_eq!(web2.imported, WEB_COUNT);
    assert_eq!(reg.count().await.unwrap(), 1042, "re-import must not duplicate");

    // 3) read back a known engine (Aardvark) and check its shape
    let all = reg.list(None, None, 5000, 0).await.unwrap();
    assert_eq!(all.len(), 1042);
    let aardvark = all.iter().find(|e| e.engine_name.eq_ignore_ascii_case("aardvark topsites"))
        .expect("Aardvark Topsites present");
    assert_eq!(aardvark.category, "web");
    assert!(aardvark.step_count >= 5, "Aardvark is multi-step");
    // (some engines — e.g. Aardvark — use a spun-subdomain pattern instead of
    //  a fixed base url, so `url` is not guaranteed non-empty)
    assert!(aardvark.file_path.starts_with("Engines/"), "rel path is namespaced");

    // 4) type filter + search both return sane subsets
    let dirs = reg.list(Some("directory"), None, 5000, 0).await.unwrap();
    assert!(!dirs.is_empty(), "directories exist in the corpus");
    assert!(dirs.iter().all(|e| e.engine_type.contains("irectory")));

    // note: GSA SER stores the target URL as a `%url%` step token, so the
    //   top-level base-url field is empty across the corpus — verify step count
    //   instead (that's the real signal a real engine spec was parsed).
    assert!(
        all.iter().filter(|e| e.step_count >= 3).count() > 500,
        "majority of catalog should be multi-step real engine specs"
    );

    let found = reg.list(None, Some("aardvark"), 100, 0).await.unwrap();
    assert_eq!(found.len(), 1);

    // 5) breakdown is exhaustive: sums back to 1042
    let bd = reg.type_breakdown().await.unwrap();
    assert!(bd.len() >= 5, "many engine types in corpus; got {bd:?}");
    let sum: i64 = bd.iter().map(|(_, c)| c).sum();
    assert_eq!(sum, 1042);

    // 6) unknown id -> None (no panic)
    assert!(reg.get(9_999_999_999).await.unwrap().is_none());
}
