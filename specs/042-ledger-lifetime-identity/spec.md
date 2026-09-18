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
  - { spec: "013-ledger-decision-chain", unit: "crates/rahi-ledger/testdata/chains/", nature: additive }
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
  happened, because spec 013 D-5 tells a lost compare-and-swap from a
  duplicate id by asking whether the id is resident and spec 014 deletes the
  row when its segment is sealed. Past the hot window a retried append
  therefore writes a second record under the same id, and full verification
  passes because a duplicate id is not a broken link. This spec gives every
  decision a narrow resident identity row that survives sealing, makes that
  row's primary key the arbitration inside the append transaction rather than
  a read before it, stamps the row in the seal transaction, and answers
  presence, proven absence, unproven history and proven ambiguity as four
  different answers. A chain sealed before this spec has no rows for its
  archived ids, so a cell refuses to serve until `rahi ledger reindex` has
  rebuilt them: the upgrade costs a stop, and the alternative is a cell that
  serves while its audit proof is silently wrong.
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
segments stay immutable, and every answer this spec gives is computed from
resident state alone, so a chain still verifies without archived history
resident. What it does not do quietly is pretend that a chain sealed under
spec 014 already carries the state this spec needs. It does not, the missing
state cannot be reconstructed without the archive, and B-10 makes that an
operator's stop rather than a silently wrong answer.

## 2. Territory

One new module in `crates/rahi-ledger`, `identity.rs`, and its integration
test. The module owns the identity table, the collision table, the identity
digest, the lifetime lookup, the coverage accounting, and the reindex walk.

Additive extends on spec 013's `append.rs` (the insert transaction gains the
identity statement and classification reads the identity row), `chain.rs`
(the baseline DDL, the open-time backfill, and the boot coverage gate),
`record.rs` (canonical bytes of a decision with its parent omitted),
`lib.rs` (the exports), `tests/append.rs`, and `testdata/chains/` (the
0.1.0 fixture AC-3 is proved against); on spec 014's `seal.rs` (the seal
transaction stamps the rows it archives) and `tests/seal.rs`; and on spec
030's `rahi-cli` for the `ledger reindex` verb and the coverage lines
`ledger verify` prints.

The consumer contract's limitation paragraph is rewritten by the session that
lands this spec. `docs/` is in the coupling gate's bypass floor, so that edit
needs no edge; the `references` edge above records the dependency in the
other direction.

Two things this spec's Territory deliberately does not contain, because they
belong to owners it may not amend: spec 030 B-1's verb list, which does not
name `ledger reindex` (D-required-1), and spec 014's rationale that the
replicated store holds no history that grows without bound (D-required-2).
Section 8 states both.

## 3. Behavior

- **B-1 (the identity row).** `kernel_decision_identity (id TEXT PRIMARY
  KEY, identity_digest TEXT NOT NULL, record_hash TEXT NOT NULL,
  segment_hash TEXT NULL)`. `Ledger::open` creates it idempotently, as the
  chassis's own baseline rather than an application migration, for the reason
  spec 013 D-4 gives for `kernel_decisions`: the shape is fixed here, no app
  versions it, and a boot that found no table could not tell an empty chain
  from a deleted one. Hashes are stored as the lowercase hex text
  `Hash::as_str` yields, the encoding `kernel_decisions.hash` and
  `kernel_segments.segment_hash` already use; B-9 prices that choice against
  a 32-byte blob and says why the crate's existing convention wins.
- **B-2 (the identity digest).** `identity_digest` is sha256 over the
  canonical bytes of the `Decision` with `prev_hash` omitted. Every other
  field is covered: id, kind, actor, capability, outcome, reason, payload,
  and `at`. The parent is excluded because a retry re-chains onto whatever
  head it finds, so the parent is the one field an honest retry is expected
  to change. No hashed byte of a record, a segment, or an export changes:
  the digest lives in a resident table and enters no envelope.
- **B-3 (one transaction writes both).** The record insert and its identity
  insert commit in one `txn`, the genesis record spec 013 B-4 writes
  included. `StoreHandle::txn` is one Raft log operation inside one SQLite
  transaction, so the pair is atomic and is ordered against every other
  write in this crate by the Raft log itself. The identity table's primary
  key MUST be the arbitration for a duplicate id, exactly as the unique
  parent index is the arbitration for a lost compare-and-swap: `append`
  never gates on a read, because a read before a write is not atomic against
  another node's append or another task's seal, which is what the second
  reproduction above demonstrates. Constitution IX and XI both hold
  unchanged.
