---
id: "014-ledger-sealing-and-archive"
title: "Ledger sealing: a resident hot window, immutable segments archived to object storage"
status: approved
kind: "feature"
domain: "ledger"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: complete
risk: high
wave: 1
depends_on:
  - "013-ledger-decision-chain"
establishes:
  - "crates/rahi-ledger/src/seal.rs"
  - "crates/rahi-ledger/src/segment.rs"
  - "crates/rahi-ledger/src/archive.rs"
  - "crates/rahi-ledger/tests/seal.rs"
extends:
  - { spec: "013-ledger-decision-chain", unit: "crates/rahi-ledger/src/lib.rs", nature: additive }
  - { spec: "013-ledger-decision-chain", unit: "crates/rahi-ledger/src/chain.rs", nature: additive }
  - { spec: "013-ledger-decision-chain", unit: "crates/rahi-ledger/Cargo.toml", nature: additive }
  - { spec: "010-workspace-and-core-types", unit: { kind: section, file: "Cargo.toml", anchor: "workspace.dependencies" }, nature: additive }
summary: >
  hiqlite replicates the whole database to every node, so an unbounded audit
  history cannot live in it; the head and a hot window do. When the window
  exceeds its bound the oldest contiguous run is sealed into an immutable
  segment (first id, last id, count, segment hash, previous segment hash),
  written to an object store behind a trait with an S3 implementation, and
  deleted from the hot table. Segments link to their predecessor and the
  head links to the last segment, so the chain verifies end to end without
  archived history resident. Archive is never conflated with backup.
  Carries enrahitu://032 §3.7 and §3.9.
---

# 014: Ledger sealing and archive

## 1. Purpose

Linearizable read-modify-write is needed only at the head; unboundedness is
a property only of the tail. Splitting at that seam satisfies both, and
keeping the archive separate from backup keeps a procurement answer
("three-year-old decisions are retrievable") from being answered with a
thirty-day snapshot schedule.

## 2. Territory

Three modules in `crates/rahi-ledger` and their test. Extends 013's
`lib.rs` and `Cargo.toml` (the object-store dependency) and the workspace
dependency table.

## 3. Behavior

- **B-1 (bound).** `SealPolicy { hot_window: u32 (default 10_000),
  segment_size: u32 (default 1_000) }`. `Ledger::seal_if_needed()` runs
  after every append and, when the resident count exceeds `hot_window`,
  seals the oldest `segment_size` contiguous records.
- **B-2 (segment).** `Segment { first_id, last_id, count, segment_hash,
  prev_segment_hash, records: Vec<LedgerRecord> }`; `segment_hash` is
  sha256 over the canonical bytes of the records. The segment record
  (everything but `records`) is inserted into `kernel_segments` in the same
  `txn` that deletes the archived rows, after the body has been written to
  the archive and the write acknowledged.
- **B-3 (archive trait).** `trait Archive { async fn put(key, bytes) ->
  Result<()>; async fn get(key) -> Result<Bytes>; async fn list(prefix) ->
  Result<Vec<String>> }` with `S3Archive` (endpoint, bucket, credentials
  from `Config`) and `FsArchive` for tests. Keys are
  `ledger/segments/<first_id>-<last_id>.json`.
- **B-4 (verify with segments).** `verify_chain` walks the resident chain
  and, at its oldest record, checks that `prev_hash` equals the last
  segment's `segment_hash`, then walks segment records back to the genesis
  segment by `prev_segment_hash`. Segment bodies are fetched only when
  `verify_chain(Depth::Full)` is requested; boot uses `Depth::Resident`.
- **B-5 (immutability).** No function in this crate deletes or overwrites
  an archived segment; `Archive::put` to an existing key is
  `Error::Conflict`.
- **B-6 (not a backup).** The archive stores segments only. Store snapshots
  (spec 011 B-5) and the restore verb (030) never read or write
  `ledger/segments/`.

## 4. Functional requirements

- **FR-001.** With `hot_window = 20` and `segment_size = 10`, thirty
  appends leave twenty resident records, one segment in `FsArchive`, and a
  `verify_chain(Depth::Full)` that passes.
- **FR-002.** Corrupting a byte of an archived segment makes
  `Depth::Full` fail and `Depth::Resident` still pass, and the failure names
  the segment.
- **FR-003.** A `put` to an existing key returns `Error::Conflict`.
- **FR-004.** An archive write failure leaves the hot table unchanged (no
  deletion without an acknowledged body).

## 5. Acceptance criteria

- **AC-1.** `cargo test -p rahi-ledger --locked --test seal` passes.

## 6. Out of scope

Retention schedules and legal holds (an application concern); the S3
credentials' provisioning (031, 032).

## 7. Resolved decisions

