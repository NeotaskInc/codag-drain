//! codag-drain reference server library surface.
//!
//! The thin template host's HTTP routing and session state, exposed as a
//! library so integration tests can drive the [`routes::router`] directly (via
//! `tower::ServiceExt::oneshot`) without a real network, and so the binary
//! (`main.rs`) is a thin wrapper around it.

pub mod routes;
pub mod session;
