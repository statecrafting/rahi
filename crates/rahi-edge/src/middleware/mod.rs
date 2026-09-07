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
/// It is a pass-through today and named anyway, because the position is the
/// contract. Spans, metrics, and the request log land here, outside every
/// refusal, so that a 403 from the CSRF check and a 429 from the limiter are
/// observed like any other answer.
pub async fn observation(request: Request, next: Next) -> Response {
    next.run(request).await
}
