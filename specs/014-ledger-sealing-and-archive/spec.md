---
id: "014-ledger-sealing-and-archive"
title: "Ledger sealing: a resident hot window, immutable segments archived to object storage"
status: approved
kind: "feature"
domain: "ledger"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: pending
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

None yet.

## Verification

```verify:cli
cargo test -p rahi-ledger --locked --test seal
```
