//! The scope gate, and the union of scopes it publishes (spec 025 B-9, B-2).
//!
//! A scope is what an access token was granted, and [`RequireScope`] is the
//! layer that refuses a request whose token was not granted enough. It sits
//! beside spec 022's `RequireRole` and is not a substitute for it: a role is
//! what the IdP says a person *is*, a scope is what a client was *authorised
//! to do on their behalf*, and an agent runtime holding a token for a user
//! with every role in the deployment still reaches only what its scopes name.
//!
//! **Matching is exact.** Scopes are space-delimited per RFC 6749 §3.3, and
//! `notes` never admits `notes.write`, `notes:read` never admits `notes`, and
//! no prefix or wildcard rule exists to be reasoned about at three in the
//! morning. A gate that requires `notes.write` is satisfied by the string
//! `notes.write` appearing in the token's `scope` claim and by nothing else.
//!
//! Every gate declares its scope into a process-wide union as it is built,
//! and [`crate::resource`] serves that union as `scopes_supported`. A route
//! therefore publishes what it requires by existing, and the metadata
//! document cannot fall behind the routes it describes.

use std::collections::BTreeSet;
use std::sync::{Mutex, PoisonError};

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, header};
use axum::middleware::{Next, from_fn_with_state};
use axum::response::Response;
use rahi_kernel::Kernel;
use rahi_types::{Error, Principal};

use crate::bearer::Bearer;
use crate::session::answer;

/// The decision kind a scope refusal is recorded under.
pub const DECISION_SCOPE_DENIED: &str = "resource.scope";
/// The RFC 6750 error code a scope refusal answers with (B-6).
pub const ERROR_INSUFFICIENT_SCOPE: &str = "insufficient_scope";

/// The scopes in a `scope` claim: space-delimited, deduplicated, ordered.
///
/// RFC 6749 §3.3 makes the claim a space-delimited list. Any run of ASCII
/// whitespace separates here, because a token that arrived with a tab in it
/// was still granted the scopes on either side of it and refusing to read
/// them would be a parser's opinion about somebody else's document.
#[must_use]
pub fn parse(claim: &str) -> BTreeSet<String> {
    claim
        .split_whitespace()
        .filter(|scope| !scope.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The scope gate: one scope, and the kernel that records a refusal (B-9).
#[derive(Clone)]
pub struct RequireScope {
    scope: &'static str,
    kernel: Kernel,
}

impl std::fmt::Debug for RequireScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequireScope")
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}

impl RequireScope {
    /// Require `scope`, recording every refusal in `kernel`'s chain.
    ///
    /// Building the gate declares the scope into the process-wide union
    /// [`supported`] answers with, which is what B-2's `scopes_supported`
    /// serves: the declaration happens where the requirement is stated.
    #[must_use]
    pub fn new(scope: &'static str, kernel: Kernel) -> Self {
        declare(scope);
        Self { scope, kernel }
    }

    /// The scope this gate requires.
    #[must_use]
    pub const fn scope(&self) -> &'static str {
        self.scope
    }
}

/// Wrap `router` in a scope gate (B-9).
///
/// The gate reads the credential the bearer layer resolved, so it goes
/// inside [`crate::bearer::with_bearer`], never outside it.
pub fn with_scope<S>(require: RequireScope, router: Router<S>) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    router.layer(from_fn_with_state(require, enforce_scope))
}

