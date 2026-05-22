//! HTTP routes for the codag real-time capsule daemon.
//!
//! ```text
//! GET  /health                          -> "ok"
//! POST /v1/session/:id/ingest           -> {"ingested": n, "total": m}
//! GET  /v1/session/:id/capsule          -> rendered text | json capsule (404 if absent)
//! GET  /v1/session/:id/subscribe        -> SSE stream of periodic capsule snapshots
//! POST /v1/compress                     -> stateless one-shot capsule (text | json)
//! ```
//!
//! `mode=lossless|balanced|aggressive` (default balanced) selects the projection
//! aggressiveness; `format=text|json` (default text) selects the response shape.

use std::convert::Infallible;
use std::time::Instant;

use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{header::CONTENT_TYPE, HeaderMap, StatusCode},
    response::sse::{Event, KeepAlive, Sse},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use codag::{compress, CompressorConfig, Mode};
use serde::Deserialize;
use serde_json::json;
use tokio_stream::StreamExt;

use crate::session::{parse_body, AppState, BodyFormat, SessionEntry, SSE_INTERVAL};

/// Build the application router.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/session/:id/ingest", post(ingest))
        .route("/v1/session/:id/capsule", get(capsule))
        .route("/v1/session/:id/subscribe", get(subscribe))
        .route("/v1/compress", post(compress_oneshot))
        .with_state(state)
}

/// Common query string: `?mode=...&format=...`.
#[derive(Debug, Deserialize, Default)]
pub struct CapsuleQuery {
    mode: Option<String>,
    format: Option<String>,
}

fn parse_mode(s: Option<&str>) -> Mode {
    match s.map(|m| m.to_ascii_lowercase()).as_deref() {
        Some("lossless") => Mode::Lossless,
        Some("aggressive") => Mode::Aggressive,
        // default + explicit "balanced" + anything unrecognized
        _ => Mode::Balanced,
    }
}

fn wants_json(format: Option<&str>) -> bool {
    matches!(format.map(|f| f.to_ascii_lowercase()).as_deref(), Some("json"))
}

/// Decide the ingest body format: explicit `?format=json|ndjson`, else sniff the
/// `Content-Type` header (`application/x-ndjson` / `application/json`), else text.
fn body_format(headers: &HeaderMap, format: Option<&str>) -> BodyFormat {
    if let Some(f) = format.map(|f| f.to_ascii_lowercase()) {
        match f.as_str() {
            "json" | "ndjson" => return BodyFormat::Ndjson,
            "text" => return BodyFormat::Text,
            _ => {}
        }
    }
    let ct = headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if ct.contains("ndjson") || ct.contains("application/json") {
        BodyFormat::Ndjson
    } else {
        BodyFormat::Text
    }
}

// --------------------------------------------------------------------------
// Handlers
// --------------------------------------------------------------------------

async fn health() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

async fn ingest(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<CapsuleQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let fmt = body_format(&headers, q.format.as_deref());
    let text = String::from_utf8_lossy(&body);
    let new_lines = parse_body(&text, fmt);
    let ingested = new_lines.len();

    let mut sessions = state.sessions.write().await;
    let entry = sessions
        .entry(id.clone())
        .or_insert_with(SessionEntry::new);
    for l in new_lines {
        entry.index.push(l);
    }
    entry.last_touch = Instant::now();
    let total = entry.index.len();

    tracing::info!(session = %id, ingested, total, ?fmt, "ingest");
    Json(json!({ "ingested": ingested, "total": total })).into_response()
}

async fn capsule(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<CapsuleQuery>,
) -> Response {
    let cfg = CompressorConfig::for_mode(parse_mode(q.mode.as_deref()));
    let sessions = state.sessions.read().await;
    let Some(entry) = sessions.get(&id) else {
        return (StatusCode::NOT_FOUND, format!("no such session: {id}")).into_response();
    };
    let result = entry.index.capsule_with(&cfg);
    if wants_json(q.format.as_deref()) {
        Json(result.as_json()).into_response()
    } else {
        result.render().into_response()
    }
}

/// SSE: emit a capsule snapshot immediately and then every [`SSE_INTERVAL`].
///
/// Snapshots are full re-renders, not deltas. Delta streaming (emit only
/// changed output lines since the last tick) is a deliberate follow-up — it
/// requires a stable per-output-line identity across projections, which the
/// current `CompressionResult` does not expose.
async fn subscribe(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<CapsuleQuery>,
) -> Response {
    {
        // 404 up front if the session doesn't exist yet.
        let sessions = state.sessions.read().await;
        if !sessions.contains_key(&id) {
            return (StatusCode::NOT_FOUND, format!("no such session: {id}")).into_response();
        }
    }
    let cfg = CompressorConfig::for_mode(parse_mode(q.mode.as_deref()));
    let json = wants_json(q.format.as_deref());

    // Tick immediately, then every SSE_INTERVAL.
    let ticker = tokio_stream::wrappers::IntervalStream::new({
        let mut iv = tokio::time::interval(SSE_INTERVAL);
        iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        iv
    });

    // `.then` runs an async projection per tick — we await the (async) read
    // lock properly rather than blocking a worker thread.
    let stream = ticker.then(move |_| {
        let cfg = cfg.clone();
        let id = id.clone();
        let state = state.clone();
        async move {
            let data = {
                let sessions = state.sessions.read().await;
                match sessions.get(&id) {
                    Some(entry) => {
                        let result = entry.index.capsule_with(&cfg);
                        if json {
                            serde_json::to_string(&result.as_json())
                                .unwrap_or_else(|_| "{}".to_string())
                        } else {
                            result.render()
                        }
                    }
                    None => String::new(),
                }
            };
            Ok::<Event, Infallible>(Event::default().event("capsule").data(data))
        }
    });

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

async fn compress_oneshot(
    Query(q): Query<CapsuleQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let fmt = body_format(&headers, q.format.as_deref());
    let text = String::from_utf8_lossy(&body);
    let lines = parse_body(&text, fmt);
    let cfg = CompressorConfig::for_mode(parse_mode(q.mode.as_deref()));
    let result = compress(&lines, &cfg);
    if wants_json(q.format.as_deref()) {
        Json(result.as_json()).into_response()
    } else {
        result.render().into_response()
    }
}
