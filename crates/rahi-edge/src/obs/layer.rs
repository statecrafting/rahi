//! The two layers that make a request observable (spec 023 B-2, B-4, B-5).
//!
//! One is a `tracing` layer: it watches spans open and close, keeps what they
//! recorded, and pushes a finished request into the ring. The other is the
//! axum middleware spec 020 B-2 reserved the outermost position for, which
//! opens the request span, counts the answer, and closes it.
//!
//! A kernel decision reaches the trace as an event rather than as a field the
//! kernel had to declare. `rahi-kernel` imports nothing from this crate
//! (B-4), so the seam is `tracing` itself: the observer registered at init
//! emits an event on a known target, and this layer walks from that event out
//! to the request span it happened inside and merges the decision onto it.
//! That is what makes the root span of a denied request carry the same
//! decision id the 403 body does.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::sync::Arc;
use std::time::Instant;

use axum::extract::{MatchedPath, Request};
use axum::middleware::Next;
use axum::response::Response;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Instrument as _, Subscriber};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::registry::LookupSpan;

use crate::obs::metrics::Metrics;
use crate::obs::ring::{Ring, SpanRecord, Trace};
use crate::probes::{HEALTHZ_PATH, READYZ_PATH};

/// The name of the span one request opens.
pub const REQUEST_SPAN: &str = "http.request";
/// The field carrying the trace id, on the span and on the exported span.
pub const TRACE_ID: &str = "rahi.trace.id";
/// The event target the kernel observer emits a decision on.
pub const DECISION_TARGET: &str = "rahi.decision";
/// How an event's field is named on the span it is merged onto.
///
/// The event carries plain names because a `tracing` macro reads a leading
/// string literal as its message; the span carries the dotted names spec 023
/// B-4 fixes, which is what a reader and an exporter both see (D-5).
pub const DECISION_FIELDS: [(&str, &str); 5] = [
    ("id", DECISION_ID),
    ("outcome", DECISION_OUTCOME),
    ("capability", DECISION_CAPABILITY),
    ("reason", DECISION_REASON),
    ("ledger_failure", LEDGER_FAILURE),
];

/// The decision id, on the request span of the request it refused.
pub const DECISION_ID: &str = "rahi.decision.id";
/// The decision's outcome.
pub const DECISION_OUTCOME: &str = "rahi.decision.outcome";
/// The capability the decision was nearest to, when it named one.
pub const DECISION_CAPABILITY: &str = "rahi.decision.capability";
/// Why the decision went the way it did.
pub const DECISION_REASON: &str = "rahi.decision.reason";
/// The ledger failure that lost a decision (spec 015 B-6).
pub const LEDGER_FAILURE: &str = "rahi.ledger.failure";
/// The route label used for the metrics path, which is not instrumented.
pub const METRICS_PATH: &str = "/metrics";

/// What a span has recorded so far, held in the span's own extensions.
struct SpanState {
    name: String,
    started: Instant,
    fields: BTreeMap<String, String>,
    children: Vec<SpanRecord>,
}

/// Renders every field a span or an event records into a string map.
struct Fields<'m>(&'m mut BTreeMap<String, String>);

impl Visit for Fields<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        self.0.insert(field.name().to_owned(), format!("{value:?}"));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name().to_owned(), value.to_owned());
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.0.insert(field.name().to_owned(), value.to_string());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.0.insert(field.name().to_owned(), value.to_string());
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.0.insert(field.name().to_owned(), value.to_string());
    }
}

/// The layer that fills the ring and counts governed store operations.
#[derive(Clone, Debug)]
pub struct RingLayer {
    ring: Arc<Ring>,
    metrics: Metrics,
}

impl RingLayer {
    /// A layer over `ring`, counting into `metrics`.
    #[must_use]
    pub const fn new(ring: Arc<Ring>, metrics: Metrics) -> Self {
        Self { ring, metrics }
    }
}

