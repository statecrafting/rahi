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
  - { spec: "013-ledger-decision-chain", unit: "crates/rahi-ledger/src/verify.rs", nature: additive }
  - { spec: "013-ledger-decision-chain", unit: "crates/rahi-ledger/src/lib.rs", nature: additive }
  - { spec: "013-ledger-decision-chain", unit: "crates/rahi-ledger/tests/append.rs", nature: additive }
  - { spec: "013-ledger-decision-chain", unit: "crates/rahi-ledger/tests/common/", nature: additive }
  - { spec: "013-ledger-decision-chain", unit: "crates/rahi-ledger/testdata/chains/", nature: additive }
  - { spec: "014-ledger-sealing-and-archive", unit: "crates/rahi-ledger/src/seal.rs", nature: additive }
  - { spec: "014-ledger-sealing-and-archive", unit: "crates/rahi-ledger/src/segment.rs", nature: additive }
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
  serves while its audit proof is silently wrong. The refusal, the reindex's
  behavior on damaged or ambiguous history, the cutover from a binary that
  does not stamp, and the permanent resident cost are all stated here rather
  than discovered in production.
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

The defect is reproduced, not theorized, and the reproductions are the
consumer's, running against the published 0.1.0 chassis. A consumer appended
a decision, lost the receipt acknowledgement, sealed genesis and that
decision through `Ledger::seal_if_needed` into `FsArchive`, reopened the same
durable node and retried: two lifetime copies, identical in every field but
the assigned parent. A second reproduction defeats every pre-read repair.
Pause a retry after a complete and verified negative lookup, append and seal
the same decision in another task, then resume the retry: both appends
succeed at different hashes, so an archive scan or a segment count taken
before the insert closes nothing. A third records what a damaged archive
does: missing, corrupt and unreadable bodies answer `NotFound`, `Integrity`
and `Io`, and a resident-only lookup reports absence in all three, which is
the conflation this spec exists to remove. A process-local lock cannot cover
another node, and an application attempt marker can refuse a recovery it is
unsure of but cannot make the chassis's own append atomic with sealing.
Section 2.0 of the consumer contract records the limitation and says atomic
lifetime idempotence and its migration need a separately governed design.
This spec is that design.

