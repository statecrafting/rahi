---
id: "045-store-receipts-and-work-claims"
title: "Receipts and work claims: an inbound item is recorded once per content revision, processed once per processor revision, and never silently dropped"
status: approved
kind: kernel
domain: store
created: "2026-09-24"
authors: ["Bartek Kus"]
implementation: complete
risk: critical
wave: 3
depends_on:
  - "011-store-hiqlite"
  - "012-store-coordination"
  - "023-observability"
  - "036-manifest-and-schema-evolution"
establishes:
  - { kind: file, path: "crates/rahi-store/src/receipt.rs" }
  - { kind: file, path: "crates/rahi-store/src/work.rs" }
  - { kind: file, path: "crates/rahi-store/tests/receipt.rs" }
  - { kind: file, path: "crates/rahi-store/tests/work.rs" }
  - { kind: file, path: "crates/rahi-store/tests/receipt_recovery.rs" }
extends:
  - { spec: "011-store-hiqlite", unit: "crates/rahi-store/src/lib.rs", nature: additive }
  - { spec: "011-store-hiqlite", unit: "crates/rahi-store/src/error.rs", nature: additive }
  - { spec: "012-store-coordination", unit: "crates/rahi-store/src/lock.rs", nature: additive }
  - { spec: "023-observability", unit: "crates/rahi-edge/src/obs/metrics.rs", nature: additive }
  - { spec: "023-observability", unit: "crates/rahi-edge/tests/obs.rs", nature: additive }
refines:
  - { aspect: "a work claim that outlives the lease TTL is a chassis row created and renewed under a short-held lease and fenced by its own token (012 D-10)", unit: { kind: symbol, id: "rahi_store::lock::Lease" } }
references:
  - { unit: { kind: file, path: "docs/design/02-operational-prerequisites.md" }, role: context }
  - { unit: { kind: file, path: "docs/design/01-consumer-contract.md" }, role: context }
obligations:
  - id: "I-1"
    kind: invariant
    text: "A receipt identity is (tenant, namespace, key digest); one identity has at most one row per content revision, and revision numbers for one identity are dense and strictly increasing."
    anchor: "3-1-receipt-identity"
  - id: "I-2"
    kind: invariant
    text: "Content that differs from every recorded revision of an identity is never discarded: it is recorded as a new accepted revision or as a collision row before the caller can acknowledge the source."
    anchor: "3-2-classification-outcomes"
  - id: "I-3"
    kind: invariant
    text: "A receipt row, its processing rows, the domain writes they describe, and their outbox rows commit in one txn or not at all."
    anchor: "3-3-atomic-participation"
  - id: "I-4"
    kind: invariant
    text: "A processing identity (receipt identity, receipt revision, processor, processor revision) reaches done at most once; a new receipt revision or a new processor revision is a new processing identity and may be processed again."
    anchor: "3-4-processing-identity"
  - id: "I-5"
    kind: invariant
    text: "Every write made under a work claim is guarded by the claim's fence token, and a holder whose claim was reclaimed, completed, or dead-lettered commits nothing."
    anchor: "3-5-work-reservation"
  - id: "I-6"
    kind: invariant
    text: "No replicated statement this spec issues evaluates a clock or random function in SQL; every time and every jitter is a bound parameter."
    anchor: "3-5-work-reservation"
  - id: "I-7"
    kind: invariant
    text: "An erased receipt keeps a tombstone holding only its key digest, which classifies a redelivery of the erased item as erased, never as first seen, and no call re-accepts an erased identity."
    anchor: "3-6-retention-and-erasure"
  - id: "I-8"
    kind: invariant
    text: "Every work metric the chassis exports is a count labelled by processor and state only; no tenant, namespace, key, or digest is ever a label."
    anchor: "3-5-work-reservation"
  - id: "V-1"
    kind: verification
    text: "Classification, atomic staging, claims, retry, dead letter, retention, erasure, and every crash point in the recovery table are exercised against a real single-node store."
    anchor: "verification"
    inputs:
      - "cargo test -p rahi-store --locked --test receipt"
      - "cargo test -p rahi-store --locked --test work"
      - "cargo test -p rahi-store --locked --test receipt_recovery"
      - "cargo test -p rahi-edge --locked --test obs"
summary: >
  Every product that ingests from an outside source rebuilds the same
  table: a key, a body digest, the first outcome, and a rule for a repeat
  (statecraft-platform's `sc_idempotency`; aicortex's work-claim rows over
  the Lease and the Outbox). This spec puts one primitive in rahi-store.
  A receipt names an inbound item by tenant, source namespace, and
  external key (or a synthetic key when the source has none) and records
  a digest of its content; classification distinguishes first seen, exact
  redelivery, changed content under the same key (recorded as a new
  revision or a collision, never dropped), and erased. Receipt rows stage
  into the caller's TxnBuilder beside the domain writes and Outbox::stage.
  Processing is identified separately (receipt revision plus processor and
  its policy revision) so a correction or a new policy reprocesses. Work
  is reserved under the existing Lease into a fenced claim row with
  attempt history, bounded retry, a visible dead-letter state, and reclaim
  after expiry; queue counts reach /metrics redacted. Retention compacts to
  a tombstone, erasure keeps only the key digest and is never undone, and
  a crash-point table states recovery at every boundary. Owner decisions
  of 2026-09-24 are recorded.
---

# 045: Receipts and work claims

