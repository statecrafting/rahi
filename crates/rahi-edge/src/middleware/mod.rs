//! The middleware chain, in the order B-2 fixes and the app cannot change.
//!
//! Outermost to innermost: [`observation`] (spec 023's layer, a pass-through
//! until it lands), [`security_headers`], [`csrf`], [`rate_limit`], and then
//! whatever the app mounted. The probes and, when spec 023 adds it,
//! `/metrics` sit outside the CSRF and rate-limit layers: a readiness check
//! that could be rate limited, or that had to carry a token, would answer a
//! question about the check rather than about the cell.

pub mod csrf;
pub mod rate_limit;
pub mod security_headers;

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;

/// The outermost layer: observation (spec 023).
///
/// The position was the contract before there was anything to put in it.
/// Spec 023 filled it: the request span opens here and the answer is counted
/// here, outside every refusal, so that a 403 from the CSRF check and a 429
/// from the limiter are observed like any other answer. Before
/// `obs::init` has run, and for the paths spec 023 B-5 leaves alone, it is
/// still a pass-through.
pub async fn observation(request: Request, next: Next) -> Response {
    crate::obs::observe(request, next).await
}