Those three reproductions are load-bearing and this spec inherits them rather
than restating them: they are the consumer's `pinned_archived_retry_
duplicates_after_reopen`, `pinned_verified_lookup_does_not_fence_concurrent_
append_and_seal` and `pinned_archive_failure_is_not_proven_absence`, which
pass today because they assert the defect. FR-001, FR-002 and FR-004 are the
chassis-side transcriptions of the same three scenarios with the assertions
inverted, and they are red until this spec is implemented. The consumer's
diagnostics stay as they are until a chassis release carries this behavior;
flipping them to positive assertions is the consumer's work on the consumer's
own corpus, not this spec's, and nothing here asks for them to be deleted.

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
test. The module owns the identity table, the collision table, the per
segment coverage counters, the identity digest, the lifetime lookup, the
coverage accounting, and the reindex walk.

Additive extends on spec 013's `append.rs` (the insert transaction gains the
identity statement and classification reads the identity row), `chain.rs`
(the baseline DDL, the open-time backfill, the boot coverage gate, and the
repair open of B-15), `record.rs` (canonical bytes of a decision with its
parent omitted), `verify.rs` (the per-segment verification seam reindex
needs), `lib.rs` (the exports), `tests/append.rs`, `tests/common/`, and
`testdata/chains/` (the 0.1.0 fixture AC-3 is proved against); on spec 014's
`seal.rs` (the seal transaction stamps the rows it archives), `segment.rs`
(reading and verifying one archived body on its own) and `tests/seal.rs`;
and on spec 030's `rahi-cli` for the `ledger reindex` verb and the coverage
lines `ledger verify` prints.

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

- **B-1 (the resident state).** Three tables, created idempotently by
  `Ledger::open` as the chassis's own baseline rather than an application
  migration, for the reason spec 013 D-4 gives for `kernel_decisions`: the
  shape is fixed here, no app versions it, and a boot that found no table
  could not tell an empty chain from a deleted one.
  - `kernel_decision_identity (id TEXT PRIMARY KEY, identity_digest TEXT NOT
    NULL, record_hash TEXT NOT NULL, segment_hash TEXT NULL)`, with
    `INDEX kernel_decision_identity_segment (segment_hash)`, which every
    coverage repair and every per-segment count reads.
  - `kernel_decision_collisions (id TEXT, record_hash TEXT, segment_hash TEXT
    NULL, identity_digest TEXT NOT NULL, PRIMARY KEY (id, record_hash))`,
    with `INDEX kernel_decision_collisions_segment (segment_hash)`, which the
    coverage recomputation reads beside the identity index (B-12).
  - `kernel_decision_coverage (segment_hash TEXT PRIMARY KEY, stamped INTEGER
    NOT NULL)`: how many of that segment's records are **accounted for**,
    which is the number of identity rows carrying its hash plus the number of
    collision rows carrying its hash (B-7). It is never incremented: it is
    written only by being recomputed from those two tables inside the
    transaction that wrote them, so it cannot drift from them. It is a cache
    of a count, never of a fact: every value in it is recomputable from the
    two tables, and FR-018 asserts that.

  Hashes are stored as the lowercase hex text `Hash::as_str` yields, the
  encoding `kernel_decisions.hash` and `kernel_segments.segment_hash` already
  use; B-9 prices that choice against a 32-byte blob and says why the crate's
  existing convention wins.
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
  reused id. Classification reads that id's collision rows in the same
  query, and an id carrying any collision row is `Error::Conflict` naming
  every copy whatever the surviving identity row's digest says (B-12): on a
  twice-spent id, a digest that matches one copy proves nothing about which
  copy a retry meant. No row of either kind means either the insert failed
  for the reason spec 013 B-3 handles, which the head question decides, or that coverage is
  incomplete, which B-11 decides first. `Ledger::append` keeps its signature
  and becomes lifetime idempotent on a covered chain.
- **B-5 (sealing stamps, never deletes, and never redirects).** The
  transaction that inserts a segment header and deletes the archived rows
  (spec 014 B-2) additionally accounts for every record it archives. It
  cannot decide anything by reading first: `StoreHandle::txn` takes a batch
  of statements and offers no read between them, so each record's accounting
  is two self-arbitrating statements, in the same spirit as B-3's insert.

  The first is a **conditional** upsert, which may create a row but may
  never redirect one:

  ```sql
  INSERT INTO kernel_decision_identity (id, identity_digest, record_hash,
                                        segment_hash)
  VALUES (?id, ?digest, ?record_hash, ?segment_hash)
  ON CONFLICT(id) DO UPDATE SET segment_hash = excluded.segment_hash
    WHERE kernel_decision_identity.record_hash = excluded.record_hash
      AND kernel_decision_identity.segment_hash IS NULL
  ```

  An insert rather than a plain update, because a record appended by a
  binary that does not stamp has no row to update, and a seal that failed on
  it would make the first post-upgrade seal a hard stop; the insert instead
  closes that record's coverage from the body it is archiving, and the
  number of rows it had to create that way is B-13's detector for a writer
  that does not stamp. The `WHERE` is what makes the upsert safe on a chain
  that already spent an id twice. An existing row whose `record_hash` names
  a *different* record is B-12's collision, and stamping it would point that
  identity row at a segment which does not contain the record it names:
  `lookup` would then answer `Sealed { record_hash, segment_hash }` with a
  pair no body satisfies, `recover` would fetch the wrong segment and report
  `Integrity` on a healthy archive, and the segment that does hold the named
  record would silently lose a row from its count. The `WHERE` makes that
  statement a no-op instead. An already-stamped row is left alone for the
  same reason, which also makes a repeated seal idempotent.

  The second statement accounts for whatever the first one declined, and
  writes nothing when the first one accounted for the record:

  ```sql
  INSERT INTO kernel_decision_collisions (id, record_hash, segment_hash,
                                          identity_digest)
  SELECT ?id, ?record_hash, ?segment_hash, ?digest
  WHERE NOT EXISTS (SELECT 1 FROM kernel_decision_identity
                    WHERE id = ?id AND record_hash = ?record_hash
                      AND segment_hash = ?segment_hash)
  ON CONFLICT(id, record_hash) DO NOTHING
  ```

  Statements run in order inside one transaction, so this one sees the
  first one's effect: a record the upsert inserted or stamped is already
  accounted for and no collision row is written; a record it declined gets
  one. Every archived record therefore ends accounted for exactly once,
  in one table or the other, which is the invariant B-7 counts and AC-12
  asserts.

  The transaction's last statement recomputes the segment's counter from
  those two tables rather than incrementing anything:

  ```sql
  INSERT INTO kernel_decision_coverage (segment_hash, stamped)
  VALUES (?segment_hash,
          (SELECT COUNT(*) FROM kernel_decision_identity
            WHERE segment_hash = ?segment_hash)
        + (SELECT COUNT(*) FROM kernel_decision_collisions
            WHERE segment_hash = ?segment_hash))
  ON CONFLICT(segment_hash) DO UPDATE SET stamped = excluded.stamped
  ```

  so the counter cannot drift from the rows it counts, however the
  statements above resolved, and seal and reindex (B-8) reach the same
  number for the same rows because they run the same statement. The seal
  then asserts the written `stamped` equals the header's `count` and is
  `Error::Integrity` naming the segment otherwise, in the same form as the
  existing deletion-count assertion: that assertion is a **post-commit
  detection**, not a rollback, because `txn` commits the batch before any
  result is inspected. What the error reports is that the segment is not
  fully accounted for, which is exactly the uncovered state B-7 reports and
  B-8 repairs, so the detection hands the operator a repairable condition
  rather than a half-written one. This spec claims no rollback it cannot
  perform.

  No path in this crate deletes an identity row or a collision row: an id is
  spent for the life of the chain, and those two tables are the only place
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
  proven absence is how a duplicate gets written, and it is what the
  consumer's third reproduction records the current chassis doing.
- **B-7 (coverage is proven from resident state, and is cheap to prove).**
  Coverage is an accounting over **records**, not over ids, because an id can
  name more than one record on a chain written before this spec. A record is
  *accounted for* when the resident state holds exactly one row naming it: an
  identity row whose `id` and `record_hash` are its own, or a collision row
  under that same `(id, record_hash)` key (B-12). Coverage is complete when
  every resident record is accounted for and, for every row of
  `kernel_segments`, `kernel_decision_coverage` holds a `stamped` equal to
  that header's `count`, where `stamped` is the recomputed number of identity
  *and* collision rows carrying that segment's hash (B-1, B-5). Counting
  records rather than ids is what lets a segment holding a twice-spent id
  still reach its header's `count`, which is what B-12 relies on when it lets
  an ambiguous chain serve: the surviving copy is an identity row, every
  further copy is a collision row, and all of them are counted. The two rules
  are one rule, and AC-12 asserts it directly. Both halves are answerable
  without the archive and without scanning the identity table: the resident
  half is bounded by the hot window (spec 014 B-1, default 10,000 rows) and
  the sealed half is one row per segment. A segment with no counter row, or
  a counter below its header's `count`, is uncovered: it was sealed by a
  binary that does not stamp, or a reindex over it has not finished.
  - `Ledger::coverage() -> Coverage` reports the uncovered segment hashes,
    the count of resident records that are not accounted for (no identity row
    and no collision row naming them), the identity row count, and the
    collision count, and reads through the leader
    (`query_consistent`, spec 011 B-3) because it is the input to a refusal.
  - It is evaluated at `open`, by `Ledger::recheck_coverage()`, and by
    `rahi ledger verify` and `rahi preflight`. It is **not** evaluated per
    append: a leader round trip inside every append would make the audit
    path the slowest path in the cell. `append` reads a cached verdict that
    `open` and `recheck_coverage` set, and that only this node's own
    observations can move (B-11).
  - `rahi ledger verify` prints all four numbers at both depths, and nothing
    consults the archive to compute any of them.
- **B-8 (reindex closes a chain sealed before this spec, or says why it
  cannot).** `Ledger::reindex(&dyn Archive) -> ReindexReport` walks the
  uncovered segments from the newest backwards. For each one it fetches the
  body, verifies it at full depth, and only then writes, in one transaction
  per segment: the accounting for every record in the body, by exactly
  the three statements B-5 uses and in the same order, so seal and reindex
  are one accounting rather than two that must be kept agreeing. The
  conditional upsert stamps a row that names the same record and was not yet
  stamped, inserts one where none exists, and declines to redirect one that
  names a different record; the collision statement catches whatever the
  upsert declined; and the counter for that segment is recomputed from both
  tables. Reindex is therefore idempotent, is resumable after an
  interruption, converges under two reindexers on the same segment, and
  never overwrites, redirects, or deletes a row another transaction wrote.
  The unstamped-row case is why reindex needs the upsert rather than a plain
  insert-or-ignore: a record backfilled while resident (FR-008) and then
  sealed by a replica that does not stamp leaves a row that exists but
  carries no segment, and an insert-or-ignore would skip it and leave that
  segment permanently one short of its `count`.

  What it does with damaged or contradictory history is the point of this
  behavior, and in every case it is to fail visibly and continue rather than
  to choose:
  - A body that is **missing**, **unreadable**, or **fails verification** is
    never used as a source of identity rows. The segment stays uncovered,
    the report records it under its cause (`NotFound`, `Io`, `Integrity`),
    the walk continues to the remaining segments, and the verb exits with
    the most severe of the causes it collected. Coverage is not complete and
    is never reported as complete.
  - A body whose record count disagrees with its header's `count`, or whose
    recomputed `segment_hash` differs from the header's, is `Integrity` for
    that segment by the same rule. This spec adds no repair for a segment
    header and a body that disagree.
  - A body that names an id already carried by an identity row, or twice
    within itself, is B-12: recorded as a collision, never resolved.
  - A **partially covered** segment (some rows present from an earlier run,
    some missing) is completed, because the writes are insert-or-ignore and
    the counter is recomputed rather than incremented.
  - A segment whose counter already equals its header's `count` is skipped
    without an archive fetch.

  `ReindexReport` carries, per segment, whether it is now covered and why not
  when it is not, plus the totals: segments walked, rows written, collisions
  recorded, segments left uncovered by cause. `rahi ledger reindex` prints
  it, exits 0 only when every segment ended covered and no collision was
  recorded, and otherwise exits on the most severe error it collected
  (`Error::exit_code`, spec 030 B-1). A report that reaches the operator
  saying "complete" while a body was unreadable is the one outcome this
  behavior exists to make impossible.

  `serve` never runs reindex: it needs the archive, and B-10 makes the cell
  refuse to start rather than run an archive-dependent repair on a boot path
  (spec 014 D-3). Reindex opens the ledger through B-15's repair open, so it
  is not blocked by the gate it exists to clear.
- **B-9 (the cost, declared and priced).** This buys lifetime uniqueness with
  one narrow resident row per decision, forever, which is the growth spec 014
  exists to keep out of the replicated store. The trade is deliberate and the
  arithmetic belongs in the spec rather than in a surprise.

  *Per row.* An id, two hex hashes, and a nullable third. Hashes are hex text
  rather than 32-byte blobs (B-1): a blob would save 32 bytes per hash but
  would make this the only table in the crate whose hash column does not
  round-trip through `Hash::parse`, and the saving does not change the order
  of magnitude. `DecisionId` is a caller-supplied string, so the id's length
  is the consumer's choice; the figures below take 36 bytes, the shape a UUID
  or a prefixed ULID has, and a longer id costs proportionally more. Three
  B-trees carry each row, not one: the table (about 240 bytes of payload plus
  per-row header), the `TEXT PRIMARY KEY` index, which stores the id a second
  time (about 50 bytes), and the `segment_hash` index (about 75 bytes). With
  B-tree page slack the total is about **400 bytes per decision**, roughly a
  third of which is index rather than data. The `kernel_decision_coverage`
  table is one row per segment, about 100 bytes, which at spec 014's default
  `segment_size` of 1,000 is 0.1 bytes per decision and rounds away.

  *Per cluster, and everywhere the store is copied.* The figures below are
  estimates derived from the per-row arithmetic above, not measurements of an
  implementation that does not exist yet. AC-9 is what turns them into a
  measurement, and is written so that the spec can only be wrong about this
  cost in a way the suite catches.

  | decisions | identity state, one replica | at spec 032's N=3 |
  |---|---|---|
  | 100,000 | about 40 MB | about 120 MB |
  | 1,000,000 | about 400 MB | about 1.2 GB |
  | 10,000,000 | about 4 GB | about 12 GB |
  | 100,000,000 | about 40 GB | about 120 GB |

  hiqlite replicates the whole database, so the per-replica column is paid
  on every node, and the state is additionally carried in every store
  snapshot, in every `rahi backup` artifact (spec 030 B-5), in every restore,
  and over the network by any node joining the cluster before it can serve.
  Hex text compresses well, so the backup artifact grows by materially less
  than the table does, but the disk, the snapshot and the join transfer grow
  by the full figure.

  *Expected growth, and the regime this design is sized for.* The rate is one
  row per appended decision and nothing ever removes one. A cell appending
  one decision per second reaches about 31.5 million rows and about 12.6 GB
  per replica per year, which this design does not carry. A cell whose chain
  records governed acts (denials, admin decisions, lifecycle and erasure
  events, manifest transitions) rather than a row per request lands in the
  10^5 to 10^7 range over its life, which it does carry. That boundary is a
  real limit of this design and not a tuning knob: above roughly 10^7
  lifetime decisions per cell the resident cost stops being narrow, and
  D-required-2 is the place that question is decided rather than discovered.

  *Lookup and coverage cost.* The append path gains one row insert and two
  index insertions inside the transaction it already runs: no extra Raft
  operation, no extra leader round trip, and no read. Classification reads
  one row by primary key, and only on the failure path it already takes.
  `lookup` is one primary-key read. Coverage is the reason
  `kernel_decision_coverage` exists: without it, proving coverage would mean
  a `GROUP BY segment_hash` over every row in the identity table at every
  boot, which at 10^7 rows is a multi-second scan on the boot path; with it,
  coverage is one bounded scan of the hot window plus one row per segment,
  and it is read at `open` and on explicit re-check only, never per append
  (B-7).

  *Rejected alternatives.* A constraint over a bounded window, which cannot
  refuse an id whose record has been archived; consulting the archive inside
  the insert, which is not atomic and reintroduces the reproduced defect; a
  monotonic-id watermark, which bounds the state but cannot return the
  original record hash or compare content, two of the four things a retry
  needs; dropping `record_hash` and recomputing it from the archived body,
  which halves the row but makes answering a retry require the archive and so
  destroys B-6's promise that `Sealed` is answerable from resident state; and
  a probabilistic filter over spent ids, which cannot return a hash and whose
  false positives would refuse ids that were never used. `rahi ledger verify`
  reports the row count so the growth is visible to the operator who is
  paying for it. Whether that growth is compatible with spec 014's stated
  reason for existing is an owner's question, not this spec's: section 8
  states it.
- **B-10 (a chain this spec cannot vouch for does not append, and says so at
  the boot).** `Ledger::open` computes coverage after its backfill.
  Incomplete coverage is `Error::Stale` (exit 2) naming the uncovered segment
  count and the command `rahi ledger reindex`, never `Error::Integrity`: an
  unreindexed chain is a missing upgrade step, not tampering, the same
  distinction spec 036 B-4 draws for an unadopted manifest.

  The refusal is scoped to the paths that can append, and the read-only
  paths keep working, because an operator diagnosing this needs them most:
  - `serve` refuses to start (exit 2, the reindex command in its output).
  - Every verb that appends refuses for the same reason and by the same
    code, because each of them opens the ledger first. Today that is `serve`
    alone; spec 036 B-3's `migrate --adopt-manifest` and spec 041's deploy
    step are the boot-time appends that join it when they land, which fixes
    the deploy order stated in B-13: reindex runs before the adopt step, not
    after it.
  - `preflight` reports coverage as a named check and fails on it (exit 1),
    and mutates nothing (spec 030 B-3).
  - `ledger verify`, `ledger export` and `ledger reindex` open through B-15
    and work on an uncovered chain, each printing the uncovered count so no
    reader mistakes the output for a proof about a chain that has none.
  - The genesis append of spec 013 B-4 is never blocked: it runs only when
    nothing is resident and nothing is sealed, and such a chain has no
    uncovered segment by construction.

  The enforcement point is the boot rather than the append because a denial
  whose append fails is counted as a lost denial (spec 035 B-3,
  `Cause::Failed`) and the cell keeps serving: an append-only gate would turn
  a visible refusal into a cell that serves while its audit proof silently
  stops recording, which constitution XI calls worse than a cell that is
  down.

  The cost of this refusal has a floor the owner should see plainly: a chain
  whose archive has permanently lost a body can never be covered, so under
  this behavior alone it can never serve again. Reindex will keep reporting
  that segment uncovered by cause `NotFound` forever, which is honest and is
  also unrecoverable. That case is the strongest argument for the escape
  hatch D-required-3 puts to the owner, and this spec does not decide it.
- **B-11 (the append backstop).** Coverage can still degrade under a running
  cell, because a replica on a binary that does not stamp can append an
  unstamped record after this node booted. So `append` keeps a backstop
  driven by the cached verdict of B-7 plus this node's own observations:
  when the verdict is incomplete, an id with no identity row is
  `Error::Conflict` naming the uncovered segments and the reindex command,
  because the chain cannot prove that id is free; an id whose row's digest
  matches is still the verified retry of B-4 and returns the stored hash;
  and an id whose row's digest differs is still `Error::Conflict`.

  The cached verdict moves to incomplete without a leader read when this node
  observes the evidence itself: a seal that had to create an identity row
  rather than update one (B-5), or a resident record met without a row. It
  moves back to complete only through `recheck_coverage`, which the reindex
  verb runs and `serve` does not, so a degraded cell never talks itself back
  into confidence. Refusing the unknown and admitting the verified is what
  keeps a recovery working through an incident that a full refusal would
  strand. The backstop is a second line, not the first: on a cluster upgraded
  the way B-13 requires it is never reached. It is reached only on a handle
  that opened successfully and then observed degradation, which is the
  scenario FR-010 tests it through. A repair handle is not that path: it
  refuses every append unconditionally (B-15), so the backstop's admitting
  branch does not exist on one, and no test may establish this behavior
  there.
- **B-12 (an id already spent twice is reported, never resolved).** Reindex
  and backfill can meet an id that history already spent more than once,
  which is the defect on a chain written before this spec. The two cases are
  both recorded and neither is decided. When a body or a resident record
  names an id that already has an identity row, B-5's two statements
  compare for it: an equal `record_hash` is the same record met twice and
  writes no collision row, though the upsert still stamps that row if it was
  unstamped; anything else is a collision, and every copy beyond the first is
  written to `kernel_decision_collisions` under the segment it was found
  in. No archived body is read
  for anything but comparison, no archived byte is written, and no identity
  row is overwritten, so immutable history is preserved exactly as spec 014
  B-5 requires. The identity row that happens to stand is the one the walk
  reached first and is not a winner: `lookup` of a colliding id answers
  `Ambiguous` carrying every copy and never `Resident`, `Sealed` or
  `Absent`; `append` of a colliding id is `Error::Conflict` naming every
  copy, because no retry can be verified against a history that spent the id
  twice; `coverage()` counts the collisions and `rahi ledger verify` prints
  them and exits 1 (`Error::Conflict`, spec 030 B-1's mapping); and
  `reindex` exits non-zero when it recorded one, so no operator is told the
  repair succeeded on a chain that contradicts itself (B-8).

  Coverage and ambiguity are two different questions and this spec keeps them
  apart. A collision does not make a segment uncovered, and B-7's accounting
  is what makes that true rather than a wish: coverage counts records, every
  copy beyond the first is a collision row carrying its segment's hash, and
  the counter recomputed by B-5's last statement counts identity and
  collision rows together, so a segment holding a twice-spent id reaches its
  header's `count` exactly. Ambiguity also cannot be repaired by
  any amount of reindexing, because the duplicate is history. So an
  ambiguous chain reaches complete coverage, `serve` starts on it, and the
  containment is per id: every colliding id refuses its own appends and
  answers `Ambiguous`, while the rest of the chain works. Whether that is the
  right trade, or whether ambiguity should also stop the cell, is the second
  half of D-required-3. `verify_chain` at either depth still passes: a
  duplicated id is not a broken link, and this spec does not change what spec
  013 and 014 mean by an intact chain. Resolving a collision is an owner's
  act on the consuming application's own terms and is out of scope here.
- **B-13 (the upgrade is controlled, what enforces it, and what that costs).**
  A cluster is upgraded to a binary that stamps by stopping every replica
  before starting any replica on the new binary, then running `rahi ledger
  reindex` against the archive while nothing appends, then starting the
  cluster.

  *What enforces the cutover, honestly.* Nothing in the store can fence a
  binary that is already published, and this spec does not pretend otherwise.
  An old binary checks what it was built to check: spec 030 B-2 refuses a
  store whose `schema_version` is *behind* the binary's migrations and says
  nothing about a store that is ahead, and the identity tables are chassis
  baseline DDL (B-1) that an old binary neither creates nor reads. So the
  one-time cutover from a pre-042 binary is **operationally guaranteed, not
  enforced**: the operator stops every writer, and the deployment mechanism
  is what makes that true (spec 032's migration Job and rollout, or a
  compose stack brought fully down). A claim of mixed-version safety would be
  a claim about a binary that cannot be changed, so none is offered.

  Three things make that operational guarantee less fragile than it sounds.
  First, reindex's own correctness does not depend on quiescence: its writes
  are insert-or-ignore in per-segment transactions, so a concurrent appender
  cannot corrupt it. The stop is needed so that coverage *converges*, not so
  that the repair is safe. Second, the new binary detects an old writer
  rather than trusting the procedure: a seal that has to create an identity
  row instead of updating one (B-5) and a resident record with no row are
  both counted, both move the cached verdict to incomplete (B-11), both are
  printed by `ledger verify` and `preflight`, and each is logged at `warn`
  naming the id. An old replica that survives the stop therefore shows up as
  a falling coverage number rather than as a silent duplicate. Third, a chain
  the new binary cannot vouch for *at open* does not serve (B-10), and an id
  it cannot prove free is not appended once this node's verdict has moved
  (B-11).

  *What that detection does not establish.* It is after the fact and
  eventual, never preventive, and this spec states the limit rather than
  rounding it up to a guarantee. B-11's verdict is a cache that only this
  node's own observations move, and both observations are late ones: a seal
  that had to create a row, and a resident record met without a row. Neither
  is synchronous with an old replica's append. So in the window between an
  old binary's unstamped append and this node's next observation of it, the
  cached verdict still says complete, an append of that same id finds no
  identity row, and the insert succeeds: a second record under one id, which
  is the defect this spec exists to close. A cached `complete` therefore
  means "this node has seen no evidence of incompleteness", never "the chain
  is proven complete now". The stronger reading holds only where coverage is
  computed through the leader: at `open`, at `recheck_coverage`, and in
  `ledger verify` and `preflight` (B-7).

  What the detection does buy is that such a duplicate is discovered rather
  than silent: it becomes a B-12 collision that `reindex` records, that
  `lookup` answers `Ambiguous` for, that `append` refuses from then on, and
  that `ledger verify` prints and exits non-zero on. That is a strictly
  smaller claim than "a refusal rather than a wrong answer", and it is the
  one the mechanism supports. A mixed-version cluster is out of scope
  (section 6) and no guarantee is offered across one; what is offered is
  that the cost of violating the procedure is a detectable, bounded,
  repairable ambiguity rather than an undetected one.

  *What can enforce the next one.* Spec 036 B-8 makes `serve` refuse a store
  that is ahead of the binary across a non-additive migration, and B-10
  refuses an old image after a manifest transition. Both are checks in the
  *new* binary, so neither helps against 0.1.0, but once 036 has shipped, a
  later downgrade past this spec can be fenced by shipping the identity
  tables as a migration declared non-additive instead of as baseline DDL.
  Making that the cutover mechanism means building 036 first; D-required-4
  puts the ordering to the owner and recommends it for exactly this reason.

  *What it costs.* Downtime is one full cluster stop plus one reindex: one
  archive fetch and one full-depth verification per uncovered segment, so it
  scales with archived history divided by `segment_size` and is bounded by
  archive latency rather than by the store. A chain with no sealed segments
  needs no reindex and its downtime is the restart alone. Rolling back to a
  binary that does not stamp is a one-way door in the other direction: the
  old binary appends unstamped records, coverage breaks again, and returning
  to the new binary requires another stop and another reindex.
- **B-14 (what `append_once` knows, and what it cannot).** `Ledger::
  append_once(decision) -> Landing` with `Landing { hash, appended_now:
  bool, sealed_in: Option<Hash> }`. The three fields answer different
  questions and only the first is a total guarantee.
  - `hash` is **durable presence**: after any successful return the decision
    is in the chain exactly once and `hash` names it. This holds across lost
    acknowledgements, retries, process restarts, and replicas, and it is the
    whole of what this spec guarantees to a caller.
  - `appended_now` is **knowledge about this invocation**, not about the
    decision. `true` MUST be returned only when this invocation's own
    transaction was acknowledged as committed to this invocation. When the
    commit outcome is ambiguous, which is every timeout, dropped connection,
    leader change, or shutdown between the send and the acknowledgement, the
    invocation MUST NOT return `true`: it returns an error, or, if it
    re-reads and finds the decision present, it returns `appended_now =
    false`. So the field is sound and deliberately incomplete: `false` means
    only that the decision was already present when this invocation looked,
    and an earlier invocation *by the same caller* whose acknowledgement was
    lost is one of the ways that happens. `Landing` carries no author, no
    attempt count and no "probably yours" hint, because the evidence to
    populate one does not exist: a lost acknowledgement destroys the
    knowledge of who appended permanently, and no interface can return what
    was never recorded. After an error return the caller knows nothing at
    all: the transaction may or may not have committed, and the only way to
    find out is to ask again, whereupon the answer is presence and never
    authorship.
  - `sealed_in` names the segment when the decision is already archived.
  This is exactly-once **append**, not exactly-once **delivery**. A caller
  that fires a side effect on `appended_now == true` will skip that side
  effect after a lost acknowledgement, because its retry sees `false`.
  `appended_now` is therefore a diagnostic and MUST NOT be a delivery
  trigger; a caller that needs a side effect to happen exactly once records
  its intent in its own transaction and drives the effect from that durable
  row, which is the pattern the consumer contract already documents. This
  spec claims nothing about what happens downstream of the chain.
- **B-15 (the repair open).** `Ledger::open_for_repair` opens a ledger
  without B-10's coverage gate and with appending disabled: `append` and
  `append_once` on such a handle are `Error::Conflict` naming the gate,
  whatever coverage says. It exists because the repair cannot be gated on the
  state it repairs, and it is narrow so that it cannot become a way to serve
  an unproven chain: only `ledger reindex`, `ledger verify` and `ledger
  export` construct one, `serve` has no flag that reaches it, and the type
  makes the refusal to append a property of the handle rather than a check
  someone can forget. Backfill (FR-008) still runs on it, and every read it
  offers reports coverage alongside its answer.

## 4. Functional requirements

- **FR-001.** The reproduced case closes. Append a decision, discard the
  result as a lost acknowledgement, seal it into `FsArchive`, reopen the
  ledger against the same store, and retry the identical decision:
  `append` returns the original record hash, `append_once` reports
  `appended_now = false` and the segment it was sealed into, the chain holds
  one copy of the id, and `verify_chain(Depth::Full)` passes. This is the
  consumer's `pinned_archived_retry_duplicates_after_reopen` with its
  assertion inverted.
- **FR-002.** The concurrent case closes. A retry paused at the seam after
  a completed lookup, while another task appends and seals the same
  decision, resumes and reports the original hash; the chain holds one copy.
  The pause is injected at a test seam, not waited out on a clock. This is
  the consumer's `pinned_verified_lookup_does_not_fence_concurrent_append_
  and_seal` with its assertion inverted.
- **FR-003.** A decision whose id is spent under different content is
  `Error::Conflict` naming the stored hash, whether that record is resident
  or sealed, and no record is written.
- **FR-004.** With a corrupt archived body, `lookup` of a stamped id still
  answers `Sealed` from resident state, `recover` answers
  `Error::Integrity` naming the segment, a removed body answers
  `Error::NotFound`, and an unreadable one answers `Error::Io`. No input
  makes any of them answer `Absent`. This is the consumer's
  `pinned_archive_failure_is_not_proven_absence` with its resident-only
  absence assertion inverted.
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
  failing check, while `rahi ledger verify` and `rahi ledger export` both
  succeed on it and print the uncovered count. After `reindex`, all of them
  start or pass. A chain with no sealed segments and no identity rows
  backfills and opens without an archive.
- **FR-010 (the append backstop, B-11).** The backstop is exercised on a
  normal appending handle, because that is the only handle that can reach
  it: a repair handle refuses every append unconditionally (B-15, FR-019),
  so the admitting branch does not exist there and no successful retry can
  be observed through one. The supported scenario is the degradation B-11
  exists for. Open a covered chain normally, so `open`'s gate passes and the
  cached verdict is complete; write an unstamped record behind this binary's
  back as FR-020 does; then drive one of the two observations that move the
  verdict to incomplete (a seal that has to create an identity row, or a
  resident record met without a row), asserting that no leader read was
  issued to move it. On that same running handle: an append of an id with no
  identity row is `Error::Conflict` naming the evidence that moved the
  verdict and the reindex command, and writes no record; an append of an id
  whose row's digest matches returns the stored hash; an append of an id
  whose row's digest differs is `Error::Conflict`. Only `recheck_coverage`
  moves the verdict back, and `serve` never calls it. The same three
  outcomes are asserted on a chain whose verdict went incomplete from an
  uncovered segment, where the refusal names the uncovered segments
  instead.
- **FR-011 (transaction-level enforcement, B-3 and B-5).** A test asserts
  that the append transaction carries both statements and that neither lands
  alone: an injected failure of the identity insert leaves no
  `kernel_decisions` row, and an injected failure of the record insert
  leaves no identity row. A test asserts the seal transaction accounts for
  exactly `count` records, including for records that had no row, writes the
  recomputed `kernel_decision_coverage` row in the same transaction, and that
  a `stamped` below the header's `count` is `Error::Integrity` naming the
  segment. A test asserts the seal never redirects: with an identity row
  already naming a *different* record under the same id, the seal leaves that
  row's `segment_hash` and `record_hash` untouched, writes the archived copy
  to `kernel_decision_collisions` under this segment's hash, and the segment
  still reaches its header's `count`; `lookup` of that id then answers
  `Ambiguous`, and `recover` of the copy the identity row names still fetches
  the segment that actually contains it. A test asserts the unstamped-row
  case of B-8: a record backfilled while resident and then archived leaves a
  stamped row, not a skipped one. A test runs a backfill and an
  append concurrently and asserts one identity row per id and no error from
  either.
- **FR-012 (reindex meets a reused id, B-12).** A fixture archive holding
  two segments that each contain a record with the same id reindexes without
  error, writes one identity row and one collision row, leaves both archived
  bodies byte-identical, reports one collision, and exits non-zero.
  `lookup` of that id answers `Ambiguous` carrying both copies; `append` of
  it is `Error::Conflict` naming both; `append` of every other id on the
  same chain still succeeds; `verify_chain` passes at both depths; `rahi
  ledger verify` prints one collision and exits 1; and `serve` starts,
  because ambiguity is not incomplete coverage (B-12). The same fixture with
  identical content under one id (same `identity_digest`, different
  `record_hash`) produces the same outcome, a body that repeats an id within
  itself produces the same outcome, and a genuine idempotent repeat (same
  digest and same record hash, reached twice by an interrupted reindex)
  produces no collision row. Coverage over that fixture is **complete**, and
  the test asserts why: `kernel_decision_coverage` holds, for each segment, a
  `stamped` equal to its header's `count`, counting the collision row as the
  copy it is. A chain can be fully accounted for and ambiguous at the same
  time, and that is precisely what lets `serve` start on it while every
  colliding id refuses its own appends.
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
  word. They never skip. An absent fixture directory is a **failure** naming the
  directory and `write.sh`, because the fixture is committed and its absence
  is a broken checkout rather than a missing optional tool; a skip there
  would let AC-3, the whole migration proof, pass on a tree that proves
  nothing. crates.io being unreachable is likewise no reason to skip: the
  fixture is committed and the script is only how it was made. The one skip
  this spec permits anywhere is FR-007's, for an external binary the
  repository does not vendor.
- **FR-015 (reindex over a damaged archive, B-8).** Over a three-segment
  fixture in which one body is removed, one is corrupted, and one is
  readable and valid, `reindex` covers the valid segment, leaves the other
  two uncovered with causes `NotFound` and `Integrity`, writes no identity
  row sourced from either damaged body, returns a report naming each
  segment and its cause, and exits non-zero. `coverage()` afterwards still
  reports two uncovered segments, `open` still refuses, and no output of any
  verb describes the chain as complete. A body whose record count disagrees
  with its header's `count`, and a body whose recomputed `segment_hash`
  differs from its header's, are each `Integrity` for that segment alone and
  leave the other segments' repair unaffected.
- **FR-016 (interruption and restart, B-8).** A reindex killed between
  segments, and a reindex killed inside a segment's transaction, both leave
  the chain in a state a second run converges from: no segment is left
  half-written (per-segment transaction), the counter never exceeds the rows
  it counts, and the second run reaches the same coverage, the same identity
  rows and the same collision rows as an uninterrupted run over the same
  fixture, asserted by comparing both tables. Running `reindex` twice with
  no interruption writes nothing the second time and fetches no body for an
  already covered segment.
- **FR-017 (concurrent append, seal and reindex, B-8 and B-11).** Against
  one chain with an uncovered segment, an appender, a sealer and a reindexer
  run concurrently at a test seam: every append either succeeds or is the
  backstop's `Error::Conflict` and never writes a duplicate id, the seal
  succeeds and stamps its rows, the reindex converges, the chain ends with
  exactly one identity row per id, `verify_chain` passes at both depths, and
  the counters equal the row counts they claim. Two reindexers over the same
  uncovered segment converge to the same rows and neither errors.
- **FR-018 (coverage is cheap and never a per-append read, B-7).** A test
  counts the leader reads issued across N appends on a covered chain and
  asserts the count does not grow with N. A test asserts every
  `kernel_decision_coverage` value equals the number of identity rows plus
  collision rows carrying that segment hash, after a seal, after a reindex,
  after an interrupted reindex, and over a segment holding a twice-spent
  id.
- **FR-019 (the repair open, B-15).** `append` and `append_once` on a handle
  from `open_for_repair` are `Error::Conflict` naming the gate, on a covered
  chain as well as an uncovered one, and no argument or environment variable
  reaches `serve` through it.
- **FR-020 (the cutover is detected, B-13).** After an unstamped record is
  written to a covered chain behind this binary's back, simulating a replica
  on an old image, `coverage()` reports one unstamped resident record,
  `ledger verify` prints it, `preflight` fails on it, and the next seal that
  archives that record creates its identity row, counts the creation, and
  logs at `warn` naming the id. The cached verdict moves to incomplete on
  that observation without a leader read.
- **FR-021 (`appended_now` under an ambiguous commit, B-14).** With the
  commit acknowledgement dropped after the transaction committed, the
  invocation does not return `appended_now = true`: it returns an error, or
  a `false` reached by re-reading. With the transaction failing before
  commit and the acknowledgement also dropped, the retry appends once and
  the chain holds one copy. No path returns `true` for an invocation whose
  commit this process did not observe.

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
- **AC-4.** The boot gate, the verb matrix and the backstop hold: FR-009,
  FR-010 and FR-019 pass, `rahi serve` against an uncovered chain exits 2
  with the reindex command in its output, and `ledger verify` and `ledger
  export` succeed against the same chain while naming its uncovered count.
- **AC-5.** The guarantee survives concurrency: FR-002, FR-011 and FR-017
  pass, and no test in this spec's suite establishes its result with a
  process-local lock or a pre-read.
- **AC-6.** Ambiguity is reported and never resolved: FR-012 passes, and the
  archived bodies of its fixture are byte-identical before and after
  `reindex`, asserted by digest. FR-012's coverage assertion is part of this
  criterion: the ambiguous chain is fully accounted for, which is what lets
  `serve` start on it, and no seal or reindex over it redirects an identity
  row to a record or a segment other than the one it names (FR-011).
- **AC-7.** `append_once`'s contract is documented as B-14 states it, FR-013
  and FR-021 pass, and neither the crate documentation nor the consumer
  contract claims exactly-once delivery or any form of authorship.
- **AC-8.** Damaged history fails visibly: FR-015 and FR-016 pass, and no
  command in this spec prints or returns a complete-coverage result for a
  chain with an unreadable, missing, corrupt or self-contradictory segment
  body. `rahi ledger reindex` exits non-zero in every such case and in every
  case where it recorded a collision.
- **AC-9.** The cutover is detectable and priced: FR-020 passes, and B-9's
  per-decision figure is checked against the implementation by a test that
  writes a known number of identity rows to a temporary store, measures the
  resulting database growth, and fails if it exceeds B-9's figure by more
  than a stated factor. B-9's table is an estimate until this test runs; this
  test is the measurement, and it is what makes the spec wrong about cost
  only in a way the suite catches.
- **AC-10.** Coverage stays cheap: FR-018 passes, and the boot path issues
  no scan whose cost grows with the number of archived decisions.
- **AC-11.** The three consumer reproductions named in section 1 are
  transcribed as FR-001, FR-002 and FR-004 with their assertions inverted,
  each test naming the reproduction it descends from in a comment, so a
  reader can follow a red diagnostic in the consumer's corpus to the green
  regression that closes it here.

- **AC-12 (the accounting is consistent, B-5, B-7 and B-12).** Across the
  fixtures this spec builds, including FR-012's twice-spent id, FR-015's
  damaged archive and FR-016's interrupted reindex, a test asserts the
  accounting invariant directly rather than inferring it from the verbs:
  every resident record and every record in every readable archived body is
  named by exactly one row across `kernel_decision_identity` and
  `kernel_decision_collisions`; no record is named by both; every identity
  row's `segment_hash`, when set, names a segment whose body contains the
  record that row's `record_hash` names; every `kernel_decision_coverage`
  value equals the identity rows plus collision rows counted for its
  segment; and a segment reports covered if and only if that value equals
  its header's `count`. A segment whose body is unreadable is excluded from
  the archived half and reported uncovered, never assumed accounted for.
  This is the criterion that makes B-7's counting rule and B-12's
  serve-while-ambiguous rule one rule rather than two that contradict.
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
A Prometheus series for coverage or for unstamped writers: `/metrics` is spec
023's territory and this spec does not extend it, so the detection of B-13
reaches an operator through `ledger verify`, `preflight` and logs. Flipping
the consumer's three `pinned_` diagnostics to positive assertions, which is
the consumer's corpus's work after a chassis release carries this behavior.
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
- **D-required-2 (spec 014's reason for existing, and the ceiling B-9
  implies).** Spec 014's summary says the replicated store cannot hold an
  audit history that grows without bound, and its Purpose says unboundedness
  is a property only of the tail. B-9 puts a table in the replicated store
  that grows one row per decision forever, at about 400 bytes per decision
  per replica, and states that the design is sized for cells in the 10^5 to
  10^7 lifetime-decision range and does not carry a cell that appends a
  decision per request. Spec 014 is `approved` and `implementation:
  complete`, so nothing in this draft touches it and no build session may.
  Two questions for the owner, and the second is the one that binds a
  consumer: whether 014's rationale binds later specs, and if it does,
  whether 042 is recorded in 014 as a carve-out or must find a bounded
  mechanism B-9 argues does not exist; and whether the stated ceiling is
  acceptable as the chassis's answer, or whether an eviction or compaction
  story must exist before this design is approved rather than after the
  first cell reaches it. Checked against the consumer that needs this: the
  aicortex requirement is one identity row per erasure and lifecycle
  Decision, not per memory or per request, which sits inside the supported
  regime; the check is stated here so that the owner is approving a number
  rather than a direction.
- **D-required-3 (the availability promise of B-8's predecessor, and what an
  ambiguous chain may do).** Two related availability calls.
  - *(a) The uncovered chain.* Spec 014 D-3 reasons that a cell with no
    object storage configured must still open, append, and verify, which is
    why the archive is an argument rather than a field. B-10 suspends that
    for exactly one population: a chain with sealed segments and no identity
    rows, between the upgrade and the reindex, cannot boot without the
    archive. Every chain appended entirely under this spec keeps 014 D-3's
    promise intact. B-10 also states the floor: a chain whose archive has
    permanently lost a body can never be covered and so, under this draft,
    can never serve again. The owner decides whether that is acceptable or
    whether `serve` should offer an explicit `--allow-uncovered` start, in
    which case the cell serves with B-11's backstop refusing unknown ids and
    `kernel_decisions_lost` rising. This draft recommends the escape hatch
    only as an explicit, logged, non-default flag whose use is itself
    appended to the chain when the chain can take an append: a permanently
    unrecoverable archive is a real state, and a chassis with no exit from
    it converts a lost object into a dead cell. This draft does not add the
    flag, because adding it is the owner's call.
  - *(b) The ambiguous chain.* B-12 lets a chain with recorded collisions
    reach complete coverage and serve, containing the damage per id. The
    alternative is to treat any collision as a refusal to serve. This draft
    chooses containment because a collision is unrepairable history: a
    refusal would be permanent, and it would punish every other id in the
    chain for two records written years earlier. The owner decides.
- **D-required-4 (sequencing against 036, refreshed).** The corpus currently
  declares these overlaps for 042: with 036 on `crates/rahi-cli/src/lib.rs`,
  `crates/rahi-ledger/src/chain.rs`, `crates/rahi-ledger/src/lib.rs`,
  `crates/rahi-ledger/src/segment.rs` and `crates/rahi-ledger/src/verify.rs`,
  and with 040 on `crates/rahi-cli/src/lib.rs`. Spec 036 B-4 rewrites
  `Ledger::open` and B-5 changes what a segment header carries; this spec
  changes both files too, and adds the coverage gate to the same `open`. The
  overlap is on shared files, not a dependency: 042 `depends_on` 013, 014 and
  030, all `complete`, so it is buildable without 036.

  The approval state matters more than the ready set here, and it has moved
  since this question was first written. The ready set is 036, 038, 040 and
  042; of those **036 and 038 are `approved` and `implementation: pending`**,
  while 040 and 042 are still `draft`. So the owner is not choosing among
  four peers: two specs are already cleared to build and this one is not
  cleared at all, which means any ordering that puts 042 first also asks for
  042 to be approved first. Against 038 there is nothing to sequence: the
  corpus declares no territory overlap between 038 and 042 (038's overlaps
  are with 036 and 040, on `crates/rahi-cli/src/serve.rs`), and 038 touches
  the edge and identity surfaces rather than the ledger, so the two can run
  in either order without either session inheriting the other's
  reconciliation. The sequencing question is 036 alone.

  This draft recommends **036 first, then 042**, for three reasons rather
  than for tidiness. `Ledger::open` gains two refusals that must compose into
  one message and one exit code (an unadopted manifest and an uncovered
  chain, both `Error::Stale`, exit 2), and writing the second onto a settled
  first is smaller than reconciling two half-built ones. 036 B-5 changes the
  segment header, which B-8's reindex reads and verifies per segment, so
  building reindex against the final header shape avoids a rewrite. And
  036 B-8's non-additive migration check is the only mechanism in the corpus
  that could fence a downgrade past 042 (B-13), so 042 landing after it can
  be shipped as that migration rather than as unfenceable baseline DDL. The
  cost of this order is that the consumer's unblocking waits for 036. If the
  owner prefers 042 first, nothing in this draft blocks it and the 036
  session inherits the reconciliation instead; the decision is the owner's
  and the backlog's, not this spec's.

## Verification

```verify:cli
cargo test -p rahi-ledger --locked --test identity
cargo test -p rahi-ledger --locked --test append
cargo test -p rahi-ledger --locked --test seal
cargo test -p rahi-ledger --locked --test verify
cargo test -p rahi-cli --locked --test cli
```