## 1. Purpose

A cell that ingests from a source it does not control (a mailbox, a
webhook, a CLI that retries) must answer the same four questions for every
delivery: have I seen this item, is it the same content, has the content
changed under the same name, and has the work it caused been done. The
chassis answers none of them today, so each product answers them in its
own table, with its own gaps:

- statecraft-platform keeps `sc_idempotency(key, body_digest, outcome)`
  (its `crates/statecraft-platform/src/migrations.rs`, spec 003 B-22). The
  key is global rather than scoped, so intake has to re-check that a cached
  outcome belongs to the scope it is answering for; the row is written by
  its own `execute` after the domain writes rather than in their
  transaction; and an erased artifact could be recreated by a fresh key
  until that spec's 2026-09-17 correction added a separate check.
- aicortex models a work claim as an application row over rahi-store's
  `Lease` and `Outbox`, the pattern 012 D-10 recommends, and every other
  consumer that needs a claim longer than ten seconds will write it again.

The first consumer of this spec is travel-memory, a domain service on rahi
at N=1 ingesting email: a mailbox redelivers, a provider rewrites headers,
a message can arrive without a `Message-ID`, and extraction policy changes
over time and must be rerun over messages already accepted. travel-memory
links aicortex as a library in the same cell, so one intake or completion
batch carries travel-memory's receipt and domain writes, aicortex's claim
writes, and `Outbox::stage` together in one `TxnBuilder` (B-9, D-13).

The primitive serves thesis responsibility 2 (replicated state) and holds
constitution IX: the receipt is a row, it commits in the caller's `txn`,
and notify stays a hint.

## 2. Territory

### 2.1 What is new

- `crates/rahi-store/src/receipt.rs`: `ReceiptKey`, `ContentDigest`,
  `Receipts::classify`, the staging calls, retention and erasure, and
  `receipt_migration(version)`.
- `crates/rahi-store/src/work.rs`: `ProcessingKey`, `Claim`, `RetryPolicy`,
  reserve, renew, complete, fail, the dead-letter reads and requeue, and
  the sweep.
- Three test files: `tests/receipt.rs`, `tests/work.rs`,
  `tests/receipt_recovery.rs`.
- Four tables, created only by `receipt_migration`: `rahi_receipt`,
  `rahi_receipt_head`, `rahi_processing`, `rahi_processing_attempt`.

### 2.2 What is reused unchanged

- `TxnBuilder` and `StoreHandle::txn` (011, 012 B-4): every write this spec
  makes is a statement appended to the caller's builder or a batch of its
  own through `txn`. There is no second write path.
- `Outbox::stage` and `Outbox::drain` (012 B-4): a receipt or claim
  transition that consumers must hear about is announced by the caller
  staging an envelope in the same builder. This spec adds no outbox and no
  drain loop.
- `StoreHandle::lease`, `Lease`, and `fenced_txn` (012 B-1, B-2): every
  reservation and renewal of 3.5 and the sweep of 3.5 and 3.6 run under a
  short-held lease and write through `fenced_txn` (D-5).
- `FenceToken` (010) is the claim token's type, and the batch abort that a
  superseded token provokes is 012 D-3's technique, applied to the claim
  row instead of `lease_fence`.
- `attest-ledger-core`'s sha256, already a rahi-store dependency (036
  B-7), for digests and synthetic keys. No new third-party dependency.
- `Migration` and the `migrate` verb (011 B-4, 036): the tables are DDL
  the application places in its own list.

### 2.3 What is extended

- `crates/rahi-store/src/lib.rs` (011) re-exports the new types and
  `receipt_migration`. `coordination_migration` is not touched: its SQL is
  part of a checksum every deployed store has recorded (036 B-7), so its
  tables stay exactly as they are and the new tables arrive in a second
  migration.
- `Lease` (012) is refined, not changed: 012 D-10 refused a renewable
  lease and recommended an application claim row
  (`key`, `holder`, `fence`, `expires_at`) created and renewed through
  `fenced_txn` under a short-held lease. This spec makes that row a
  chassis row with one shape, follows that recommendation literally (D-5),
  and leaves B-1's ten-second, non-renewable lease exactly as 012 states
  it.
- `crates/rahi-edge/src/obs/metrics.rs` (023) gains one gauge of redacted
  work counts (B-17, D-12).

No approved spec's text changes.

## 3. Behavior

### 3.1 Receipt identity

- **B-1 (the key).** `ReceiptKey { tenant, namespace, key }`. `tenant` is
  an opaque, required, non-empty application string the chassis never
  interprets (tenancy stays with the consumer, note 02 section 6.4); a
  single-tenant cell passes one constant; an empty tenant is
  `Error::Validation` (D-6). `namespace` names the source and the provider
  account (`email:imap:acct-7`, `statecraft.intake`), non-empty, at most
  256 bytes. `key` is the source's own identifier for the item
  (`Message-ID`, a webhook delivery id, a client idempotency key), at most
  1 KiB.
- **B-2 (the key digest).** The identity column is `key_digest`, sha256
  over a domain-separated encoding of the three parts (a fixed prefix
  `rahi.receipt.key/v1`, then each part length-prefixed). The raw `key` is
  stored beside it for operators and cleared on erasure (B-21), so the
  identity survives erasure without the identifier that may be personal
  data.
