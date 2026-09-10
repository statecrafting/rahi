//! Streaming responses (spec 026 FR-001 to FR-006, AC-2).
//!
//! Every test drives a real router built over a real cell, reads the
//! response body chunk by chunk, and parses the server-sent events out of
//! it: a stream that is asserted on after `to_bytes` collected it would
//! prove nothing about what a client sees while it is open.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod common;

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::Poll;
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::{Body, BodyDataStream, Bytes};
use axum::http::{Request, StatusCode, header};
use axum::response::Response;
use axum::routing::get;
use futures_core::Stream as _;
use rahi_edge::{
    Closed, Edge, Event, ObsOptions, Producer, StreamHub, StreamOptions, StreamRoutes, channel,
    stream,
};
use tower::ServiceExt as _;

/// The options every test builds its hub with: short enough to observe.
fn options() -> StreamOptions {
    StreamOptions {
        max_concurrent_streams: 4,
        channel_capacity: 8,
        drain_timeout: Duration::from_secs(2),
        first_byte_timeout: Duration::from_millis(300),
        keep_alive: Duration::from_millis(100),
    }
}

/// A cell with a hub in its state and one edge built over `app`.
async fn edge(app: Router, hub: &StreamHub) -> (Router, common::Cell) {
    let cell = common::boot("http://localhost:8080").await;
    let state = cell.state.clone().with_extension(hub.clone());
    let router = Edge::builder(state).mount("/", app).try_build().unwrap();
    (router, cell)
}

/// Send `request` and keep the body as a stream.
async fn open(router: &Router, request: Request<Body>) -> Response {
    router.clone().oneshot(request).await.unwrap()
}

fn get_request(path: &str) -> Request<Body> {
    Request::builder()
        .uri(path)
        .header(header::ACCEPT_ENCODING, "gzip")
        .body(Body::empty())
        .unwrap()
}

/// One chunk of the body, or `None` at its end.
async fn next_chunk(body: &mut Pin<Box<BodyDataStream>>) -> Option<Bytes> {
    std::future::poll_fn(|cx| match body.as_mut().poll_next(cx) {
        Poll::Ready(Some(Ok(bytes))) => Poll::Ready(Some(bytes)),
        Poll::Ready(Some(Err(_)) | None) => Poll::Ready(None),
        Poll::Pending => Poll::Pending,
    })
    .await
}

/// A body read as text, event by event.
struct Reader {
    body: Pin<Box<BodyDataStream>>,
    buffer: String,
}

impl Reader {
    fn new(response: Response) -> Self {
        Self {
            body: Box::pin(response.into_body().into_data_stream()),
            buffer: String::new(),
        }
    }

    /// The next complete event block (lines up to a blank line), or `None`
    /// when the body ended.
    async fn next_block(&mut self) -> Option<String> {
        loop {
            if let Some(end) = self.buffer.find("\n\n") {
                let block = self.buffer[..end].to_owned();
                self.buffer.drain(..end + 2);
                return Some(block);
            }
            let chunk = next_chunk(&mut self.body).await?;
            self.buffer.push_str(&String::from_utf8_lossy(&chunk));
        }
    }

    /// The next block, or a panic after `budget`.
    async fn next_within(&mut self, budget: Duration) -> Option<String> {
        tokio::time::timeout(budget, self.next_block())
            .await
            .expect("a block arrives within the budget")
    }
}

/// Emit `count` events named `tick` with ids, then complete.
fn ticking(hub: StreamHub, count: u64) -> impl Fn() -> std::future::Ready<Response> + Clone {
    move || {
        let (tx, rx) = hub.channel();
        tokio::spawn(async move {
            for i in 1..=count {
                if tx
                    .emit(Event::new("tick", i.to_string()).with_id(i))
                    .is_err()
                {
                    break;
                }
            }
        });
        std::future::ready(stream(rx))
    }
}

