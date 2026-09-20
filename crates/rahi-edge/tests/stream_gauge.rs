//! The open gauge pairs with the attachment, not with the global
//! observability context (spec 026 B-9, AC-2).
//!
//! Its own binary on purpose. `rahi_edge::obs::init` installs a
//! process-global `OnceLock`, so a test that needs to observe a stream
//! opening *before* that global exists cannot share a process with a test
//! that has already installed it. `tests/stream.rs` installs one.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod common;

use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, header};
use axum::routing::get;
use rahi_edge::{Edge, ObsOptions, StreamHub, StreamOptions, StreamRoutes, stream};
use tower::ServiceExt as _;

fn options() -> StreamOptions {
    StreamOptions {
        max_concurrent_streams: 4,
        channel_capacity: 8,
        drain_timeout: Duration::from_secs(2),
        first_byte_timeout: Duration::from_millis(300),
        keep_alive: Duration::from_millis(100),
    }
}

/// A stream that opens while observability is uninitialised and closes
/// after it is initialised must leave the gauge where it found it.
///
/// Before the correction the two accounting points each read the global
/// separately: `attach` found `None` and incremented nothing, `close`
/// found the context installed in between and decremented it, and
/// `rahi_streams_open` went to -1. The gauge is an `IntGauge`, so a
/// negative value is representable and observable, and AC-2's
/// `streams_open() >= 0` is the assertion that catches it.
#[tokio::test]
async fn the_gauge_never_goes_negative_when_obs_is_installed_mid_stream() {
    assert!(
        rahi_edge::obs::current().is_none(),
        "this binary must reach the stream with no observability installed"
    );

    let hub = StreamHub::new(options());
    let app = Router::new().stream_route(
        "/hold",
        get(|| {
            let (_tx, rx) = rahi_edge::channel(8);
            std::future::ready(stream(rx))
        }),
    );
    let cell = common::boot("http://localhost:8080").await;
    let state = cell.state.clone().with_extension(hub.clone());
    let router = Edge::builder(state).mount("/", app).try_build().unwrap();

    // Attach with no observability context: nothing is counted.
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/hold")
                .header(header::ACCEPT_ENCODING, "gzip")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(hub.open_total(), 1, "the stream is attached");

    // Install it between the two accounting points.
    let obs = rahi_edge::obs::init(ObsOptions::from_config(&common::config(
        "http://localhost:8080",
    )))
    .unwrap();
    assert_eq!(
        obs.metrics().streams_open(),
        0,
        "a fresh registry starts at zero"
    );

    // Close: the dropped body releases the slot and closes as client_gone.
    drop(response);
    assert_eq!(hub.open_total(), 0, "the stream is released");

    assert_eq!(
        obs.metrics().streams_open(),
        0,
        "a close that was never counted open must not decrement"
    );
    assert!(
        obs.metrics().streams_open() >= 0,
        "AC-2's bound, which this defect broke"
    );

    // The outcome counter is the other half of the same pairing: a stream
    // that was never observed open is not observed closed either.
    let exposition = common::send(&router, common::get("/metrics")).await;
    assert!(
        exposition
            .body
            .contains("rahi_streams_closed_total{outcome=\"client_gone\"} 0"),
        "an unobserved close is not counted:\n{}",
        exposition.body
    );

    cell.stop().await;
}