- **B-3 (synthetic keys).** A source with no stable identifier uses
  `ReceiptKey::synthetic(tenant, namespace, parts: &[&[u8]])`: the key is
  `syn:` followed by the hex sha256 of the caller's chosen stable parts
  (for email without a `Message-ID`: the provider's UID validity and UID,
  or failing that the canonical content). The row records `key_kind =
  synthetic`. A synthetic key derived from content can never show changed
  content, since a change is a new key; the consumer chooses its parts
  knowing that, and the classification of such a key is only ever first
  seen, redelivered, or erased.
- **B-4 (the content digest).** `ContentDigest` is 32 bytes, stored as
  lowercase hex. The caller computes it over its canonical form of the
  item; canonicalization is the consumer's, because only the consumer
  knows which bytes a provider may rewrite without changing the item (a
  `Received:` header, a signature timestamp). `ContentDigest::of(bytes)`
  is the sha256 helper for the common case.

### 3.2 Classification outcomes

- **B-5 (classify).** `Receipts::classify(store, &ReceiptKey, &ContentDigest)
  -> Classification` reads with `query_consistent`, because a stale answer
  here is a wrong decision (012's rule for admission). It returns one of:
  - `FirstSeen`: no row for the identity.
  - `Redelivered { revision, is_head, outcome }`: the digest equals a
    recorded accepted revision's digest. `is_head` is false when the
    source has redelivered an older version (content A, then B, then A
    again); that is a redelivery, never a new revision (D-7). `outcome` is the reference recorded for that revision, if any,
    so a caller can replay its first answer, including a refusal
    (statecraft 003 B-22).
  - `Changed { head_revision, head_digest }`: rows exist and no accepted
    or collision revision has this digest.
  - `CollisionRedelivered { revision }`: the digest equals a recorded
    collision's digest.
  - `Erased { erased_at }`: the identity carries an erasure tombstone.
  A retention tombstone (B-20) answers as the live rows did, with every
  `outcome` absent.
- **B-6 (staging a decision).** A classification is turned into writes by
  exactly one staging call on the caller's builder, each carrying the head
  revision it was classified against:
  - `stage_first(txn, key, digest, meta)` inserts revision 1 and the head.
  - `stage_revision(txn, key, digest, expected_head, meta)` inserts
    revision `expected_head + 1` as `accepted` and advances the head.
  - `stage_collision(txn, key, digest, expected_head, meta)` inserts
    revision `expected_head + 1` as `collision`, advancing the revision
    counter but not the accepted head. A consumer whose rule is "a changed
    body under a used key is refused" (statecraft 003 B-22) records the
    refusal here rather than dropping it.
  - `stage_seen(txn, key, revision, now)` optionally bumps `last_seen_at`
    and `seen_count` on a redelivery. It is optional because it costs a
    Raft write per redelivery; an unrecorded redelivery loses a counter,
    never content (D-8).
  `meta` carries `now`, `retain_until` (B-20), and an optional `outcome`
  reference of at most 4 KiB, which is a reference (ids, a status code, a
  digest) and never the item's body.
- **B-7 (changed content is never dropped).** `Classification::Changed` is
  `#[must_use]`, and the typed handle it carries is the only way to call
  `stage_revision` or `stage_collision`; there is no call that
  acknowledges a changed item without writing a row. The documented rule
  for the caller is that a source is acknowledged only after the batch
  holding one of the B-6 writes has committed (3.7).
- **B-8 (races are a conflict, not a fork).** Two deliveries of one
  identity classified concurrently both stage against the same expected
  head. The head row is the compare-and-swap: each staging call guards
  the batch on `rahi_receipt_head.revision = expected_head` (0 for a first
  insert), and a guard that matches no row aborts the whole batch by 012
  D-3's technique, surfacing as `Error::Conflict`. The loser classifies
  again and sees `Redelivered` or `Changed`. The domain writes, processing
  rows, and outbox rows in the loser's batch roll back with it (B-9).

### 3.3 Atomic participation

- **B-9 (one txn).** Every staging call of 3.2 and every processing
  transition of 3.4 and 3.5 appends statements to a `TxnBuilder` the caller
  owns and submits with `StoreHandle::txn`. The intake batch is: the
  receipt write, the domain rows it produces, one `rahi_processing` row per
  processor that must run (B-11), and the caller's `Outbox::stage`
  envelope. Either all commit or none does; there is no helper that writes
  a receipt outside the caller's batch, because a receipt committed without
  the work it records is exactly the gap this spec closes. The builder is
  shared with any library the cell links: travel-memory links aicortex in
  the same cell, and aicortex's claim writes are statements in the same
  `TxnBuilder` as travel-memory's receipt, its domain rows, and
  `Outbox::stage`, so all of them commit in one `txn` (D-13). A library
  that wants to take part stages into a builder it is handed and never
  submits one of its own for the same decision.
- **B-10 (reads are outside the batch).** hiqlite's `txn` takes statements,
  not an interactive transaction, so classification is a read before the
  batch and every decision the read informed is re-checked inside the
  batch by a guard (B-8, B-13). A guard that fails aborts the batch; it
  never commits a partial decision.

### 3.4 Processing identity

- **B-11 (the key).** `ProcessingKey { receipt: ReceiptKey, revision,
  processor, processor_revision }`. `processor` names the stage
  (`extract.itinerary`), `processor_revision` names the policy, model, or
  code version whose output the stage writes (`v3`, a policy digest). The
  receipt revision is part of the key, so a corrected item (a new accepted
  revision) is new work, and so is a new `processor_revision` over an old
  revision.
- **B-12 (enqueue).** `stage_work(txn, &ProcessingKey, now)` inserts a
  `pending` row, or does nothing when that identity already exists
  (`INSERT ... ON CONFLICT DO NOTHING`): enqueuing is idempotent.
  Reprocessing a corpus under a new policy is `stage_work` for each receipt
  with the new `processor_revision`, in batches the caller sizes; the rows
  of the old revision stay as history.
- **B-13 (done is terminal).** A processing row moves `pending` to
  `claimed` to `done`, or through `failed` back to `claimed`, or to `dead`.
  `done` is terminal for its identity, and the completing batch is guarded
  on the claim (B-14), so the domain writes of one processing identity
  commit at most once.

### 3.5 Work reservation

- **B-14 (the claim).** `Work::reserve(store, &ProcessingKey, holder,
  hold_for, now) -> Option<Claim>` follows 012 D-10 literally (D-5): it
  takes `StoreHandle::lease("rahi.work/" + namespace + "/" + processor)`,
  and under that lease submits, through `fenced_txn`, a conditional
  `UPDATE` on the processing row that succeeds when the row is `pending`,
  or `claimed` with `expires_at <= now` (reclaim), or `failed` with
  `next_attempt_at <= now`; it increments the row's own `fence`, sets
  `holder` and `expires_at = now + hold_for`, increments `attempt`, and
  opens a row in `rahi_processing_attempt`; then it releases the lease.
  `Claim { key, token: FenceToken, attempt, expires_at }` carries the row
  fence it minted, which guards every later write (B-15) after the
  ten-second lease is gone. A reclaim closes the prior attempt as
  `expired`. `Work::next(store, namespace, processor, holder, hold_for,
  now, limit)` reserves up to `limit` of the oldest eligible rows under one
  lease acquisition. The lease key is per `(namespace, processor)`, not per
  item, so `lease_fence` gains one row per queue rather than one per item.
- **B-14a (the cost, stated).** A reservation costs a lock acquisition,
  a token mint (a write and a `query_consistent` read, 012 B-1), and the
  fenced write: three Raft round trips where a bare conditional `UPDATE`
  would take one, and reservations on one queue are serialised by its
  lease. `Work::next` amortises the first two over a batch. A reservation
  by a single conditional `UPDATE` without the lease (its row fence alone
  deciding the race) remains a possible later optimisation; it needs its
  own amendment and evidence, and is not part of this spec (D-5).
- **B-15 (every write under a claim is fenced).** `Work::guard(txn,
  &Claim, now)` prefixes the caller's batch with a statement that aborts it
  unless the row still has `fence = claim.token`, `state = claimed`, and
  `expires_at > now`, by 012 D-3's `NOT NULL` technique; the abort surfaces
  as `Error::Conflict`. `Work::complete(txn, &Claim, outcome, now)` stages
  the guard, the move to `done`, and the attempt's close, beside the
  caller's domain writes and outbox rows. `Work::renew(store, &Claim,
  hold_for, now)` takes the queue's lease like B-14 and submits, through
  `fenced_txn`, the same guard plus an `expires_at` update, which is 012
  D-10's renewal of the application row, now a chassis call. A zombie
  holder, whose claim expired and was reclaimed, commits nothing (I-5).
- **B-16 (bounded retry and the dead letter).** `Work::fail(txn, &Claim,
  error, &RetryPolicy, now)` records the attempt's error class and a
  bounded detail (at most 2 KiB, never the item's body), then either sets
  `failed` with `next_attempt_at = now + backoff(attempt)` or, when
  `attempt >= policy.max_attempts`, sets `dead`. The backoff is
  exponential with a cap and a jitter seeded from the key digest and the
  attempt number, computed in Rust and bound as a parameter, the way 013
  D-9 seeds its retry wait. A reclaim after expiry counts as an attempt,
  so an item that crashes its worker every time also reaches `dead`.
- **B-17 (the dead letter is visible).** `Work::dead(store, filter,
  page)` lists dead rows with their attempt history through
  `query_paged`, and `Work::requeue(txn, &ProcessingKey, now)` moves a dead
  row back to `pending` with its attempt count kept and a `requeued`
  attempt recorded, so an operator's retry is history, not an erasure of
  it. `Work::counts(store, now)` reads the number of pending, claimed,
  failed, and dead rows per `processor`, aggregated over every tenant and
  namespace. rahi-edge's `/metrics` (023) exports them as the gauge
  `rahi_work_items{processor, state}`, which the cell sets from its sweep
  tick (the chassis runs no loop, B-19). The export is redacted counts
  only (D-12): `processor` is a name the cell's code declares, and no
  tenant, namespace, key, or digest is ever a label or a value (I-8, the
  bounded signals of 041 B-10).
- **B-18 (no SQL clock).** `now` is a `UnixSeconds` parameter on every
  call. No statement uses `unixepoch()`, `CURRENT_TIMESTAMP`, or
  `random()`: hiqlite replicates statements, and a function evaluated on
  each node would diverge (the reason 016 refuses extensions). At N=3 the
  nodes' clocks may disagree; that can reclaim a claim early or late,
  which costs duplicate or delayed work but never a double commit, because
  the fence guard, not the clock, decides who commits (I-5).
- **B-19 (the sweep runs under the existing lease).** `Work::sweep(store,
  namespace, now, limit)` takes `StoreHandle::lease("rahi.work.sweep/" +
  namespace)` and, through `fenced_txn`, closes expired claims as
  `expired`, promotes `failed` rows whose budget is spent to `dead`, and
  deletes rows past retention (B-20). It works in chunks sized to finish
  inside `LEASE_TTL_SECONDS`, as 012 requires of every lease holder. The
  chassis never runs the sweep; the cell runs it on its own tick, as it
  runs `Outbox::drain` (012 B-4).

### 3.6 Retention and erasure

- **B-20 (retention compacts to a tombstone).** Every receipt row carries
  `retain_until` and `tombstone_until`, set by the caller at staging time
  from its per-namespace policy (`tombstone_until` at or after
  `retain_until`). Once `retain_until <= now` and every processing row of
  that receipt is `done` or `dead`, the sweep compacts it (D-9): it
  deletes the processing and attempt rows, clears `key`, `outcome`, and
  the per-delivery metadata, and keeps a compact tombstone of the head row
  and, per revision, the revision number, its disposition, and its content
  digest. A late redelivery against that tombstone is still recognised
  (B-5): the same content is `Redelivered` with no outcome, different
  content is `Changed`. Only when `tombstone_until <= now` does the sweep
  delete the tombstone, after which a redelivery is `FirstSeen`. The
  default for a namespace with no stated horizon is no expiry (both
  null).
- **B-21 (erasure is staged).** `Receipts::stage_erasure(txn, scope, now)`,
  where `scope` is one identity, a namespace within a tenant, or a whole
  tenant, appends statements that clear `key`, `outcome`, and attempt
  details, delete processing and attempt rows and every revision row, and
  leave per identity one tombstone row holding only `key_digest` and
  `erased_at` (D-10). No content digest survives an erasure, because a
  digest of a low-entropy item can confirm a guess at it. For a synthetic
  key derived from content (B-3) the key digest is itself derived from
  that content; the consumer that chose content as the key parts accepted
  that when it chose them. The caller stages its own domain erasure and an
  `Outbox::stage` envelope of kind `rahi.receipt.erased` in the same
  builder, which is the hook: every derived store that listens re-reads
  and erases its copy, and polls the watermark as every consumer must.
- **B-22 (no resurrection).** Classification of an erased identity is
  `Erased` whatever the digest, and every staging call of B-6 against it
  aborts the batch (the B-8 guard sees the erased head). There is no call
  that re-accepts an erased identity (D-11): an erased item is never
  accepted again under the same identity, and an erasure tombstone is
  never removed by retention.

### 3.7 Crash-point recovery

The intake path is: classify, commit the intake batch (B-9), acknowledge
the source. The work path is: reserve, process, commit the completing
batch (B-15), acknowledge any caller waiting on the result. Every crash
point below is a test in `tests/receipt_recovery.rs` that stops the
process (or drops the future) at that point, restarts over the same data
directory, and asserts the stated end state.

| Crash point | What is durable | What recovery does | End state |
|---|---|---|---|
| After classify, before the intake batch | nothing | the source redelivers; classify again | same as a first delivery |
| After the receipt write (intake batch committed), before the source is acknowledged | receipt, domain rows, pending processing rows, outbox row | the source redelivers; classify answers `Redelivered`; the caller acknowledges without writing | one receipt, one set of work |
| During the intake batch (outcome unknown to the caller) | all of the batch or none of it (one Raft entry, 011 B-2) | the caller classifies again with `query_consistent`: `Redelivered` means it landed, `FirstSeen` means it did not | at most one receipt |
| After reserve, before processing ends | a claimed row with an open attempt | the claim expires; the next `reserve` or the sweep reclaims it and closes the attempt as `expired` | reprocessed; the attempt counts toward the dead letter |
| After processing, before the completing batch | the claim; nothing the processing produced | as above; processing must be repeatable, and any effect outside the store is keyed by the processing key and the claim token | reprocessed once more, committed once |
| During the completing batch (outcome unknown) | all of the batch or none of it | the worker reads the row with `query_consistent`: `done` with its token means committed; otherwise it retries the batch, and a retry after another holder won aborts on the guard | domain writes committed at most once (I-4) |
| After the completing batch, before the outbox drain or the caller's acknowledgement | done row, domain rows, outbox row | the next drain publishes (at least once, 012 B-4); a caller that retries its request gets `Redelivered` with the recorded outcome | consumers idempotent by key; caller replayed |
| A zombie holder commits after its claim was reclaimed | the new holder's claim | the zombie's guard aborts its batch with `Error::Conflict` | nothing from the zombie |
| During a sweep | chunks already committed through `fenced_txn` | the next sweep continues; a sweeper whose lease was superseded commits nothing (012 B-2) | no half-applied chunk |

## 4. Functional requirements

- **FR-001.** Classification returns each of `FirstSeen`, `Redelivered`
  (head and non-head), `Changed`, `CollisionRedelivered`, and `Erased`
  against rows written by the staging calls, and the key digest of the
  same three parts is equal across two processes.
- **FR-002.** Two tasks staging `stage_first` for one identity: exactly one
  batch commits, the other returns `Error::Conflict` and leaves none of its
  domain rows, processing rows, or outbox row.
- **FR-003.** A changed digest staged with `stage_collision` leaves the
  accepted head unchanged and is listed; no public call acknowledges a
  `Changed` classification without a staging call (a compile-fail doc
  test).
- **FR-004.** A new `processor_revision` over an accepted receipt enqueues
  new work; `stage_work` for an existing processing identity is a no-op.
- **FR-005.** A claim whose token was superseded by a reclaim commits
  nothing and returns `Error::Conflict`; its domain writes are absent.
- **FR-006.** With `max_attempts = 3`, three failures (or three expiries)
  reach `dead`; the attempt history holds three rows; `requeue` restores
  `pending` and adds a `requeued` attempt.
- **FR-007.** No statement text issued by `receipt.rs` or `work.rs`
  contains a SQL time or random function (a test over the statement
  builders).
- **FR-008.** Retention compacts only receipts whose processing rows are
  all terminal; a compacted receipt still classifies a late redelivery
  until `tombstone_until`; erasure leaves a tombstone holding only the key
  digest that classifies as `Erased` and refuses every staging call.
- **FR-010.** A reservation and a renewal hold the queue's lease and write
  through `fenced_txn`; a reservation whose lease was superseded commits
  nothing.
- **FR-011.** `/metrics` renders `rahi_work_items` with the labels
  `processor` and `state` only, and a test asserts no tenant, namespace,
  or key string from the fixture appears in the rendered text.
- **FR-009.** Every row of the table in 3.7 holds on a single-node store
  restarted over its data directory.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p rahi-store --locked` passes, including
  `tests/receipt.rs`, `tests/work.rs`, and `tests/receipt_recovery.rs`.
