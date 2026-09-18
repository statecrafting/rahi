---
id: "042-ledger-lifetime-identity"
title: "Lifetime identity: one id names one decision for the life of the chain, and absence is proven rather than assumed"
status: draft
kind: kernel
domain: ledger
created: "2026-09-17"
authors: ["Bartek Kus"]
implementation: pending
risk: critical
wave: 3
depends_on:
  - "013-ledger-decision-chain"
  - "014-ledger-sealing-and-archive"
  - "030-operational-verbs"
establishes:
  - "crates/rahi-ledger/src/identity.rs"
  - "crates/rahi-ledger/tests/identity.rs"
extends:
  - { spec: "013-ledger-decision-chain", unit: "crates/rahi-ledger/src/append.rs", nature: additive }
  - { spec: "013-ledger-decision-chain", unit: "crates/rahi-ledger/src/chain.rs", nature: additive }
  - { spec: "013-ledger-decision-chain", unit: "crates/rahi-ledger/src/record.rs", nature: additive }
  - { spec: "013-ledger-decision-chain", unit: "crates/rahi-ledger/src/lib.rs", nature: additive }
  - { spec: "013-ledger-decision-chain", unit: "crates/rahi-ledger/tests/append.rs", nature: additive }
  - { spec: "014-ledger-sealing-and-archive", unit: "crates/rahi-ledger/src/seal.rs", nature: additive }
  - { spec: "014-ledger-sealing-and-archive", unit: "crates/rahi-ledger/tests/seal.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/src/lib.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/src/verbs.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/tests/cli.rs", nature: additive }
constrains:
  - { flavor: invariant-freeze, unit: "crates/rahi-ledger/src/identity.rs", note: "one id names one decision for the life of the chain; absence is never inferred from unavailable history" }
references:
  - { unit: { kind: file, path: "docs/design/01-consumer-contract.md" }, role: context }
summary: >
  The chain can say what happened but not whether a given decision ever
  happened, because spec 013 tells a lost compare-and-swap from a duplicate
  id by asking whether the id is resident and spec 014 deletes the row when
  its segment is sealed. Past the hot window a retried append therefore
  writes a second record under the same id, and full verification passes
  because a duplicate id is not a broken link. This spec gives every decision
  a narrow resident identity row that survives sealing, makes the id's
  primary key the arbitration inside the append transaction rather than a
  read before it, stamps the row when its segment is sealed, and answers
  presence, absence and unproven history as three different answers. A
  reindex verb closes a chain sealed before this spec existed.
---

# 042: Lifetime identity

## 1. Purpose

A governed cell's exactly-once story rests on being able to ask the chain one
question: has this decision already been recorded? Spec 013 D-5 answers it by
re-reading the chain after a failed insert, because hiqlite reports every
statement error in a transaction as one error and the variant carries no
signal. Spec 014 then makes that answer expire: when a segment is sealed the
rows leave `kernel_decisions`, so past the hot window "never appended" and
"appended and archived" are the same answer, and an appender that lost its
acknowledgement and retried appends a second record with the same id, the
same content, and a different parent. `verify_chain` still passes at both
depths, because the chain is linear and every link is intact; only the id is
used twice.

The defect is reproduced, not theorized. A consumer on the pinned 0.1.0
chassis appended a decision, lost the receipt acknowledgement, sealed genesis
and that decision through `Ledger::seal_if_needed` into `FsArchive`, reopened
the same durable node and retried: two lifetime copies, identical in every
field but the assigned parent. A second reproduction defeats every pre-read
repair. Pause a retry after a complete and verified negative lookup, append
and seal the same decision in another task, then resume the retry: both
appends succeed at different hashes, so an archive scan or a segment count
taken before the insert closes nothing. A process-local lock cannot cover
another node, and an application attempt marker can refuse a recovery it is
unsure of but cannot make the chassis's own append atomic with sealing.
Section 2.0 of the consumer contract records the limitation and says atomic
lifetime idempotence and its migration need a separately governed design.
This spec is that design.

It serves constitution XI without amending it. The append stays a
compare-and-swap on a unique parent index inside one transaction, sealed
segments stay immutable, and the new answer is computed from resident state
alone, so a chain still verifies without archived history resident.

## 2. Territory

One new module in `crates/rahi-ledger`, `identity.rs`, and its integration
test. The module owns the identity table, the identity digest, the lifetime
lookup, the coverage accounting, and the reindex walk.

