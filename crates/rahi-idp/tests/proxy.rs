//! The `/auth/*` proxy against a real upstream (spec 021 FR-001, B-2).

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]

mod common;

use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::{Json, Router};
use rahi_idp::{Proxy, proxy_router};
use serde_json::json;
use tower::ServiceExt as _;

use common::{idp_config, serve};

/// Two mebibytes: large enough that nothing on the path can be buffering it
/// by accident and still call itself a stream.
const BIG: usize = 2 * 1024 * 1024;

/// The stub rauthy: it says back what it was asked, redirects on one path,
/// and serves and swallows large bodies on two others.
fn upstream() -> Router {
    Router::new()
        .route(
            "/auth/v1/redirect",
            any(|| async {
                Response::builder()
                    .status(StatusCode::FOUND)
                    .header(
                        header::LOCATION,
                        "https://cell.example.com/auth/callback?code=abc",
                    )
                    .header(header::SET_COOKIE, "rauthy-session=xyz; Path=/auth")
                    .body(Body::empty())
                    .expect("a well-formed response")
            }),
        )
        .route(
            "/auth/v1/download",
            any(|| async { Body::from(vec![b'r'; BIG]).into_response() }),
        )
        .route(
            "/auth/v1/upload",
            any(|body: Body| async move {
                let bytes = axum::body::to_bytes(body, usize::MAX)
                    .await
                    .expect("the body arrives");
                Json(json!({ "received": bytes.len(), "first": bytes.first().copied() }))
            }),
        )
        .fallback(any(echo))
}

/// Everything the upstream saw, as JSON.
async fn echo(request: Request) -> Response {
    let method = request.method().to_string();
    let uri = request.uri().to_string();
    let headers = header_map(request.headers());
    Json(json!({ "method": method, "uri": uri, "headers": headers })).into_response()
}

fn header_map(headers: &HeaderMap) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    for (name, value) in headers {
        out.insert(
            name.as_str().to_owned(),
            json!(value.to_str().unwrap_or_default()),
        );
    }
    serde_json::Value::Object(out)
}

async fn send(router: &Router, request: Request<Body>) -> (StatusCode, HeaderMap, Vec<u8>) {
    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("the router is infallible");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("the body is readable");
    (status, headers, bytes.to_vec())
}

/// FR-001: the method, the path, the query, and the headers arrive; the
/// forwarded proto and host are the cell's public origin; hop-by-hop headers
/// do not travel.
#[tokio::test]
async fn what_the_client_sent_is_what_rauthy_sees() {
    let stub = serve(upstream()).await;
    let router = proxy_router(
        Proxy::new(&idp_config("https://cell.example.com", stub.addr)).expect("the proxy builds"),
    );

    let request = Request::builder()
        .method("GET")
        .uri("/auth/v1/authorize?client_id=hello-cell&state=abc%20def")
        .header(header::COOKIE, "rauthy-session=xyz")
        .header(header::ACCEPT, "application/json")
        .header(header::HOST, "cell.example.com")
        .header(header::CONNECTION, "keep-alive")
        .header("x-custom", "kept")
        .body(Body::empty())
        .expect("a well-formed request");
    let (status, _headers, body) = send(&router, request).await;

    assert_eq!(status, StatusCode::OK);
    let seen: serde_json::Value = serde_json::from_slice(&body).expect("the echo is JSON");
    assert_eq!(seen["method"], "GET");
    assert_eq!(
        seen["uri"], "/auth/v1/authorize?client_id=hello-cell&state=abc%20def",
        "the path and query are forwarded unchanged"
    );
    assert_eq!(seen["headers"]["cookie"], "rauthy-session=xyz");
    assert_eq!(seen["headers"]["accept"], "application/json");
    assert_eq!(seen["headers"]["x-custom"], "kept");
    assert_eq!(seen["headers"]["x-forwarded-proto"], "https");
    assert_eq!(seen["headers"]["x-forwarded-host"], "cell.example.com");
    assert_eq!(
        seen["headers"]["host"],
        json!(stub.addr.to_string()),
        "the client's Host is replaced by the upstream's own: rauthy builds its \
         redirects from the forwarded headers instead"
    );
    assert_eq!(
        seen["headers"]["connection"],
        serde_json::Value::Null,
        "a hop-by-hop header does not travel"
    );
}