- **AC-2.** `cargo test -p rahi-store --locked --test outbox` and `--test
  lock` pass unchanged: nothing in 012's surface moved.
- **AC-2a.** `cargo test -p rahi-edge --locked --test obs` passes with
  the new gauge.
- **AC-3.** `coordination_migration`'s SQL, and therefore its recorded
  checksum, is byte for byte what it was before this spec.

## 6. Out of scope

- Migrating any consumer. statecraft-platform's `sc_idempotency` and
  aicortex's claim rows move in their own repositories; see the follow-up
  under section 7.
- A chassis-run loop. The sweep, the drain, and the worker loop are the
  cell's (012 B-4, thesis section 6's refusal of schedulers in the
  chassis).
- Canonicalization of any source's content (B-4), and any rule that
  decides whether changed content is a correction or an attack: the
  chassis records both; the consumer decides which staging call to make.
- Exactly-once effects outside the store. The claim token is offered as
  the idempotency key for an external call; honouring it is the external
  system's.
- N=3 specifics beyond B-18. The statements are deterministic and every
  guard is a replicated predicate, but no multi-node test is asked for
  here; 032's topology tests would carry one.

## 7. Resolved decisions

- **D-1 (2026-09-24, owner decision).** rahi-store gains a generic
  idempotency and receipt primitive so that products stop building their
  own. The first consumer is travel-memory (N=1, hiqlite, email intake).
  This spec is a draft; approval is a separate human act.
