//! Integration tests for the codag daemon, driving the axum `Router` directly
//! via `tower::ServiceExt::oneshot` (no real network).

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt; // for `oneshot`

use codag::{compress, CompressorConfig, LogLine, Mode};
use codag_server::routes;
use codag_server::session::{self, AppState};

/// The golden db-pool cascade as raw text lines (level-prefixed where the
/// fixture carries a level), matching `codag`'s internal fixture so the
/// server-side `parse_line` heuristic reproduces the same `LogLine`s.
fn db_pool_cascade_text() -> String {
    let mut lines: Vec<String> = Vec::new();
    for k in 0..30 {
        lines.push(format!("acquired connection from pool, in_use={}", 10 + (k % 5)));
    }
    lines.push("acquired connection from pool, in_use=9000".to_string());
    for k in 0..5 {
        lines.push(format!("acquired connection from pool, in_use={}", 12 + (k % 3)));
    }
    lines.push("warn db connection pool saturated at 95%".to_string());
    lines.push("error db_pool exhausted waiting=12".to_string());
    lines.push(r#"error 10.0.0.1 - - "GET /checkout HTTP/1.1" 503 0"#.to_string());
    lines.push("error query timeout after 30s on /checkout".to_string());
    lines.push("fatal circuit breaker payments OPEN".to_string());
    lines.join("\n")
}

/// The same input, parsed through the *server's* parse_line heuristic, so the
/// expected library output is computed from the exact same `LogLine`s.
fn parsed_lines() -> Vec<LogLine> {
    db_pool_cascade_text()
        .lines()
        .map(session::parse_line)
        .collect()
}

async fn body_string(resp: axum::response::Response) -> String {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn app() -> axum::Router {
    routes::router(AppState::new())
}

#[tokio::test]
async fn health_returns_ok() {
    let resp = app()
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_string(resp).await, "ok");
}

#[tokio::test]
async fn capsule_404_for_unknown_session() {
    let resp = app()
        .oneshot(
            Request::get("/v1/session/nope/capsule")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn ingest_then_capsule_text_parity() {
    let app = app();
    let body = db_pool_cascade_text();

    // POST ingest.
    let resp = app
        .clone()
        .oneshot(
            Request::post("/v1/session/s1/ingest")
                .body(Body::from(body.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ingest_json: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let expected_n = parsed_lines().len();
    assert_eq!(ingest_json["ingested"].as_u64().unwrap() as usize, expected_n);
    assert_eq!(ingest_json["total"].as_u64().unwrap() as usize, expected_n);

    // GET capsule text, balanced.
    let resp = app
        .oneshot(
            Request::get("/v1/session/s1/capsule?format=text&mode=balanced")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let got = body_string(resp).await;

    // The library's batch structural compress at Balanced over the SAME parsed
    // lines — byte-for-byte.
    let mut cfg = CompressorConfig::for_mode(Mode::Balanced);
    cfg.grouper = codag::GrouperKind::Structural; // streaming is structural-only
    let expected = compress(&parsed_lines(), &cfg).render();
    assert_eq!(got, expected, "capsule text != library compress render");
}

#[tokio::test]
async fn compress_oneshot_parity() {
    let body = db_pool_cascade_text();
    let resp = app()
        .oneshot(
            Request::post("/v1/compress?mode=balanced&format=text")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let got = body_string(resp).await;

    // /v1/compress is stateless and mirrors the CLI, which uses the Balanced
    // preset *as-is* (Adaptive grouper). Assert against that exact call.
    let cfg = CompressorConfig::for_mode(Mode::Balanced);
    let expected = compress(&parsed_lines(), &cfg).render();
    assert_eq!(got, expected, "/v1/compress != library compress render");
}

#[tokio::test]
async fn capsule_json_shape() {
    let app = app();
    let body = db_pool_cascade_text();
    app.clone()
        .oneshot(
            Request::post("/v1/session/sj/ingest")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    let resp = app
        .oneshot(
            Request::get("/v1/session/sj/capsule?format=json&mode=balanced")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(json.get("lines").unwrap().is_array());
    assert!(json.get("original_count").is_some());
    assert!(json.get("kept_count").is_some());
    assert!(json.get("line_compression").is_some());
    assert!(json.get("rendered").is_some());
}

#[tokio::test]
async fn ingest_counts_accumulate_across_two_calls() {
    let app = app();

    let r1 = app
        .clone()
        .oneshot(
            Request::post("/v1/session/acc/ingest")
                .body(Body::from("alpha one\nbeta two\n"))
                .unwrap(),
        )
        .await
        .unwrap();
    let j1: serde_json::Value = serde_json::from_str(&body_string(r1).await).unwrap();
    assert_eq!(j1["ingested"].as_u64().unwrap(), 2);
    assert_eq!(j1["total"].as_u64().unwrap(), 2);

    let r2 = app
        .clone()
        .oneshot(
            Request::post("/v1/session/acc/ingest")
                .body(Body::from("gamma three\n"))
                .unwrap(),
        )
        .await
        .unwrap();
    let j2: serde_json::Value = serde_json::from_str(&body_string(r2).await).unwrap();
    assert_eq!(j2["ingested"].as_u64().unwrap(), 1);
    assert_eq!(j2["total"].as_u64().unwrap(), 3);
}

#[tokio::test]
async fn ingest_ndjson_format() {
    let app = app();
    let body = "{\"message\":\"boom\",\"level\":\"error\"}\n{\"message\":\"again\",\"level\":\"error\"}\n";
    let r = app
        .oneshot(
            Request::post("/v1/session/nd/ingest?format=ndjson")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let j: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    assert_eq!(j["ingested"].as_u64().unwrap(), 2);
}
