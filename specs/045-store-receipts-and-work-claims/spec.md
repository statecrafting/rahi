---
id: "045-store-receipts-and-work-claims"
title: "Receipts and work claims: an inbound item is recorded once per content revision, processed once per processor revision, and never silently dropped"
status: draft
kind: kernel
domain: store
created: "2026-09-24"
authors: ["Bartek Kus"]
implementation: pending
risk: critical
wave: 3
depends_on:
  - "011-store-hiqlite"
  - "012-store-coordination"
  - "036-manifest-and-schema-evolution"
establishes:
  - { kind: file, path: "crates/rahi-store/src/receipt.rs", planned: true }
  - { kind: file, path: "crates/rahi-store/src/work.rs", planned: true }
  - { kind: file, path: "crates/rahi-store/tests/receipt.rs", planned: true }
  - { kind: file, path: "crates/rahi-store/tests/work.rs", planned: true }
  - { kind: file, path: "crates/rahi-store/tests/receipt_recovery.rs", planned: true }
extends:
  - { spec: "011-store-hiqlite", unit: "crates/rahi-store/src/lib.rs", nature: additive }
refines:
  - { aspect: "a work claim that outlives the lease TTL is a chassis row fenced by its own token (012 D-10)", unit: { kind: symbol, id: "rahi_store::lock::Lease" } }
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
    text: "An erased receipt keeps a tombstone that classifies a redelivery of the erased item as erased, never as first seen."
    anchor: "3-6-retention-and-erasure"
  - id: "V-1"
    kind: verification
    text: "Classification, atomic staging, claims, retry, dead letter, retention, erasure, and every crash point in the recovery table are exercised against a real single-node store."
    anchor: "verification"
    inputs:
      - "cargo test -p rahi-store --locked --test receipt"
      - "cargo test -p rahi-store --locked --test work"
      - "cargo test -p rahi-store --locked --test receipt_recovery"
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
  is reserved by a fenced claim row with attempt history, bounded retry, a
  visible dead-letter state, and reclaim after expiry; sweeps run under
  the existing Lease. Retention and erasure are staged calls, and a
  crash-point table states recovery at every boundary.
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
over time and must be rerun over messages already accepted.

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
- `StoreHandle::lease`, `Lease`, and `fenced_txn` (012 B-1, B-2): the sweep
  of 3.5 and 3.6 runs under a lease and writes through `fenced_txn`.
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
  (`key`, `holder`, `fence`, `expires_at`) renewed through a fenced write.
  This spec makes that row a chassis row with one shape, and leaves B-1's
  ten-second, non-renewable lease exactly as 012 states it.

No approved spec's text changes.

## 3. Behavior

### 3.1 Receipt identity