- **D-2 (2026-09-24, owner decision).** The primitive reuses 012's `Lease`
  and `Outbox` rather than duplicating them, and what is new versus
  extended is stated (section 2). Approved specs are related by typed
  edges and are not edited.
- **D-3 (2026-09-24, owner decision).** The requirements in scope are the
  six this spec's section 3 is organized by: identity with a content
  digest and four distinct outcomes including a synthetic key; atomic
  participation with domain writes and `Outbox::stage`; a processing
  identity separate from the receipt identity, keyed by processor and
  policy revision; work reservation with fencing, attempt history, bounded
  retry, a visible dead letter, and reclaim after expiry; retention and
  erasure hooks; and a crash-point recovery table. Changed content under
  a used key is never silently dropped.
- **D-4 (2026-09-24, owner decision).** Consumer migration is out of
  scope and recorded as follow-up (below).

- **D-5 (2026-09-24, owner decision; open question 1).** A reservation
  and a renewal hold a short-held lease, following 012 D-10: B-14 and B-15
  as written. The per-reservation cost is stated in B-14a. A reservation
  by a single conditional update without the lease stays a possible later
  optimisation, not part of this spec. Rejected for now: the lease-free
  conditional update this draft first proposed.
- **D-6 (2026-09-24, owner decision; open question 2).** `tenant` is a
  required, non-empty string (B-1). Rejected: an `Option` as
  `Envelope.tenant` has.