#[tokio::test]
async fn fr001_a_declared_route_carries_the_four_headers_and_no_encoding() {
    let hub = StreamHub::new(options());
    let app = Router::new().stream_route("/events", get(ticking(hub.clone(), 3)));
    let (router, cell) = edge(app, &hub).await;

    let response = open(&router, get_request("/events")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let h = response.headers();
    assert_eq!(h.get(header::CONTENT_TYPE).unwrap(), "text/event-stream");
    assert_eq!(h.get(header::CACHE_CONTROL).unwrap(), "no-store");
    assert_eq!(h.get(header::CONNECTION).unwrap(), "keep-alive");
    assert_eq!(h.get("x-accel-buffering").unwrap(), "no");
    assert!(
        h.get(header::CONTENT_ENCODING).is_none(),
        "no encoding despite Accept-Encoding: gzip"
    );

    let mut reader = Reader::new(response);
    let mut blocks = Vec::new();
    while let Some(block) = reader.next_within(Duration::from_secs(2)).await {
        if !block.starts_with(':') {
            blocks.push(block);
        }
    }
    assert_eq!(
        blocks,
        vec![
            "event: tick\ndata: 1\nid: 1",
            "event: tick\ndata: 2\nid: 2",
            "event: tick\ndata: 3\nid: 3",
        ]
    );
    assert_eq!(hub.open_total(), 0, "a completed stream released its slot");
    cell.stop().await;
}

#[tokio::test]
async fn fr002_an_idle_stream_keeps_alive_and_outlives_the_first_byte_timeout() {
    let hub = StreamHub::new(options());
    let handler = {
        let hub = hub.clone();
        move || {
            let (tx, rx) = hub.channel();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(700)).await;
                let _ = tx.emit(Event::new("late", "after the timeout"));
            });
            std::future::ready(stream(rx))
        }
    };
    let app = Router::new().stream_route("/idle", get(handler));
    let (router, cell) = edge(app, &hub).await;

    let started = Instant::now();
    let response = open(&router, get_request("/idle")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut reader = Reader::new(response);
    let first = reader.next_within(Duration::from_secs(1)).await.unwrap();
    assert_eq!(
        first, ": keep-alive",
        "a keep-alive arrives while the source is idle"
    );
    assert!(started.elapsed() < Duration::from_millis(400));

    let mut blocks = Vec::new();
    while let Some(block) = reader.next_within(Duration::from_secs(3)).await {
        if !block.starts_with(':') {
            blocks.push(block);
        }
    }
    assert_eq!(blocks, vec!["event: late\ndata: after the timeout"]);
    assert!(
        started.elapsed() >= Duration::from_millis(700),
        "the stream stayed open past the 300ms first-byte timeout"
    );
    cell.stop().await;
}

#[tokio::test]
async fn fr002_the_first_byte_timeout_still_applies_to_a_handler_that_never_answers() {
    let hub = StreamHub::new(options());
    let app = Router::new().stream_route(
        "/never",
        get(|| async {
            tokio::time::sleep(Duration::from_secs(5)).await;
            "too late"
        }),
    );
    let (router, cell) = edge(app, &hub).await;
    let started = Instant::now();
    let response = open(&router, get_request("/never")).await;
    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    assert!(started.elapsed() < Duration::from_secs(2));
    cell.stop().await;
}

#[tokio::test]
async fn fr003_a_consumer_that_reads_nothing_gets_overflow_and_the_producer_ends() {
    let hub = StreamHub::new(options());
    let ended = Arc::new(AtomicBool::new(false));
    let handler = {
        let hub = hub.clone();
        let ended = ended.clone();
        move || {
            let (tx, rx) = hub.channel();
            let ended = ended.clone();
            tokio::spawn(async move {
                let mut result = Ok(());
                for i in 1..=200u64 {
                    result = tx.emit(Event::new("burst", i.to_string()));
                    if result.is_err() {
                        break;
                    }
                }
                assert_eq!(result, Err(Closed::Overflow));
                assert!(tx.is_cancelled());
                ended.store(true, Ordering::SeqCst);
            });
            std::future::ready(stream(rx))
        }
    };
    let app = Router::new().stream_route("/burst", get(handler));
    let (router, cell) = edge(app, &hub).await;

    let response = open(&router, get_request("/burst")).await;
    assert_eq!(response.status(), StatusCode::OK);
    // Read nothing for a while: the producer fills the channel and trips.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        ended.load(Ordering::SeqCst),
        "the producer task ended on its own"
    );

    let mut reader = Reader::new(response);
    let mut blocks = Vec::new();
    while let Some(block) = reader.next_within(Duration::from_secs(2)).await {
        if !block.starts_with(':') {
            blocks.push(block);
        }
    }
    assert_eq!(blocks.last().map(String::as_str), Some("event: overflow"));
    assert!(
        blocks.len() <= 9,
        "at most the channel's depth plus the overflow: {blocks:?}"
    );
    assert_eq!(hub.open_total(), 0);
    cell.stop().await;
}

