//! codag-server — real-time capsule daemon.
//!
//! An axum service exposing:
//!   GET  /health                      (liveness)
//!   POST /v1/session/:id/ingest       (stream lines into a session)
//!   GET  /v1/session/:id/capsule      (current capsule; text or json)
//!   GET  /v1/session/:id/subscribe    (SSE live capsule snapshots)
//!   POST /v1/compress                 (stateless one-shot, mirrors the CLI)
//!
//! Each session is an in-memory `codag::StreamingIndex`; idle sessions are
//! evicted by a background TTL sweeper.

use std::time::Instant;

use codag_server::routes;
use codag_server::session::{AppState, SESSION_TTL, SWEEP_INTERVAL};
use tokio::net::TcpListener;

/// Background task: drop sessions idle longer than [`SESSION_TTL`].
async fn ttl_sweeper(state: AppState) {
    let mut iv = tokio::time::interval(SWEEP_INTERVAL);
    iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        iv.tick().await;
        let now = Instant::now();
        let mut sessions = state.sessions.write().await;
        let before = sessions.len();
        sessions.retain(|_, e| now.duration_since(e.last_touch) < SESSION_TTL);
        let evicted = before - sessions.len();
        if evicted > 0 {
            tracing::info!(evicted, remaining = sessions.len(), "ttl sweep");
        }
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "codag_server=info,tower_http=info".into()),
        )
        .init();

    let state = AppState::new();
    tokio::spawn(ttl_sweeper(state.clone()));

    let app = routes::router(state);

    let addr = std::env::var("CODAG_SERVER_ADDR").unwrap_or_else(|_| "0.0.0.0:8088".to_string());
    let listener = TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| panic!("failed to bind {addr}: {e}"));
    tracing::info!(%addr, "codag-server listening");

    axum::serve(listener, app)
        .await
        .expect("server error");
}
