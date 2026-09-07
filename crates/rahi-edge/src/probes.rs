//! Liveness and readiness (spec 020 B-6).
//!
//! The two probes answer different questions and that difference is the whole
//! point. `/healthz` says the process is running its event loop: it touches
//! no dependency, so a store outage never restarts a container that is
//! perfectly able to serve again the moment the store returns. `/readyz` says
//! the cell can serve: it reads the store's health and the ledger's head, and
//! names the component that failed when it cannot.
//!
//! The harness (spec 033) waits on `/readyz` to know a cell is up rather than
//! merely listening, which is why the readiness answer touches the real
//! dependencies rather than a cached verdict.

use axum::Router;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use rahi_types::Error;
use serde_json::json;

use crate::state::AppState;

/// The liveness path.
pub const HEALTHZ_PATH: &str = "/healthz";
/// The readiness path.
pub const READYZ_PATH: &str = "/readyz";

/// The two probe routes, to be merged outside the CSRF and rate-limit layers.
pub fn router() -> Router<AppState> {
    Router::new()
        .route(HEALTHZ_PATH, get(healthz))
        .route(READYZ_PATH, get(readyz))
}

/// Liveness: the process answers. Nothing else is asserted and nothing is
/// touched.
async fn healthz() -> Response {
    json_response(StatusCode::OK, &json!({ "status": "alive" }))
}

/// Readiness: the store is healthy and the chain is readable.
///
/// The chain head is read through the leader (spec 013), so a cell that
/// cannot reach a quorum reports not ready rather than accepting writes it
/// would fail to ledger. Verification itself is a boot-time property that
/// already failed closed (constitution XI); what readiness re-asks is whether
/// the verified chain is still reachable.
async fn readyz(State(state): State<AppState>) -> Response {
    if let Err(err) = state.store().health().await {
        return not_ready("store", &err);
    }
    if let Err(err) = state.ledger().head().await {
        return not_ready("ledger", &err);
    }
    json_response(
        StatusCode::OK,
        &json!({ "status": "ready", "store": "up", "ledger": "verified" }),
    )
}

/// The 503 that names the component that failed and why.
fn not_ready(component: &str, error: &Error) -> Response {
    json_response(
        StatusCode::SERVICE_UNAVAILABLE,
        &json!({
            "status": "not_ready",
            "component": component,
            "error": error.kind(),
            "message": error.message(),
        }),
    )
}

fn json_response(status: StatusCode, body: &serde_json::Value) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, crate::error::PROBLEM_CONTENT_TYPE)],
        body.to_string(),
    )
        .into_response()
}
