//! API server binary — `cargo run -p ser-api`.
//!
//! Reads SEER_DATABASE_URL, builds the pool, serves the REST API, and
//! (when web/dist exists) serves the built Vue client as the same-origin
//! fallback — one process ships UI + API.
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::ServeDir;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let bind: std::net::SocketAddr = std::env::var("SEER_API_BIND")
        .unwrap_or_else(|_| "127.0.0.1:8080".into())
        .parse()?;

    let dsn = std::env::var("SEER_DATABASE_URL").ok();
    let state = ser_api::app_state(dsn.as_deref()).await?;

    // F#17 — background scheduler loop. Ticks every SEER_SCHEDULER_TICK_MS
    // (default 30 s); fires enabled schedules whose `next_run_at` is <= now,
    // claiming the slot atomically (restart-safe, no double-fire). The task
    // outlives this scope — the runtime keeps it alive for the process
    // lifetime. Disable with SEER_SCHEDULER_DISABLE=1 (debugging).
    if std::env::var("SEER_SCHEDULER_DISABLE").map(|v| v == "1" || v.eq_ignore_ascii_case("true")).unwrap_or(false) {
        eprintln!("ser-api: scheduler DISABLED (SEER_SCHEDULER_DISABLE set)");
    } else {
        let h = ser_api::scheduler::spawn(state.clone());
        eprintln!("ser-api: scheduler spawned (handle {}); tick_ms from SEER_SCHEDULER_TICK_MS (default 30000)", h.id());
    }

    let mut app = ser_api::router(state).layer(cors());

    // Optional same-origin Vue build (production path, crash isolation intact:
    // a UI-serving fault cannot kill the API handlers; a worker/service crash
    // cannot kill this process or the browser shell).
    // Optional same-origin Vue build. SPA-aware: real files (assets) are
    // served by ServeDir; unknown paths (client routes like /engines) fall
    // back to index.html so vue-router handles them.
    let dist = std::env::var("SEER_WEB_DIST")
        .ok()
        .map(std::path::PathBuf::from)
        .or_else(|| {
            let p = std::path::PathBuf::from("web/dist");
            (p.is_dir()).then_some(p)
        });

    if let Some(d) = dist {
        // SPA fallback reads index.html fresh per request so a rebuilt frontend
        // (bumped asset hashes) is served without a process restart.
        let index_path = d.join("index.html");
        let spa_fallback = {
            let path = index_path;
            axum::routing::get(move || async move {
                match tokio::fs::read(&path).await {
                    Ok(idx) => (
                        [
                            (
                                axum::http::header::CONTENT_TYPE,
                                "text/html; charset=utf-8",
                            ),
                        ],
                        idx,
                    ),
                    Err(_) => (
                        [(axum::http::header::CONTENT_TYPE, "text/plain")],
                        b"index.html not found (web/dist missing index.html)".to_vec(),
                    ),
                }
            })
        };
        app = app.fallback_service(
            ServeDir::new(d)
                .append_index_html_on_directories(true)
                .fallback(spa_fallback),
        );
    }

    let listener = tokio::net::TcpListener::bind(bind).await?;
    eprintln!("ser-api listening on http://{bind}");
    axum::serve(listener, app).await?;
    Ok(())
}

fn cors() -> CorsLayer {
    // Open for local dev (Vite :5173, Tauri webview); production is same-origin.
    CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any)
}