- **D-1 (2026-09-06, build session; refines B-2 and B-4).** The segment
  record carries a sixth field, `last_hash`: the record hash of the
  segment's newest record. B-4 asks the oldest resident record's
  `prev_hash` to be checked against "the last segment's `segment_hash`",
  but B-2 defines `segment_hash` as sha256 over the canonical bytes of the
  records, and a record's parent link is the *record hash* of its
  predecessor, fixed when the append chained it. The two values are
  different by construction and no ordering of the seal can make them
  equal. Two hashes with two jobs resolves it: `segment_hash` is the
  content digest the next segment binds through `prev_segment_hash`, and
  `last_hash` is what the next record links to, whether that record is the
  oldest one still resident or the first of the following segment. Both
  live in `kernel_segments` and in the archived body, so `Depth::Resident`
  proves the seam and the segment chain without fetching anything, which is
  what B-4 asks for. Rejected alternatives: redefining `segment_hash` as
  the terminal record hash (it would stop being a content digest, and a
  rewritten body would verify); and reading `last_id`'s record out of the
  archive at boot (it makes B-4's resident depth fetch bodies).
- **D-2 (2026-09-06, build session; extends 013's `chain.rs`).** Sealing
  changes what the resident chain is rooted at, so spec 013's `chain.rs` is
  extended additively and this spec's `extends` list says so. `Ledger`
  gains `resident_root()`: the last segment's `last_hash`, or the genesis
  parent when nothing has been sealed. `records()` orders from it,
  `head()` falls back to it on an empty table, `open()` writes a genesis
  record only when nothing is resident *and* nothing is sealed, and both
  `open()` and `verify()` go through `verify_chain(Depth::Resident)`. B-4
  requires exactly this ("boot uses `Depth::Resident`") and the Territory
  section named only `lib.rs` and `Cargo.toml`; without the edit a cell
  could not reboot after its first seal, because 013's boot verification
  orders the resident chain from the genesis parent and every record would
  be unreachable from it. Nothing 013 requires is changed: on a ledger with
  no segments the resident root *is* the genesis parent, and every 013 test
  passes unmodified.
- **D-3 (2026-09-06, build session; refines B-1's and B-4's signatures).**
  `seal_if_needed(&dyn Archive, &SealPolicy)` and `verify_chain(Depth)`
  take what they need as arguments rather than reading it off the ledger.
  B-1 sketches `Ledger::seal_if_needed()`, but the `Ledger` struct is spec
  013's and a cell with no object storage configured must still open,
  append, and verify, so the archive cannot be a field of it. `Depth` is
  `Resident` or `Full(&dyn Archive)`, which makes "segment bodies are
  fetched only at full depth" a property of the type: a boot path holding
  no archive cannot express the deep check. `SealPolicy::new` validates its
  two numbers on construction (`1 <= segment_size <= hot_window`, and
  `segment_size <= MAX_SEGMENT_SIZE = 10_000`, one `DELETE` parameter per
  archived id), so a policy that would empty the hot table and leave no
  head to append onto is refused where it is configured rather than at the
  append that trips over it.
- **D-4 (2026-09-06, build session; refines B-3).** The archive trait is
  `async-trait` over `Vec<u8>`, and its credentials are its own type.
  `async_trait` because the trait is held as `&dyn Archive` and an
  `async fn` in a trait is not dyn-compatible without boxed futures;
  `Vec<u8>` rather than a `Bytes` type because the bodies are read whole
  and a shared-buffer type would be a dependency bought for a type alias.
  `S3Archive` is built on `s3-simple`, which hiqlite already carries for
  its own backups, so no second S3 implementation enters the tree.
  Credentials come from `archive::S3Config` rather than
  `rahi_types::Config` (B-3's wording): the chassis config is derived from
  one public URL and carries no object-store credentials (spec 010 B-7),
  provisioning them is 031 and 032's and out of scope here, and reusing
  `rahi_store::S3Backup` would conflate the archive bucket with the backup
  bucket, which B-6 forbids.
- **D-5 (2026-09-06, build session; refines B-2).** `segment_hash` is
  sha256 over the canonical JSON array of the segment's records, and the
  archived body is the header flattened into the same object as `records`.
  The records are `SignedRecord`s, not bare `LedgerRecord`s as B-2's sketch
  writes: 013 D-2 fixed the stored and exported shape as the envelope plus
  its signature and public key, and a body without signatures could not be
  verified by the same code that verifies a resident chain. The digest is
  taken over the same key-sorted serialization every other hash in this
  crate uses, so an auditor recomputes it from the fetched body with a JSON
  canonicalizer and a sha256 and nothing else.
- **D-6 (2026-09-06, build session; reads FR-001's arithmetic).** The
  thirty appends of FR-001 are thirty records in the chain, the genesis
  record spec 013 writes at open included: one seal fires at the
  twenty-first record, ten records leave, and the chain finishes at twenty
  resident and one segment. Counting thirty appends *after* genesis gives
  thirty-one records, two seals, and eleven resident, which is not the
  outcome FR-001 states. The test is written the first way.

## Verification

```verify:cli
cargo test -p rahi-ledger --locked --test seal
```
