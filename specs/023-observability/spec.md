---
id: "023-observability"
title: "Observability: /metrics always on, an in-process OTel tracer with a ring buffer, decisions on spans"
status: approved
kind: "feature"
domain: "edge"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: pending
risk: medium
wave: 2
depends_on:
  - "020-edge-server"
establishes:
  - "crates/rahi-edge/src/obs/mod.rs"
  - "crates/rahi-edge/src/obs/metrics.rs"
  - "crates/rahi-edge/src/obs/tracer.rs"
  - "crates/rahi-edge/src/obs/ring.rs"
  - "crates/rahi-edge/src/obs/layer.rs"
  - "crates/rahi-edge/tests/obs.rs"
extends:
  - { spec: "020-edge-server", unit: "crates/rahi-edge/src/lib.rs", nature: additive }
  - { spec: "020-edge-server", unit: "crates/rahi-edge/src/middleware/mod.rs", nature: additive }
  - { spec: "010-workspace-and-core-types", unit: { kind: section, file: "Cargo.toml", anchor: "workspace.dependencies" }, nature: additive }
summary: >
  The substrate contract every cell exposes: Prometheus text on /metrics
  from an in-process registry (process metrics, HTTP counters and duration
  histograms labeled by route pattern and status class, store operation
  counters, kernel decisions and ledger failures), and an OTel tracer with
  a span per request and per store operation that exports OTLP only when an
  endpoint is configured and always retains a bounded ring buffer of recent
  traces queryable in-process. Kernel decisions attach their id to the active
  span through the observe hook. Carries enrahitu://022.
---

# 023: Observability

## 1. Purpose

Constitution XIII. The contract is non-negotiable and has no flag; the
export is the operator's choice; and the ring buffer exists so that a cell
with no collector can still answer "what happened in the last minute" to an
operator or a harness.

## 2. Territory

The `obs` module tree inside `crates/rahi-edge` and its test. Extends 020's
`lib.rs` and middleware module (the observation layer becomes real).

## 3. Behavior

- **B-1 (metrics).** `GET /metrics` serves Prometheus text from a
  process-wide registry: `process_*`, `http_requests_total{route, method,
  status_class}`, `http_request_duration_seconds{route, method}`,
  `store_ops_total{op}`, `store_op_duration_seconds{op}`,
  `kernel_decisions_total{outcome}`, `kernel_ledger_failures_total`. Labels
  are static route patterns, never raw paths or ids. The endpoint is
  unauthenticated at the app layer and kept off the public ingress by
  deployment (032).
- **B-2 (tracer).** A `tracing` subscriber with an OpenTelemetry layer;
  a span per request (route, method, status, duration, principal sub hash)
  and per governed store operation. When `Config.otlp_endpoint` is set,
  spans export OTLP over gRPC; unset means no exporter and no outbound
  connection.
- **B-3 (ring buffer).** Independent of export, completed traces are kept
  in a bounded ring (default 1,000 traces, env-tunable) with
  `obs::list_traces()`, `obs::get_trace(id)`, and `obs::subscribe()` as
  in-process functions. This is a module, not an API; an app may expose it
  behind the operator role (024).
- **B-4 (decision correlation).** The tracer subscribes to
  `rahi_kernel::observe::on_decision` and attaches
  `rahi.decision.id`, `outcome`, `capability`, and `reason` to the active
  span. The kernel imports nothing from this crate.
- **B-5 (what is not measured).** The static slot and `/metrics` itself
  are not instrumented (a scrape must not fill the buffer with
  self-observation).

## 4. Functional requirements

- **FR-001.** After ten requests to a route, `/metrics` shows the counter
  at ten with the route pattern label, and the histogram has ten samples.
- **FR-002.** A denied request produces a trace in the ring whose root span
  carries `rahi.decision.id` equal to the id in the 403 body.
- **FR-003.** With no OTLP endpoint configured, a test asserts no outbound
  connection is attempted (a listener that would have received it sees
  nothing).
- **FR-004.** The ring evicts the oldest trace at capacity plus one.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p rahi-edge --locked --test obs` passes.

## 6. Out of scope

Dashboards; log shipping; alerting rules (032 ships a ServiceMonitor
example only).

## 7. Resolved decisions

None yet.

## Verification

```verify:cli
cargo test -p rahi-edge --locked --test obs
```
