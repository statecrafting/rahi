//! The in-process tracer, and the exporter that is the operator's choice
//! (spec 023 B-2).
//!
//! The subscriber is always installed and the ring is always filled. What an
//! operator decides is whether spans also leave the process: with
//! `RAHI_OTLP_ENDPOINT` set, an OTLP exporter is built and batched spans go
//! to it over gRPC; unset, no exporter exists and the cell opens no outbound
//! connection at all (FR-003). A cell with no collector is still observable,
//! which is the whole point of constitution XIII having no flag.

use std::sync::Arc;

use opentelemetry::KeyValue;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::{SpanExporter, WithExportConfig as _};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::trace::SdkTracerProvider;
use rahi_types::{Error, Result};
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

use crate::obs::layer::RingLayer;
use crate::obs::metrics::Metrics;
use crate::obs::ring::Ring;

/// The tracer's name in an exported span's instrumentation scope.
pub const TRACER_NAME: &str = "rahi";
/// The `service.name` attribute exported spans carry when the app names none.
pub const DEFAULT_SERVICE_NAME: &str = "rahi-cell";

/// Install the subscriber for this process.
///
/// Returns the provider when an exporter was built, so the caller can flush
/// and shut it down; `None` means no exporter and no outbound connection.
///
/// # Errors
///
/// [`Error::Config`] when an OTLP endpoint is configured and the exporter
/// cannot be built for it. A subscriber another component installed first is
/// not an error: the process has one, and this returns without replacing it.
pub fn install(
    ring: Arc<Ring>,
    metrics: Metrics,
    otlp_endpoint: Option<&str>,
    service_name: &str,
) -> Result<Option<SdkTracerProvider>> {
    let provider = match otlp_endpoint {
        None => None,
        Some(endpoint) => Some(exporter(endpoint, service_name)?),
    };

    let otel = provider
        .as_ref()
        .map(|provider| tracing_opentelemetry::layer().with_tracer(provider.tracer(TRACER_NAME)));

    // A subscriber that is already installed stays installed: two tracers in
    // one process would be two answers about the same request.
    let _ = tracing_subscriber::registry()
        .with(RingLayer::new(ring, metrics))
        .with(otel)
        .try_init();

    Ok(provider)
}

/// Build the OTLP exporter and the provider that batches into it.
fn exporter(endpoint: &str, service_name: &str) -> Result<SdkTracerProvider> {
    let exporter = SpanExporter::builder()
        .with_tonic()
        .with_endpoint(endpoint)
        .build()
        .map_err(|err| {
            Error::Config(format!(
                "the OTLP exporter cannot be built for {endpoint}: {err}"
            ))
        })?;
    Ok(SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(
            Resource::builder()
                .with_attributes([KeyValue::new("service.name", service_name.to_owned())])
                .build(),
        )
        .build())
}