- **B-1 (the key).** `ReceiptKey { tenant, namespace, key }`. `tenant` is
  an opaque, non-empty application string the chassis never interprets
  (tenancy stays with the consumer, note 02 section 6.4); a single-tenant
  cell passes one constant. `namespace` names the source and the provider
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
    again). `outcome` is the reference recorded for that revision, if any,
    so a caller can replay its first answer, including a refusal
    (statecraft 003 B-22).
  - `Changed { head_revision, head_digest }`: rows exist and no accepted
    or collision revision has this digest.
  - `CollisionRedelivered { revision }`: the digest equals a recorded
    collision's digest.
  - `Erased { erased_at }`: the identity carries an erasure tombstone.
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
    never content.
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
  the work it records is exactly the gap this spec closes.
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
  hold_for, now) -> Option<Claim>` is one conditional `UPDATE` on the
  processing row: it succeeds when the row is `pending`, or `claimed` with
  `expires_at <= now` (reclaim), or `failed` with `next_attempt_at <= now`;
  it increments the row's `fence`, sets `holder` and `expires_at = now +
  hold_for`, increments `attempt`, and opens a row in
  `rahi_processing_attempt`. `Claim { key, token: FenceToken, attempt,
  expires_at }` carries the fence it minted. A reclaim closes the prior
  attempt as `expired`. `Work::next(store, namespace, processor, holder,
  hold_for, now, limit)` reserves the oldest eligible rows, one statement
  each.
- **B-15 (every write under a claim is fenced).** `Work::guard(txn,
  &Claim, now)` prefixes the caller's batch with a statement that aborts it
  unless the row still has `fence = claim.token`, `state = claimed`, and
  `expires_at > now`, by 012 D-3's `NOT NULL` technique; the abort surfaces
  as `Error::Conflict`. `Work::complete(txn, &Claim, outcome, now)` stages
  the guard, the move to `done`, and the attempt's close, beside the
  caller's domain writes and outbox rows. `Work::renew(store, &Claim,
  hold_for, now)` is the same guard plus an `expires_at` update, which is
  012 D-10's renewal of the application row, now a chassis call. A zombie
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
  it. The chassis exposes counts of pending, claimed, failed, and dead rows
  per `(namespace, processor)` as a read; a metric over them carries those
  two names only, never a key, a digest, or a tenant (041 B-10's bounded
  signals).
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

- **B-20 (retention).** Every receipt row carries `retain_until`, set by
  the caller at staging time from its per-namespace policy. The sweep
  deletes a receipt's rows, and its processing and attempt rows, once
  `retain_until <= now` and every processing row of that receipt is `done`
  or `dead`. A deleted receipt is forgotten: a redelivery after retention
  classifies as `FirstSeen`. The consumer therefore sets retention at or
  above its source's redelivery horizon; the default for a namespace with
  no stated horizon is no expiry (`retain_until` null).
- **B-21 (erasure is staged).** `Receipts::stage_erasure(txn, scope, now)`,
  where `scope` is one identity, a namespace within a tenant, or a whole
  tenant, appends statements that clear `key`, `outcome`, and attempt
  details, delete processing and attempt rows, and leave per identity one
  tombstone row holding `key_digest`, the digests of its revisions, and
  `erased_at`. The caller stages its own domain erasure and an
  `Outbox::stage` envelope of kind `rahi.receipt.erased` in the same
  builder, which is the hook: every derived store that listens re-reads
  and erases its copy, and polls the watermark as every consumer must.
- **B-22 (no resurrection).** Classification of a tombstoned identity is
  `Erased` whatever the digest, and `stage_first` against a tombstoned
  identity aborts the batch (the B-8 guard sees the head). A consumer that
  must accept an item after erasure (a user re-sends it deliberately) says
  so by `stage_unerase(txn, key, digest, meta)`, which records the
  decision as a new revision; it is never the default path.

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
- **FR-008.** Retention deletes only receipts whose processing rows are
  all terminal; erasure leaves a tombstone that classifies as `Erased` and
  refuses `stage_first`.
- **FR-009.** Every row of the table in 3.7 holds on a single-node store
  restarted over its data directory.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p rahi-store --locked` passes, including
  `tests/receipt.rs`, `tests/work.rs`, and `tests/receipt_recovery.rs`.
- **AC-2.** `cargo test -p rahi-store --locked --test outbox` and `--test
  lock` pass unchanged: nothing in 012's surface moved.
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

### Open questions for the owner

1. **Reserve without the lease (B-14).** This draft reserves a claim with
   one conditional `UPDATE` whose token is the processing row's own fence,
   and uses `Lease` only for the sweep. 012 D-10's recommendation reads as
   "created and renewed by `fenced_txn` under a short-held lease", which
   would cost a lock, a token mint, and a write per reservation. Is the
   single-statement claim acceptable, or must every reserve hold a lease?
2. **Tenant shape (B-1).** Required non-empty string, or `Option` as
   `Envelope.tenant` is?
3. **Older-revision redelivery (B-5).** `Redelivered { is_head: false }`
   (proposed), or a new revision recording the reversion?
4. **Redelivery bookkeeping (B-6).** Is `stage_seen` optional (proposed),
   or must every redelivery be counted at the cost of a write?
5. **Retention floor (B-20).** Delete the receipt outright after retention
   (proposed), or keep a compact tombstone (key digest and digests) for a
   second, longer period so a very late redelivery is still recognized?
6. **Erasure tombstone contents (B-21).** Content digests of low-entropy
   items can confirm a guess. Keep them (proposed, needed for B-22), keep
   only the key digest, or key them with a per-tenant secret?
7. **Un-erase (B-22).** Keep `stage_unerase`, or refuse re-acceptance of
   an erased identity entirely at the chassis?
8. **Metrics (B-17).** Should rahi-edge's `/metrics` export the counts, or
   is the read enough and the cell exports its own?
9. **Ordinal.** 043 and 044 are drafts in flight on their own branches;
   this draft takes 045 so none of the three collide.

## Verification

Planned: none of these files exists until the spec is built.

```verify:cli
cargo test -p rahi-store --locked --test receipt
cargo test -p rahi-store --locked --test work
cargo test -p rahi-store --locked --test receipt_recovery
cargo test -p rahi-store --locked --test outbox
cargo test -p rahi-store --locked --test lock
```
