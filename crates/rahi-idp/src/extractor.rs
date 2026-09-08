//! `Authenticated(Principal)` and the role gate (spec 022 B-6).
//!
//! A handler writes `Authenticated(principal): Authenticated` and gets the
//! principal the IdP describes *now*. Everything B-5 asks for happens on the
//! way there and nothing in the handler knows about it: the envelope is
//! opened, the assertion is read from the cache group, and when it is missing
//! or expired the refresh token goes to rauthy, userinfo is re-read, and the
//! cookie comes back rotated.
//!
//! The work is in [`resolve`], a layer, rather than in the extractor itself,
//! and that is not an implementation detail: **an extractor cannot write a
//! `Set-Cookie`.** It sees the request and returns a value; the response is
//! built after it. B-5 requires the envelope to be rotated onto the new
//! refresh token, and rotation is a response header, so the round-trip must
//! happen somewhere that holds both halves. The extractor then reads what the
//! layer resolved, which is what makes it infallible about renewal and honest
//! about absence: no layer, no principal, 401.
//!
//! Spec 025 adds one refusal on the way in: a request that presents a session
//! cookie *and* an `Authorization` header is answered 400 before the envelope
//! is opened (025 B-10). The check is the resource server's
//! [`both_credentials`], read here as well as there, so the answer does not
//! depend on which of the two layers the app put outermost, and so an
//! ambiguous request never causes a renewal round-trip or a rotated cookie on
//! its way to being refused.
//!
//! [`RequireRole`] is the other half. It refuses a principal without the role
//! and the refusal becomes a record in the decision chain, because a refusal
//! nobody can audit is indistinguishable from a bug (constitution X). The
//! record is minted by the kernel's own denial path, so it lands in the same
//! chain, under the same id scheme, without the request waiting for the append
//! (spec 015 B-6).

use axum::Router;
use axum::extract::{FromRequestParts, Request, State};
use axum::http::header;
use axum::http::request::Parts;
use axum::middleware::{Next, from_fn_with_state};
use axum::response::Response;
use rahi_kernel::Kernel;
use rahi_types::{Error, Principal, Role};

use crate::bearer::{ambiguous, both_credentials};
use crate::envelope::{Envelope, cookie_value, open, seal};
use crate::login::login_cookie;
use crate::refresh::{ends_the_session, renew};
use crate::session::{Sessions, answer};

/// The decision kind a role refusal is recorded under.
pub const DECISION_ROLE_DENIED: &str = "session.role";

/// The authenticated principal, for a handler that requires one.
///
/// ```no_run
/// # use rahi_idp::Authenticated;
/// async fn whoami(Authenticated(principal): Authenticated) -> String {
///     principal.sub.as_str().to_owned()
/// }
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Authenticated(pub Principal);

impl<S> FromRequestParts<S> for Authenticated
where
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<Principal>()
            .cloned()
            .map(Self)
            .ok_or_else(|| {
                answer(&Error::Unauthorized(
                    "this request carries no session".to_owned(),
                ))
            })
    }
}

/// Wrap `router` in the layer that resolves, renews, and rotates (B-5, B-6).
///
/// Every route that an [`Authenticated`] extractor can appear on must be
/// inside this layer. A route outside it answers 401 to every request, which
/// is the failure this chassis prefers: absence is never permission.
pub fn with_sessions<S>(sessions: Sessions, router: Router<S>) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    router.layer(from_fn_with_state(sessions, resolve))
}

/// Open the envelope, make the assertion current, and rotate the cookie.
///
/// A request with no session cookie passes through untouched and reaches the
/// handler without a principal: this layer authenticates, it does not
/// authorize, and a public route under it stays public.
pub async fn resolve(State(sessions): State<Sessions>, request: Request, next: Next) -> Response {
    // Two credentials is not a session to resolve; it is a request nobody
    // should be choosing between (spec 025 B-10).
    if both_credentials(request.headers(), sessions.cookie_scheme()) {
        return answer(&ambiguous());
    }
    let cookies = request
        .headers()
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let Some(sealed) = cookies
        .as_deref()
        .and_then(|header| cookie_value(header, sessions.cookie().name()))
    else {
        return next.run(request).await;
    };

    // A cookie this cell did not seal is not a stale session, it is a forged
    // one. It ends here, and the cookies go with it.
    let envelope: Envelope = match open(sealed, sessions.key()) {
        Ok(envelope) => envelope,
        Err(err) => return refuse_session(&sessions, &err),
    };

    match sessions.load_assertion(&envelope.sid).await {
        Ok(Some(session)) => {
            let mut request = request;
            request.extensions_mut().insert(session.principal);
            next.run(request).await
        }
        Ok(None) => renewed(&sessions, envelope, request, next).await,
        // The cache group is derived state, but a session it cannot answer for
        // is a session this cell cannot authenticate. Fail closed.
        Err(err) => answer(&err),
    }
}

