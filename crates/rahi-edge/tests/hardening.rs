//! Spec 024: trusted client identity, the operator gate, the exposure table.
//!
//! The three lessons enrahitu's exposure review left (enrahitu://025), each
//! asserted rather than documented: an address is an identity only as far as
//! the operator vouched for it, an operator surface is gated by role and not
//! by a ceiling, and every route is classified before the router exists.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;
use std::panic::AssertUnwindSafe;

use axum::Router;
use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{Next, from_fn};
use axum::response::Response;
use axum::routing::get;
use rahi_edge::exposure::{self, Route, RouteClass};
use rahi_edge::middleware::rate_limit::RateLimits;
use rahi_edge::{AppState, ClientIdentity, Edge};
use rahi_types::{Config, Principal, Role, Sub, UnixSeconds};

use common::{Answer, boot, send};

/// The role the fixture manifest names in `auth.operator_role`.
const OPERATOR_ROLE: &str = "rahi-operator";
/// The header the test's stand-in for spec 022's session layer reads.
const TEST_ROLES: &str = "x-test-roles";

// ---------------------------------------------------------------- B-1, FR-001

fn ip(text: &str) -> IpAddr {
    text.parse().expect("a fixture address")
}

fn forwarded(value: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        rahi_edge::client_identity::X_FORWARDED_FOR,
        value.parse().expect("a well-formed header value"),
    );
    headers
}

/// FR-001: hops 0, 1, and 2 against a three-entry header.
#[test]
fn fr001_a_three_entry_header_is_read_from_the_right() {
    let headers = forwarded("9.9.9.9, 10.0.0.1, 198.51.100.4");
    let peer = ip("192.0.2.7");

    assert_eq!(
        ClientIdentity::resolve(&headers, peer, 0),
        peer,
        "no trusted hop, no header"
    );
    assert_eq!(
        ClientIdentity::resolve(&headers, peer, 1),
        ip("198.51.100.4"),
        "one hop is the first address from the right"
    );
    assert_eq!(
        ClientIdentity::resolve(&headers, peer, 2),
        ip("10.0.0.1"),
        "two hops is the second address from the right"
    );

    for hops in [0, 1, 2] {
        assert_ne!(
            ClientIdentity::resolve(&headers, peer, hops),
            ip("9.9.9.9"),
            "the leftmost entry is the one the client chose, at {hops} hops"
        );
    }
}

/// FR-001: hops 0, 1, and 2 against a one-entry header.
#[test]
fn fr001_a_one_entry_header_answers_for_one_hop_only() {
    let headers = forwarded("198.51.100.4");
    let peer = ip("192.0.2.7");

    assert_eq!(ClientIdentity::resolve(&headers, peer, 0), peer);
    assert_eq!(
        ClientIdentity::resolve(&headers, peer, 1),
        ip("198.51.100.4"),
        "the one entry is what the one trusted hop wrote"
    );
    assert_eq!(
        ClientIdentity::resolve(&headers, peer, 2),
        peer,
        "the header is shorter than the declared topology, so it is not evidence"
    );
}

/// FR-001: hops 0, 1, and 2 with no header at all.
#[test]
fn fr001_no_header_is_always_the_peer() {
    let headers = HeaderMap::new();
    let peer = ip("192.0.2.7");
    for hops in [0, 1, 2] {
        assert_eq!(ClientIdentity::resolve(&headers, peer, hops), peer);
    }
}

/// B-1 through the router: the limiter counts the client, not the proxy.
#[tokio::test]
async fn b1_the_limiter_keys_on_the_forwarded_client() {
    let cell = boot("http://127.0.0.1:8080").await;
    let state = AppState::new(
        cell.state.kernel().clone(),
        cell.store.clone(),
        cell.state.ledger().clone(),
        behind_one_proxy("http://127.0.0.1:8080"),
    );
    let router = Edge::builder(state)
        .mount("/api", ping())
        .rate_limits(RateLimits::new().default_limit(1))
        .clock(std::sync::Arc::new(|| 1_767_225_600))
        .build();

    let first = send(&router, from_client("/api/ping", "203.0.113.9")).await;
    assert_eq!(first.status, StatusCode::OK);

    let again = send(&router, from_client("/api/ping", "203.0.113.9")).await;
    assert_eq!(
        again.status,
        StatusCode::TOO_MANY_REQUESTS,
        "the same client spends its own window"
    );

    let other = send(&router, from_client("/api/ping", "203.0.113.10")).await;
    assert_eq!(
        other.status,
        StatusCode::OK,
        "a different client behind the same proxy has its own window"
    );
}

// ---------------------------------------------------------------- B-2, FR-002

/// FR-002: 403 with a decision id for a principal without the operator role.
#[tokio::test]
async fn fr002_a_non_operator_is_refused_with_a_ledgered_decision() {
    let cell = boot("http://127.0.0.1:8080").await;
    let router = operator_router(&cell.state);

    let refused = send(&router, as_roles("/operator/traces", "reader")).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    let body = refused.json();
    assert_eq!(body["error"], "denied");
    let decision = body["decision"].as_str().unwrap_or_default().to_owned();
    assert!(
        decision.starts_with("kernel:"),
        "the refusal names the decision the chain holds: {}",
        refused.body
    );
    assert!(
        body["message"]
            .as_str()
            .unwrap_or_default()
            .contains(OPERATOR_ROLE),
        "{}",
        refused.body
    );
}

