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
    Histogram, HistogramOpts, HistogramVec, IntCounter, IntCounterVec, IntGauge, IntGaugeVec, Opts,
    Registry, TextEncoder,
};
use rahi_kernel::observe::Cause;
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
/// Appends of a decision that failed (spec 015 B-6); from spec 035 B-3,
/// nothing else.
pub const KERNEL_LEDGER_FAILURES: &str = "kernel_ledger_failures_total";
/// Denied decisions the kernel's full queue refused (spec 035 B-3).
pub const KERNEL_DECISIONS_DROPPED: &str = "kernel_decisions_dropped_total";
/// Denied decisions still owed when a stop's drain bound expired
/// (spec 035 B-2).
pub const KERNEL_DECISIONS_ABANDONED: &str = "kernel_decisions_abandoned_total";
/// Streams open right now (spec 026 B-9).
pub const STREAMS_OPEN: &str = "rahi_streams_open";
/// How long a stream stayed open (spec 026 B-9).
pub const STREAM_DURATION: &str = "rahi_stream_duration_seconds";
/// Events delivered over streams (spec 026 B-9).
pub const STREAM_EVENTS: &str = "rahi_stream_events_total";
/// Streams closed, by outcome (spec 026 B-9).
pub const STREAMS_CLOSED: &str = "rahi_streams_closed_total";
/// Work items per processor and state, redacted counts only: never a
/// tenant, namespace, key, or digest (spec 045 B-17, I-8).
pub const WORK_ITEMS: &str = "rahi_work_items";
/// How the previous boot stopped, by outcome and cause (spec 043 B-10).
pub const PREVIOUS_STOP: &str = "rahi_previous_stop";
/// Revocation rows held, by kind (spec 043 B-6 (i), D-10).
pub const REVOCATION_ROWS: &str = "rahi_revocation_rows";
/// Entries found on the legacy path's fence (spec 043 B-5, D-17 (e)).
pub const LEGACY_PATH_DEBRIS: &str = "rahi_legacy_path_debris";

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
    kernel_decisions_dropped: IntCounter,
    kernel_decisions_abandoned: IntCounter,
    streams_open: IntGauge,
    stream_duration: Histogram,
    stream_events: IntCounter,
    streams_closed: IntCounterVec,
    work_items: IntGaugeVec,
    previous_stop: IntGaugeVec,
    revocation_rows: IntGaugeVec,
    legacy_path_debris: IntGauge,
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
        let kernel_decisions_dropped = IntCounter::with_opts(Opts::new(
            KERNEL_DECISIONS_DROPPED,
            "Denied decisions the kernel's full queue refused",
        ))
        .map_err(config)?;
        let kernel_decisions_abandoned = IntCounter::with_opts(Opts::new(
            KERNEL_DECISIONS_ABANDONED,
            "Denied decisions still owed when a stop's drain bound expired",
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
        registry
            .register(Box::new(kernel_decisions_dropped.clone()))
            .map_err(config)?;
        registry
            .register(Box::new(kernel_decisions_abandoned.clone()))
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
        // Spec 045 B-17: one gauge, set from the cell's own sweep tick. The
        // chassis runs no loop (B-19), so nothing here writes to it but a
        // caller through `set_work_items`.
        let work_items = IntGaugeVec::new(
            Opts::new(WORK_ITEMS, "Work items per processor and state"),
            &["processor", "state"],
        )
        .map_err(config)?;
        registry
            .register(Box::new(work_items.clone()))
            .map_err(config)?;
        // Spec 043 B-10, B-6 (i) and B-5: set once at boot by the composer,
        // from what the process found on its volume and in its store.
        let previous_stop = IntGaugeVec::new(
            Opts::new(
                PREVIOUS_STOP,
                "How the previous boot stopped: 1 on its outcome and cause",
            ),
            &["outcome", "cause"],
        )
        .map_err(config)?;
        let revocation_rows = IntGaugeVec::new(
            Opts::new(REVOCATION_ROWS, "Retained bearer revocation rows, by kind"),
            &["kind"],
        )
        .map_err(config)?;
        let legacy_path_debris = IntGauge::with_opts(Opts::new(
            LEGACY_PATH_DEBRIS,
            "Entries beside the fence on the legacy store path",
        ))
        .map_err(config)?;
        for collector in [
            Box::new(previous_stop.clone()) as Box<dyn prometheus::core::Collector>,
            Box::new(revocation_rows.clone()),
            Box::new(legacy_path_debris.clone()),
        ] {
            registry.register(collector).map_err(config)?;
        }
        register_process_collector(&registry)?;

        Ok(Self {
            registry,
            http_requests,
            http_duration,
            store_ops,
            store_duration,
            kernel_decisions,
            kernel_ledger_failures,
            kernel_decisions_dropped,
            kernel_decisions_abandoned,
            streams_open,
            stream_duration,
            stream_events,
            streams_closed,
            work_items,
            previous_stop,
            revocation_rows,
            legacy_path_debris,
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

    /// Count one denied decision that did not reach the chain, on the family
    /// its cause names (spec 035 B-3).
    pub fn record_loss(&self, cause: Cause) {
        match cause {
            Cause::Dropped => self.kernel_decisions_dropped.inc(),
            Cause::Failed => self.kernel_ledger_failures.inc(),
            Cause::Abandoned => self.kernel_decisions_abandoned.inc(),
        }
    }

    /// How many denied decisions were lost to `cause`, for a test or a probe.
    #[must_use]
    pub fn losses(&self, cause: Cause) -> u64 {
        match cause {
            Cause::Dropped => self.kernel_decisions_dropped.get(),
            Cause::Failed => self.kernel_ledger_failures.get(),
            Cause::Abandoned => self.kernel_decisions_abandoned.get(),
        }
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

    /// Set the count of work items on `processor` in `state` (spec 045
    /// B-17). `processor` is a name the cell's code declares; `state` is
    /// one of `pending`, `claimed`, `failed`, or `dead`. Never a tenant,
    /// namespace, key, or digest (I-8).
    pub fn set_work_items(&self, processor: &str, state: &str, count: i64) {
        self.work_items
            .with_label_values(&[processor, state])
            .set(count);
    }

    /// The current value of a work item gauge, for a test or a probe.
    #[must_use]
    pub fn work_items(&self, processor: &str, state: &str) -> i64 {
        self.work_items.with_label_values(&[processor, state]).get()
    }

    /// Record how the previous boot stopped (spec 043 B-10): the one series
    /// for `outcome` and `cause` is `1`.
    pub fn set_previous_stop(&self, outcome: &str, cause: &str) {
        self.previous_stop.reset();
        self.previous_stop
            .with_label_values(&[outcome, cause])
            .set(1);
    }

    /// Set the retained revocation rows of `kind` (`jti` or `subject`).
    pub fn set_revocation_rows(&self, kind: &str, count: i64) {
        self.revocation_rows.with_label_values(&[kind]).set(count);
    }

    /// Set the number of entries beside the legacy path's fence.
    pub fn set_legacy_path_debris(&self, entries: i64) {
        self.legacy_path_debris.set(entries);
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
            KERNEL_DECISIONS_DROPPED,
            KERNEL_DECISIONS_ABANDONED,
        ] {
            assert!(text.contains(family), "{family} is exposed:\n{text}");
        }
        assert!(text.contains("route=\"/api/notes\""), "{text}");
        assert!(text.contains("status_class=\"2xx\""), "{text}");
        assert!(text.contains("outcome=\"deny\""), "{text}");
    }

    #[test]
    fn each_cause_counts_on_its_own_family_and_every_family_renders_at_zero() {
        let metrics = Metrics::new().expect("the registry builds");
        let text = metrics.render().expect("the exposition encodes");
        for family in [
            KERNEL_LEDGER_FAILURES,
            KERNEL_DECISIONS_DROPPED,
            KERNEL_DECISIONS_ABANDONED,
        ] {
            assert!(
                text.lines().any(|line| line == format!("{family} 0")),
                "{family} renders before any loss:\n{text}"
            );
        }

        metrics.record_loss(Cause::Dropped);
        metrics.record_loss(Cause::Abandoned);
        metrics.record_loss(Cause::Abandoned);
        assert_eq!(metrics.losses(Cause::Dropped), 1);
        assert_eq!(metrics.losses(Cause::Failed), 0, "a drop is not a failure");
        assert_eq!(metrics.losses(Cause::Abandoned), 2);
        let text = metrics.render().expect("the exposition encodes");
        assert!(
            text.contains(&format!("{KERNEL_DECISIONS_DROPPED} 1")),
            "{text}"
        );
        assert!(
            text.contains(&format!("{KERNEL_LEDGER_FAILURES} 0")),
            "{text}"
        );
        assert!(
            text.contains(&format!("{KERNEL_DECISIONS_ABANDONED} 2")),
            "{text}"
        );
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
