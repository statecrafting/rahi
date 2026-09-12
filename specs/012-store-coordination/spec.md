---
id: "012-store-coordination"
title: "Store coordination: fenced leases, local notify, the outbox, and the revision watermark"
status: approved
kind: "kernel"
domain: "store"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: complete
risk: critical
wave: 1
depends_on:
  - "011-store-hiqlite"
establishes:
  - "crates/rahi-store/src/lock.rs"
  - "crates/rahi-store/src/notify.rs"
  - "crates/rahi-store/src/outbox.rs"
  - "crates/rahi-store/src/watermark.rs"
  - "crates/rahi-store/src/cache.rs"
  - "crates/rahi-store/tests/lock.rs"
  - "crates/rahi-store/tests/notify.rs"
  - "crates/rahi-store/tests/outbox.rs"
  - "crates/rahi-store/tests/watermark.rs"
  - "crates/rahi-store/tests/cache.rs"
extends:
  - { spec: "011-store-hiqlite", unit: "crates/rahi-store/src/lib.rs", nature: additive }
  - { spec: "011-store-hiqlite", unit: "crates/rahi-store/Cargo.toml", nature: additive }
  - { spec: "010-workspace-and-core-types", unit: { kind: section, file: "Cargo.toml", anchor: "workspace.dependencies" }, nature: additive }
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
  distributed lock and, while holding it, mints the fencing token from the
  SQL group: it bumps the key's row in `lease_fence` and reads the new
  value back with `query_consistent` (D-1). `Lease { token: FenceToken,
  release: fn }` exposes that token and an explicit `release().await`; the
  handle's `Drop` is a backstop, not the mechanism. Tokens for one key are
  strictly increasing and survive a restore; a token is comparable only
  with tokens of the same key, so one lease key guards one resource set.
  The TTL is documented as ten seconds and not configurable, and a lease
  cannot be renewed: a holder that needs longer acquires again, and a
  claim that must outlive the TTL is an application row written under a
  lease (D-10). *(Amended 2026-09-12, D-10; the text before named the
  Raft log id as the token.)*
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
  Delivery is at least once: a crash between the notify and the delete
  publishes the row again on the next drain. The chassis never runs the
  drain; an application that stages envelopes runs the loop. The `outbox`
  and `lease_fence` tables are DDL the application places in its own
  migration list (D-5). *(Amended 2026-09-12, D-10.)*
- **B-5 (watermark).** `Watermark::next(txn: &mut TxnBuilder, table)`
  computes `max(revision) + 1` inside the transaction and stamps the row.
  `Watermark::since(store, table, after: Revision) -> Vec<Revision>`
  returns unseen revisions with `query`. A consumer persists its last seen
  revision and calls `since` on every tick whether or not a notify arrived.
- **B-6 (cache is derived).** KV and counter helpers (`kv_put`, `kv_get`,
  `kv_del` with TTL; `counter_add`, `counter_get`) are provided for rate
  limits and caches and are documented as non-durable. Non-durable is a
  rule for application code, not a property of hiqlite: hiqlite 0.14
  persists the cache group's Raft log and replays it at startup, so a
  value and a counter outlive a restart (D-6). A test asserts that
  persistence, so an upgrade that changes it is caught, and asserts that a
  value expires on its TTL and that a delete is a delete. Those two
  properties, and the rule that no transaction that decides anything
  writes to the group, are what make it unfit for durable state. *(Amended
  2026-09-12, D-10; the text before asked for a test that a restart clears
  the group.)*

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

- **D-1 (2026-09-06, build session; contradicts B-1, pending a human
  amendment).** The fencing token is minted from the SQL group, not read
  from hiqlite's lock. B-1 says the token is the Raft log id, but
  `hiqlite::Lock` keeps its `id` field private in 0.14 and exposes no
  accessor, no `Debug`, and no `Serialize`, so the log id is unreachable
  from a dependent crate. `lease(key)` therefore takes hiqlite's lock for
  mutual exclusion and, while holding it, bumps a row in a `lease_fence`
  table (`INSERT ... ON CONFLICT DO UPDATE SET token = token + 1`) and
  reads the new value back with `query_consistent`. The lock serialises the
  bump for its key, so tokens for one key are strictly increasing
  (FR-001), and the table is durable, which the Raft log id would not have
  been across a restore. The consequence is that a token is only
  comparable with other tokens of the same lease key: one lease key guards
  one resource set. Alternative rejected: the cache group's Raft
  `last_log_index`, which is reachable but resets when the group is
  rebuilt, and would then hand out tokens below the ones already recorded
  in `fence` columns, wedging every later write. Amended into B-1 on
  2026-09-12 (D-10).
- **D-2 (2026-09-06, build session).** `Lease::release` is `async` and
  consumes the handle, but it cannot acknowledge: hiqlite releases on
  `Lock::drop` by spawning a task and offers no confirmed release. A
  caller that must observe the handover observes it by acquiring again.
  A lease that `fenced_txn` has found superseded does not drop its lock at
  all: hiqlite's lock handler panics when told to release a lock another
  holder now owns, and that panic kills the node's whole locking
  subsystem, so the superseded handle leaks the lock (one key string and
  one client handle) instead of taking the node down on exactly the path
  fencing exists for.
