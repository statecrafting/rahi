//! The process-wide Prometheus registry (spec 023 B-1).
//!
//! Constitution XIII makes this contract non-negotiable and gives it no flag:
//! every cell serves `/metrics`, always, and deployment (spec 032) is what
//! keeps it off the public ingress rather than a setting an operator can get
//! wrong.
//!
//! Every label is a static route pattern or a fixed word. A raw path or an
//! id in a label is an unbounded label set, which is how a metrics endpoint
//! becomes the thing that takes a cell down.

use std::time::Duration;

use prometheus::{
    Histogram, HistogramOpts, HistogramVec, IntCounter, IntCounterVec, IntGauge, Opts, Registry,
    TextEncoder,
};
use rahi_types::{Error, Result};

/// Requests served, by route pattern, method, and status class.
pub const HTTP_REQUESTS: &str = "http_requests_total";
/// How long a request took, by route pattern and method.
pub const HTTP_DURATION: &str = "http_request_duration_seconds";
/// Governed store operations, by operation.
pub const STORE_OPS: &str = "store_ops_total";
/// How long a governed store operation took, by operation.
pub const STORE_DURATION: &str = "store_op_duration_seconds";
/// Kernel decisions, by outcome.
pub const KERNEL_DECISIONS: &str = "kernel_decisions_total";
/// Appends of a decision that failed (spec 015 B-6).
pub const KERNEL_LEDGER_FAILURES: &str = "kernel_ledger_failures_total";
/// Streams open right now (spec 026 B-9).
pub const STREAMS_OPEN: &str = "rahi_streams_open";
/// How long a stream stayed open (spec 026 B-9).
pub const STREAM_DURATION: &str = "rahi_stream_duration_seconds";
/// Events delivered over streams (spec 026 B-9).
pub const STREAM_EVENTS: &str = "rahi_stream_events_total";
/// Streams closed, by outcome (spec 026 B-9).
pub const STREAMS_CLOSED: &str = "rahi_streams_closed_total";

/// The buckets a stream's lifetime falls into, in seconds: a stream lives
/// seconds to hours, not milliseconds.
pub const STREAM_BUCKETS: [f64; 10] = [
    0.1, 0.5, 1.0, 5.0, 15.0, 60.0, 300.0, 900.0, 3600.0, 14400.0,
];

/// The buckets a request duration falls into, in seconds.
pub const REQUEST_BUCKETS: [f64; 11] = [
    0.001, 0.005, 0.010, 0.025, 0.050, 0.100, 0.250, 0.500, 1.0, 2.5, 10.0,
];

/// The registry and the collectors spec 023 B-1 names.
#[derive(Clone, Debug)]
pub struct Metrics {
    registry: Registry,
    http_requests: IntCounterVec,
    http_duration: HistogramVec,
    store_ops: IntCounterVec,
    store_duration: HistogramVec,
    kernel_decisions: IntCounterVec,
    kernel_ledger_failures: IntCounter,
    streams_open: IntGauge,
    stream_duration: Histogram,
    stream_events: IntCounter,
    streams_closed: IntCounterVec,
}

