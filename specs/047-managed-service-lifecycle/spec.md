---
id: "047-managed-service-lifecycle"
title: "Manage application background services through the cell lifecycle"
status: draft
kind: kernel
domain: ops
created: "2026-09-27"
authors: ["Bartek Kus"]
implementation: pending
risk: critical
wave: 3
depends_on:
  - "030-operational-verbs"
  - "035-denials-survive-shutdown"
  - "043-patched-dependency-adoption"
establishes:
  - "crates/rahi-cli/src/service.rs"
  - "crates/rahi-cli/tests/services.rs"
extends:
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/src/cell.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/src/lib.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/src/serve.rs", nature: amending }
  - { spec: "043-patched-dependency-adoption", unit: "crates/rahi-ops/src/stop.rs", nature: additive }
  - { spec: "043-patched-dependency-adoption", unit: "crates/rahi-cli/tests/stop_budget.rs", nature: amending }
summary: >
  A Cell can declare routes and migrations, but it cannot give the chassis an
  application worker whose lifetime is tied to serve. Applications therefore
  either omit durable background work or detach tasks that Rahi cannot cancel,
  observe, or join before the application store shuts down. This spec adds a
  named managed-service declaration to Cell, starts each service once after
  successful composition, treats an unexpected return, error, or panic as a
  process failure, broadcasts cancellation for every stop cause, and joins or
  aborts every service before draining its final denials and shutting down the
  store. The service bound overlaps the existing HTTP drains so the published
  stop grace does not grow.
---

# 047: Managed service lifecycle

## 1. Purpose

The chassis owns the process lifetime and the application store, but the
application has no corresponding lifecycle seam for durable background work.
`Cell` declares routes, migrations, and static content. `serve` drains streams,
connections, and kernel denials, then shuts down the store. An application can
spawn a worker from route construction, but that task is detached: Rahi cannot
name its failure, cancel it on every stop path, or prove that it released its
store handle before store shutdown began.

This gap blocks applications whose durable outbox is completed by an
in-process worker. A worker must keep using the application store while it
finishes an admitted item, yet it must not outlive the store. This spec makes
that lifetime part of the binary composer. It serves constitution IX by
keeping durable work in the transaction and treating notification as a hint,
constitution XIII by making failures and overruns observable, and constitution
XIV by letting an app compose a worker without making the chassis depend on
the app.

The contract is general. An embedding worker is one consumer, not the product
boundary. Rahi schedules no domain work and understands no application outbox
schema.

## 2. Territory

This spec establishes `rahi-cli`'s public managed-service types and the
process-level acceptance fixture. It extends `Cell` with an empty-by-default
service declaration, re-exports the public types, and amends `serve` with the
startup, cancellation, failure, and join phases. It extends spec 043's stop
model with a bounded `service_join` phase and structured service failure
reasons.

It does not change `rahi-store`, the transaction API, the kernel, or the
metrics registry. A service receives the same cloned `AppState` that routes
receive. Application collectors continue to register through the existing
public `Metrics::registry()` surface after observability has initialized.

## 3. Behavior

- **B-1 (the declaration).** `Cell` MUST expose
  `services(state: AppState, shutdown: ServiceShutdown) ->
  Result<Vec<ManagedService>>` with a default of an empty vector. A
  `ManagedService` MUST contain a stable, non-empty name and one owned,
  `Send + 'static` future whose output is `rahi_types::Result<()>`.
  `ServiceShutdown` MUST be cloneable and MUST let application code await or
  inspect cancellation, but MUST NOT give application code authority to cancel
  the process.
- **B-2 (one owner).** `serve` MUST be the only owner of each managed future
  and join handle. Service names MUST be unique within one cell. An empty name,
  a duplicate name, or an error returned while declaring services MUST fail
  composition before the listener accepts a request. A managed service MUST
  NOT detach work whose lifetime can exceed its own future; a service that
  needs child tasks owns and joins them itself.
- **B-3 (startup).** Rahi MUST call the declaration once per successful
  `serve` composition, after the store, ledger, kernel, observability, and
  `AppState` exist. It MUST spawn every declared service exactly once per
  process after the listener binds and before the server begins accepting
  requests. Other verbs MUST NOT construct or run services. If binding fails,
  no service future is polled.
- **B-4 (one cancellation broadcast).** The first external stop request,
  terminal store failure, listener or server failure, or managed-service
  failure MUST latch the stop cause and broadcast cancellation exactly once.
  All remaining services receive the same cancellation event. Later causes
  MAY add observations but MUST NOT replace the first process result.
- **B-5 (ordered shutdown).** Cancellation starts concurrently with graceful
  stream and connection draining, and the B-6 bound runs concurrently with
  both. Once request handling has ended, Rahi MUST finish joining every
  managed service, then drain kernel denials, then begin application store
  shutdown. Finishing the join awaits only handles that have completed or that
  B-6 has already aborted; it never defers the B-6 abort. No managed future or
  its owned child work may remain alive when store shutdown begins.
- **B-6 (bounded join).** The service join bound MUST be ten seconds measured
  from the cancellation broadcast, not ten additional seconds after connection
  draining. A service still running at the bound MUST be aborted at the
  bound, whether or not request handling has ended, and its join handle MUST
  be awaited before store shutdown. The stop MUST be unconfirmed
  and non-zero, naming every service that exceeded the bound. Because the bound
  overlaps the existing stream and connection drains and is no larger than
  their composed bound, `SERVE_GRACE` remains forty seconds and
  `CONTAINER_GRACE` remains fifty seconds.