/// FR-001: a redirect is relayed, not followed, with its headers intact.
#[tokio::test]
async fn a_redirect_is_relayed_rather_than_followed() {
    let stub = serve(upstream()).await;
    let router = proxy_router(
        Proxy::new(&idp_config("https://cell.example.com", stub.addr)).expect("the proxy builds"),
    );

    let (status, headers, _body) = send(
        &router,
        Request::builder()
            .uri("/auth/v1/redirect")
            .body(Body::empty())
            .expect("a well-formed request"),
    )
    .await;

    assert_eq!(status, StatusCode::FOUND);
    assert_eq!(
        headers.get(header::LOCATION).and_then(|v| v.to_str().ok()),
        Some("https://cell.example.com/auth/callback?code=abc")
    );
    assert_eq!(
        headers
            .get(header::SET_COOKIE)
            .and_then(|v| v.to_str().ok()),
        Some("rauthy-session=xyz; Path=/auth"),
        "rauthy's own cookie reaches the browser untouched"
    );
}

/// FR-001: two mebibytes travel in each direction.
#[tokio::test]
async fn a_large_body_travels_in_both_directions() {
    let stub = serve(upstream()).await;
    let router = proxy_router(
        Proxy::new(&idp_config("https://cell.example.com", stub.addr)).expect("the proxy builds"),
    );

    let (status, _headers, body) = send(
        &router,
        Request::builder()
            .uri("/auth/v1/download")
            .body(Body::empty())
            .expect("a well-formed request"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.len(), BIG, "the whole download is relayed");

    let (status, _headers, body) = send(
        &router,
        Request::builder()
            .method("POST")
            .uri("/auth/v1/upload")
            .body(Body::from(vec![b'q'; BIG]))
            .expect("a well-formed request"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let seen: serde_json::Value = serde_json::from_slice(&body).expect("the answer is JSON");
    assert_eq!(seen["received"], BIG, "the whole upload arrives");
    assert_eq!(seen["first"], u64::from(b'q'));
}

/// B-2: the subtree is the route. `/auth` itself and everything under it
/// reach rauthy, and nothing else is this router's business.
#[tokio::test]
async fn the_route_is_the_auth_subtree_and_only_it() {
    let stub = serve(upstream()).await;
    let router = proxy_router(
        Proxy::new(&idp_config("http://localhost:8080", stub.addr)).expect("the proxy builds"),
    );

    for path in ["/auth", "/auth/v1/token", "/auth/v1/deeply/nested/path"] {
        let (status, _headers, body) = send(
            &router,
            Request::builder()
                .uri(path)
                .body(Body::empty())
                .expect("a well-formed request"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{path}");
        let seen: serde_json::Value = serde_json::from_slice(&body).expect("JSON");
        assert_eq!(seen["uri"], path);
        assert_eq!(
            seen["headers"]["x-forwarded-proto"], "http",
            "a plain http cell forwards its own scheme"
        );
    }

    let (status, _headers, _body) = send(
        &router,
        Request::builder()
            .uri("/api/notes")
            .body(Body::empty())
            .expect("a well-formed request"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "the app's routes are not the proxy's"
    );
}

/// An upstream that is not there is `Error::Upstream`, which the edge maps to
/// 502 (spec 020 B-8, D-2).
#[tokio::test]
async fn an_absent_rauthy_is_a_bad_gateway() {
    let stub = serve(upstream()).await;
    let addr = stub.addr;
    drop(stub);

    let router = proxy_router(
        Proxy::new(&idp_config("https://cell.example.com", addr)).expect("the proxy builds"),
    );
    let (status, _headers, body) = send(
        &router,
        Request::builder()
            .uri("/auth/v1/token")
            .body(Body::empty())
            .expect("a well-formed request"),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_GATEWAY);
    let answer: serde_json::Value = serde_json::from_slice(&body).expect("the answer is JSON");
    assert_eq!(answer["error"], "upstream");
}
