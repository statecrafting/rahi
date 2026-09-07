---
id: "023-observability"
title: "Observability: /metrics always on, an in-process OTel tracer with a ring buffer, decisions on spans"
status: approved
kind: "feature"
domain: "edge"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: complete
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
  - { spec: "020-edge-server", unit: "crates/rahi-edge/src/router.rs", nature: additive }
  - { spec: "020-edge-server", unit: "crates/rahi-edge/Cargo.toml", nature: additive }
  - { spec: "020-edge-server", unit: "crates/rahi-edge/testdata/", nature: additive }
  - { spec: "015-kernel-manifest-and-adjudication", unit: "crates/rahi-kernel/src/facade.rs", nature: additive }
  - { spec: "015-kernel-manifest-and-adjudication", unit: "crates/rahi-kernel/Cargo.toml", nature: additive }
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

- **D-1 (2026-09-07, build session; completes B-3's "env-tunable").** The
  ring reads `RAHI_TRACE_RING_CAPACITY` through an injected
  `rahi_types::EnvReader`, not through `std::env`. Spec 010 D-4 fixed the
  `RAHI_*` set and spec 010 B-7 keeps the process environment out of a
  library, so the variable is named here and read by the caller that already
  holds a reader. `ObsOptions::from_env(&Config, &dyn EnvReader)` is that
  seam; a capacity of zero becomes one, because a ring that holds nothing is
  a ring that is silently off. Rejected alternative: a direct `std::env`
  read, which is a library reaching for process state a test cannot pin.
- **D-2 (2026-09-07, build session; bounds B-1's `process_*`).** The process
  collector is a Linux-only dependency, declared under
  `[target.'cfg(target_os = "linux")'.dependencies]`. It reads `/proc`, which
  is where a cell runs and is not where it is developed, so the exposition
  carries `process_*` in the container and is silent about it on a macOS
  workstation. Saying nothing is the honest form: inventing the families from
  another source would put a different number under the same name. Rejected
  alternative: enabling the feature everywhere, which does not build on the
  machine most of this repository is written on.
- **D-3 (2026-09-07, build session; completes B-5).** A request is observed
  when it matched a route and its path is not `/metrics`. B-5 names the
  static slot and the scrape; the mechanical form of "the static slot" is
  "matched no route", which also covers a 404. The reason is B-1's rather
  than B-5's: the only label available for an unrouted request is its raw
  path, and a label an unauthenticated client can vary is an unbounded label
  set, which is how a metrics endpoint becomes the thing that takes a cell
  down. Rejected alternative: a fixed `unmatched` label, which keeps a 404
  flood visible at the cost of a second reason to read the raw path.
- **D-4 (2026-09-07, build session; identifies a trace).** The edge mints the
  trace id: sixteen bytes of system entropy in hex, recorded on the request
  span as `rahi.trace.id`. The ring is indexed by it and an exported span
  carries it as an attribute, so a trace an operator finds in the ring and
  the same trace at a collector are the same trace. Reading the exporter's
  own trace id instead would tie the ring to the internals of
  `tracing-opentelemetry` and would leave a cell with no exporter unable to
  name its own traces. Rejected alternative: no id at all, which makes
  `get_trace(id)` unimplementable.
- **D-5 (2026-09-07, build session; realises B-4's attachment).** A decision
  reaches the trace as a `tracing` event on the `rahi.decision` target, and
  this crate's layer walks from that event out to the request span it
  happened inside and merges the fields onto it. B-4 says the kernel imports
  nothing from here, and the decision fires inside the governed operation's
  span rather than the request's, so a field written on the current span
  would land on a child. The event carries plain field names (`id`,
  `outcome`) because a `tracing` macro reads a leading string literal as its
  message; the layer renames them to the dotted names B-4 fixes. Rejected
  alternative: declaring `rahi.decision.*` fields on the kernel's own span,
  which makes spec 015 carry spec 023's vocabulary.
- **D-6 (2026-09-07, build session; places B-2's store span).** The span for
  a governed store operation is opened in `rahi_kernel::facade`, at the one
  choke point every governed call passes (`Governed::call`, and the fenced
  transaction that admits on its own), and this spec declares `extends` edges
  on that file and its manifest. The alternative is instrumenting the store
  from the edge, which cannot see which operations were governed. `tracing`
  is a facade over whatever subscriber the binary installed, so the kernel
  still imports nothing from this crate; what the two share is a span name,
  exported as `rahi_kernel::facade::STORE_SPAN` so neither side spells it
  twice. This crate's layer turns each closed span into `store_ops_total` and
  `store_op_duration_seconds`.
- **D-7 (2026-09-07, build session; bounds `init`).** `obs::init` is
  idempotent and serialised: the first caller in a process builds, and every
  later one gets what it built. A second registry would be a second answer to
  a scrape and a second subscriber a second answer about a request. The lock
  is not decoration: without it two callers could each build a ring, install
  one in the subscriber and publish the other, and every trace would land
  where nobody could read it. That is a race this session hit and fixed, not
  a hypothetical. Rejected alternative: caching the failure in a
  `OnceLock<Result<..>>`, which makes one bad endpoint permanent for the
  life of the process.
- **D-8 (2026-09-07, build session; mounts B-1's route).** `/metrics` is
  merged into the router's unguarded branch beside the probes, which is what
  spec 020 B-2 requires of it, so this spec declares an `extends` edge on
  spec 020's `router.rs`. Leaving the app to mount it would put the scrape
  behind the CSRF check and the rate limiter, where a Prometheus server would
  be refused for having no token and then throttled for asking twice.

## Verification

```verify:cli
cargo test -p rahi-edge --locked --test obs
```
