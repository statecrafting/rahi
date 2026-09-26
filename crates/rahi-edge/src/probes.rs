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

/// The future a [`ReadinessCheck`] runs.
type CheckFuture = std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), Error>> + Send>>;

/// One more dependency `/readyz` re-reads on every call, registered by the
/// composer as an [`AppState`] extension (spec 043 B-8): a co-deployed
/// Rauthy that is up and unready fails the cell's readiness, and never ends
/// the process. The check is the real dependency's answer, never a cached
/// verdict.
#[derive(Clone)]
pub struct ReadinessCheck {
    component: &'static str,
    check: std::sync::Arc<dyn Fn() -> CheckFuture + Send + Sync>,
}

impl ReadinessCheck {
    /// A check named `component`, whose `Err` makes the cell not ready.
    pub fn new<F, Fut>(component: &'static str, check: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<(), Error>> + Send + 'static,
    {
        Self {
            component,
            check: std::sync::Arc::new(move || Box::pin(check())),
        }
    }

    /// The component a failure names.
    #[must_use]
    pub fn component(&self) -> &'static str {
        self.component
    }

    /// Run the check.
    ///
    /// # Errors
    ///
    /// The dependency's own error.
    pub async fn run(&self) -> Result<(), Error> {
        (self.check)().await
    }
}

impl std::fmt::Debug for ReadinessCheck {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadinessCheck")
            .field("component", &self.component)
            .finish_non_exhaustive()
    }
}

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
        // Spec 043 D-15: while hiqlite is still applying the log it held at
        // start it answers `Recovering`; the cell reports that as its own
        // state, never as a store that is down, and accepts no work.
        if rahi_store::is_recovering(&err) {
            return recovering(&err);
        }
        return not_ready("store", &err);
    }
    if let Err(err) = state.ledger().head().await {
        return not_ready("ledger", &err);
    }
    if let Some(extra) = state.extension::<ReadinessCheck>()
        && let Err(err) = extra.run().await
    {
        return not_ready(extra.component(), &err);
    }
    json_response(
        StatusCode::OK,
        &json!({ "status": "ready", "store": "up", "ledger": "verified" }),
    )
}

/// The 503 of a store still in startup recovery (spec 043 D-15): not ready,
/// and not down.
fn recovering(error: &Error) -> Response {
    json_response(
        StatusCode::SERVICE_UNAVAILABLE,
        &json!({
            "status": "recovering",
            "component": "store",
            "store": "recovering",
            "message": error.message(),
        }),
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