- **D-7 (2026-09-24, owner decision; open question 3).** A redelivery of
  an older revision is `Redelivered { is_head: false }`, never a new
  revision (B-5).
- **D-8 (2026-09-24, owner decision; open question 4).** Redelivery
  counting through `stage_seen` is optional, as drafted (B-6).
- **D-9 (2026-09-24, owner decision; open question 5).** After retention
  a receipt compacts to a tombstone kept until `tombstone_until` (B-20).
  Rejected: deleting the receipt outright when retention ends.
- **D-10 (2026-09-24, owner decision; open question 6).** An erasure
  tombstone keeps only the key digest (B-21). Rejected: keeping content
  digests, and keying them with a per-tenant secret.
- **D-11 (2026-09-24, owner decision; open question 7).** An erased item
  is never accepted again: there is no un-erase call (B-22). Rejected:
  the `stage_unerase` this draft first proposed.
- **D-12 (2026-09-24, owner decision; open question 8).** Queue counts are
  exported on the chassis `/metrics`, as redacted counts only (B-17, I-8).
  This adds the dependency on 023 and the additive edge on its
  `metrics.rs`.
- **D-13 (2026-09-24, owner statement).** travel-memory, the first
  consumer, links aicortex as a library in the same cell, so its receipt
  writes, aicortex's claim writes, and `Outbox::stage` share one
  transaction (B-9).
