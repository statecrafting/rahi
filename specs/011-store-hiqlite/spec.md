---
id: "011-store-hiqlite"
title: "The store: hiqlite in-process with txn, two read calls, migrations, and backup"
status: approved
kind: "kernel"
domain: "store"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: pending
risk: critical
wave: 1
depends_on:
  - "010-workspace-and-core-types"
establishes:
  - "crates/rahi-store/Cargo.toml"
  - "crates/rahi-store/src/lib.rs"
  - "crates/rahi-store/src/config.rs"
  - "crates/rahi-store/src/store.rs"
  - "crates/rahi-store/src/txn.rs"
  - "crates/rahi-store/src/query.rs"
  - "crates/rahi-store/src/migrate.rs"
  - "crates/rahi-store/src/backup.rs"
  - "crates/rahi-store/tests/store.rs"
  - "crates/rahi-store/tests/txn.rs"
  - "crates/rahi-store/tests/migrate.rs"
  - "crates/rahi-store/tests/backup.rs"
extends:
  - { spec: "010-workspace-and-core-types", unit: { kind: section, file: "Cargo.toml", anchor: "workspace.dependencies" }, nature: additive }
constrains:
  - { flavor: invariant-freeze, unit: "crates/rahi-store/src/txn.rs", note: "one txn is the atomic unit; constitution IX" }
summary: >
  The application store: hiqlite linked as a library, opened from the Config
  with its own data directory and ports (never rauthy's), exposing exactly
  the surface the interface contract decided: execute, txn as the write path
  for anything with an invariant, query on the local replica and
  query_consistent through the leader as two named calls, migrations as
  versioned DDL run by a verb and never at boot, and the leader-only
  encrypted backup surface. Carries enrahitu://032 §3.1, §3.3, §3.6, §3.8.
---

# 011: The store

## 1. Purpose

hiqlite is a library, not an API (enrahitu://002 retired its HTTP surface
for that reason). This spec is the one place the chassis opens it, and it
writes the surface down first, derived from the ten decisions enrahitu made
against hiqlite's implementation rather than its docs (enrahitu://032). The
constraints those checks found (a ten-second lease, one global notify
channel, replayed events after restart) shape spec 012; this spec holds the
atomicity boundary, read consistency, migration ownership, and backup.

## 2. Territory

The whole of `crates/rahi-store` as it stands after this spec. Coordination
(lock, notify, outbox, watermark) is spec 012's territory inside the same
crate; it `extends` this spec's `lib.rs`. The store exposes `Store` and a
`StoreHandle` clone for other crates; nothing else in the workspace names
hiqlite.

## 3. Behavior

- **B-1 (open).** `Store::open(cfg: &StoreConfig) -> Result<Store, Error>`
  starts a hiqlite node with `RaftType::Sqlite` and `RaftType::Cache`
  groups under `cfg.data_dir` (default `/data/hiqlite`), bound to
  `cfg.raft_addr` and `cfg.api_addr`, a single voter unless `cfg.nodes`
  lists peers, and encryption keys from `cfg.enc_keys`. It awaits the Raft
  election before returning. It MUST refuse a `data_dir` equal to or inside
  the rauthy data directory (`Error::Config`).
- **B-2 (execute and txn).** `execute(sql, params) -> Result<ExecuteResult>`
  is the single-statement write. `txn(statements: Vec<Statement>) ->
  Result<Vec<ExecuteResult>>` submits a batch as one Raft operation and is
  the write path for anything with an invariant. There is no API that
  commits a SQL write and a notify atomically, because there cannot be one.
- **B-3 (two read calls).** `query<T: DeserializeOwned>(sql, params) ->
  Result<Vec<T>>` reads the local replica. `query_consistent<T>(sql, params)
  -> Result<Vec<T>>` takes the leader round-trip. There is no consistency
  flag; the two names force every call site to state its requirement.
- **B-4 (migrations).** `migrate(migrations: &[Migration]) ->
  Result<MigrationReport>` reads `schema_version` with `query_consistent`,
  applies each higher-versioned `Migration { version: u32, name, sql }`
  through `txn` in order, and records each in `schema_version` inside the
  same `txn` as its DDL. It is called by the `migrate` verb (spec 030) and
  MUST NOT be called from `Store::open`. The store's own baseline migration
  (`schema_version` itself) is version 0.
- **B-5 (backup).** `backup() -> Result<BackupId>` issues hiqlite's
  `VACUUM main INTO` on the writer thread; it is leader-only and returns
  `Error::Stale` on a follower. `backup_list_local()` and
  `backup_list_s3()` list snapshots. Restore is NOT on this surface: it is a
  boot-time concern of spec 030.
- **B-6 (feature set).** hiqlite is built with `sqlite`, `cache`,
  `dlock`, `listen_notify_local`, `backup`, and `s3`; never
  `listen_notify` (remote listeners are a second egress path).
- **B-7 (rauthy's store is invisible).** No function in this crate accepts
  or derives a path under the rauthy data directory; the config type has no
  field for it.

## 4. Functional requirements

- **FR-001.** Tests run against a single-voter node in a temp directory
  (no network peers) and cover: open and health; `execute` round-trip;
  `txn` atomicity (a batch whose last statement fails leaves no row from
  its first); `query` after `execute` on the same node; `migrate` applied
  once, idempotent on re-run, refusing a lower version than recorded.
- **FR-002.** A test asserts `Store::open` refuses a `data_dir` inside the
  rauthy directory with `Error::Config`.
- **FR-003.** A test asserts `backup()` produces a file under the backup
  directory and `backup_list_local()` lists it; S3 listing is exercised only
  when `RAHI_TEST_S3` names a bucket, otherwise skipped with a message.
- **FR-004.** `StoreConfig` is built from `rahi_types::Config` by one pure
  function, tested for the port defaults that leave `8100`/`8200` free.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p rahi-store --locked` passes.
- **AC-2.** `cargo tree -p rahi-store` shows `rahi-types` as the only
  workspace dependency.

## 6. Out of scope

Locks, notify, outbox, and the revision watermark (012); the decision chain
(013); the restore verb and the first-boot marker (030, 031).

## 7. Resolved decisions

None yet.

## Verification

```verify:cli
cargo test -p rahi-store --locked
```
