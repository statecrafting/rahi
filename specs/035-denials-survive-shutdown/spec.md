---
id: "035-denials-survive-shutdown"
title: "Denials survive a graceful shutdown, and every lost denial is counted by its cause"
status: draft
kind: kernel
domain: kernel
created: "2026-09-11"
authors: ["Bartek Kus"]
implementation: pending
risk: high
wave: 3
depends_on:
  - "015-kernel-manifest-and-adjudication"
  - "023-observability"
  - "030-operational-verbs"
establishes:
  - "crates/rahi-cli/tests/shutdown.rs"
extends:
  - { spec: "015-kernel-manifest-and-adjudication", unit: "crates/rahi-kernel/src/lib.rs", nature: additive }
  - { spec: "015-kernel-manifest-and-adjudication", unit: "crates/rahi-kernel/src/observe.rs", nature: additive }
  - { spec: "023-observability", unit: "crates/rahi-edge/src/obs/metrics.rs", nature: additive }
  - { spec: "023-observability", unit: "crates/rahi-edge/src/obs/mod.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/src/serve.rs", nature: additive }
  - { spec: "032-cluster-topology", unit: "deploy/README.md", nature: additive }
references:
  - { unit: { kind: file, path: "docs/design/01-consumer-contract.md" }, role: context }
summary: >
  Constitution X says the kernel ledgers every denial. Spec 015 answers a
  denial before its record is durable and drains the records through a
  bounded queue, and serve never drains that queue on shutdown, so a
  graceful stop silently loses the denials still queued (reproduced: 50
  denials answered 403 with decision ids, 33 and 40 of them in the chain
  after SIGTERM, nothing counted). This spec drains the queue within a
  bound when serve stops, counts what the bound abandons, and separates a
  dropped record from a failed append on /metrics, so the only uncounted
  loss left is a process that is killed.
---

# 035: Denials survive a graceful shutdown

## 1. Purpose

Spec 015 B-6 keeps the denial path off the request path: the record goes
to a bounded channel and one task appends it. D-7 accepts that a full queue
drops a record, provided the drop is counted, because "this is the one
failure that must never be silent". Two losses escape that rule today.

- **Shutdown.** `Kernel::flush` exists and nothing in the serve path calls
  it (`crates/rahi-cli/src/serve.rs`, `serve_until`). A graceful stop drops
  the listener, shuts the store, and ends the runtime with records still
  queued. Nothing counts them and nothing logs them. On 2026-09-11 an
  out-of-tree cell at `444bcf8` answered 50 concurrent denials with `403`
  and a decision id each, received SIGTERM, and kept 40 of them; a second
  run kept 33.
- **Indistinguishable causes.** `rahi-edge` registers one failure observer
  that increments `kernel_ledger_failures_total` for a dropped record and
  for a failed append alike. An operator reading `/metrics` cannot tell a
  saturated queue from a failing store.

This spec closes both. It serves constitution X and spec 015 D-7 and
changes neither text: it makes the kernel's existing promise true under a
graceful stop, and it states the remaining limit.

## 2. Territory

No new crate. Additive changes to four units others own, and one new test
file in `rahi-cli`.

- `crates/rahi-kernel/src/lib.rs` (015): `Kernel::flush` reports what it
  abandoned; a `Kernel::drain` for the shutdown path.
- `crates/rahi-kernel/src/observe.rs` (015): the failure observer learns
  the cause.
- `crates/rahi-edge/src/obs/metrics.rs` and `obs/mod.rs` (023): two new
  counter families.
- `crates/rahi-cli/src/serve.rs` (030): the drain in the stop sequence.
- `crates/rahi-cli/tests/shutdown.rs` (new): the end-to-end proof.

## 3. Behavior

- **B-1 (drain on stop).** When `serve` receives its stop signal, it stops
  accepting, lets in-flight requests finish as it does today, and then
  drains the kernel's denial queue before it shuts the store: every record
  queued at that moment is appended or abandoned within
  `RAHI_DENIAL_DRAIN_TIMEOUT_SECS` (default 5). The store is not shut while
  the drain runs.
- **B-2 (abandonment is counted).** A record still queued when the bound
  expires is abandoned. Each abandoned record raises
  `kernel_decisions_abandoned_total` and writes one error line at target
  `rahi.decision` naming its decision id. The bound expiring is itself one
  warning line naming how many were abandoned.
- **B-3 (causes are distinct).** The failure observer receives the cause:
  `dropped` (the queue was full), `failed` (the append returned an error),
  or `abandoned` (B-2). `/metrics` exposes `kernel_decisions_dropped_total`
  and `kernel_decisions_abandoned_total` beside the existing
  `kernel_ledger_failures_total`, which from this spec counts only failed
  appends.
- **B-4 (the promise, stated).** A denial answered with a decision id is
  in the chain unless the queue was full, the append failed, the drain
  bound expired, or the process was killed without a stop signal. The
  first three are counted by name; the fourth cannot be, and the
  deployment documentation says so.
- **B-5 (no request waits).** The request path still never awaits an
  append (015 B-6). This spec adds no synchronous mode.

## 4. Functional requirements

- **FR-001.** `tests/shutdown.rs` boots a fixture cell whose route is
  denied, sends 200 concurrent requests, asserts 200 answers of `403` each
  carrying a decision id, sends SIGTERM, waits for exit `0`, and asserts
  `ledger verify` reports every one of the 200 ids in the chain.
- **FR-002.** A kernel test holds the appender behind a gate longer than a
  one-second bound, stops, and asserts the abandoned count equals the
  records still queued and that each abandoned id reached the observer
  with the cause `abandoned`.
- **FR-003.** A kernel test fills a queue of capacity 1 and asserts the
  overflow reaches the observer as `dropped`, not `failed`; an edge test
  asserts the three counter families render on `/metrics`.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p rahi-kernel --locked`, `cargo test -p rahi-edge
  --locked`, and `cargo test -p rahi-cli --locked --test shutdown` pass.
- **AC-2.** Spec 015 AC-2 still holds: `cargo tree -p rahi-kernel` shows
  `rahi-ledger`, `rahi-store`, and `rahi-types` as its only workspace
  dependencies.
- **AC-3.** `deploy/README.md` states B-4's promise and its one uncounted
  case.

## 6. Out of scope

- A synchronous denial, where the `403` waits for the append. It would
  make the denial path as slow as a Raft write and is the next step only
  if a consumer needs a denial to be durable before it is answered.
- Ledgering allows (015 D-5 stands).
- Durability across SIGKILL, an out-of-memory kill, or power loss.
- The supervisor's own stop order beyond giving `serve` its bound (031).

## 7. Resolved decisions

None yet. Before approval a human decides:

- the default bound (5 seconds proposed; spec 026's stream drain is the
  sibling setting);
- whether a manifest may opt into synchronous denials (`[ledger] denials =
  "sync"`), which this draft leaves out.

## Verification

```verify:cli
cargo test -p rahi-kernel --locked
cargo test -p rahi-edge --locked
cargo test -p rahi-cli --locked --test shutdown
```