- **D-14 (2026-09-24, ordinal).** 043 and 044 are drafts in flight on
  their own branches, so this draft takes 045 and none of the three
  collide.
- **D-15 (2026-09-24, owner approval).** The owner approved this spec for
  implementation on 2026-09-24, with D-5 to D-14 as recorded, by issuing
  the travel-memory work order for rahi. `status` moves from `draft` to
  `approved`; `implementation` stays `pending` until a session builds it.
  D-1's sentence that approval is a separate human act is kept as the
  record of the draft; this entry is that act.
- **D-16 (2026-09-24, build session).** `crates/rahi-store/src/error.rs`'s
  `map` classified only hiqlite's own `H::ConstraintViolation` as
  `Error::Conflict`; a constraint violation raised inside a `txn` batch
  (012 D-3's `NOT NULL` guard technique, and a bare primary-key collision
  alike) surfaces as `H::Transaction` instead, which fell through to
  `Error::Validation`. That contradicts FR-002 and FR-005's `Error::Conflict`
  and the function's own stated rule ("a constraint is Conflict"); spec
  011's `tests/txn.rs` already hedges the exact ambiguity
  (`Error::Conflict(_) | Error::Validation(_)`). Fixed additively (extends
  edge above, spec 011): an `H::Transaction` whose message names a SQL
  constraint failure now also maps to `Error::Conflict`. No text of spec 011
  or 012 states the narrower mapping, so nothing shipped is contradicted.
  `Receipts::stage_first` was rewritten to match: it seeds the head row at
  revision 0 (`SEED_HEAD`, `ON CONFLICT DO NOTHING`, which never raises) and
  then applies the same CAS `stage_revision` uses, rather than leaning on
  the primary key's own collision.