impl Metrics {
    /// Build the registry and register every collector.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when a collector cannot be built or registered,
    /// which means two collectors were given the same name.
    pub fn new() -> Result<Self> {
        let registry = Registry::new();
        let http_requests = IntCounterVec::new(
            Opts::new(
                HTTP_REQUESTS,
                "Requests served, by route, method, and status class",
            ),
            &["route", "method", "status_class"],
        )
        .map_err(config)?;
        let http_duration = HistogramVec::new(
            HistogramOpts::new(HTTP_DURATION, "How long a request took")
                .buckets(REQUEST_BUCKETS.to_vec()),
            &["route", "method"],
        )
        .map_err(config)?;
        let store_ops = IntCounterVec::new(
            Opts::new(STORE_OPS, "Governed store operations, by operation"),
            &["op"],
        )
        .map_err(config)?;
        let store_duration = HistogramVec::new(
            HistogramOpts::new(STORE_DURATION, "How long a governed store operation took")
                .buckets(REQUEST_BUCKETS.to_vec()),
            &["op"],
        )
        .map_err(config)?;
        let kernel_decisions = IntCounterVec::new(
            Opts::new(KERNEL_DECISIONS, "Kernel decisions, by outcome"),
            &["outcome"],
        )
        .map_err(config)?;
        let kernel_ledger_failures = IntCounter::with_opts(Opts::new(
            KERNEL_LEDGER_FAILURES,
            "Decisions the ledger could not be told about",
        ))
        .map_err(config)?;

        registry
            .register(Box::new(http_requests.clone()))
            .map_err(config)?;
        registry
            .register(Box::new(http_duration.clone()))
            .map_err(config)?;
        registry
            .register(Box::new(store_ops.clone()))
            .map_err(config)?;
        registry
            .register(Box::new(store_duration.clone()))
            .map_err(config)?;
        registry
            .register(Box::new(kernel_decisions.clone()))
            .map_err(config)?;
        registry
            .register(Box::new(kernel_ledger_failures.clone()))
            .map_err(config)?;

        // Spec 026 B-9: one gauge, one histogram, two counters per stream.
        // The closed counter is initialised for every outcome, so a scrape
        // sees the whole vocabulary before any stream has closed.
        let streams_open = IntGauge::with_opts(Opts::new(STREAMS_OPEN, "Streams open right now"))
            .map_err(config)?;
        let stream_duration = Histogram::with_opts(
            HistogramOpts::new(STREAM_DURATION, "How long a stream stayed open")
                .buckets(STREAM_BUCKETS.to_vec()),
        )
        .map_err(config)?;
        let stream_events =
            IntCounter::with_opts(Opts::new(STREAM_EVENTS, "Events delivered over streams"))
                .map_err(config)?;
        let streams_closed = IntCounterVec::new(
            Opts::new(STREAMS_CLOSED, "Streams closed, by outcome"),
            &["outcome"],
        )
        .map_err(config)?;
        for outcome in crate::stream::Outcome::ALL {
            streams_closed
                .with_label_values(&[outcome.as_str()])
                .inc_by(0);
        }
        registry
            .register(Box::new(streams_open.clone()))
            .map_err(config)?;
        registry
            .register(Box::new(stream_duration.clone()))
            .map_err(config)?;
        registry
            .register(Box::new(stream_events.clone()))
            .map_err(config)?;
        registry
            .register(Box::new(streams_closed.clone()))
            .map_err(config)?;
        register_process_collector(&registry)?;

        Ok(Self {
            registry,
            http_requests,
            http_duration,
            store_ops,
            store_duration,
            kernel_decisions,
            kernel_ledger_failures,
            streams_open,
            stream_duration,
            stream_events,
            streams_closed,
        })
    }

    /// The exposition, in Prometheus text format.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the gathered families cannot be encoded, which
    /// would mean a collector produced something the encoder refuses.
    pub fn render(&self) -> Result<String> {
        TextEncoder::new()
            .encode_to_string(&self.registry.gather())
            .map_err(|err| Error::Io(format!("the metrics exposition cannot be encoded: {err}")))
    }

    /// The registry, for a caller that has its own collector to register.
    #[must_use]
    pub const fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Count one served request.
    ///
    /// `route` is a route pattern (`/api/notes/{id}`), never a path.
    pub fn record_request(&self, route: &str, method: &str, status: u16, elapsed: Duration) {
        self.http_requests
            .with_label_values(&[route, method, status_class(status)])
            .inc();
        self.http_duration
            .with_label_values(&[route, method])
            .observe(elapsed.as_secs_f64());
    }

    /// Count one governed store operation.
    pub fn record_store_op(&self, op: &str, elapsed: Duration) {
        self.store_ops.with_label_values(&[op]).inc();
        self.store_duration
            .with_label_values(&[op])
            .observe(elapsed.as_secs_f64());
    }

    /// Count one kernel decision.
    pub fn record_decision(&self, outcome: &str) {
        self.kernel_decisions.with_label_values(&[outcome]).inc();
    }