- **B-7 (failure semantics).** A service that returns `Ok(())` before
  cancellation is an unexpected exit. A service that returns `Err` at any
  time is a failure. A panicked service is a failure. Each case MUST trigger
  B-4, preserve the service name and error or panic classification in the stop
  observation, shut down through B-5, and make `serve` return non-zero. Rahi
  MUST NOT restart a failed service in-process.
- **B-8 (normal completion).** After cancellation, a service that returns
  `Ok(())` within the bound has completed normally. A cancelled service that
  returns `Err` or panics still fails the stop. When all services complete
  normally and every existing stop phase confirms, the stop remains confirmed
  and exits zero.
- **B-9 (compatible absence).** A cell that does not override `services`
  MUST behave as it did before this spec: no task is spawned, no synthetic
  service failure is recorded, and the existing stream, connection, denial,
  store, and Rauthy stop order and exit semantics remain intact.
- **B-10 (observability without a second registry).** The stop record MUST
  include a `service_join` phase whenever at least one service was started.
  Structured reasons MUST distinguish unexpected exit, returned error, panic,
  and join timeout by service name. Service-specific operational metrics belong
  to the application and MUST use the registry already exposed by
  `Metrics::registry()`; this spec MUST NOT add another registry or a
  domain-specific collector to Rahi.

## 4. Functional requirements

- **FR-001.** A process fixture with two named services proves that each starts
  exactly once, receives a usable clone of `AppState`, observes the same
  cancellation broadcast on SIGTERM, finishes its store work, and is joined
  before the store shutdown phase begins.
- **FR-002.** The fixture records the shutdown events in order and proves
  `service_join` precedes `denial_drain`, which precedes `store_shutdown`.
  Reopening the store after exit proves that the service's final committed
  write is durable and that no task retained the store owner lock.
- **FR-003.** Separate fixtures make a service return `Ok(())` early, return a
  named `Error`, and panic. Each makes the process non-zero, cancels and joins
  its sibling, releases the store, and records the service name with the
  correct structured reason.
- **FR-004.** A fixture ignores cancellation past the ten-second bound. Rahi
  aborts and joins it, records `service_join_timeout` with its name, performs
  denial drain and store shutdown afterwards, records an unconfirmed stop, and
  exits with the infrastructure code.
- **FR-005.** A declaration with an empty name, duplicate names, or a returned
  configuration error fails before any request is accepted and still shuts the
  already-open store down with its outcome observed.
- **FR-006.** The stop-budget test reads the enforced service bound and proves
  that it is measured from the original cancellation instant, overlaps the
  stream and connection drains, and does not raise the shipped forty-second
  serve grace or fifty-second container grace.
- **FR-007.** An external-style test cell that implements only the pre-047
  `Cell` methods compiles and serves unchanged through the default empty
  declaration.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p rahi-cli --locked --test services` passes and covers
  FR-001 through FR-005 and FR-007 against real process fixtures.
- **AC-2.** `cargo test -p rahi-cli --locked --test stop_budget` passes with the
  managed-service overlap asserted from the constants and phase timestamps the
  implementation uses.
- **AC-3.** The normal SIGTERM fixture records `service_join`,
  `denial_drain`, and `store_shutdown` in that order, exits zero, and the next
  boot reads the previous stop as confirmed.
- **AC-4.** The early-return, error, panic, and timeout fixtures each exit
  non-zero, release the store owner lock, and leave a whole stop record naming
  the service and the exact failure class.
- **AC-5.** The workspace gate and all existing `rahi-cli` tests pass, proving
  that an application with no managed services retains its prior lifecycle.

## 6. Out of scope

Dynamic service registration, per-service restart policy, leader election,
work scheduling, outbox schemas, provider selection, service readiness
handshakes, and domain metrics are application concerns. This spec supplies a
process lifetime, not a job framework.

Forceful process death remains outside graceful guarantees. SIGKILL, process
abort, power loss, or a runtime failure can prevent cancellation and joining;
spec 043's previous-stop classification remains the honest record for those
cases. Rahi cannot govern tasks a service detaches contrary to B-2.

Publishing a release and activating any consumer worker are separate lifecycle
acts after this draft is approved, implemented, verified, and merged.

## 7. Resolved decisions

- **D-1 (2026-09-27, this spec).** The application receives a one-way
  `ServiceShutdown`, not the sender. A worker may observe process authority but
  may not exercise it. Failure is communicated by completing its managed
  future, which lets Rahi classify and order the stop.
- **D-2 (2026-09-27, this spec).** A failed service terminates the process and
  is never restarted in-process. Durable work already has retry and lease
  semantics; an implicit restart loop would hide a broken worker and create a
  second, ungoverned availability policy.
- **D-3 (2026-09-27, this spec).** Service cancellation begins at the original
  stop trigger and its ten-second bound overlaps HTTP draining. Adding a fifth
  serial allowance would raise the published container grace for every cell,
  although a cooperative worker needs the store only while the existing
  drains already run.
- **D-4 (2026-09-27, this spec).** Services join before the kernel denial
  queue drains. A service can exercise a governed operation while completing;
  draining denials first could leave its final denial queued when the store
  shuts down.
- **D-5 (2026-09-27, this spec).** The existing observability singleton is
  initialized before `Cell::services` is called. Application collectors use
  `Metrics::registry()` there. A second registry or a Rahi API specialized for
  one worker is rejected.

## Verification

```verify:cli
cargo test -p rahi-cli --locked --test services
cargo test -p rahi-cli --locked --test stop_budget
cargo test -p rahi-cli --locked
```
