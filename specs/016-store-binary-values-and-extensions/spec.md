---
id: "016-store-binary-values-and-extensions"
title: "Binary values and the extension policy: BLOBs are first class, loadable extensions are refused"
status: approved
kind: "kernel"
domain: "store"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: in-progress
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
  - { spec: "011-store-hiqlite", unit: "crates/rahi-store/src/store.rs", nature: additive }
  - { spec: "011-store-hiqlite", unit: "crates/rahi-store/Cargo.toml", nature: additive }
  - { spec: "010-workspace-and-core-types", unit: { kind: section, file: "Cargo.toml", anchor: "workspace.dependencies" }, nature: additive }
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

- **D-2 (2026-09-06, build session).** The boot assertion of B-4 asserts
  the outcome, not the flag. hiqlite 0.14 owns the connection pool, hands
  out no rusqlite handle, and drops row-level errors on every local read
  (`while let Some(Ok(row)) = rows.next()`), so a load request comes back
  as `Ok(vec![])` whether the engine refused it or merely failed it: the
  connection's flag is not readable from here. `Store::open` therefore
  asserts what is readable and is what matters: the request returns no
  row, and a load that had succeeded would return exactly one. The rest of
  the ban is static (the grep of FR-004, and no API that could). Rejected
  alternative: probing through `execute`, which would append a probe
  statement to the Raft log at every boot and would assert the leader's
  engine rather than this node's own.

- **D-3 (2026-09-06, build session).** The ceiling is a property of the
  handle, not a `StoreConfig` field. `MAX_VALUE_BYTES` is where every
  handle starts; `StoreHandle::with_max_value_bytes` returns a clone with a
  lower one and refuses anything above the current value, which is what
  "configurable downward only" means without a mutable global.
  `StoreHandle::engine_report()` is the pair of facts AC-2 has `preflight`
  print. The check covers `Text` parameters as well as `Blob`, because
  both are replicated verbatim, and runs on the write paths only, because
  a read parameter is never appended to the log. It lands in 011's
  `store.rs`, declared as an additive `extends` edge, because `execute`,
  `txn`, and `open` all live there. Rejected alternative: a
  `max_value_bytes` field on `StoreConfig`, which 016's territory does not
  reach and which would have rewritten every `StoreConfig` literal in
  specs 011, 013, and 015.

- **D-4 (2026-09-06, build session).** `query_paged` appends
  `LIMIT $n+1 OFFSET $n+2` rather than B-3's literal `LIMIT ? OFFSET ?`,
  because hiqlite binds positionally and SQLite would derive a bare `?`'s
  index from the highest number already in the statement, which the
  caller, not the chassis, controls. A `Page` whose size is zero is
  `Error::Validation`: it never advances a sweep. The paged read is local,
  like every other scan; a sweep that took a leader round-trip per page
  would pause Raft once per page.

- **D-5 (2026-09-06, build session).** The grep of B-4 has no exemption
  list. `blob.rs` assembles the SQL function's name from two halves with
  `concat!`, so the token appears nowhere in the crate's sources and any
  occurrence at all is a defect. `tests/blob.rs` scans
  `crates/rahi-store/src` for all three names and proves the scanner
  fails on each of them in a sample rather than assuming it would.

- **D-6 (2026-09-06, build session).** FR-005's N=3 test lives in
  `crates/rahi-store/tests/blob.rs`. Spec 033's harness boots the packaged
  binary over HTTP, is a different instrument, and does not exist yet,
  while hiqlite starts three in-process nodes in one test as its own
  cluster tests do. The test writes 1000 BLOB rows through the leader and
  compares a paged `query_consistent` sweep and each node's own local
  sweep, byte for byte.

- **D-7 (2026-09-06, build session).** `serde_bytes` enters
  `[workspace.dependencies]` and `rahi-store`'s `[dev-dependencies]` only.
  B-1 names `serde_bytes::ByteBuf` as a supported column shape, and the
  claim is worth testing; no chassis code depends on it, and the store's
  own path carries bytes without it.

## 8. Status

- **2026-09-06.** B-1 to B-6, FR-001 to FR-005, and AC-1 hold. AC-2 does
  not, and cannot yet: `preflight` is `crates/rahi-ops/src/preflight.rs`,
  spec 030's territory, and spec 030 is `pending`. The two facts AC-2 has
  it print are implemented and tested here as
  `StoreHandle::engine_report()`, whose `Display` is
  `extensions: none, max_value_bytes: <n>`. The session that builds 030
  adds that line to the preflight check list of its B-3 and flips this
  spec to `implementation: complete`; until then it stays `in-progress`.

- **2026-09-06 (second build session).** The gate is green and AC-1 passes;
  the hold above is the only thing outstanding, and it needs one act this
  build session may not perform. Spec 030's B-3 enumerates the preflight
  checks (config parses, `/data` writable, key file modes, hiqlite elects,
  rauthy answers, ledger verifies, free disk) and does not name the engine
  report, so closing AC-2 is not a matter of the 030 session remembering to
  call `engine_report()`: it needs an authoring edit to 030's B-3 adding
  that check. Amending 030 to match this spec's acceptance is not a build
  session's call, so the contradiction is surfaced rather than resolved.
  See decision D-8.

- **2026-09-08 (third build session).** The authoring edit the note above
  asked for has landed: spec 030's B-3 now names the store's engine report
  in its preflight check list, recorded as 030 D-1 (2026-09-07). The
  contradiction is therefore resolved, and what remains is a scheduled
  handoff rather than an open question: `crates/rahi-ops` does not exist
  and spec 030 is still `implementation: pending`, so AC-2 has no subject
  to run against. 030 D-1 names the closer explicitly, that the session
  building 030 adds the check and flips this spec in the same change, so
  this session does not flip it. No `depends_on` edge was added in either
  direction, by design: nothing depends on 016, and an edge from 030 would
  make 030 blocked by a spec only 030 can unblock. The gate was re-run
  whole on this date and is green (`make spine`, `couple`, `make ci`,
  `index coverage --fail-on-untraced` all exit 0), and AC-1 passes 8 of 8,
  including the N=3 divergence test of FR-005. Nothing in this spec's
  territory is outstanding.

## Verification

```verify:cli
cargo test -p rahi-store --locked --test blob
```
