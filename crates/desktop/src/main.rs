//! Native Tauri desktop shell for the SER Replica suite.
//!
//! `cargo run -p ser-desktop` opens a real OS window that:
//!   1. boots the Rust API in-process on a loopback port (no separate server
//!      process needed — the UI, API, and DB pool all live here), and
//!   2. loads the pre-built Vue SPA served same-origin by that API.
//!
//! Crash isolation is preserved: the API runs in its own tokio task; a UI
//! navigation or a handler panic cannot take down the window or the other.
use std::path::Path;

use axum::routing::get;
use tower_http::services::ServeDir;

/// Absolute path to the Vue build, resolved at build time so the desktop
/// binary works no matter what the CWD is at launch. The crate lives at
/// crates/desktop, the SPA at web/dist — six parents up reaches the repo root.
fn web_dist() -> std::path::PathBuf {
    if let Some(d) = std::env::var("SEER_WEB_DIST").ok() {
        let p = Path::new(&d);
        if p.is_dir() {
            return p.to_path_buf();
        }
    }
    let rel = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join("web/dist");
    let abs = rel
        .canonicalize()
        .ok()
        .or_else(|| {
            // Fall back to a CWD-relative path (e.g. running from the repo root).
            Path::new("web/dist").canonicalize().ok()
        });
    abs.unwrap_or(rel)
}

#[tokio::main]
async fn main() {
    // --- 1. Boot the API in-process on a free loopback port ------------------
    let dsn = std::env::var("SEER_DATABASE_URL").ok();
    let dist = web_dist();
    let index = tokio::fs::read(dist.join("index.html")).await.unwrap_or_default();

    let state = ser_api::app_state(dsn.as_deref()).await.unwrap_or_else(|e| {
        eprintln!("ser-desktop: failed to connect to DB: {e}");
        std::process::exit(2);
    });

    // SPA-aware static serving, identical to ser-api's binary.
    let app = {
        let spa_fallback = get(move || async move {
            (
                [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
                index,
            )
        });
        ser_api::router(state.clone())
            .fallback_service(
                ServeDir::new(&dist)
                    .append_index_html_on_directories(true)
                    .fallback(spa_fallback),
            )
    };

    // Bind to a free loopback port (0 = OS assigns).
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind loopback");
    let port = listener.local_addr().expect("local addr").port();
    eprintln!("ser-desktop: API + UI serving on http://127.0.0.1:{port}");

    let serve = app;
    let srv = tokio::spawn(async move {
        axum::serve(listener, serve)
            .await
            .expect("axum server task ended");
    });

    // --- 2. Open the native window pointed at that URL ------------------------
    let start_url = format!("http://127.0.0.1:{port}/");
    tauri::Builder::default()
        .setup(move |app| {
            // Tauri v2 takes the URL in the constructor's third arg (a
            // WebviewUrl), not as a builder method.
            let window = tauri::WebviewWindowBuilder::new(
                app,
                "main",
                tauri::WebviewUrl::External(
                url::Url::parse(&start_url).expect("valid start url"),
            )
            )
            .title("SER Replica — Search Engine Ranker")
            .inner_size(1280.0, 820.0)
            .min_inner_size(900.0, 560.0)
            .center()
            .build()?;
            let _ = window.set_focus();
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("Tauri run loop ended");

    let _ = srv.await;
}
