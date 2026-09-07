//! Observability: the contract every cell exposes (spec 023).
//!
//! Constitution XIII gives this no flag. A cell serves `/metrics` from a
//! process-wide registry, runs a tracer in-process whether or not anyone
//! collects from it, and keeps a bounded ring of recent traces so that an
//! operator or the harness can ask what just happened without a collector
//! standing by.
//!
//! Three things are wired together here and nowhere else:
//!
//! - [`metrics`] holds the registry and the families spec 023 B-1 fixes.
//! - [`ring`] holds the last traces, bounded (B-3).
//! - [`layer`] is the `tracing` layer that fills the ring and the axum
//!   middleware spec 020 B-2 reserved the outermost position for, and
//!   [`tracer`] installs the subscriber and, when an endpoint is configured,
//!   the OTLP exporter (B-2).
//!
//! The kernel imports nothing from here (B-4). The seam runs the other way:
//! [`init`] registers observers on `rahi_kernel::observe`, and a decision
//! reaches the trace as a `tracing` event this crate's layer picks up.

pub mod layer;
pub mod metrics;
pub mod ring;
pub mod tracer;

use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use axum::Router;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse as _, Response};
use axum::routing::get;
use opentelemetry_sdk::trace::SdkTracerProvider;
use rahi_types::{Config, EnvReader, Error, Result};
use tokio::sync::broadcast;

pub use layer::{
    DECISION_CAPABILITY, DECISION_ID, DECISION_OUTCOME, DECISION_REASON, DECISION_TARGET,
    METRICS_PATH, REQUEST_SPAN, RingLayer, TRACE_ID, observe,
};
pub use metrics::Metrics;
pub use ring::{DEFAULT_CAPACITY, ENV_RING_CAPACITY, Ring, SpanRecord, Trace};

/// The one observability context of this process.
static OBS: OnceLock<Obs> = OnceLock::new();

/// Held while one caller builds it. Two callers that both built would install
/// one ring in the subscriber and publish the other, and every trace would
/// land where nobody could read it.
static BUILDING: Mutex<()> = Mutex::new(());

/// What `init` needs to know.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObsOptions {
    /// How many traces the ring holds.
    pub ring_capacity: usize,
    /// Where to export spans, if anywhere.
    pub otlp_endpoint: Option<String>,
    /// The `service.name` exported spans carry.
    pub service_name: String,
}

impl ObsOptions {
    /// The options a cell's configuration implies.
    ///
    /// The exporter follows `RAHI_OTLP_ENDPOINT` through
    /// [`Config::otlp_endpoint`] (spec 010 D-4); the ring takes its default.
    #[must_use]
    pub fn from_config(config: &Config) -> Self {
        Self {
            ring_capacity: DEFAULT_CAPACITY,
            otlp_endpoint: config.otlp_endpoint.clone(),
            service_name: tracer::DEFAULT_SERVICE_NAME.to_owned(),
        }
    }

    /// [`ObsOptions::from_config`], with the ring's size read from the
    /// environment (B-3).
    ///
    /// The reader is injected for the same reason spec 010 B-7 injects one:
    /// a library that reaches for the process environment is a library a test
    /// cannot pin down.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when [`ENV_RING_CAPACITY`] is set to something that
    /// is not a number.
    pub fn from_env(config: &Config, reader: &dyn EnvReader) -> Result<Self> {
        let mut options = Self::from_config(config);
        if let Some(raw) = reader.get(ENV_RING_CAPACITY) {
            let raw = raw.trim();
            if !raw.is_empty() {
                options.ring_capacity = raw.parse().map_err(|_| {
                    Error::Config(format!(
                        "{ENV_RING_CAPACITY} must be a whole number of traces, got {raw:?}"
                    ))
                })?;
            }
        }
        Ok(options)
    }

    /// Name the service exported spans belong to.
    #[must_use]
    pub fn with_service_name(mut self, name: &str) -> Self {
        self.service_name = name.to_owned();
        self
    }

    /// Set how many traces the ring holds.
    #[must_use]
    pub const fn with_ring_capacity(mut self, traces: usize) -> Self {
        self.ring_capacity = traces;
        self
    }
}

/// The registry, the ring, and the exporter, for the life of the process.
#[derive(Debug)]
pub struct Obs {
    metrics: Metrics,
    ring: Arc<Ring>,
    provider: Option<SdkTracerProvider>,
}