- **B-4 (classification is lifetime-scoped).** After a failed insert,
  classification reads the identity row rather than `kernel_decisions`. A row
  whose `identity_digest` equals this decision's means the decision is
  already in the chain: return the stored `record_hash`, whether the record
  is resident or sealed. A row with a different digest is `Error::Conflict`
  naming the stored hash, as spec 013's contract already promises for a
  reused id. No row means either the insert failed for the reason spec 013
  B-3 handles, which the head question decides, or that coverage is
  incomplete, which B-11 decides first. `Ledger::append` keeps its signature
  and becomes lifetime idempotent on a covered chain.
- **B-5 (sealing stamps, never deletes).** The transaction that inserts a
  segment header and deletes the archived rows (spec 014 B-2) additionally
  writes `segment_hash` onto those records' identity rows, as an upsert
  computed from the record bodies the seal already holds: `INSERT INTO
  kernel_decision_identity (...) VALUES (...) ON CONFLICT(id) DO UPDATE SET
  segment_hash = excluded.segment_hash`. An upsert rather than an update,
  because a record appended by a binary that does not stamp has no row to
  update, and a seal that failed on it would make the first post-upgrade
  seal a hard stop; the upsert instead closes that record's coverage from
  the body it is archiving. The transaction asserts that it touched exactly
  `count` identity rows and is `Error::Integrity` otherwise, as it already
  asserts the deletion count. No path in this crate deletes an identity row:
  an id is spent for the life of the chain, and the table is the only place
  that remembers it once the record is archived.
- **B-6 (lookup and recovery).** `Ledger::lookup(id) -> Presence` with
  `Presence::Resident { record_hash }`, `Sealed { record_hash, segment_hash
  }`, `Absent`, `Unproven { segment_hash: Option<Hash> }`, and `Ambiguous {
  copies: Vec<Copy> }` (B-12). `Ledger::recover(id, &dyn Archive)` fetches
  exactly the one segment body the row names, verifies it the way spec 014
  B-4 verifies a body at full depth, and returns the `SignedRecord`. A body
  that is missing, corrupt, or unreadable is `Error::NotFound`,
  `Error::Integrity`, and `Error::Io` respectively. None of the five answers
  is ever produced from unavailable evidence: `Absent` requires complete
  coverage, `Ambiguous` requires two recorded copies, and an archive failure
  is an error rather than any of them. Conflating unavailable evidence with
  proven absence is how a duplicate gets written.
- **B-7 (coverage is proven from resident state).** Coverage is complete
  when every resident record has an identity row and, for every segment
  header, the number of identity rows stamped with that segment's hash
  equals the header's `count`. A segment whose stamped rows fall short, and
  an unstamped row whose id is no longer resident, both mean a run was
  sealed by a binary that does not stamp; that segment is uncovered.
  `Ledger::coverage() -> Coverage` reports the uncovered segment hashes, the
  count of resident records with no identity row, the identity row count,
  and the collision count, and reads through the leader
  (`query_consistent`), because `append`'s admission gate and `serve`'s boot
  gate both turn on it and a stale local read would answer both wrongly
  (spec 011 B-3). `rahi ledger verify` prints all four numbers and nothing
  consults the archive to compute any of them.
- **B-8 (reindex closes a chain sealed before this spec).**
  `Ledger::reindex(&dyn Archive) -> ReindexReport` walks the uncovered
  segments from the newest backwards, fetches each body, verifies it at full
  depth, and writes the missing identity rows stamped with that segment's
  hash, one transaction per segment. Each write is an insert that does
  nothing on conflict, so reindex is idempotent, is resumable after an
  interruption, converges under two reindexers on the same segment, and
  never overwrites a row another transaction wrote. It is bounded by one
  segment at a time. What it does when a body names an id some other body or
  some resident record already spent is B-12. `rahi ledger reindex` is the
  operator verb. `serve` never runs it: it needs the archive, and B-10 makes
  the cell refuse to start rather than run an archive-dependent repair on a
  boot path (spec 014 D-3).