Additive extends on spec 013's `append.rs` (the insert transaction gains the
identity statement and classification reads the identity row), `chain.rs`
(the baseline DDL and the open-time backfill), `record.rs` (canonical bytes
of a decision with its parent omitted), `lib.rs` (the exports), and
`tests/append.rs`; on spec 014's `seal.rs` (the seal transaction stamps the
rows it archives) and `tests/seal.rs`; and on spec 030's `rahi-cli` for the
`ledger reindex` verb and the coverage line `ledger verify` prints.

The consumer contract's limitation paragraph is rewritten by the session that
lands this spec. `docs/` is in the coupling gate's bypass floor, so that edit
needs no edge; the `references` edge above records the dependency in the
other direction.

## 3. Behavior

- **B-1 (the identity row).** `kernel_decision_identity (id TEXT PRIMARY
  KEY, identity_digest BLOB NOT NULL, record_hash BLOB NOT NULL,
  segment_hash BLOB NULL)`. `Ledger::open` creates it idempotently, as the
  chassis's own baseline rather than an application migration, for the reason
  spec 013 D-4 gives for `kernel_decisions`: the shape is fixed here, no app
  versions it, and a boot that found no table could not tell an empty chain
  from a deleted one.
- **B-2 (the identity digest).** `identity_digest` is sha256 over the
  canonical bytes of the `Decision` with `prev_hash` omitted. Every other
  field is covered: id, kind, actor, capability, outcome, reason, payload,
  and `at`. The parent is excluded because a retry re-chains onto whatever
  head it finds, so the parent is the one field an honest retry is expected
  to change. No hashed byte of a record, a segment, or an export changes:
  the digest lives in a resident table and enters no envelope.
- **B-3 (one transaction writes both).** The record insert and its identity
  insert commit in one `txn`, the genesis record spec 013 B-4 writes
  included. The identity table's primary key MUST be the arbitration for a
  duplicate id, exactly as the unique parent index is the arbitration for a
  lost compare-and-swap: `append` never gates on a read, because a read
  before a write is not atomic against another node's append or another
  task's seal, which is what the second reproduction above demonstrates.
  Constitution IX and XI both hold unchanged.
- **B-4 (classification is lifetime-scoped).** After a failed insert,
  classification reads the identity row rather than `kernel_decisions`. A row
  whose `identity_digest` equals this decision's means the decision is
  already in the chain: return the stored `record_hash`, whether the record
  is resident or sealed. A row with a different digest is `Error::Conflict`
  naming the stored hash, as spec 013's contract already promises for a
  reused id. No row means the insert failed for the reason spec 013 B-3
  handles, and the head question decides between a retry and the store's own
  error. `Ledger::append` keeps its signature and becomes lifetime
  idempotent. `Ledger::append_once` returns `Landing { hash, appended_now,
  sealed_in }` for a caller that must know whether this call was the one
  that appended, which an interrupted delivery needs and a hash alone cannot
  tell it.
- **B-5 (sealing stamps, never deletes).** The transaction that inserts a
  segment header and deletes the archived rows (spec 014 B-2) additionally
  sets `segment_hash` on those records' identity rows. No path in this crate
  deletes an identity row: an id is spent for the life of the chain, and the
  table is the only place that remembers it once the record is archived.
- **B-6 (lookup and recovery).** `Ledger::lookup(id) -> Presence` with
  `Presence::Resident { record_hash }`, `Sealed { record_hash, segment_hash
  }`, `Absent`, and `Unproven { segment_hash: Option<Hash> }`.
  `Ledger::recover(id, &dyn Archive)` fetches exactly the one segment body
  the row names, verifies it the way spec 014 B-4 verifies a body at full
  depth, and returns the `SignedRecord`. A body that is missing, corrupt, or
  unreadable is `Error::NotFound`, `Error::Integrity`, and `Error::Io`
  respectively. None of the three is ever reported as `Absent`: unavailable
  evidence and proven absence are different answers, and conflating them is
  how a duplicate gets written.
- **B-7 (coverage is proven from resident state).** `Absent` is returned
  only when coverage is complete. Coverage is complete when every resident
  record has an identity row and, for every segment header, the number of
  identity rows stamped with that segment's hash equals the header's
  `count`. A segment whose stamped rows fall short, and an unstamped row
  whose id is no longer resident, both mean a run was sealed by a binary
  that does not stamp; that segment is uncovered, and an id with no row is
  then `Unproven` rather than `Absent`. `Ledger::coverage()` reports the
  uncovered segments and the identity row count, `rahi ledger verify` prints
  both, and nothing consults the archive to compute either.