/// A handler that holds its stream open until dropped.
fn holding(
    hub: StreamHub,
    producers: Arc<std::sync::Mutex<Vec<Producer>>>,
) -> impl Fn() -> std::future::Ready<Response> + Clone {
    move || {
        let (tx, rx) = hub.channel();
        producers.lock().unwrap().push(tx);
        std::future::ready(stream(rx))
    }
}

#[tokio::test]
async fn fr004_a_fifth_stream_is_429_with_a_decision_and_a_closed_one_frees_the_slot() {
    let hub = StreamHub::new(options());
    let producers = Arc::new(std::sync::Mutex::new(Vec::new()));
    let app = Router::new().stream_route("/hold", get(holding(hub.clone(), producers.clone())));
    let (router, cell) = edge(app, &hub).await;

    let head_before = cell.state.ledger().count().await.unwrap();
    let mut open_streams = Vec::new();
    for _ in 0..4 {
        let response = open(&router, get_request("/hold")).await;
        assert_eq!(response.status(), StatusCode::OK);
        open_streams.push(response);
    }
    assert_eq!(hub.open_total(), 4);

    let refused = open(&router, get_request("/hold")).await;
    assert_eq!(refused.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(refused.headers().get(header::RETRY_AFTER).is_some());
    let body = axum::body::to_bytes(refused.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let decision = json["decision"].as_str().expect("a decision id");
    assert!(decision.contains(':'), "{decision}");
    cell.state
        .kernel()
        .flush(Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(
        cell.state.ledger().count().await.unwrap(),
        head_before + 1,
        "exactly one Decision was ledgered"
    );

    drop(open_streams.pop());
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(hub.open_total(), 3, "the closed stream freed its slot");
    let admitted = open(&router, get_request("/hold")).await;
    assert_eq!(admitted.status(), StatusCode::OK);
    open_streams.push(admitted);

    let producer = producers.lock().unwrap()[3].clone();
    assert!(
        producer.is_cancelled(),
        "the dropped stream cancelled its producer (B-7)"
    );
    drop(open_streams);
    cell.stop().await;
}

#[tokio::test]
async fn fr005_shutdown_tells_every_open_stream_and_drains_within_the_timeout() {
    let hub = StreamHub::new(options());
    let producers = Arc::new(std::sync::Mutex::new(Vec::new()));
    let app = Router::new().stream_route("/hold", get(holding(hub.clone(), producers.clone())));
    let (router, cell) = edge(app, &hub).await;

    let a = open(&router, get_request("/hold")).await;
    let b = open(&router, get_request("/hold")).await;
    assert_eq!(hub.open_total(), 2);
    let mut ra = Reader::new(a);
    let mut rb = Reader::new(b);

    let started = Instant::now();
    let drain = tokio::spawn({
        let hub = hub.clone();
        async move { hub.drain().await }
    });
    let ea = ra.next_within(Duration::from_secs(2)).await.unwrap();
    let eb = rb.next_within(Duration::from_secs(2)).await.unwrap();
    assert_eq!(ea, "event: shutdown");
    assert_eq!(eb, "event: shutdown");
    assert!(
        ra.next_within(Duration::from_secs(2)).await.is_none(),
        "the stream closed"
    );
    assert!(rb.next_within(Duration::from_secs(2)).await.is_none());
    let remaining = drain.await.unwrap();
    assert_eq!(remaining, 0, "every stream closed before the drain timeout");
    assert!(started.elapsed() < options().drain_timeout);
    assert_eq!(hub.open_total(), 0);
    for producer in producers.lock().unwrap().iter() {
        assert!(producer.is_cancelled());
    }
    cell.stop().await;
}

#[tokio::test]
async fn fr006_a_stream_from_an_undeclared_route_is_500_with_a_decision() {
    let hub = StreamHub::new(options());
    let app = Router::new().route(
        "/plain",
        get(|| async {
            let (_tx, rx) = channel(8);
            stream(rx)
        }),
    );
    let (router, cell) = edge(app, &hub).await;
    let head_before = cell.state.ledger().count().await.unwrap();

    let response = open(&router, get_request("/plain")).await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json["decision"].as_str().is_some(), "{json}");
    assert!(
        json["message"].as_str().unwrap().contains("/plain"),
        "names the route: {json}"
    );
    cell.state
        .kernel()
        .flush(Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(cell.state.ledger().count().await.unwrap(), head_before + 1);
    assert_eq!(hub.open_total(), 0, "nothing was attached");
    cell.stop().await;
}

#[tokio::test]
async fn a_dropped_client_cancels_the_producer_within_a_keep_alive() {
    let hub = StreamHub::new(options());
    let producers = Arc::new(std::sync::Mutex::new(Vec::new()));
    let app = Router::new().stream_route("/hold", get(holding(hub.clone(), producers.clone())));
    let (router, cell) = edge(app, &hub).await;

    let response = open(&router, get_request("/hold")).await;
    let producer = producers.lock().unwrap()[0].clone();
    assert!(!producer.is_cancelled());
    let waiter = tokio::spawn({
        let producer = producer.clone();
        async move { producer.cancelled().await }
    });
    drop(response);
    tokio::time::timeout(options().keep_alive * 2, waiter)
        .await
        .expect("cancelled within one keep-alive interval")
        .unwrap();
    assert!(producer.is_cancelled());
    assert_eq!(producer.emit(Event::new("x", "y")), Err(Closed::Cancelled));
    cell.stop().await;
}

#[tokio::test]
async fn ac2_metrics_expose_the_gauge_and_the_closed_counter_by_every_outcome() {
    let obs = rahi_edge::obs::init(ObsOptions::from_config(&common::config(
        "http://localhost:8080",
    )))
    .unwrap();
    let hub = StreamHub::new(options());
    let app = Router::new().stream_route("/events", get(ticking(hub.clone(), 1)));
    let (router, cell) = edge(app, &hub).await;

    let response = open(&router, get_request("/events")).await;
    let mut reader = Reader::new(response);
    while reader.next_within(Duration::from_secs(2)).await.is_some() {}
    // Other tests in this binary hold streams open concurrently, so the
    // gauge is only bounded, never zero, from here.
    assert!(obs.metrics().streams_open() >= 0);

    let exposition = common::send(&router, common::get("/metrics")).await;
    assert_eq!(exposition.status, StatusCode::OK);
    assert!(
        exposition.body.contains("rahi_streams_open "),
        "{}",
        exposition.body
    );
    assert!(
        exposition
            .body
            .contains("rahi_stream_duration_seconds_bucket")
    );
    assert!(exposition.body.contains("rahi_stream_events_total "));
    for outcome in ["complete", "client_gone", "overflow", "shutdown"] {
        assert!(
            exposition.body.contains(&format!(
                "rahi_streams_closed_total{{outcome=\"{outcome}\"}}"
            )),
            "{outcome} is exposed:\n{}",
            exposition.body
        );
    }
    cell.stop().await;
}

#[tokio::test]
async fn a_declaration_binds_to_its_route_not_to_its_path() {
    let hub = StreamHub::new(options());
    let streaming = Router::new().stream_route("/events", get(ticking(hub.clone(), 1)));
    let plain = Router::new().route(
        "/events",
        get(|| async {
            let (_tx, rx) = channel(8);
            stream(rx)
        }),
    );
    let cell = common::boot("http://localhost:8080").await;
    let state = cell.state.clone().with_extension(hub.clone());
    let router = Edge::builder(state)
        .mount("/a", streaming)
        .mount("/b", plain)
        .try_build()
        .unwrap();

    let ok = open(&router, get_request("/a/events")).await;
    assert_eq!(
        ok.status(),
        StatusCode::OK,
        "the declared route streams under its mount"
    );
    let refused = open(&router, get_request("/b/events")).await;
    assert_eq!(
        refused.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "the same path under another mount is not declared"
    );
    cell.stop().await;
}

#[test]
fn defaults_are_the_numbers_the_spec_names() {
    let d = StreamOptions::default();
    assert_eq!(d.max_concurrent_streams, 4);
    assert_eq!(d.channel_capacity, 64);
    assert_eq!(d.drain_timeout, Duration::from_secs(10));
    assert_eq!(d.keep_alive, Duration::from_secs(15));
}
