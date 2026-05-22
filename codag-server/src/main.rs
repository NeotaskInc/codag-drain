//! codag-server — real-time capsule daemon.
//!
//! Phase 3 deliverable: an axum service exposing
//!   POST /v1/session/:id/ingest      (stream lines)
//!   GET  /v1/session/:id/capsule     (current capsule)
//!   GET  /v1/session/:id/subscribe   (SSE live capsule)
//!   POST /v1/compress                (stateless one-shot)
//! Scaffold for now — the library neck (codag) is built and proven first.

fn main() {
    println!("codag-server scaffold — daemon lands in Phase 3 (see plan).");
}
