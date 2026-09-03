---
id: "016-store-binary-values-and-extensions"
title: "Binary values and the extension policy: BLOBs are first class, loadable extensions are refused"
status: approved
kind: "kernel"
domain: "store"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: pending
risk: high
wave: 1
depends_on:
  - "011-store-hiqlite"
  - "012-store-coordination"
establishes:
  - "crates/rahi-store/src/blob.rs"
  - "crates/rahi-store/tests/blob.rs"
extends:
  - { spec: "011-store-hiqlite", unit: "crates/rahi-store/src/lib.rs", nature: additive }
  - { spec: "011-store-hiqlite", unit: "crates/rahi-store/src/query.rs", nature: additive }
constrains:
  - { flavor: invariant-freeze, unit: "crates/rahi-store/src/store.rs", note: "no SQLite extension is ever loaded; every node runs one identical engine" }
summary: >
  Applications above the chassis store vectors, thumbnails, and signatures,
  so the store must round-trip binary values with a declared ceiling and a
  paged read that never materializes a table. It must also answer, once and
  in writing, whether a loadable SQLite extension may be used: it may not.
  hiqlite replicates statements rather than pages, so a node whose engine
  differs diverges silently, and a virtual table's behavior under snapshot,
  backup, and restore is unproven. The escape hatch is named here so a later
  spec can open it with evidence rather than by assumption.
---

# 016: Binary values and the extension policy

## 1. Purpose

Spec 011 gave the store a typed read and a transaction and said nothing
about bytes or about the SQLite engine's surface area. Both silences are
load-bearing for the products that consume this chassis: one of them stores
embedding vectors as BLOBs and wants similarity search, and the obvious way
to get similarity search is to load `sqlite-vec`. This spec makes bytes
work and refuses the extension, for reasons that are properties of the
replication model rather than of the extension's quality.

hiqlite replicates the SQL statement, not the page. Every node therefore
executes the statement against its own engine. If one node has a virtual
table module and another does not, the follower's apply fails or, worse,
succeeds differently, and Raft's guarantee (every node applies the same log
to the same state) is void. Constitution VIII names two stores and never a
third; this spec adds that there is also exactly one engine.

## 2. Territory

One module and one test in `crates/rahi-store`. Extends 011's `lib.rs` to
re-export `Blob` and `MAX_VALUE_BYTES`, and 011's `query.rs` with the paged
read. Freezes the property on 011's `store.rs` that the connection is never
handed an extension.

## 3. Behavior

- **B-1 (binary parameters and columns).** `execute` and `txn` accept
  `Vec<u8>` and `&[u8]` parameters, and `query<T>` deserializes a BLOB
  column into a `Vec<u8>` or `serde_bytes::ByteBuf` field of `T`. A NULL
  BLOB deserializes into `Option<Vec<u8>>` as `None`. No base64 hop exists
  anywhere in the path.
- **B-2 (value ceiling).** A single parameter larger than
  `MAX_VALUE_BYTES` (default 1 MiB, configurable downward only) is
  `Error::Validation` before the statement is submitted. The reason is
  replication cost: the value is written to the Raft log, shipped to every
  follower, and retained in the log until the next snapshot, so a large
  value is paid for N times plus the log.
- **B-3 (paged read).** `query_paged<T>(sql, params, page: Page) ->
  Result<Vec<T>>` appends `LIMIT ? OFFSET ?` and is the supported way to
  sweep a table. `Page { size: u32, after: u64 }` defaults to 1024 rows.
  A full-table scan in an application is a loop over `query_paged`, never
  one unbounded `query`, so peak memory is a property of the page size.
- **B-4 (no loadable extensions).** The store never calls
  `load_extension`, never enables it on a connection, and exposes no API
  that could. A test greps the crate's own sources and fails on any
  occurrence of `load_extension`, `enable_load_extension`, or
  `sqlite3_auto_extension`. `Store::open` asserts at boot that extension
  loading is disabled on the connection.
- **B-5 (the escape hatch, closed).** A later spec MAY open B-4 only by
  establishing all four of: the extension is loaded identically on every
  node before the Raft group accepts its first append; the extension and
  its exact version are named in `Config` and reported by `preflight`; an
  N=3 divergence test writes through a virtual table and asserts byte
  equality of all three engines' state; and a backup taken with the virtual
  table present restores on a node that has never seen a write. Until such
  a spec exists and ships, the answer to "can we use sqlite-vec" is no, and
  an application that needs similarity search reads B-1 vectors through
  B-3 and ranks in its own process.
- **B-6 (no similarity primitive).** The store offers no distance
  function, no index type, and no ranking. Ranking is application logic
  above the chassis (constitution XIV).

## 4. Functional requirements

- **FR-001.** A 6 KiB vector written through `execute` and read back
  through `query` is byte-identical, including a value containing a NUL
  byte and a value that is not valid UTF-8.
- **FR-002.** A parameter of `MAX_VALUE_BYTES + 1` bytes is
  `Error::Validation`, and no row is written.
- **FR-003.** `query_paged` over a 5000-row fixture returns every row
  exactly once across pages, with a stable order, and never holds more
  than one page in memory (asserted by page size, not by measurement).
- **FR-004.** The grep test of B-4 fails when a call to
  `enable_load_extension` is introduced into the crate.
- **FR-005.** An N=3 harness test writes 1000 BLOB rows and asserts all
  three nodes return identical bytes for a `query_consistent` sweep.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p rahi-store --locked --test blob` passes.
- **AC-2.** `preflight` reports `extensions: none` and the configured
  `max_value_bytes`.

## 6. Out of scope

Vector similarity, approximate indexes, and full-text search, which are
application concerns above the chassis and are specified by the product
that needs them. Compression of stored values, which an application may do
before it hands the store bytes.

## 7. Resolved decisions

- **D-1 (2026-09-03, this spec).** Loadable extensions are refused rather
  than gated behind a config flag. A flag would let a single-node
  deployment enable something that silently breaks on promotion to N=3,
  which is the worst possible failure shape: correct in development,
  divergent in production. Rejected alternative: allow extensions when
  `cluster.size == 1`.

## Verification

```verify:cli
cargo test -p rahi-store --locked --test blob
```