- **D-3 (2026-09-06, build session).** `fenced_txn` prefixes the caller's
  batch with a guard statement,
  `UPDATE lease_fence SET token = CASE WHEN token <= :t THEN token ELSE
  NULL END WHERE lease_key = :k`. The `fence <= :token` predicate B-2
  requires is applied to every statement as well, but a predicate alone is
  not enough: a superseded holder's `UPDATE` matching no row is not an
  error, so the batch would commit and the outbox row beside it would land.
  SQLite has no `RAISE` outside a trigger, so a `NOT NULL` violation is the
  abort; it rolls the whole batch back, and the guard's result is stripped
  before the caller's results are returned. hiqlite reports a failed
  statement inside a transaction as `Error::Transaction` rather than as a
  constraint violation, so the guard is recognised by the column it names
  and mapped to `Error::Conflict` (FR-002).
- **D-4 (2026-09-06, build session).** `Statement::fenced` rewrites the SQL
  rather than adding a parameter: the token is written as an integer
  literal so the caller's positional parameters keep their numbers, and the
  `WHERE` it appends to is found by scanning for the first `WHERE` keyword
  at parenthesis depth zero and outside every literal, quoted identifier,
  and comment, so a subquery's `WHERE` is never mistaken for the
  statement's. Only `UPDATE` and `DELETE` can be fenced; anything else is
  `Error::Validation`, because an `INSERT` has no row whose token can be
  compared. A caller inserting under a lease stamps `fence` with
  `lease.token` itself.
- **D-5 (2026-09-06, build session).** The `lease_fence` and `outbox`
  tables are DDL like any other, so they are not created at first use:
  `coordination_migration(version)` returns them as one `Migration` that
  the app places in its own list for the `migrate` verb to apply (spec 011
  B-4, constitution IX). An app that neither leases nor stages an envelope
  does not carry it.
- **D-6 (2026-09-06, build session; contradicts B-6, pending a human
  amendment).** A restart does not clear the cache group. B-6 asks for a
  test asserting that it does; hiqlite 0.14 persists the cache Raft log
  under `data_dir` and replays it at startup, so a KV value and a counter
  both outlive the process (measured, not read: `tests/cache.rs`). The
  test asserts what hiqlite actually does, so an upgrade that changes it is
  caught, and it asserts the properties that do make the group unfit for
  durable state: a value expires on its TTL, a delete is a delete, and
  nothing in the group is written by the transaction that decided
  anything. The invariant "nothing durable lives in the cache group" stands
  as a rule for application code; it was never a guarantee from hiqlite.
  Amended into B-6 on 2026-09-12 (D-10).
- **D-7 (2026-09-06, build session).** `listen()` returns the named type
  `Listen`, which implements `futures_core::Stream<Item = Envelope>` and
  also offers `recv().await`, rather than an anonymous `impl Stream`: spec
  015 wraps this surface in `Governed<Notify>` and needs a nameable type.
  It yields only events published after this process started
  (`listen_after_start`), because the cache group replays older ones. The
  stream ends rather than looping when the group closes or an event does
  not decode, which is survivable exactly because a consumer polls
  `Watermark::since` anyway. `futures-core` is the one new workspace
  dependency (the trait, no runtime).
- **D-8 (2026-09-06, build session).** hiqlite gains the `counters`
  feature, additively on spec 010's `workspace.dependencies` and spec 011
  B-6's set. B-6 of this spec requires `counter_add` and `counter_get`, and
  hiqlite gates them behind that feature; the alternative, a read-modify-
  write over the KV cache, is not atomic and so is not usable for the rate
  limit the counters exist for.
- **D-9 (2026-09-06, build session).** `Watermark::next` stamps the rows
  the batch left at `Revision::ZERO`, the value spec 010 defines as "never
  written": the caller writes its row with `revision = 0` and `next`
  appends one `UPDATE ... SET revision = (SELECT COALESCE(MAX(revision), 0)
  + 1 FROM t) WHERE revision = 0`. One transaction is therefore one
  revision, and two rows written together share it. `max(revision) + 1`
  hands the same revision out twice if the newest row is deleted, so a
  table whose consumers must not miss a change keeps a tombstone instead of
  deleting. `TxnBuilder` lives in `outbox.rs` because the outbox is the
  reason a batch is shared, and the KV and counter helpers of B-6 live in a
  fifth module, `cache.rs`, rather than inside one of the four the
  Territory names; `tests/watermark.rs` and `tests/cache.rs` cover FR-004
  and B-6.
- **D-10 (2026-09-12, corpus amendment; owner decision RH-06).** D-1 and
  D-6 recorded that B-1 and B-6 contradicted the implementation and left
  both pending a human amendment. The owner decided that the lease and
  outbox text be amended to match real behavior, so B-1 now names the token
  `lease_fence` mints (D-1), B-6 now states that hiqlite replays the cache
  group and what the test asserts instead (D-6), and B-4 now states what the
  outbox
  already does: delivery at least once, a drain the chassis never runs,
  and tables the application migrates (D-5). No code changed except a doc
  comment in `tests/cache.rs` that quoted B-6's old text, and no
  requirement moved away from what `cargo test -p rahi-store` already
  proves. The same decision refused a renewable chassis lease API, which
  aicortex 035 asked for (a requested duration and a renewal call): B-1's
  ten-second, non-renewable lease stands, and a work claim that must
  outlive it is an application row (`key`, `holder`, `fence`,
  `expires_at`) created and renewed through `fenced_txn` under a
  short-held lease, as `docs/design/02-operational-prerequisites.md`
  section 6.5 recommends. Rejected alternative: a renew call on `Lease`,
  which hiqlite 0.14's lock does not offer, and which would make the TTL
  a chassis promise the lock cannot keep.

## Verification

```verify:cli
cargo test -p rahi-store --locked --test lock
cargo test -p rahi-store --locked --test outbox
```
