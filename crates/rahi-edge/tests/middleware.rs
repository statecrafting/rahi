//! The chain every request passes through (spec 020 FR-002, FR-003, FR-004,
//! B-2, B-3, B-4, B-5, B-7, B-8).

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]

mod common;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use axum::routing::{get, post};
use rahi_edge::middleware::csrf;
use rahi_edge::middleware::security_headers as headers;
use rahi_edge::{Edge, RateLimits};
use rahi_types::Error;

use common::{Answer, boot, get as get_request, send};

/// An app router: one route that answers, one that fails, and one that takes
/// a POST.
fn app() -> Router {
    Router::new()
        .route("/notes", get(|| async { "notes" }))
        .route("/notes", post(|| async { "created" }))
        .route(
            "/broken",
            get(|| async {
                Err::<&str, rahi_edge::EdgeError>(
                    Error::Conflict("the note moved on".to_owned()).into(),
                )
            }),
        )
}

/// The rauthy proxy's territory (spec 021), stood in for here.
fn auth() -> Router {
    Router::new().route("/token", post(|| async { "issued" }))
}

fn post_request(path: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(path)
        .body(Body::empty())
        .expect("a well-formed request")
}

fn assert_security_headers(answer: &Answer, https: bool, what: &str) {
    assert_eq!(
        answer.header(header::CONTENT_SECURITY_POLICY.as_str()),
        Some(headers::CONTENT_SECURITY_POLICY),
        "{what} carries a CSP"
    );
    assert_eq!(
        answer.header(header::X_CONTENT_TYPE_OPTIONS.as_str()),
        Some(headers::CONTENT_TYPE_OPTIONS),
        "{what} refuses sniffing"
    );
    assert_eq!(
        answer.header(header::REFERRER_POLICY.as_str()),
        Some(headers::REFERRER_POLICY),
        "{what} carries a referrer policy"
    );
    assert_eq!(
        answer.header("permissions-policy"),
        Some(headers::PERMISSIONS_POLICY),
        "{what} denies camera, microphone, and geolocation"
    );
    let hsts = answer.header(header::STRICT_TRANSPORT_SECURITY.as_str());
    if https {
        assert_eq!(
            hsts,
            Some(headers::STRICT_TRANSPORT_SECURITY),
            "{what} is served over https and carries HSTS"
        );
    } else {
        assert_eq!(hsts, None, "{what} is plain http and carries no HSTS");
    }
}

// ------------------------------------------------------------------- CSRF

/// FR-002: an unproven POST is refused under an app prefix and forwarded
/// under `/auth/`, where rauthy answers for itself.
#[tokio::test]
async fn an_unproven_post_is_refused_under_the_app_and_forwarded_under_auth() {
    let cell = boot("https://cell.example.com").await;
    let router = Edge::builder(cell.state.clone())
        .mount("/api", app())
        .mount("/auth", auth())
        .build();

    let refused = send(&router, post_request("/api/notes")).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.body);
    assert_eq!(refused.json()["error"], "csrf");

    let forwarded = send(&router, post_request("/auth/token")).await;
    assert_eq!(forwarded.status, StatusCode::OK, "{}", forwarded.body);
    assert_eq!(
        forwarded.body, "issued",
        "the exempt POST reached the proxy's handler"
    );

    cell.stop().await;
}

/// The pair the middleware issues is the pair it accepts, and a mismatched
/// one is not accepted.
#[tokio::test]
async fn a_matching_pair_passes_and_a_mismatched_one_does_not() {
    let cell = boot("https://cell.example.com").await;
    let router = Edge::builder(cell.state.clone())
        .mount("/api", app())
        .build();

    let issued = send(&router, get_request("/api/notes")).await;
    assert_eq!(issued.status, StatusCode::OK, "{}", issued.body);
    let cookie = issued
        .header(header::SET_COOKIE.as_str())
        .expect("a safe request without a token is issued one");
    assert!(cookie.starts_with(csrf::COOKIE_SECURE), "{cookie}");
    let token = cookie
        .split(';')
        .next()
        .and_then(|pair| pair.split_once('='))
        .map(|(_name, value)| value.to_owned())
        .expect("the cookie carries a token");

    let proven = send(
        &router,
        Request::builder()
            .method("POST")
            .uri("/api/notes")
            .header(header::COOKIE, format!("{}={token}", csrf::COOKIE_SECURE))
            .header(csrf::HEADER, &token)
            .body(Body::empty())
            .expect("a well-formed request"),
    )
    .await;
    assert_eq!(proven.status, StatusCode::OK, "{}", proven.body);

    let mismatched = send(
        &router,
        Request::builder()
            .method("POST")
            .uri("/api/notes")
            .header(header::COOKIE, format!("{}={token}", csrf::COOKIE_SECURE))
            .header(csrf::HEADER, "a-token-from-somewhere-else")
            .body(Body::empty())
            .expect("a well-formed request"),
    )
    .await;
    assert_eq!(
        mismatched.status,
        StatusCode::FORBIDDEN,
        "{}",
        mismatched.body
    );

    cell.stop().await;
}

