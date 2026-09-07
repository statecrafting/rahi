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