- **B-9 (the cost, declared).** This buys lifetime uniqueness with one
  narrow resident row per decision, forever, which is the growth spec 014
  exists to keep out of the replicated store. The trade is deliberate and
  the arithmetic belongs in the spec rather than in a surprise. A row holds
  an id, two hex hashes, and a nullable third. Hashes are hex text rather
  than 32-byte blobs (B-1): a blob would save 32 bytes per hash but would
  make this the only table in the crate whose hash column does not round-
  trip through `Hash::parse`, and the saving does not change the order of
  magnitude. At a 36-byte decision id, hex hashes, and SQLite's separate
  index B-tree for a `TEXT PRIMARY KEY` (the id is stored twice), a row
  costs roughly 300 bytes including per-row and page overhead, so a million
  decisions is roughly 300 MB. That figure is **per replica**, because
  hiqlite replicates the whole database; at the N=3 of spec 032 it is
  roughly 900 MB of disk across the cluster. It is additionally carried in
  every store snapshot and therefore in every `rahi backup` artifact (spec
  030 B-5) and every restore. There is no cheaper form that still holds.
  Rejected alternatives: a constraint over a bounded window, which cannot
  refuse an id whose record has been archived; consulting the archive inside
  the insert, which is not atomic and reintroduces the reproduced defect; a
  monotonic-id watermark, which bounds the state but cannot return the
  original record hash or compare content, two of the four things a retry
  needs; dropping `record_hash` and recomputing it from the archived body,
  which halves the row but makes answering a retry require the archive and
  so destroys B-6's promise that `Sealed` is answerable from resident state;
  and a probabilistic filter over spent ids, which cannot return a hash and
  whose false positives would refuse ids that were never used. `rahi ledger
  verify` reports the row count so the growth is visible to the operator who
  is paying for it. Whether that growth is compatible with spec 014's stated
  reason for existing is an owner's question, not this spec's: section 8
  states it.
- **B-10 (a chain this spec cannot vouch for does not serve).** `Ledger::
  open` computes coverage after its backfill. Incomplete coverage is
  `Error::Stale` (exit 2) naming the uncovered segment count and the command
  `rahi ledger reindex`, never `Error::Integrity`: an unreindexed chain is a
  missing upgrade step, not tampering, the same distinction spec 036 B-4
  draws for an unadopted manifest. `serve` therefore refuses to start, and
  `preflight` reports coverage as a named check and fails on it. The
  enforcement point is the boot rather than the append because a denial
  whose append fails is counted as a lost denial (spec 035 B-3,
  `Cause::Failed`) and the cell keeps serving: an append-only gate would
  turn a visible refusal into a cell that serves while its audit proof
  silently stops recording, which constitution XI calls worse than a cell
  that is down. This is the one place this spec makes a cell less available
  than spec 014 D-3 left it, and B-13 states the boundary of that cost.
- **B-11 (the append backstop).** Coverage can still degrade under a running
  cell, because a replica on a binary that does not stamp can append an
  unstamped record after this node booted. So `append` keeps a backstop:
  when coverage is incomplete, an id with no identity row is
  `Error::Conflict` naming the uncovered segments and the reindex command,
  because the chain cannot prove that id is free; an id whose row's digest
  matches is still the verified retry of B-4 and returns the stored hash;
  and an id whose row's digest differs is still `Error::Conflict`. Refusing
  the unknown and admitting the verified is what keeps a recovery working
  through an incident that a full refusal would strand. The backstop is a
  second line, not the first: on a cluster upgraded the way B-13 requires it
  is never reached.
