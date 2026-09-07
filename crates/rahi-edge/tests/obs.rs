//! The observability contract, against a real cell (spec 023 FR-001 to
//! FR-004, B-1 to B-5).

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]

mod common;

use std::net::TcpListener;
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use rahi_edge::obs::layer::REQUEST_SPAN;
use rahi_edge::obs::{self, DECISION_ID, ObsOptions, Ring, SpanRecord, TRACE_ID, Trace};
use rahi_edge::{AppState, Edge, EdgeError};
use rahi_kernel::{CapabilityKind, Governed};
use rahi_types::Sub;

use common::{boot, get as get_request, send};

/// The observability context every test in this binary shares.
///
/// `init` is idempotent by design (a second registry would be a second answer
/// to a scrape), so this is the one call and every test rides on it. Nothing
/// here configures an exporter: FR-003 is the assertion that none exists.
fn observability() -> &'static obs::Obs {
    obs::init(ObsOptions {
        ring_capacity: 1_000,
        otlp_endpoint: None,
        service_name: "rahi-test".to_owned(),
    })
    .expect("the observability context builds")
}

/// A route that answers, and one that is refused by the kernel.
fn app() -> Router<AppState> {
    Router::new()
        .route("/counted", get(|| async { "counted" }))
        .route("/denied", get(denied))
        .route("/store", get(denied))
}

/// A governed read against a service whose ceiling is empty: the kernel
/// refuses it, ledgers the refusal, and the 403 body names the decision.
async fn denied(State(state): State<AppState>) -> Result<&'static str, EdgeError> {
    let governed = Governed::new(
        state.kernel(),
        "notes",
        CapabilityKind::DbRead,
        "notes",
        state.store().clone(),
    )
    .map_err(EdgeError::from)?;
    let _rows: Vec<(String,)> = governed
        .query(&Sub::new("rauthy-subject"), "SELECT 1", vec![])
        .await
        .map_err(EdgeError::from)?;
    Ok("never reached")
}

/// The trace whose root carries `field` equal to `value`.
fn trace_where(field: &str, value: &str) -> Option<Trace> {
    obs::list_traces()
        .into_iter()
        .find(|trace| trace.root.field(field) == Some(value))
}

/// The value of one Prometheus sample, by its whole labelled name.
fn sample(text: &str, name_with_labels: &str) -> Option<f64> {
    text.lines()
        .filter(|line| !line.starts_with('#'))
        .find_map(|line| {
            let (name, value) = line.rsplit_once(' ')?;
            (name == name_with_labels).then(|| value.parse().ok())?
        })
}

// ------------------------------------------------------------------ FR-001

/// Ten requests to one route are ten in the counter and ten samples in the
/// histogram, both labelled by the route pattern rather than the path.
#[tokio::test]
async fn ten_requests_are_ten_in_the_counter_and_the_histogram() {
    let obs = observability();
    let cell = boot("https://cell.example.com").await;
    let router = Edge::builder(cell.state.clone())
        .mount("/fr001", app().with_state(cell.state.clone()))
        .build();

    for _ in 0..10 {
        let answer = send(&router, get_request("/fr001/counted")).await;
        assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    }

    assert_eq!(obs.metrics().requests("/fr001/counted", "GET", 200), 10);
    assert_eq!(obs.metrics().request_samples("/fr001/counted", "GET"), 10);

    let exposition = send(&router, get_request("/metrics")).await;
    assert_eq!(exposition.status, StatusCode::OK, "{}", exposition.body);
    assert_eq!(
        sample(
            &exposition.body,
            "http_requests_total{method=\"GET\",route=\"/fr001/counted\",status_class=\"2xx\"}"
        ),
        Some(10.0),
        "the exposition carries the counter:\n{}",
        exposition.body
    );
    assert_eq!(
        sample(
            &exposition.body,
            "http_request_duration_seconds_count{method=\"GET\",route=\"/fr001/counted\"}"
        ),
        Some(10.0),
        "and ten samples in the histogram:\n{}",
        exposition.body
    );

    cell.stop().await;
}

// ------------------------------------------------------------------ FR-002

/// A refused request leaves a trace whose root span names the same decision
/// the client was told about.
#[tokio::test]
async fn a_denied_request_leaves_its_decision_id_on_the_trace() {
    let _obs = observability();
    let cell = boot("https://cell.example.com").await;
    let router = Edge::builder(cell.state.clone())
        .mount("/fr002", app().with_state(cell.state.clone()))
        .build();

    let refused = send(&router, get_request("/fr002/denied")).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.body);
    let body = refused.json();
    let decision = body["decision"]
        .as_str()
        .expect("the 403 body names the decision (spec 020 B-8)");

    let trace = trace_where(DECISION_ID, decision)
        .expect("the ring holds the trace of the request that was refused");
    assert_eq!(trace.root.name, REQUEST_SPAN);
    assert_eq!(trace.root.field(DECISION_ID), Some(decision));
    assert_eq!(trace.root.field("route"), Some("/fr002/denied"));
    assert_eq!(trace.root.field("status"), Some("403"));
    assert_eq!(
        trace.root.field("rahi.decision.outcome"),
        Some("deny"),
        "the outcome travels with the id"
    );
    assert_eq!(trace.id.len(), 32, "the trace is identified: {}", trace.id);
    assert_eq!(trace.root.field(TRACE_ID), Some(trace.id.as_str()));

    // B-2: the governed operation opened a span of its own, inside the
    // request, and B-1 counted it.
    let store: Vec<&SpanRecord> = trace
        .children
        .iter()
        .filter(|span| span.name == rahi_kernel::facade::STORE_SPAN)
        .collect();
    assert_eq!(store.len(), 1, "one governed operation, one span");
    assert_eq!(store[0].field("kind"), Some("db.read"));
    assert!(
        _obs.metrics().registry().gather().iter().any(|family| {
            family.name() == "store_ops_total"
                && family
                    .get_metric()
                    .iter()
                    .any(|m| m.get_counter().get_value() >= 1.0)
        }),
        "the store operation was counted"
    );

    cell.stop().await;
}