/// Spec 038 B-1 as spec 051 B-1 replaced it, and 051 FR-003: the composer's
/// exemption lets a bearer write and an anonymous write through with no CSRF
/// pair, and declines everything else.
///
/// This crate cannot call the predicate `serve` passes,
/// `rahi_idp::bearer::csrf_exempt`: spec 020 AC-2 keeps `rahi-idp` out of
/// this crate's tree, dev-dependencies included. So the stand-in below is
/// this test's fixture for the edge's half of the seam (an exempt request
/// skips the check and is minted no cookie; a declined one is checked), and
/// the predicate's own truth table is asserted where it lives, in
/// `rahi-idp`'s `bearer` test (051 FR-002), and composed as `serve`
/// composes it in `rahi-cli`'s `anonymous_challenge` test (051 FR-004).
#[tokio::test]
async fn a_write_without_a_session_cookie_needs_no_pair_and_nothing_else_is_exempt() {
    const SESSION_COOKIE: &str = "__Host-session";

    let cell = boot("https://cell.example.com").await;
    let exemption: rahi_edge::CsrfExemption =
        Arc::new(|path: &str, headers: &axum::http::HeaderMap| {
            let bearer_route = path.starts_with("/api/v1/");
            let session = headers
                .get(header::COOKIE)
                .and_then(|value| value.to_str().ok())
                .and_then(|header| csrf::cookie_value(header, SESSION_COOKIE))
                .is_some();
            bearer_route && !session
        });
    let router = Edge::builder(cell.state.clone())
        .mount("/api", app())
        .mount("/api/v1", app())
        .csrf_exemption(exemption)
        .build();

    let bearer = |path: &str, extra: Option<(&str, String)>| {
        let mut request = Request::builder()
            .method("POST")
            .uri(path)
            .header(header::AUTHORIZATION, "Bearer a.b.c");
        if let Some((name, value)) = extra {
            request = request.header(name, value);
        }
        request.body(Body::empty()).expect("a well-formed request")
    };
    let session_cookie = || {
        (
            header::COOKIE.as_str(),
            format!("{SESSION_COOKIE}=whatever"),
        )
    };

    let written = send(&router, bearer("/api/v1/notes", None)).await;
    assert_eq!(
        written.status,
        StatusCode::OK,
        "a bearer write on a declared route needs no pair: {}",
        written.body
    );

    // 051 FR-003: the request an MCP client opens with, no credential at
    // all, passes the check with no pair and is minted no cookie.
    let anonymous = send(&router, post_request("/api/v1/notes")).await;
    assert_eq!(
        anonymous.status,
        StatusCode::OK,
        "an anonymous write on a declared route needs no pair: {}",
        anonymous.body
    );
    assert_eq!(
        anonymous.header(header::SET_COOKIE.as_str()),
        None,
        "an exempt request is minted no CSRF cookie"
    );

    let both = send(&router, bearer("/api/v1/notes", Some(session_cookie()))).await;
    assert_eq!(
        both.status,
        StatusCode::FORBIDDEN,
        "a request carrying a session cookie is not exempt: {}",
        both.body
    );
    assert_eq!(both.json()["error"], "csrf");

    let (name, value) = session_cookie();
    let cookied = send(
        &router,
        Request::builder()
            .method("POST")
            .uri("/api/v1/notes")
            .header(name, value)
            .body(Body::empty())
            .expect("a well-formed request"),
    )
    .await;
    assert_eq!(
        cookied.status,
        StatusCode::FORBIDDEN,
        "a session cookie without a pair is checked as before: {}",
        cookied.body
    );
    assert_eq!(cookied.json()["error"], "csrf");

    let undeclared = send(&router, bearer("/api/notes", None)).await;
    assert_eq!(
        undeclared.status,
        StatusCode::FORBIDDEN,
        "a route nobody declared bearer is checked as before: {}",
        undeclared.body
    );

    let undeclared_anonymous = send(&router, post_request("/api/notes")).await;
    assert_eq!(
        undeclared_anonymous.status,
        StatusCode::FORBIDDEN,
        "an anonymous write to an undeclared route is checked as before: {}",
        undeclared_anonymous.body
    );

    cell.stop().await;
}