/// The renewal round-trip, and the response it rewrites (B-5).
async fn renewed(
    sessions: &Sessions,
    envelope: Envelope,
    request: Request,
    next: Next,
) -> Response {
    let renewed = match renew(sessions, &envelope).await {
        Ok(renewed) => renewed,
        Err(err) if ends_the_session(&err) => {
            let _ = sessions.drop_assertion(&envelope.sid).await;
            return refuse_session(sessions, &err);
        }
        // rauthy is unreachable rather than refusing. The session is not over,
        // so nothing is cleared and the status says the cell is the problem.
        Err(err) => return answer(&err),
    };

    let mut request = request;
    request
        .extensions_mut()
        .insert(renewed.session.principal.clone());
    let mut response = next.run(request).await;

    match seal(&renewed.envelope, sessions.key()) {
        Ok(sealed) => {
            if let Ok(value) = axum::http::HeaderValue::from_str(&sessions.cookie().set(&sealed)) {
                response.headers_mut().append(header::SET_COOKIE, value);
            }
            response
        }
        Err(err) => answer(&err),
    }
}

/// 401 with both cookies cleared: this session is over (B-5).
fn refuse_session(sessions: &Sessions, error: &Error) -> Response {
    let mut response = answer(error);
    for cookie in [
        sessions.cookie().clear(),
        login_cookie(sessions.cookie_scheme()).clear(),
    ] {
        if let Ok(value) = axum::http::HeaderValue::from_str(&cookie) {
            response.headers_mut().append(header::SET_COOKIE, value);
        }
    }
    response
}

/// The role gate: one role, and the kernel that records a refusal (B-6).
///
/// Spec 024 builds `RequireOperator` on this by naming
/// `manifest.auth.operator_role`.
#[derive(Clone)]
pub struct RequireRole {
    role: Role,
    kernel: Kernel,
}

impl std::fmt::Debug for RequireRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequireRole")
            .field("role", &self.role)
            .finish_non_exhaustive()
    }
}

impl RequireRole {
    /// Require `role`, recording every refusal in `kernel`'s chain.
    #[must_use]
    pub const fn new(role: Role, kernel: Kernel) -> Self {
        Self { role, kernel }
    }

    /// The role this gate requires.
    #[must_use]
    pub const fn role(&self) -> &Role {
        &self.role
    }
}

/// Wrap `router` in a role gate (B-6).
pub fn with_role<S>(require: RequireRole, router: Router<S>) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    router.layer(from_fn_with_state(require, enforce_role))
}

/// Answer 403 with a ledgered decision when the principal lacks the role.
///
/// A request with no principal at all is 401, not 403: the difference between
/// "you are not who you say" and "you are, and it is not enough" is the
/// difference between logging in again and asking an operator, and a client
/// that cannot tell them apart cannot do either.
pub async fn enforce_role(
    State(require): State<RequireRole>,
    request: Request,
    next: Next,
) -> Response {
    let Some(principal) = request.extensions().get::<Principal>().cloned() else {
        return answer(&Error::Unauthorized(
            "this request carries no session".to_owned(),
        ));
    };
    if principal.has_role(&require.role) {
        return next.run(request).await;
    }

    let mut payload = serde_json::Map::new();
    payload.insert(
        "role".to_owned(),
        serde_json::Value::from(require.role.as_str()),
    );
    payload.insert(
        "path".to_owned(),
        serde_json::Value::from(request.uri().path()),
    );
    let reason = format!(
        "the principal does not hold the role {:?} this route requires",
        require.role.as_str()
    );
    let id = require.kernel.refuse(
        DECISION_ROLE_DENIED,
        &principal.sub,
        reason.clone(),
        payload,
    );

    answer(&Error::Denied(format!("{id}: {reason}")))
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::collections::BTreeSet;

    use axum::http::StatusCode;
    use rahi_types::{Sub, UnixSeconds};

    use super::*;

    fn principal(roles: &[&str]) -> Principal {
        Principal {
            sub: Sub::new("s-1"),
            email: None,
            email_verified: false,
            roles: roles.iter().map(|r| Role::new(*r)).collect::<BTreeSet<_>>(),
            issued_at: UnixSeconds::new(0),
        }
    }

    #[tokio::test]
    async fn the_extractor_refuses_a_request_the_layer_left_unresolved() {
        let request = axum::http::Request::builder()
            .uri("/api/notes")
            .body(axum::body::Body::empty())
            .expect("a request");
        let (mut parts, _) = request.into_parts();
        let rejection = Authenticated::from_request_parts(&mut parts, &())
            .await
            .expect_err("no principal, no answer");
        assert_eq!(rejection.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn the_extractor_hands_over_what_the_layer_resolved() {
        let request = axum::http::Request::builder()
            .uri("/api/notes")
            .body(axum::body::Body::empty())
            .expect("a request");
        let (mut parts, _) = request.into_parts();
        parts.extensions.insert(principal(&["reader"]));
        let Authenticated(resolved) = Authenticated::from_request_parts(&mut parts, &())
            .await
            .expect("the layer resolved one");
        assert_eq!(resolved.sub.as_str(), "s-1");
        assert!(resolved.has_role(&Role::new("reader")));
    }
}