    /// Count one decision the ledger could not be told about.
    pub fn record_ledger_failure(&self) {
        self.kernel_ledger_failures.inc();
    }

    /// One more stream is open (spec 026 B-9).
    pub fn record_stream_open(&self) {
        self.streams_open.inc();
    }

    /// A stream closed with `outcome` after `elapsed`, having delivered
    /// `events` (spec 026 B-9).
    pub fn record_stream_closed(
        &self,
        outcome: crate::stream::Outcome,
        elapsed: Duration,
        events: u64,
    ) {
        self.streams_open.dec();
        self.stream_duration.observe(elapsed.as_secs_f64());
        self.stream_events.inc_by(events);
        self.streams_closed
            .with_label_values(&[outcome.as_str()])
            .inc();
    }

    /// Streams open right now, for a test or a probe.
    #[must_use]
    pub fn streams_open(&self) -> i64 {
        self.streams_open.get()
    }

    /// The current value of a request counter, for a test or a probe.
    #[must_use]
    pub fn requests(&self, route: &str, method: &str, status: u16) -> u64 {
        self.http_requests
            .with_label_values(&[route, method, status_class(status)])
            .get()
    }

    /// How many samples the request histogram holds for a route.
    #[must_use]
    pub fn request_samples(&self, route: &str, method: &str) -> u64 {
        let histogram: Histogram = self.http_duration.with_label_values(&[route, method]);
        histogram.get_sample_count()
    }
}

/// The class a status belongs to: `2xx`, `4xx`, and so on. A class rather
/// than the code keeps the label set at five values.
#[must_use]
pub fn status_class(status: u16) -> &'static str {
    match status / 100 {
        1 => "1xx",
        2 => "2xx",
        3 => "3xx",
        4 => "4xx",
        5 => "5xx",
        _ => "other",
    }
}

/// The process collector, where there is a `/proc` to read (spec 023 D-2).
#[cfg(target_os = "linux")]
fn register_process_collector(registry: &Registry) -> Result<()> {
    registry
        .register(Box::new(
            prometheus::process_collector::ProcessCollector::for_self(),
        ))
        .map_err(config)
}

/// Elsewhere, the exposition carries no `process_*` families and says so by
/// their absence rather than by inventing them.
#[cfg(not(target_os = "linux"))]
const fn register_process_collector(_registry: &Registry) -> Result<()> {
    Ok(())
}

fn config(err: prometheus::Error) -> Error {
    Error::Config(format!("the metrics registry refused a collector: {err}"))
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn a_status_becomes_its_class() {
        assert_eq!(status_class(200), "2xx");
        assert_eq!(status_class(302), "3xx");
        assert_eq!(status_class(403), "4xx");
        assert_eq!(status_class(503), "5xx");
        assert_eq!(status_class(99), "other");
    }

    #[test]
    fn the_exposition_names_every_family_the_spec_fixes() {
        let metrics = Metrics::new().expect("the registry builds");
        metrics.record_request("/api/notes", "GET", 200, Duration::from_millis(3));
        metrics.record_store_op("db.read", Duration::from_millis(1));
        metrics.record_decision("deny");
        metrics.record_ledger_failure();

        let text = metrics.render().expect("the exposition encodes");
        for family in [
            HTTP_REQUESTS,
            HTTP_DURATION,
            STORE_OPS,
            STORE_DURATION,
            KERNEL_DECISIONS,
            KERNEL_LEDGER_FAILURES,
        ] {
            assert!(text.contains(family), "{family} is exposed:\n{text}");
        }
        assert!(text.contains("route=\"/api/notes\""), "{text}");
        assert!(text.contains("status_class=\"2xx\""), "{text}");
        assert!(text.contains("outcome=\"deny\""), "{text}");
    }

    #[test]
    fn two_registries_do_not_collide() {
        let first = Metrics::new().expect("the first registry builds");
        let second = Metrics::new().expect("a second registry builds too");
        first.record_request("/a", "GET", 200, Duration::from_millis(1));
        assert_eq!(first.requests("/a", "GET", 200), 1);
        assert_eq!(second.requests("/a", "GET", 200), 0, "they are separate");
    }
}