/// Over plain http the `__Host-` prefix is not legal, so the cookie is the
/// unprefixed one and carries no `Secure` (spec 010 D-4).
#[tokio::test]
async fn the_cookie_name_follows_the_public_url_scheme() {
    let cell = boot("http://localhost:8080").await;
    let router = Edge::builder(cell.state.clone())
        .mount("/api", app())
        .build();

    let issued = send(&router, get_request("/api/notes")).await;
    let cookie = issued
        .header(header::SET_COOKIE.as_str())
        .expect("a token is issued");
    assert!(cookie.starts_with(csrf::COOKIE_PLAIN), "{cookie}");
    assert!(!cookie.contains("Secure"), "{cookie}");

    cell.stop().await;
}

// -------------------------------------------------------- security headers

/// FR-003: the headers reach every answer, including the ones no handler
/// produced.
#[tokio::test]
async fn the_security_headers_reach_every_answer() {
    let dir = tempfile::tempdir().expect("a temp dir");
    std::fs::write(
        dir.path().join("index.html"),
        "<!doctype html><title>x</title>",
    )
    .expect("the fixture SPA is written");
    std::fs::write(dir.path().join("app.4f3a2b1c.js"), "console.log(1)")
        .expect("the fixture asset is written");

    let cell = boot("https://cell.example.com").await;
    let router = Edge::builder(cell.state.clone())
        .mount("/api", app())
        .static_slot(dir.path())
        .build();

    let ok = send(&router, get_request("/api/notes")).await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.body);
    assert_security_headers(&ok, true, "a handler's answer");

    let failed = send(&router, get_request("/api/broken")).await;
    assert_eq!(failed.status, StatusCode::CONFLICT, "{}", failed.body);
    assert_eq!(failed.json()["error"], "conflict");
    assert_eq!(failed.json()["message"], "the note moved on");
    assert_security_headers(&failed, true, "an error");

    let refused = send(&router, post_request("/api/notes")).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    assert_security_headers(&refused, true, "a CSRF refusal");

    let asset = send(&router, get_request("/app.4f3a2b1c.js")).await;
    assert_eq!(asset.status, StatusCode::OK, "{}", asset.body);
    assert_security_headers(&asset, true, "a static file");

    let probe = send(&router, get_request(rahi_edge::READYZ_PATH)).await;
    assert_security_headers(&probe, true, "a probe");

    cell.stop().await;
}

/// A cell on plain http sends every header but HSTS.
#[tokio::test]
async fn plain_http_sends_no_transport_security() {
    let cell = boot("http://localhost:8080").await;
    let router = Edge::builder(cell.state.clone())
        .mount("/api", app())
        .build();

    let answer = send(&router, get_request("/api/notes")).await;
    assert_security_headers(&answer, false, "a plain http answer");

    // The floor holds for an answer no handler produced: the layer is applied
    // to the router itself, so the fallback is inside it.
    let missing = send(&router, get_request("/nothing-is-mounted-here")).await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    assert_security_headers(&missing, false, "the router's own 404");

    cell.stop().await;
}

// ------------------------------------------------------------- static slot

