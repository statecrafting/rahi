//! Liveness and readiness against a real cell (spec 020 FR-001, B-6).

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]

mod common;

use axum::http::StatusCode;
use rahi_edge::{Edge, HEALTHZ_PATH, READYZ_PATH};

use common::{boot, get, send};

/// The two probes answer different questions: readiness touches the store and
/// the chain, liveness touches nothing, and stopping the node moves only one
/// of the two answers.
#[tokio::test]
async fn liveness_survives_the_store_that_readiness_reports() {
    let cell = boot("https://cell.example.com").await;
    let router = Edge::builder(cell.state.clone()).build();

    let ready = send(&router, get(READYZ_PATH)).await;
    assert_eq!(ready.status, StatusCode::OK, "{}", ready.body);
    assert_eq!(ready.json()["status"], "ready");
    assert_eq!(ready.json()["store"], "up");
    assert_eq!(ready.json()["ledger"], "verified");

    let alive = send(&router, get(HEALTHZ_PATH)).await;
    assert_eq!(alive.status, StatusCode::OK, "{}", alive.body);
    assert_eq!(alive.json()["status"], "alive");

    cell.stop().await;

    let alive = send(&router, get(HEALTHZ_PATH)).await;
    assert_eq!(
        alive.status,
        StatusCode::OK,
        "liveness touches no dependency: {}",
        alive.body
    );

    let ready = send(&router, get(READYZ_PATH)).await;
    assert_eq!(
        ready.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "{}",
        ready.body
    );
    let body = ready.json();
    assert_eq!(body["status"], "not_ready");
    let component = body["component"].as_str().expect("a named component");
    assert_eq!(
        component, "store",
        "the stopped dependency is the one named: {}",
        ready.body
    );
    assert!(
        !body["message"].as_str().unwrap_or_default().is_empty(),
        "the answer says why: {}",
        ready.body
    );
}

/// The probes are mounted outside the CSRF and rate-limit layers (B-2): they
/// answer without a token and are not counted against a client's window.
#[tokio::test]
async fn the_probes_are_outside_the_guarded_layers() {
    let cell = boot("https://cell.example.com").await;
    let router = Edge::builder(cell.state.clone())
        .rate_limits(rahi_edge::RateLimits::new().default_limit(1))
        .build();

    for _ in 0..8 {
        let answer = send(&router, get(HEALTHZ_PATH)).await;
        assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
        assert!(
            answer.header("set-cookie").is_none(),
            "a probe is not issued a CSRF token"
        );
    }
    cell.stop().await;
}