- **B-8 (reindex closes a chain sealed before this spec).**
  `Ledger::reindex(&dyn Archive)` walks the uncovered segments from the
  newest backwards, fetches each body, verifies it at full depth, and writes
  the missing identity rows stamped with that segment's hash, one
  transaction per segment. It is idempotent, resumable after an interruption,
  and bounded by one segment at a time. `rahi ledger reindex` is the
  operator verb. `serve` never runs it: it needs the archive, and a cell
  that cannot reach object storage must still boot, append, and verify at
  resident depth (spec 014 D-3).
- **B-9 (the cost, declared).** This buys lifetime uniqueness with one
  narrow resident row per decision, forever, which is the unboundedness spec
  014 exists to keep out of the replicated store. The trade is deliberate
  and the arithmetic belongs in the spec rather than in a surprise: digests
  are stored as 32-byte blobs, so a row is roughly 160 bytes and a million
  decisions roughly 160 MB on every node. There is no cheaper form that
  still holds. A constraint over a bounded window cannot refuse an id whose
  record has been archived; the archive cannot be consulted atomically with
  an insert; and a monotonic-id watermark would bound the state but cannot
  return the original record hash or compare content, which are two of the
  four things a retry needs. `rahi ledger verify` reports the row count so
  the growth is visible to the operator who is paying for it.

## 4. Functional requirements

- **FR-001.** The reproduced case closes. Append a decision, discard the
  result as a lost acknowledgement, seal it into `FsArchive`, reopen the
  ledger against the same store, and retry the identical decision:
  `append` returns the original record hash, `append_once` reports
  `appended_now = false` and the segment it was sealed into, the chain holds
  one copy of the id, and `verify_chain(Depth::Full)` passes.
- **FR-002.** The concurrent case closes. A retry paused at the seam after
  a completed lookup, while another task appends and seals the same
  decision, resumes and reports the original hash; the chain holds one copy.
  The pause is injected at a test seam, not waited out on a clock.
- **FR-003.** A decision whose id is spent under different content is
  `Error::Conflict` naming the stored hash, whether that record is resident
  or sealed, and no record is written.
- **FR-004.** With a corrupt archived body, `lookup` of a stamped id still
  answers `Sealed` from resident state, `recover` answers
  `Error::Integrity` naming the segment, a removed body answers
  `Error::NotFound`, and an unreadable one answers `Error::Io`. No input
  makes any of them answer `Absent`.
- **FR-005.** A chain written under specs 013 and 014 with two sealed
  segments and no identity rows reports both segments uncovered; `lookup` of
  an archived id answers `Unproven`; `reindex` over a healthy archive makes
  coverage complete; `lookup` then answers `Sealed` for that id and `Absent`
  for an id never used. Interrupting `reindex` between segments and running
  it again converges to the same coverage.
- **FR-006.** `recover` fetches exactly one segment body, asserted by an
  `Archive` that counts its gets.
- **FR-007.** Nothing hashed moves. Spec 013's `testdata/chains/` fixtures
  verify unchanged, and an export of a chain appended under this spec is
  byte-identical to an export of the same decisions appended under spec 013
  with the same signing key, so `attest-ledger verify` accepts it (skipped
  with a message when that binary is absent).
- **FR-008.** A store restored from a snapshot taken before this spec opens
  clean: `open` creates the table and backfills an identity row for every
  resident record before the first append, and the backfill is bounded by
  the hot window and idempotent across reopens.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p rahi-ledger --locked` passes, including the
  existing append, verify, and seal tests unmodified in their assertions.
- **AC-2.** `cargo test -p rahi-cli --locked` passes. `rahi ledger reindex`
  over FR-005's fixture exits 0 and reports complete coverage, and `rahi
  ledger verify` prints the coverage verdict and the identity row count at
  both depths.
- **AC-3.** A chain written by the published 0.1.0 crates and opened under
  this spec verifies at both depths, reports its sealed segments uncovered
  until `reindex` runs, and needs no edit to the archived bodies. An
  operator check at the pin the consumer is on.

## 6. Out of scope

The hiqlite 0.14.0 stale lease release after TTL takeover, and the
unverified full-node restart with a second lease: independent limitations,
neither caused nor fixed by this spec. The application migration history,
restore compatibility, and the ledgered manifest transition (036). Bounding
the identity index below one row per decision, which B-9 prices and a later
spec may revisit if the resident cost proves material. What a consumer does
with an idempotent append: aicortex's erasure receipts and their delivery
are its own corpus's business, and this spec claims no requirement there.

## 7. Resolved decisions

None yet. The build session records D-n entries here for choices this spec
is silent on (date, provenance, the decision, the alternative rejected).

## Verification

```verify:cli
cargo test -p rahi-ledger --locked --test identity
cargo test -p rahi-ledger --locked --test append
cargo test -p rahi-ledger --locked --test seal
cargo test -p rahi-cli --locked --test cli
```