- **B-12 (an id already spent twice is reported, never resolved).** Reindex
  and backfill can meet an id that history already spent more than once,
  which is the defect on a chain written before this spec. The two cases are
  both recorded and neither is decided. When a body or a resident record
  names an id that already has an identity row, the walk compares: an equal
  `identity_digest` and an equal `record_hash` is the idempotent repeat and
  writes nothing; anything else is a collision, and every copy beyond the
  first is written to `kernel_decision_collisions (id TEXT, record_hash TEXT,
  segment_hash TEXT NULL, identity_digest TEXT NOT NULL, PRIMARY KEY (id,
  record_hash))`. No archived body is read for anything but comparison, no
  archived byte is written, and no identity row is overwritten, so immutable
  history is preserved exactly as spec 014 B-5 requires. The identity row
  that happens to stand is the one the walk reached first and is not a
  winner: `lookup` of a colliding id answers `Ambiguous` carrying every copy
  and never `Resident`, `Sealed` or `Absent`; `append` of a colliding id is
  `Error::Conflict` naming every copy, because no retry can be verified
  against a history that spent the id twice; `coverage()` counts the
  collisions and `rahi ledger verify` prints them and exits 1
  (`Error::Conflict`, spec 030 B-1's mapping). `verify_chain` at either
  depth still passes: a duplicated id is not a broken link, and this spec
  does not change what spec 013 and 014 mean by an intact chain. Resolving a
  collision is an owner's act on the consuming application's own terms and
  is out of scope here.
- **B-13 (the upgrade is controlled, and what that costs).** A cluster is
  upgraded to a binary that stamps by stopping every replica before starting
  any replica on the new binary, then running `rahi ledger reindex` against
  the archive while nothing appends, then starting the cluster. It is a
  stop-the-world upgrade, not a rolling one, and it is enforced by what the
  new binary refuses rather than by anything the old binary can be made to
  do: an already published binary cannot be given a check it was built
  without, so no mixed-version guarantee is offered and none should be
  claimed. What the new binary enforces is that a chain it cannot vouch for
  does not serve (B-10) and that an id it cannot prove free is not appended
  (B-11), so a mixed cluster degrades to a visible refusal rather than to a
  wrong answer. The downtime is one full cluster stop plus one reindex: one
  archive fetch and one full-depth verification per uncovered segment, so it
  scales with archived history divided by `segment_size` and is bounded by
  archive latency rather than by the store. A chain with no sealed segments
  needs no reindex and its downtime is the restart alone. Rolling back to a
  binary that does not stamp is a one-way door in the other direction: the
  old binary appends unstamped records, coverage breaks again, and returning
  to the new binary requires another stop and another reindex. Spec 036 B-10
  governs rolling updates and refuses an old image after a manifest
  transition; whether this upgrade should ride that mechanism, so that 036
  B-4 does the refusing, is an owner's sequencing decision that section 8
  states and this spec does not take.
- **B-14 (what `append_once` knows, and what it cannot).** `Ledger::
  append_once(decision) -> Landing` with `Landing { hash, appended_now:
  bool, sealed_in: Option<Hash> }`. The three fields answer different
  questions and only the first is a total guarantee.
  - `hash` is **durable presence**: after any successful return the decision
    is in the chain exactly once and `hash` names it. This holds across lost
    acknowledgements, retries, process restarts, and replicas, and it is the
    whole of what this spec guarantees to a caller.
  - `appended_now` is **knowledge about this invocation**, not about the
    decision. `true` means this invocation's transaction committed and this
    invocation observed the commit. It is sound and it is not complete:
    `false` means only that the decision was already present when this
    invocation looked, and an earlier invocation *by the same caller* whose
    acknowledgement was lost is one of the ways that happens. A lost
    acknowledgement destroys the knowledge of who appended permanently, and
    no interface can return it, because the fact was never recorded
    anywhere. After an error return the caller knows nothing at all: the
    transaction may or may not have committed, and the only way to find out
    is to ask again, whereupon the answer is presence and never authorship.
  - `sealed_in` names the segment when the decision is already archived.
  This is exactly-once **append**, not exactly-once **delivery**. A caller
  that fires a side effect on `appended_now == true` will skip that side
  effect after a lost acknowledgement, because its retry sees `false`.
  `appended_now` is therefore a diagnostic and MUST NOT be a delivery
  trigger; a caller that needs a side effect to happen exactly once records
  its intent in its own transaction and drives the effect from that durable
  row, which is the pattern the consumer contract already documents. This
  spec claims nothing about what happens downstream of the chain.

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
  clean when nothing was sealed: `open` creates the tables and backfills an
  identity row for every resident record before the first append, the
  backfill is bounded by the hot window, is idempotent across reopens, and
  inserts nothing on conflict so that a concurrent append on another replica
  and the backfill converge.
- **FR-009 (the boot gate, B-10).** The FR-005 fixture makes `Ledger::open`
  return `Error::Stale` naming the uncovered segment count and the reindex
  command; the same fixture drives `rahi serve` to exit 2 with that command
  in its output and `rahi preflight` to exit 1 with coverage named as the
  failing check. After `reindex`, both start. A chain with no sealed
  segments and no identity rows backfills and opens without an archive.
- **FR-010 (the append backstop, B-11).** On an uncovered chain reached
  directly rather than through `open`, an append of an id with no identity
  row is `Error::Conflict` naming the reindex command and writes no record;
  an append of an id whose row's digest matches returns the stored hash; an
  append of an id whose row's digest differs is `Error::Conflict`. The
  refusal names the uncovered segments.
- **FR-011 (transaction-level enforcement, B-3 and B-5).** A test asserts
  that the append transaction carries both statements and that neither lands
  alone: an injected failure of the identity insert leaves no
  `kernel_decisions` row, and an injected failure of the record insert
  leaves no identity row. A test asserts the seal transaction upserts
  exactly `count` identity rows, including for records that had none, and
  that a count mismatch is `Error::Integrity`. A test runs a backfill and an
  append concurrently and asserts one identity row per id and no error from
  either.
- **FR-012 (reindex meets a reused id, B-12).** A fixture archive holding
  two segments that each contain a record with the same id reindexes without
  error, writes one identity row and one collision row, leaves both archived
  bodies byte-identical, and reports one collision. `lookup` of that id
  answers `Ambiguous` carrying both copies; `append` of it is
  `Error::Conflict` naming both; `verify_chain` passes at both depths; `rahi
  ledger verify` prints one collision and exits 1. The same fixture with
  identical content under one id (same `identity_digest`, different
  `record_hash`) produces the same outcome, and a genuine idempotent repeat
  (same digest and same record hash, reached twice by an interrupted
  reindex) produces no collision row.
- **FR-013 (`append_once` under a lost acknowledgement, B-14).** A first
  `append_once` whose return value is discarded, followed by a second
  `append_once` of the identical decision, yields the same `hash`,
  `appended_now = false` on the second, and one copy in the chain. The test
  asserts durable presence and asserts that the crate documents
  `appended_now` as knowledge about the invocation; it asserts nothing about
  authorship, which is not recoverable.
- **FR-014 (the published-0.1.0 proof is reproducible).** A committed
  fixture under `crates/rahi-ledger/testdata/chains/v0.1.0-sealed/` holds a
  chain written by the published 0.1.0 crates: its signing key, its genesis
  parent, its resident rows as a store fixture, its archived segment bodies,
  and `version.txt` recording the exact `rahi-ledger` version resolved when
  it was written. A sibling `write.sh` in the same directory rebuilds it in
  a temporary workspace that depends on `rahi-ledger = "=0.1.0"` from
  crates.io, in the spirit of spec 013 D-7's `RAHI_LEDGER_FIXTURES=write`,
  so the fixture is auditable beside the chains it joins. The tests that
  consume it live in `crates/rahi-ledger/tests/identity.rs` and
  `crates/rahi-cli/tests/cli.rs`, both of which the Verification block runs,
  so AC-3 is exercised by the declared commands and not by an operator's
  word. They skip with a message only when the fixture directory is absent,
  never when crates.io is unreachable: the fixture is committed, and the
  script is only how it was made.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p rahi-ledger --locked` passes, including the
  existing append, verify, and seal tests unmodified in their assertions.
- **AC-2.** `cargo test -p rahi-cli --locked` passes. `rahi ledger reindex`
  over FR-005's fixture exits 0 and reports complete coverage, and `rahi
  ledger verify` prints the uncovered segment count, the unstamped resident
  count, the identity row count, and the collision count at both depths.
- **AC-3.** The committed `v0.1.0-sealed` fixture of FR-014, opened under
  this spec, reproduces the whole migration in one test run: `open` returns
  `Error::Stale` naming the reindex command; `verify_chain` passes at both
  depths before and after; `rahi ledger verify` reports the sealed segments
  uncovered; `rahi ledger reindex` exits 0; `open` then succeeds; `lookup`
  of an archived id answers `Sealed`; and every archived body is
  byte-identical to the committed fixture afterwards. The run is the proof;
  no step is an operator check taken on faith.
- **AC-4.** The boot gate and the backstop hold: FR-009 and FR-010 pass, and
  `rahi serve` against an uncovered chain exits 2 with the reindex command
  in its output.
- **AC-5.** The guarantee survives concurrency: FR-002 and FR-011 pass, and
  no test in this spec's suite establishes its result with a process-local
  lock or a pre-read.
- **AC-6.** Ambiguity is reported and never resolved: FR-012 passes, and the
  archived bodies of its fixture are byte-identical before and after
  `reindex`, asserted by digest.
- **AC-7.** `append_once`'s contract is documented as B-14 states it, FR-013
  passes, and neither the crate documentation nor the consumer contract
  claims exactly-once delivery.

## 6. Out of scope

The hiqlite 0.14.0 stale lease release after TTL takeover, and the
unverified full-node restart with a second lease: independent limitations,
neither caused nor fixed by this spec. The application migration history,
restore compatibility, and the ledgered manifest transition (036). Bounding
the identity index below one row per decision, which B-9 prices and a later
spec may revisit if the resident cost proves material. Resolving a collision
B-12 reports: which copy of a twice-spent id is the real decision is a
question about the consuming application's semantics, not about the chain.
Any guarantee across a mixed-version cluster, which B-13 declines to offer.
What a consumer does with an idempotent append: aicortex's erasure receipts
and their delivery are its own corpus's business, and B-14 is explicit that
this spec claims no requirement there.

## 7. Resolved decisions

None yet. The build session records D-n entries here for choices this spec
is silent on (date, provenance, the decision, the alternative rejected).

## 8. Owner decisions this draft requires before approval

These are not this spec's to take. Each names a requirement owned by another
spec that this design needs changed or explicitly carved out. The coherence
guard forbids a build session resolving any of them by editing the owning
spec, and none is a waiver.

- **D-required-1 (spec 030 B-1's verb list).** B-8 adds `rahi ledger
  reindex`. Spec 030 B-1 enumerates nine verbs and its AC-2 requires
  `--help` to list *exactly* those. The existing test compares the help
  output against the `VERBS` constant rather than against B-1's text, so
  adding a tenth entry keeps the test green while making 030 AC-2 false:
  green gate, drifted corpus. Two resolutions exist and the owner picks
  one. Amend 030 B-1 to name `ledger reindex`, which is the honest form and
  costs an amendment to a `complete` spec; or express the operation as
  `rahi ledger verify --reindex <archive>`, a flag on a verb B-1 already
  names, which amends nobody and is the resolution the coherence guard
  prefers when a mechanism honors both texts. This draft is written for the
  verb because B-8 wants an operator verb that cannot be confused with a
  read-only check; if the owner declines the amendment, B-8 and the
  Verification block take the flag form with no other change.
- **D-required-2 (spec 014's reason for existing).** Spec 014's summary
  says the replicated store cannot hold an audit history that grows without
  bound, and its Purpose says unboundedness is a property only of the tail.
  B-9 puts a table in the replicated store that grows one row per decision
  forever. No B-n, FR, AC or constitution article forbids it, so this is not
  a gate failure and not a waiver; it is a contradiction between this
  design and the stated rationale of the spec it extends. The owner decides
  whether 014's rationale binds later specs, and if it does, whether 042 is
  a carve-out recorded in 014 or must find a bounded mechanism B-9 argues
  does not exist. Nothing in this draft amends 014.
- **D-required-3 (the availability promise of B-8's predecessor).** Spec 014
  D-3 reasons that a cell with no object storage configured must still open,
  append, and verify, which is why the archive is an argument rather than a
  field. B-10 suspends that for exactly one population: a chain with sealed
  segments and no identity rows, between the upgrade and the reindex, cannot
  boot without the archive. Every chain appended entirely under this spec
  keeps 014 D-3's promise intact. The owner decides whether that one-time
  suspension is acceptable or whether `serve` should instead offer an
  explicit `--allow-uncovered` start, in which case the cell serves with
  B-11's backstop refusing unknown ids and `kernel_decisions_lost` rising.
  This draft recommends against the escape hatch: it trades a visible stop
  for an invisible audit gap, which is the trade constitution XI exists to
  refuse.
- **D-required-4 (sequencing against 036).** Spec 036 B-4 rewrites
  `Ledger::open` and B-5 changes what a segment header carries; this spec
  changes both. The overlap is on shared files, not a dependency: 042
  `depends_on` 013, 014 and 030, all `complete`, so it is buildable without
  036. The owner decides the order. This draft states no claim on the
  backlog.

## Verification

```verify:cli
cargo test -p rahi-ledger --locked --test identity
cargo test -p rahi-ledger --locked --test append
cargo test -p rahi-ledger --locked --test seal
cargo test -p rahi-ledger --locked --test verify
cargo test -p rahi-cli --locked --test cli
```