// ------------------------------------------------------------------ FR-003

/// With no endpoint configured there is no exporter, and nothing dials one.
#[tokio::test]
async fn nothing_is_exported_when_no_endpoint_is_configured() {
    let obs = observability();
    assert!(
        !obs.exports(),
        "no OTLP endpoint was configured, so no exporter exists"
    );

    // A collector that would have received the spans, if one had been built.
    let collector = TcpListener::bind("127.0.0.1:0").expect("a free port");
    collector
        .set_nonblocking(true)
        .expect("a listener that does not block");

    let cell = boot("https://cell.example.com").await;
    let router = Edge::builder(cell.state.clone())
        .mount("/fr003", app().with_state(cell.state.clone()))
        .build();
    for _ in 0..5 {
        let answer = send(&router, get_request("/fr003/counted")).await;
        assert_eq!(answer.status, StatusCode::OK);
    }
    // Batched export would not be instant even if it existed, so give it time
    // to be wrong in.
    tokio::time::sleep(Duration::from_millis(200)).await;

    assert!(
        collector.accept().is_err(),
        "a cell with no configured collector opens no connection to one"
    );

    cell.stop().await;
}

// ------------------------------------------------------------------ FR-004

/// The ring is bounded: the oldest goes when the newest arrives.
#[test]
fn the_ring_evicts_the_oldest_at_capacity_plus_one() {
    let ring = Ring::with_capacity(3);
    let trace = |id: &str| Trace {
        id: id.to_owned(),
        root: SpanRecord {
            name: REQUEST_SPAN.to_owned(),
            duration_ms: 1,
            fields: std::collections::BTreeMap::new(),
        },
        children: Vec::new(),
    };

    for id in ["one", "two", "three"] {
        ring.push(trace(id));
    }
    assert_eq!(ring.len(), 3);
    assert!(ring.get("one").is_some());

    ring.push(trace("four"));
    assert_eq!(ring.len(), 3, "capacity plus one is still capacity");
    assert!(ring.get("one").is_none(), "the oldest was evicted");
    assert!(ring.get("four").is_some(), "the newest is held");
}

/// A subscriber sees traces as they complete (B-3).
#[tokio::test]
async fn a_subscriber_sees_a_trace_as_it_completes() {
    let _obs = observability();
    let mut traces = obs::subscribe().expect("the context is initialised");

    let cell = boot("https://cell.example.com").await;
    let router = Edge::builder(cell.state.clone())
        .mount("/fr004", app().with_state(cell.state.clone()))
        .build();
    let answer = send(&router, get_request("/fr004/counted")).await;
    assert_eq!(answer.status, StatusCode::OK);

    let seen = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match traces.recv().await {
                Ok(trace) if trace.root.field("route") == Some("/fr004/counted") => return trace,
                Ok(_) => continue,
                Err(err) => panic!("the channel closed: {err}"),
            }
        }
    })
    .await
    .expect("the trace arrives");
    assert_eq!(seen.root.field("status"), Some("200"));

    cell.stop().await;
}

// -------------------------------------------------------------------- B-5

/// The scrape does not observe itself, and an unrouted path carries no label.
#[tokio::test]
async fn the_scrape_and_the_static_slot_are_not_observed() {
    let obs = observability();
    let cell = boot("https://cell.example.com").await;
    let router = Edge::builder(cell.state.clone())
        .mount("/b5", app().with_state(cell.state.clone()))
        .build();

    for _ in 0..3 {
        let exposition = send(&router, get_request("/metrics")).await;
        assert_eq!(exposition.status, StatusCode::OK);
        let missing = send(&router, get_request("/no-such-route-at-all")).await;
        assert_eq!(missing.status, StatusCode::NOT_FOUND);
    }

    let exposition = send(&router, get_request("/metrics")).await;
    assert!(
        !exposition.body.contains("route=\"/metrics\""),
        "the scrape is not counted:\n{}",
        exposition.body
    );
    assert!(
        !exposition.body.contains("/no-such-route-at-all"),
        "an unrouted path never becomes a label:\n{}",
        exposition.body
    );
    // The ring is shared with every other test in this binary, so the claim
    // is about what is in it rather than how much: no trace of a scrape, and
    // none of a path that matched no route.
    let traces = obs::list_traces();
    assert!(
        !traces
            .iter()
            .any(|trace| trace.root.field("route") == Some("/metrics")),
        "the scrape left no trace"
    );
    assert!(
        !traces.iter().any(|trace| {
            trace
                .root
                .field("route")
                .is_some_and(|route| route.contains("no-such-route"))
        }),
        "an unrouted path left no trace"
    );
    let _ = obs;

    cell.stop().await;
}