/// B-2: no principal at all is 401, not 403.
#[tokio::test]
async fn b2_a_request_with_no_session_is_unauthorized() {
    let cell = boot("http://127.0.0.1:8080").await;
    let router = operator_router(&cell.state);

    let refused = send(&router, common::get("/operator/traces")).await;
    assert_eq!(refused.status, StatusCode::UNAUTHORIZED);
    assert_eq!(refused.json()["error"], "unauthorized");
}

/// B-2: the gate guards the routes it was given, not the router's fallback.
///
/// A gate applied to the fallback answers 401 for every path in the cell,
/// which tells an unauthenticated caller that everything exists, and it does
/// it loudest about the paths that do not (spec 024 D-5).
#[tokio::test]
async fn b2_a_path_nobody_mounted_is_still_a_404() {
    let cell = boot("http://127.0.0.1:8080").await;
    let router = operator_router(&cell.state);

    let missing = send(&router, common::get("/nothing-is-mounted-here")).await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);

    let under_the_gate = send(&router, common::get("/operator/nothing-here")).await;
    assert_eq!(under_the_gate.status, StatusCode::NOT_FOUND);
}

/// B-2: an operator mount does not take over the static slot's fallback.
///
/// The operator branch is merged into a router that may already answer every
/// unmatched path with the SPA (spec 020 B-7), and axum resolves a merge of
/// two default fallbacks in favour of the incoming one.
#[tokio::test]
async fn b2_an_operator_mount_leaves_the_static_slot_alone() {
    let web = tempfile::tempdir().expect("a temp dir");
    std::fs::write(
        web.path().join("index.html"),
        "<!doctype html><title>spa</title>",
    )
    .expect("the fixture SPA is written");

    let cell = boot("http://127.0.0.1:8080").await;
    let router = Edge::builder(cell.state.clone())
        .mount_operator("/operator", traces())
        .static_slot(web.path())
        .build();

    let spa = send(&router, common::get("/some/client-side/route")).await;
    assert_eq!(spa.status, StatusCode::OK, "{}", spa.body);
    assert!(spa.body.contains("spa"), "{}", spa.body);
}

/// FR-002: 200 for an operator, and four hundred of them in one window.
#[tokio::test]
async fn fr002_an_operator_is_admitted_and_never_limited() {
    let cell = boot("http://127.0.0.1:8080").await;
    let router = operator_router(&cell.state);

    let allowed = send(&router, as_roles("/operator/traces", OPERATOR_ROLE)).await;
    assert_eq!(allowed.status, StatusCode::OK);

    // The ceiling on everything else is five, so a limiter over this surface
    // would refuse the sixth of these.
    for n in 0..400 {
        let answer = send(&router, as_roles("/operator/traces", OPERATOR_ROLE)).await;
        assert_eq!(
            answer.status,
            StatusCode::OK,
            "operator request {n} was refused: {}",
            answer.body
        );
    }

    let limited = spend(&router, "/api/ping", 6).await;
    assert_eq!(
        limited.status,
        StatusCode::TOO_MANY_REQUESTS,
        "the limiter is installed, and only the operator surface is outside it"
    );
}

// ---------------------------------------------------------------- B-3, FR-003

/// FR-003: every route a reference app mounts is in the table, with its class.
#[tokio::test]
async fn fr003_the_table_names_every_route_and_its_class() {
    let cell = boot("http://127.0.0.1:8080").await;
    let web = tempfile::tempdir().expect("a temp dir");
    let builder = Edge::builder(cell.state.clone())
        .mount("/api", ping())
        .mount("/auth", ping())
        .mount_operator("/operator", ping())
        .mount_public("/public", ping())
        .expose(Route::new("/api/notes", RouteClass::Authenticated))
        .static_slot(web.path());

    let table = builder.exposure();
    for (path, class) in [
        ("/healthz", RouteClass::Probe),
        ("/readyz", RouteClass::Probe),
        ("/metrics", RouteClass::Probe),
        ("/auth", RouteClass::Proxy),
        ("/api", RouteClass::Authenticated),
        ("/api/notes", RouteClass::Authenticated),
        ("/operator", RouteClass::Operator),
        ("/public", RouteClass::Public),
        (exposure::STATIC_SLOT_PATH, RouteClass::Public),
    ] {
        assert_eq!(
            table.class_of(path),
            Some(class),
            "{path} is {class} in the table:\n{}",
            table.render()
        );
    }
    assert!(table.unclassified().is_empty());
    assert!(table.check().is_ok());

    let _router = builder.build();
    let report = exposure::report();
    for path in [
        "/healthz",
        "/readyz",
        "/metrics",
        "/auth",
        "/api",
        "/operator",
    ] {
        assert!(report.contains(path), "{path} is missing from:\n{report}");
    }
}