/// B-7: hashed assets are immutable, `index.html` is not, and an unknown path
/// under the slot is the SPA.
#[tokio::test]
async fn the_static_slot_caches_by_name_and_falls_back_to_the_spa() {
    let dir = tempfile::tempdir().expect("a temp dir");
    std::fs::write(
        dir.path().join("index.html"),
        "<!doctype html><title>spa</title>",
    )
    .expect("the fixture SPA is written");
    std::fs::write(dir.path().join("app.4f3a2b1c.js"), "console.log(1)")
        .expect("the fixture asset is written");

    let cell = boot("https://cell.example.com").await;
    let router = Edge::builder(cell.state.clone())
        .mount("/api", app())
        .static_slot(dir.path())
        .build();

    let asset = send(&router, get_request("/app.4f3a2b1c.js")).await;
    assert_eq!(asset.status, StatusCode::OK, "{}", asset.body);
    assert_eq!(
        asset.header(header::CACHE_CONTROL.as_str()),
        Some(rahi_edge::static_files::IMMUTABLE_CACHE_CONTROL)
    );

    let index = send(&router, get_request("/index.html")).await;
    assert_eq!(index.status, StatusCode::OK, "{}", index.body);
    assert_eq!(
        index.header(header::CACHE_CONTROL.as_str()),
        Some(rahi_edge::static_files::REVALIDATE_CACHE_CONTROL)
    );

    let deep_link = send(&router, get_request("/notes/42")).await;
    assert_eq!(deep_link.status, StatusCode::OK, "{}", deep_link.body);
    assert!(deep_link.body.contains("spa"), "{}", deep_link.body);
    assert_eq!(
        deep_link.header(header::CACHE_CONTROL.as_str()),
        Some(rahi_edge::static_files::REVALIDATE_CACHE_CONTROL)
    );

    cell.stop().await;
}

// -------------------------------------------------------------- rate limit

fn from_client(path: &str, client: &str) -> Request<Body> {
    let mut request = get_request(path);
    let addr: SocketAddr = format!("{client}:51000").parse().expect("a peer address");
    request.extensions_mut().insert(ConnectInfo(addr));
    request
}

/// FR-004: request 301 inside one window is refused for the client that made
/// the first 300, and the next client is admitted.
#[tokio::test]
async fn the_window_is_spent_per_client() {
    let cell = boot("https://cell.example.com").await;
    // A clock that never leaves the window, so the assertion is about the
    // ceiling and not about when the test happened to run.
    let router = Edge::builder(cell.state.clone())
        .mount("/api", app())
        .clock(Arc::new(|| 1_767_225_600))
        .build();

    for n in 1..=300 {
        let answer = send(&router, from_client("/api/notes", "10.0.0.1")).await;
        assert_eq!(
            answer.status,
            StatusCode::OK,
            "request {n} is inside the ceiling: {}",
            answer.body
        );
    }

    let spent = send(&router, from_client("/api/notes", "10.0.0.1")).await;
    assert_eq!(
        spent.status,
        StatusCode::TOO_MANY_REQUESTS,
        "{}",
        spent.body
    );
    assert_eq!(spent.json()["error"], "rate_limited");
    assert_eq!(spent.header(header::RETRY_AFTER.as_str()), Some("60"));

    let other = send(&router, from_client("/api/notes", "10.0.0.2")).await;
    assert_eq!(
        other.status,
        StatusCode::OK,
        "another client has its own window: {}",
        other.body
    );

    cell.stop().await;
}

/// A group the app declares narrows the ceiling for its own prefix and
/// nothing else.
#[tokio::test]
async fn a_declared_group_carries_its_own_ceiling() {
    let cell = boot("https://cell.example.com").await;
    let router = Edge::builder(cell.state.clone())
        .mount("/api", app())
        .rate_limits(RateLimits::new().group("/api/broken", 2))
        .clock(Arc::new(|| 1_767_225_600))
        .build();

    for _ in 0..2 {
        let answer = send(&router, from_client("/api/broken", "10.0.0.3")).await;
        assert_eq!(answer.status, StatusCode::CONFLICT, "{}", answer.body);
    }
    let spent = send(&router, from_client("/api/broken", "10.0.0.3")).await;
    assert_eq!(
        spent.status,
        StatusCode::TOO_MANY_REQUESTS,
        "{}",
        spent.body
    );

    let elsewhere = send(&router, from_client("/api/notes", "10.0.0.3")).await;
    assert_eq!(
        elsewhere.status,
        StatusCode::OK,
        "the default group is untouched: {}",
        elsewhere.body
    );

    cell.stop().await;
}