impl Obs {
    /// The metrics registry.
    #[must_use]
    pub const fn metrics(&self) -> &Metrics {
        &self.metrics
    }

    /// The trace ring.
    #[must_use]
    pub const fn ring(&self) -> &Arc<Ring> {
        &self.ring
    }

    /// Whether spans leave this process.
    #[must_use]
    pub const fn exports(&self) -> bool {
        self.provider.is_some()
    }
}

/// Build the registry and the ring, install the tracer, and hook the kernel.
///
/// Idempotent: the first call in a process wins and every later one returns
/// what it built. A second registry would be a second answer to a scrape, and
/// a second subscriber would be a second answer about a request.
///
/// # Errors
///
/// [`Error::Config`] when the registry refuses a collector, or when an OTLP
/// endpoint is configured and its exporter cannot be built.
pub fn init(options: ObsOptions) -> Result<&'static Obs> {
    if let Some(obs) = OBS.get() {
        return Ok(obs);
    }
    let _building = BUILDING.lock().unwrap_or_else(PoisonError::into_inner);
    // Another caller may have finished while this one waited for the lock.
    if let Some(obs) = OBS.get() {
        return Ok(obs);
    }

    let metrics = Metrics::new()?;
    let ring = Arc::new(Ring::with_capacity(options.ring_capacity));
    let provider = tracer::install(
        Arc::clone(&ring),
        metrics.clone(),
        options.otlp_endpoint.as_deref(),
        &options.service_name,
    )?;
    hook_kernel(metrics.clone());

    Ok(OBS.get_or_init(|| Obs {
        metrics,
        ring,
        provider,
    }))
}

/// The observability context, if [`init`] has run.
#[must_use]
pub fn current() -> Option<&'static Obs> {
    OBS.get()
}

/// Every trace held, oldest first (B-3).
#[must_use]
pub fn list_traces() -> Vec<Trace> {
    current().map(|obs| obs.ring.list()).unwrap_or_default()
}

/// One trace by id, if it has not been evicted (B-3).
#[must_use]
pub fn get_trace(id: &str) -> Option<Trace> {
    current().and_then(|obs| obs.ring.get(id))
}

/// Watch traces as they complete (B-3).
#[must_use]
pub fn subscribe() -> Option<broadcast::Receiver<Trace>> {
    current().map(|obs| obs.ring.subscribe())
}

/// The `/metrics` route, mounted outside the CSRF and rate-limit layers
/// (spec 020 B-2).
///
/// Unauthenticated at the app layer and kept off the public ingress by
/// deployment (spec 032), which is a boundary an operator can see rather than
/// a flag they can forget.
pub fn metrics_router() -> Router {
    Router::new().route(METRICS_PATH, get(render))
}

/// Serve the exposition.
async fn render() -> Response {
    let Some(obs) = current() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            "the observability context is not initialised\n",
        )
            .into_response();
    };
    match obs.metrics.render() {
        Ok(text) => (
            StatusCode::OK,
            [(
                header::CONTENT_TYPE,
                "text/plain; version=0.0.4; charset=utf-8",
            )],
            text,
        )
            .into_response(),
        Err(err) => crate::error::response(&err),
    }
}

/// Count and trace what the kernel decides (B-4).
///
/// The observers are registered once, at init, and never removed: spec 015
/// is explicit that a tracer which can be unsubscribed at runtime is a tracer
/// that can be turned off by whatever is being traced.
fn hook_kernel(metrics: Metrics) {
    let decisions = metrics.clone();
    rahi_kernel::observe::on_decision(move |decision| {
        decisions.record_decision(decision.outcome.as_str());
        // Plain field names here, canonical ones on the span: the layer
        // renames them as it merges (spec 023 D-5).
        tracing::info!(
            target: DECISION_TARGET,
            id = %decision.id,
            outcome = decision.outcome.as_str(),
            capability = decision
                .capability
                .as_ref()
                .map_or("", |capability| capability.as_str()),
            reason = %decision.reason,
        );
    });
    rahi_kernel::observe::on_failure(move |id, err| {
        metrics.record_ledger_failure();
        tracing::error!(
            target: DECISION_TARGET,
            id = %id,
            ledger_failure = %err,
        );
    });
}