/// B-4: what an app mounts is authenticated unless it says otherwise.
#[tokio::test]
async fn b4_an_app_mount_defaults_to_authenticated() {
    let cell = boot("http://127.0.0.1:8080").await;
    let table = Edge::builder(cell.state.clone())
        .mount("/api", ping())
        .mount("/", ping())
        .exposure();
    assert_eq!(table.class_of("/api"), Some(RouteClass::Authenticated));
    assert_eq!(
        table.class_of("/"),
        Some(RouteClass::Authenticated),
        "a root merge is not a public route by omission"
    );
}

/// FR-003: a route without a class fails the build.
#[tokio::test]
async fn fr003_an_unclassified_route_fails_the_build() {
    let cell = boot("http://127.0.0.1:8080").await;

    let refused = Edge::builder(cell.state.clone())
        .mount("/api", ping())
        .expose(Route::unclassified("/internal/dump"))
        .try_build()
        .expect_err("an unreviewed route is not a router");
    assert_eq!(refused.0.kind(), "config");
    assert!(
        refused.0.message().contains("/internal/dump"),
        "{}",
        refused.0.message()
    );

    // B-3: and in a debug build the infallible form panics rather than
    // serving it. `cargo test` is a debug build.
    let panicked = std::panic::catch_unwind(AssertUnwindSafe(|| {
        Edge::builder(cell.state.clone())
            .expose(Route::unclassified("/internal/dump"))
            .build()
    }));
    assert!(
        panicked.is_err(),
        "an unclassified route is a build-time panic in debug"
    );
}

// ---------------------------------------------------------------- B-5, FR-004

/// FR-004: with the store stopped, a rate-limited route answers 503.
#[tokio::test]
async fn fr004_a_limiter_that_cannot_count_does_not_admit() {
    let cell = boot("http://127.0.0.1:8080").await;
    let router = Edge::builder(cell.state.clone())
        .mount("/api", ping())
        .build();

    let before = send(&router, common::get("/api/ping")).await;
    assert_eq!(before.status, StatusCode::OK);

    cell.stop().await;

    let after = send(&router, common::get("/api/ping")).await;
    assert_eq!(
        after.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "the limiter fails closed: {}",
        after.body
    );
    assert_eq!(after.json()["error"], "rate_limit_unavailable");
}

// ------------------------------------------------------------------- fixtures

/// A route that answers without touching a dependency.
fn ping() -> Router {
    Router::new().route("/ping", get(|| async { "pong" }))
}

/// The operator surface a reference app exposes.
fn traces() -> Router {
    Router::new().route("/traces", get(|| async { "[]" }))
}

/// What a reference app mounts: one operator surface, one ordinary one.
fn operator_router(state: &AppState) -> Router {
    let router = Edge::builder(state.clone())
        .mount("/api", ping())
        .mount_operator("/operator", traces())
        .rate_limits(RateLimits::new().default_limit(5))
        .clock(std::sync::Arc::new(|| 1_767_225_600))
        .build();
    with_test_principal(router)
}

/// The stand-in for spec 022's session layer.
///
/// This crate does not depend on `rahi-idp` (spec 020 AC-2), and the gate it
/// owns reads the [`Principal`] whoever authenticated left in the request
/// extensions. The layer goes outside the whole edge router, which is where
/// spec 022's own layer sits.
fn with_test_principal(router: Router) -> Router {
    router.layer(from_fn(insert_principal))
}

async fn insert_principal(mut request: Request, next: Next) -> Response {
    let roles: Option<BTreeSet<Role>> = request
        .headers()
        .get(TEST_ROLES)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|role| !role.is_empty())
                .map(Role::new)
                .collect()
        });
    if let Some(roles) = roles {
        request.extensions_mut().insert(Principal {
            sub: Sub::new("s-1"),
            email: None,
            email_verified: false,
            roles,
            issued_at: UnixSeconds::new(0),
        });
    }
    next.run(request).await
}

/// A `GET` for `path` carrying the roles the test principal holds.
fn as_roles(path: &str, roles: &str) -> Request<Body> {
    Request::builder()
        .uri(path)
        .header(TEST_ROLES, roles)
        .body(Body::empty())
        .expect("a well-formed request")
}

/// A `GET` for `path` that one trusted proxy forwarded for `client`.
fn from_client(path: &str, client: &str) -> Request<Body> {
    Request::builder()
        .uri(path)
        .header("x-forwarded-for", client)
        .body(Body::empty())
        .expect("a well-formed request")
}

/// The configuration of a cell with one reverse proxy in front of it.
fn behind_one_proxy(public_url: &str) -> Config {
    let env = BTreeMap::from([
        ("RAHI_PUBLIC_URL", public_url),
        ("RAHI_TRUSTED_PROXY_HOPS", "1"),
    ]);
    Config::from_env(&env).expect("the fixture environment is well formed")
}

/// Send `count` requests to `path` and return the last answer.
async fn spend(router: &Router, path: &str, count: usize) -> Answer {
    let mut last = send(router, common::get(path)).await;
    for _ in 1..count {
        last = send(router, common::get(path)).await;
    }
    last
}