- **D-17 (2026-09-24, build session).** B-14 describes the claim `UPDATE`
  and the attempt row's `INSERT` together, but `fenced_txn`'s
  `Statement::fenced` accepts only `UPDATE` and `DELETE` (an `INSERT` has no
  row to compare a token against, per its own doc comment), so the two
  cannot be one `fenced_txn` call. `Work::reserve` and `Work::next` submit
  the claim through `fenced_txn` (minting the row's fence from the lease's
  own token, matching `tests/lock.rs`'s "the row records the lease that
  wrote it"), then open the attempt row through a plain `StoreHandle::txn`,
  stamped with the same token, exactly as `Statement::fenced`'s
  documentation prescribes for an insert under a lease. This is two Raft
  entries, not the one B-14a's "three round trips" implies for "the fenced
  write"; the crash-point table's row 4 names the state after `reserve`
  returns, not a crash between its two writes, so the two-write shape holds
  the table's assertion without a hand-fenced single batch. Superseded by
  D-21.
- **D-18 (2026-09-24, build session).** B-19 fixes `Work::sweep`'s signature
  at four arguments with no retry policy, yet B-16's last sentence requires
  a chain of pure expiries (no explicit `Work::fail`) to also reach `dead`
  once its budget is spent, and neither `Work::reserve`, `Work::next`, nor
  B-19's literal signature carries one. `Work::sweep` takes `&RetryPolicy`
  as a fifth argument so its own reclaim step can apply the same ceiling
  `Work::fail` applies explicitly.
- **D-19 (2026-09-24, build session).** B-19 says the sweep acts "through
  fenced_txn". Most of the rows a sweep touches (`rahi_receipt`,
  `rahi_receipt_head`, `rahi_processing_attempt`) carry no `fence` column at
  all, so `fenced_txn`'s automatic per-statement rewrite fails them outright
  ("no such column: fence"); the one table that does, `rahi_processing`,
  would have that column stamped from the sweep's own lease
  (`rahi.work.sweep/<namespace>`), a different lease key with an unrelated
  token sequence from the reservation queue's (`rahi.work/<namespace>/
  <processor>`), so comparing a row's existing fence against it is not a
  meaningful check. `Work::sweep` holds the lease for mutual exclusion
  among concurrent sweepers and writes every chunk through a plain `txn`;
  row 9 of the crash-point table (no half-applied chunk) holds regardless,
  since each chunk is still one Raft entry. Superseded by D-22.

- **D-20 (2026-09-25, build session; a revision over a compacted
  tombstone).** B-20 lets a late delivery with different content classify
  as `Changed` against a retention tombstone, and B-6 then stages a new
  revision, but neither says what becomes of the tombstone. Left as it was,
  the head stayed `tombstoned` and keyless, and the sweep would delete the
  new, live revision at the old `tombstone_until`, dropping content I-2
  says is never dropped. The B-8 guard statements of `stage_revision` and
  `stage_collision` (and `stage_first`) now also clear `tombstoned`, write
  the raw key back, and take the new revision's `retain_until` and
  `tombstone_until`. On a head that was never compacted they only take the
  newest revision's horizons. `tests/receipt.rs` covers it.
- **D-21 (2026-09-25, build session; the reservation is one batch,
  superseding D-17).** D-17's two-entry reservation could leave a claimed
  row with no open attempt if the process stopped between the entries,
  which row 4 of 3.7 does not allow. A reservation is now one `txn`: the
  queue lease's own guard statement first (012 D-3, the statement
  `fenced_txn` runs, exposed crate-internally as `Lease::guard_statement`
  and `Lease::superseded_error`, and `fenced_txn` now uses both, which is
  the additive edge on 012's `lock.rs`), then the claim `UPDATE` fenced by
  hand exactly as `Statement::fenced` would rewrite it, then the prior
  attempt's close as `expired` on a reclaim (B-14), then the new attempt's
  `INSERT ... SELECT`, which inserts only when the claim in the same batch
  took the row. The claim re-checks the row's eligibility inside the batch
  (B-10). A superseded lease aborts all of it as `Error::Conflict`
  (FR-010). `Work::next` skips a row that moved since its read, and now
  propagates a store error rather than swallowing it. Rejected: keeping two
  entries and relying on the sweep, because nothing would ever open the
  missing attempt. Evidence for FR-010's superseded half is the shared
  guard's own test (012 `tests/lock.rs`, which now runs through
  `guard_statement`): a reservation holds its lease for milliseconds and
  exposes no seam where a test could supersede it.
- **D-22 (2026-09-25, build session; the sweep is guarded, superseding
  D-19).** D-19 wrote every sweep chunk through a plain `txn`, which a
  superseded sweeper could still commit, contrary to row 9 of 3.7. Each
  phase of `Work::sweep` is now one chunk of at most `limit` rows submitted
  as one batch behind the sweep lease's guard, so a superseded sweeper
  commits nothing and no chunk is half-applied. Every statement re-checks
  inside the batch the condition it was read under: an expired claim that a
  reservation took in between is neither dead-lettered nor has its attempt
  closed; a receipt that gained open work or a new revision is not
  compacted; a tombstone that became live is not deleted. The `failed`
  promotion reads the budget from the policy D-18 passes
  (`attempt >= max_attempts`). **Measured, and recorded for 012 and the
  hiqlite work:** hiqlite's lock release is unacknowledged and lock state is
  replayed on restart, so a lease released just before a stop, or held at a
  crash, reads as held after the restart. It is taken over by the first
  lease request made after `LEASE_TTL_SECONDS`, but a request queued before
  that is never woken and times out (hiqlite expires a dead holder only on a
  new lock request). The crash tests of rows 4 and 9 therefore wait one TTL
  after the restart before the next reservation or sweep, which is the
  recovery a cell's tick gives. This spec changes nothing in 012.
- **D-23 (2026-09-25, build session; positional parameters).** SQLite reads
  `$1` as a named parameter and numbers named parameters by their first
  appearance, while hiqlite binds positionally, so a statement whose `$n`
  first appear out of order binds the wrong values silently (the CAS guards
  here reuse and reorder parameters). Every statement in `receipt.rs` and
  `work.rs` uses SQLite's explicit `?NNN` form instead. A scan of every
  other string literal in the workspace found no statement whose `$n` first
  appear out of order, so nothing else is affected.
- **D-24 (2026-09-25, build session; the requeued attempt).** B-17 records a
  `requeued` attempt with the attempt count kept, so it shares its attempt
  number with the attempt that went `dead`. The attempt table's primary key
  therefore includes `outcome`, and a requeue inserts its record only when
  the row is `dead`, so a requeue of a live row writes nothing. The attempt
  closes (`done`, `failed`, `dead`) match only the `open` row.
- **D-25 (2026-09-25, build session; what an erasure tombstone holds).**
  B-21 leaves "one tombstone row holding only `key_digest` and
  `erased_at`" per identity. The head row keeps structural columns it
  cannot drop (`revision`, `created_at`, the flags); `tenant`, `namespace`
  and `key_kind` are cleared to empty strings with `key`, `outcome` and the
  digests, because a tenant or a source account can itself identify a
  person. `stage_erasure` of one identity also inserts that tombstone when
  the identity was never delivered, so its first delivery afterwards is
  `Erased` (B-22). A namespace or tenant scope selects its identities
  before the statement that clears the scope columns.

### Follow-up: statecraft-platform's `sc_idempotency`

Recorded for statecraft's own corpus; nothing here changes that
repository. `sc_idempotency(key, body_digest, outcome)` maps onto a
receipt as: `namespace = "statecraft.intake"`, `tenant` = the
repository's tenant, `key` = the caller's idempotency key, `digest` =
`body_digest`, `outcome` = the first outcome's references (003 B-22's
replay, refusals included). A changed body is `stage_collision` plus the
refusal it already returns. What statecraft gains: the tenant scope
removes the cross-scope re-check in its intake; the receipt commits in the
same batch as the ordering rows instead of in a later `execute`; B-22's
tombstone replaces the separate resurrection check from its 2026-09-17
correction. Existing rows migrate by an `INSERT ... SELECT` in a
statecraft migration once the tenant of each key is derivable; keys whose
tenant is not derivable stay in `sc_idempotency` until their retention
ends. aicortex's claim rows are the same kind of follow-up against B-14.

## Verification

```verify:cli
cargo test -p rahi-store --locked --test receipt
cargo test -p rahi-store --locked --test work
cargo test -p rahi-store --locked --test receipt_recovery
cargo test -p rahi-store --locked --test outbox
cargo test -p rahi-store --locked --test lock
cargo test -p rahi-edge --locked --test obs
```