impl<S> Layer<S> for RingLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut fields = BTreeMap::new();
        attrs.record(&mut Fields(&mut fields));
        span.extensions_mut().insert(SpanState {
            name: span.name().to_owned(),
            started: Instant::now(),
            fields,
            children: Vec::new(),
        });
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut extensions = span.extensions_mut();
        if let Some(state) = extensions.get_mut::<SpanState>() {
            values.record(&mut Fields(&mut state.fields));
        }
    }

    /// A decision is merged onto the request span it happened inside.
    fn on_event(&self, event: &tracing::Event<'_>, ctx: Context<'_, S>) {
        if event.metadata().target() != DECISION_TARGET {
            return;
        }
        let mut raw = BTreeMap::new();
        event.record(&mut Fields(&mut raw));
        let mut fields = BTreeMap::new();
        for (from, to) in DECISION_FIELDS {
            if let Some(value) = raw.remove(from)
                && !value.is_empty()
            {
                fields.insert((*to).to_owned(), value);
            }
        }
        if fields.is_empty() {
            return;
        }
        let Some(scope) = ctx.event_scope(event) else {
            return;
        };
        for span in scope {
            let mut extensions = span.extensions_mut();
            let Some(state) = extensions.get_mut::<SpanState>() else {
                continue;
            };
            if state.name == REQUEST_SPAN {
                state.fields.extend(fields);
                return;
            }
        }
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(&id) else { return };
        let state = span.extensions_mut().remove::<SpanState>();
        let Some(state) = state else { return };

        let record = SpanRecord {
            name: state.name.clone(),
            duration_ms: u64::try_from(state.started.elapsed().as_millis()).unwrap_or(u64::MAX),
            fields: state.fields,
        };

        if record.name == rahi_kernel::facade::STORE_SPAN
            && let Some(op) = record.field(rahi_kernel::facade::STORE_SPAN_KIND)
        {
            self.metrics.record_store_op(op, state.started.elapsed());
        }

        if record.name == REQUEST_SPAN {
            let id = record.field(TRACE_ID).unwrap_or_default().to_owned();
            self.ring.push(Trace {
                id,
                root: record,
                children: state.children,
            });
            return;
        }

        // Not a request: hang it on the request it happened inside, if any.
        for ancestor in span.scope().skip(1) {
            let mut extensions = ancestor.extensions_mut();
            let Some(parent) = extensions.get_mut::<SpanState>() else {
                continue;
            };
            if parent.name == REQUEST_SPAN {
                parent.children.push(record);
                return;
            }
        }
    }
}

/// Whether a request is observed at all (B-5).
///
/// `/metrics` is not: a scrape that filled the ring with its own arrival
/// would leave a cell observing nothing but being observed. Neither is a
/// request that matched no route, which is the static slot and the 404: the
/// only label available for one is its raw path, and a label set an
/// unauthenticated client can grow is how a metrics endpoint takes a cell
/// down (spec 023 D-3).
#[must_use]
pub fn is_instrumented(path: &str, route: Option<&str>) -> bool {
    if path == METRICS_PATH {
        return false;
    }
    route.is_some()
}

/// The outermost middleware: open the span, run the request, count it.
pub async fn observe(request: Request, next: Next) -> Response {
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(|matched| matched.as_str().to_owned());
    if !is_instrumented(request.uri().path(), route.as_deref()) {
        return next.run(request).await;
    }
    let Some(obs) = crate::obs::current() else {
        return next.run(request).await;
    };

    let route = route.unwrap_or_default();
    let method = request.method().as_str().to_owned();
    let trace_id = trace_id();
    let span = tracing::info_span!(
        REQUEST_SPAN,
        "rahi.trace.id" = %trace_id,
        route = %route,
        method = %method,
        status = tracing::field::Empty,
        duration_ms = tracing::field::Empty,
    );

    let started = Instant::now();
    let response = next.run(request).instrument(span.clone()).await;
    let elapsed = started.elapsed();
    let status = response.status().as_u16();

    span.record("status", status);
    span.record(
        "duration_ms",
        u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
    );
    obs.metrics()
        .record_request(&route, &method, status, elapsed);
    drop(span);

    response
}

/// A fresh trace id: sixteen bytes of system entropy, in hex.
///
/// The exported span carries the same value as an attribute, so a trace in
/// the ring and a trace at the collector are the same trace (spec 023 D-4).
#[must_use]
pub fn trace_id() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::fill(&mut bytes).is_err() {
        // Entropy is not available: a monotonic value still separates traces
        // within this process, which is what the ring is indexed by.
        let nanos = Instant::now().elapsed().as_nanos();
        bytes[..16].copy_from_slice(&nanos.to_be_bytes());
    }
    bytes
        .iter()
        .fold(String::with_capacity(32), |mut out, byte| {
            use std::fmt::Write as _;
            let _ = write!(out, "{byte:02x}");
            out
        })
}

/// The probe paths, which an app may want to keep out of its dashboards.
#[must_use]
pub const fn probe_paths() -> [&'static str; 2] {
    [HEALTHZ_PATH, READYZ_PATH]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_metrics_path_and_the_static_slot_are_not_observed() {
        assert!(!is_instrumented(METRICS_PATH, Some("/metrics")));
        assert!(!is_instrumented("/assets/app.4f3a2b1c.js", None));
        assert!(!is_instrumented("/anything-unrouted", None));
        assert!(is_instrumented("/api/notes/7", Some("/api/notes/{id}")));
    }

    #[test]
    fn a_trace_id_is_thirty_two_hex_characters_and_fresh() {
        let (a, b) = (trace_id(), trace_id());
        assert_eq!(a.len(), 32, "{a}");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()), "{a}");
        assert_ne!(a, b);
    }
}
