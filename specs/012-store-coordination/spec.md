---
id: "012-store-coordination"
title: "Store coordination: fenced leases, local notify, the outbox, and the revision watermark"
status: approved
kind: "kernel"
domain: "store"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: in-progress
risk: critical
wave: 1
depends_on:
  - "011-store-hiqlite"
establishes:
  - "crates/rahi-store/src/lock.rs"
  - "crates/rahi-store/src/notify.rs"
  - "crates/rahi-store/src/outbox.rs"
  - "crates/rahi-store/src/watermark.rs"
  - "crates/rahi-store/tests/lock.rs"
  - "crates/rahi-store/tests/notify.rs"
  - "crates/rahi-store/tests/outbox.rs"
extends:
  - { spec: "011-store-hiqlite", unit: "crates/rahi-store/src/lib.rs", nature: additive }
summary: >
  The coordination plane over the cache Raft group: distributed leases with
  the Raft log id as a mandatory fencing token and an explicit release,
  key-only local notify whose envelope is the routing key, the outbox row
  that commits in the same txn as the resource it describes, and the
  monotonic revision watermark every consumer polls so that notify is only
  ever a latency hint. Carries enrahitu://032 §3.2, §3.4, §3.5, §3.10.
---

# 012: Store coordination

## 1. Purpose

hiqlite's lease TTL is a hardcoded ten seconds, its notify channel is one
global stream that replays after restart, and its lock id is the Raft log
id. Those three facts, found by reading the implementation, are why fencing
is mandatory rather than hardening, why the envelope is key-only, and why
every consumer must poll a watermark (enrahitu://032). This spec turns them
into a surface a controller cannot misuse by accident.

## 2. Territory

Four modules inside `crates/rahi-store` and their tests. It extends spec
011's `lib.rs` to re-export `Lease`, `Notify`, `Outbox`, and `Watermark`.

## 3. Behavior

- **B-1 (lease).** `lease(key: &str) -> Result<Lease>` acquires hiqlite's
  distributed lock. `Lease { token: FenceToken, release: fn }` exposes the
  Raft log id as the fencing token and an explicit `release().await`; the
  handle's `Drop` is a backstop, not the mechanism. The TTL is documented
  as ten seconds and not configurable.
- **B-2 (fencing predicate).** Every lease-guarded write goes through
  `Store::fenced_txn(lease: &Lease, statements)`, which appends `AND fence
  <= :token` to each statement's `WHERE` via a `Statement::fenced(token)`
  builder and sets `fence = :token`. A zombie holder's writes affect zero
  rows and `fenced_txn` returns `Error::Conflict`.
- **B-3 (notify).** `notify(env: Envelope)` publishes and `listen() ->
  impl Stream<Item = Envelope>` subscribes, local only. `Envelope { kind:
  String, tenant: Option<String>, name: String, revision: Revision }` is
  key-only; there is no payload field. Consumers filter by `kind`.
- **B-4 (outbox).** `Outbox::stage(txn: &mut TxnBuilder, env: &Envelope)`
  appends an `INSERT INTO outbox` statement to the caller's batch so the
  row commits with the resource. `Outbox::drain(store, batch_size)` reads
  staged rows with `query_consistent`, calls `notify` for each, and deletes
  them in one `txn`. A row is never notified before it is durable.
- **B-5 (watermark).** `Watermark::next(txn: &mut TxnBuilder, table)`
  computes `max(revision) + 1` inside the transaction and stamps the row.
  `Watermark::since(store, table, after: Revision) -> Vec<Revision>`
  returns unseen revisions with `query`. A consumer persists its last seen
  revision and calls `since` on every tick whether or not a notify arrived.
- **B-6 (cache is derived).** KV and counter helpers (`kv_put`, `kv_get`,
  `kv_del` with TTL; `counter_add`, `counter_get`) are provided for rate
  limits and caches and are documented as non-durable; a test asserts a
  restart clears them.

## 4. Functional requirements

- **FR-001.** Two `Lease`s on one key from two tasks: the second acquires
  only after the first releases or after eleven seconds; tokens are
  strictly increasing.
- **FR-002.** A `fenced_txn` with a stale token affects zero rows and
  returns `Error::Conflict`; a fresh token succeeds.
- **FR-003.** A resource write staged with an outbox row and a failing
  final statement leaves neither the resource nor the outbox row.
- **FR-004.** With notify delivery disabled in the test, a consumer using
  `Watermark::since` still observes every revision.
- **FR-005.** `listen` receives an envelope published on the same node and
  the envelope has no payload field (a compile-time property, asserted by
  a doc test).

## 5. Acceptance criteria

- **AC-1.** `cargo test -p rahi-store --locked` passes, including the
  lock, notify, and outbox test files.

## 6. Out of scope

Controllers themselves (an application concern); the decision chain's CAS,
which uses `txn` plus a unique index and no primitive from here (013).

## 7. Resolved decisions

None yet.

## Verification

```verify:cli
cargo test -p rahi-store --locked --test lock
cargo test -p rahi-store --locked --test outbox
```
