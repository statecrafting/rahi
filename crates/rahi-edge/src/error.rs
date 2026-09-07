//! One mapping from [`rahi_types::Error`] to an HTTP response (spec 020 B-8).
//!
//! The status table lives here and nowhere else, so a variant added to the
//! workspace error is a compile error in one file rather than a different
//! answer on every route.
//!
//! The orphan rule keeps B-8's literal signature out of reach: `IntoResponse`
//! belongs to `axum` and `Error` to `rahi-types`, so neither is local to this
//! crate and `impl IntoResponse for rahi_types::Error` cannot be written
//! here. [`EdgeError`] is the newtype that carries the same mapping, and
//! [`status_of`] exposes the table itself to a caller that has its own
//! response to build (spec 020 D-1).

use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use rahi_types::Error;
use serde_json::json;

/// The `Content-Type` every refusal this crate builds carries.
pub const PROBLEM_CONTENT_TYPE: &str = "application/json";

/// The status B-8 fixes for each variant of the workspace error.
///
/// `Upstream` is the one 502: a co-deployed or remote dependency failed and
/// the cell is the gateway in front of it. `Io` and `Config` are 500, because
/// what failed is this process (spec 020 D-2).
#[must_use]
pub const fn status_of(error: &Error) -> StatusCode {
    match error {
        Error::Validation(_) => StatusCode::BAD_REQUEST,
        Error::Unauthorized(_) => StatusCode::UNAUTHORIZED,
        Error::Denied(_) => StatusCode::FORBIDDEN,
        Error::NotFound(_) => StatusCode::NOT_FOUND,
        Error::Conflict(_) => StatusCode::CONFLICT,
        Error::Stale(_) => StatusCode::SERVICE_UNAVAILABLE,
        Error::Integrity(_) | Error::Io(_) | Error::Config(_) => StatusCode::INTERNAL_SERVER_ERROR,
        Error::Upstream(_) => StatusCode::BAD_GATEWAY,
    }
}

/// The decision id a denial's message starts with, when it carries one.
///
/// The kernel returns `Error::Denied` whose message is `"<id>: <reason>"`
/// (spec 015 B-6), and an id has no whitespace and at least one `:` of its
/// own. A denial raised anywhere else has no id and gets none in the body.
#[must_use]
pub fn decision_id(error: &Error) -> Option<&str> {
    let Error::Denied(message) = error else {
        return None;
    };
    let (id, _reason) = message.split_once(": ")?;
    (!id.is_empty() && id.contains(':') && !id.contains(char::is_whitespace)).then_some(id)
}

/// The response body: the variant's stable label, its message, and, for a
/// denial the kernel raised, the id of the decision the chain holds.
///
/// Messages are the error's own string (B-8). Nothing else is disclosed: no
/// source chain, no location, no backtrace.
#[must_use]
pub fn body(error: &Error) -> serde_json::Value {
    match decision_id(error) {
        Some(id) => json!({
            "error": error.kind(),
            "message": error.message(),
            "decision": id,
        }),
        None => json!({
            "error": error.kind(),
            "message": error.message(),
        }),
    }
}

/// The whole response for `error`: the status from [`status_of`] and the body
/// from [`body`].
#[must_use]
pub fn response(error: &Error) -> Response {
    build(status_of(error), body(error))
}

/// A refusal the middleware raises on its own account, in the same envelope.
///
/// A CSRF mismatch and a spent rate-limit window are not workspace errors:
/// nothing failed, the request was refused. They still answer in the shape a
/// client already parses.
#[must_use]
pub fn refusal(status: StatusCode, kind: &str, message: &str) -> Response {
    build(status, json!({ "error": kind, "message": message }))
}

fn build(status: StatusCode, body: serde_json::Value) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, PROBLEM_CONTENT_TYPE)],
        body.to_string(),
    )
        .into_response()
}

/// A workspace error on its way out of a handler.
///
/// Handlers return `Result<T, EdgeError>` and lean on `?`: every conversion
/// from [`Error`] lands here, and every response it produces comes from the
/// one table above.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EdgeError(pub Error);

/// The result type a chassis handler returns.
pub type EdgeResult<T> = Result<T, EdgeError>;

impl From<Error> for EdgeError {
    fn from(error: Error) -> Self {
        Self(error)
    }
}

impl std::fmt::Display for EdgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for EdgeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

impl IntoResponse for EdgeError {
    fn into_response(self) -> Response {
        response(&self.0)
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;

    /// Spec 020 FR-005: every variant maps to the status B-8 names.
    #[test]
    fn every_variant_maps_to_its_status() {
        let table = [
            (Error::Validation("v".to_owned()), 400),
            (Error::Unauthorized("u".to_owned()), 401),
            (Error::Denied("d".to_owned()), 403),
            (Error::NotFound("n".to_owned()), 404),
            (Error::Conflict("c".to_owned()), 409),
            (Error::Stale("s".to_owned()), 503),
            (Error::Integrity("i".to_owned()), 500),
            (Error::Io("io".to_owned()), 500),
            (Error::Config("cfg".to_owned()), 500),
            (Error::Upstream("up".to_owned()), 502),
        ];
        for (error, status) in table {
            assert_eq!(
                status_of(&error).as_u16(),
                status,
                "{} maps to {status}",
                error.kind()
            );
        }
    }

    #[test]
    fn an_integrity_body_says_integrity() {
        let body = body(&Error::Integrity("the chain forked".to_owned()));
        assert_eq!(body["error"], "integrity");
        assert_eq!(body["message"], "the chain forked");
    }

    #[test]
    fn a_kernel_denial_carries_its_decision_id() {
        let denied = Error::Denied(
            "kernel:0123456789abcdef:000000000001: no grant covers db.write on audit".to_owned(),
        );
        assert_eq!(
            decision_id(&denied),
            Some("kernel:0123456789abcdef:000000000001")
        );
        assert_eq!(
            body(&denied)["decision"],
            "kernel:0123456789abcdef:000000000001"
        );
    }

    #[test]
    fn a_denial_from_elsewhere_carries_no_decision_id() {
        assert_eq!(decision_id(&Error::Denied("no".to_owned())), None);
        assert_eq!(
            decision_id(&Error::Denied(
                "not an id: because it has spaces".to_owned()
            )),
            None
        );
        assert!(
            body(&Error::Denied("no".to_owned()))
                .get("decision")
                .is_none()
        );
    }

    #[test]
    fn a_message_is_the_errors_own_string() {
        let error = Error::Validation("title must not be empty".to_owned());
        assert_eq!(body(&error)["message"], "title must not be empty");
    }
}