/// Answer 403 with a ledgered decision when the token lacks the scope (B-9).
///
/// A request with no principal at all is 401: the difference between "you
/// presented nothing" and "you presented something that does not reach this"
/// is the difference between getting a token and asking for a wider one.
pub async fn enforce_scope(
    State(require): State<RequireScope>,
    request: Request,
    next: Next,
) -> Response {
    let Some(principal) = request.extensions().get::<Principal>().cloned() else {
        return answer(&Error::Unauthorized(
            "this request carries no credential".to_owned(),
        ));
    };
    let presented: BTreeSet<String> = request
        .extensions()
        .get::<Bearer>()
        .map(|bearer| bearer.scopes.clone())
        .unwrap_or_default();

    if presented.contains(require.scope) {
        return next.run(request).await;
    }

    let id = require.kernel.refuse(
        DECISION_SCOPE_DENIED,
        &principal.sub,
        insufficient(require.scope),
        payload(
            &require,
            request.method().as_str(),
            request.uri().path(),
            &presented,
        ),
    );
    let mut response = answer(&Error::Denied(format!(
        "{id}: {}",
        insufficient(require.scope)
    )));
    if let Ok(value) = HeaderValue::from_str(&challenge(require.scope)) {
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, value);
    }
    response
}

/// The reason a scope refusal carries, in the chain and in the body.
fn insufficient(scope: &str) -> String {
    format!("the presented credential does not carry the scope {scope:?} this route requires")
}

/// The `WWW-Authenticate` value of a scope refusal (B-6).
///
/// A scope is a token that cannot carry a quote (RFC 6749 §3.3 fixes the
/// character set), and a gate's scope is a `&'static str` this codebase
/// wrote, so nothing here is derived from a request.
fn challenge(scope: &str) -> String {
    format!("Bearer error=\"{ERROR_INSUFFICIENT_SCOPE}\", scope=\"{scope}\"")
}

/// What the chain records: the capability, the required scope, the presented
/// ones (B-9).
///
/// The capability is the route being exercised, named by method and path.
/// That is the granularity a scope gate protects: the manifest's capability
/// catalog (spec 015 B-1) addresses resources a service acts on, and a
/// refusal that named one of those would be describing a different event.
fn payload(
    require: &RequireScope,
    method: &str,
    path: &str,
    presented: &BTreeSet<String>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut payload = serde_json::Map::new();
    payload.insert(
        "capability".to_owned(),
        serde_json::Value::from(format!("{method} {path}")),
    );
    payload.insert("scope".to_owned(), serde_json::Value::from(require.scope));
    payload.insert(
        "presented".to_owned(),
        serde_json::Value::from(presented.iter().cloned().collect::<Vec<_>>()),
    );
    payload
}

/// The process-wide union of every scope a gate was built with (B-2).
///
/// A cell builds its gates once, so in a cell this is what the cell requires.
/// A test binary builds many, and a union rather than a replacement is what
/// keeps the answer meaningful there.
static DECLARED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// Declare `scope` into the union [`supported`] answers with.
///
/// [`RequireScope::new`] calls this. An app declares directly only for a
/// scope it enforces itself, which it then owes the same refusal for.
pub fn declare(scope: &str) {
    let mut declared = DECLARED.lock().unwrap_or_else(PoisonError::into_inner);
    declared.insert(scope.to_owned());
}

/// Every scope a mounted route requires, ordered.
#[must_use]
pub fn supported() -> Vec<String> {
    DECLARED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scope_claim_is_space_delimited() {
        let scopes = parse("openid profile notes.write");
        assert_eq!(scopes.len(), 3);
        assert!(scopes.contains("notes.write"));

        assert!(parse("").is_empty());
        assert_eq!(parse("  openid   openid  ").len(), 1, "duplicates collapse");
    }

    #[test]
    fn matching_is_exact_and_nothing_about_it_is_hierarchical() {
        let scopes = parse("notes.write");
        assert!(scopes.contains("notes.write"));
        assert!(!scopes.contains("notes"), "no prefix rule exists");
        assert!(
            !scopes.contains("notes.write.all"),
            "no wildcard rule either"
        );
    }

    #[test]
    fn a_refusal_names_the_required_scope_in_the_header() {
        assert_eq!(
            challenge("notes.write"),
            "Bearer error=\"insufficient_scope\", scope=\"notes.write\""
        );
    }

    #[test]
    fn a_declared_scope_is_published() {
        declare("scope-test.declared");
        assert!(supported().contains(&"scope-test.declared".to_owned()));
    }
}
