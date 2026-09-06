---
id: "011-store-hiqlite"
title: "The store: hiqlite in-process with txn, two read calls, migrations, and backup"
status: approved
kind: "kernel"
domain: "store"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: in-progress
risk: critical
wave: 1
depends_on:
  - "010-workspace-and-core-types"
establishes:
  - "crates/rahi-store/Cargo.toml"
  - "crates/rahi-store/src/lib.rs"
  - "crates/rahi-store/src/config.rs"
  - "crates/rahi-store/src/error.rs"
  - "crates/rahi-store/src/store.rs"
  - "crates/rahi-store/src/txn.rs"
  - "crates/rahi-store/src/query.rs"
  - "crates/rahi-store/src/migrate.rs"
  - "crates/rahi-store/src/backup.rs"
  - "crates/rahi-store/tests/store.rs"
  - "crates/rahi-store/tests/txn.rs"
  - "crates/rahi-store/tests/migrate.rs"
  - "crates/rahi-store/tests/backup.rs"
  - "crates/rahi-store/tests/common/"
extends:
  - { spec: "010-workspace-and-core-types", unit: { kind: section, file: "Cargo.toml", anchor: "workspace.dependencies" }, nature: additive }
  - { spec: "010-workspace-and-core-types", unit: "deny.toml", nature: additive }
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

- **D-1 (2026-09-05, build session).** hiqlite `0.14` with
  `default-features = false` and exactly the B-6 set. The default
  `auto-heal` (silent WAL repair) and `toml` features are off: a chassis
  that fails closed on integrity (constitution XI) does not heal storage
  silently. `serde_json` is a runtime dependency (already in the tree
  through hiqlite) for the owned-row mapping of D-2.
- **D-2 (2026-09-05, build session).** `query_consistent<T:
  DeserializeOwned>`: hiqlite returns owned rows from the leader with no
  serde path, so the store flattens each owned row into a name-to-value
  map and deserializes it, giving both read calls the same caller types.
  Alternative rejected: a `FromRow` trait, which would have put a hiqlite
  type in every caller's signature.
- **D-3 (2026-09-05, build session).** Rauthy's territory is recognised
  structurally: `Store::open` refuses any `data_dir` with a `rauthy` path
  component, because rauthy's directory is the `rauthy` sibling on the
  same volume (thesis §3) and B-7 forbids a field naming it.
- **D-4 (2026-09-05, build session).** The chassis `Config` carries no
  secrets, so `StoreConfig::from_config(&Config, StoreSecrets)` takes them
  as a second argument and stays pure. `StoreSecrets` holds the two hiqlite
  shared secrets and the at-rest key set as raw 32-byte keys; spec 030
  custodies them under `/data/keys`. `Debug` on secret-bearing types
  redacts.
- **D-5 (2026-09-05, build session).** `schema_version` has no timestamp
  column. hiqlite panics on `unixepoch()`, `time()`, and every other
  non-deterministic SQL function on the write path, because each follower
  must apply identical bytes. Consequence for every later spec: a
  timestamp that reaches the store is a caller-supplied value, never a SQL
  default.
- **D-6 (2026-09-05, build session).** `Migration.sql` may hold several
  statements separated by `;`; the store splits and applies them inside
  the same `txn` that records the version. Trigger bodies, which contain
  `;`, are not supported by the splitter. Migrations are validated as a
  list (strictly ascending, never `0`) before anything runs; a recorded
  version is skipped regardless of its text; an unapplied version below
  the recorded one is `Error::Conflict`.
- **D-7 (2026-09-05, build session).** hiqlite's `backup()` returns
  nothing, so `BackupId` is the newest `backup_node_<id>_<ts>.sqlite` that
  appears in the local listing after the call. The follower check runs
  before the call through `is_leader_db`, so a follower gets
  `Error::Stale` without a round-trip. `backup_list_s3` without a target
  is `Error::Config` rather than hiqlite's empty list.
- **D-8 (2026-09-05, build session).** The cache group has one index,
  `Cache::Kv`; spec 012 grows the enum through an `extends` edge on
  `store.rs`. `Store` owns the lifecycle (`open`, `shutdown`) and derefs
  to the clonable `StoreHandle` that carries every operation.
- **D-9 (2026-09-05, build session, needs human review).** hiqlite's tree
  fails the 010 B-3 policy on two counts, and this spec extends `deny.toml`
  additively rather than weakening the policy: `CDLA-Permissive-2.0` is
  allowed for `webpki-root-certs` (the Mozilla root store as data), and
  `RUSTSEC-2026-0194` and `RUSTSEC-2026-0195` (quick-xml 0.39, both
  denial-of-service in XML parsing) are ignored with reasons, because the
  fixed quick-xml is a semver-breaking jump that no released `cryptr`
  under hiqlite 0.14's pin can reach, and the only XML the workspace
  parses is the listing of its own configured backup bucket. The ignores
  are to be removed the moment hiqlite lifts `cryptr`. Alternative
  rejected: a `[patch]` to a fork, which the constitution forbids.

## Verification

```verify:cli
cargo test -p rahi-store --locked
```
