---
id: "030-operational-verbs"
title: "The verbs and the binary composer: preflight, migrate, backup, restore, ledger verify"
status: approved
kind: "kernel"
domain: "ops"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: in-progress
risk: critical
wave: 3
depends_on:
  - "014-ledger-sealing-and-archive"
  - "024-hardening"
establishes:
  - "crates/rahi-ops/Cargo.toml"
  - "crates/rahi-ops/src/lib.rs"
  - "crates/rahi-ops/src/preflight.rs"
  - "crates/rahi-ops/src/migrate.rs"
  - "crates/rahi-ops/src/backup.rs"
  - "crates/rahi-ops/src/restore.rs"
  - "crates/rahi-ops/src/archive.rs"
  - "crates/rahi-ops/src/rauthy_api.rs"
  - "crates/rahi-ops/tests/backup.rs"
  - "crates/rahi-ops/tests/restore.rs"
  - "crates/rahi-cli/Cargo.toml"
  - "crates/rahi-cli/src/main.rs"
  - "crates/rahi-cli/src/lib.rs"
  - "crates/rahi-cli/src/cell.rs"
  - "crates/rahi-cli/src/serve.rs"
  - "crates/rahi-cli/src/verbs.rs"
  - "crates/rahi-cli/tests/cli.rs"
extends:
  - { spec: "010-workspace-and-core-types", unit: { kind: section, file: "Cargo.toml", anchor: "workspace.dependencies" }, nature: additive }
summary: >
  Operations that exist. rahi-ops implements preflight (config, keys, both
  stores' health, disk), migrate (spec 011's migrations as a deploy step,
  leader-only), backup (one encrypted archive holding the app's hiqlite
  snapshot taken through the addon, rauthy's snapshot taken through
  rauthy's own API, and the key material that decrypts both), restore (a
  cluster reset that refuses partial input and is single-shot by a marker),
  and ledger verify (spec 013 over an exported chain). rahi-cli is the
  binary composer: an app implements the Cell trait and its main is
  rahi_cli::run(cell), which yields serve plus every verb with the four exit
  codes. Carries enrahitu://027.
---

# 030: The verbs and the binary composer

## 1. Purpose

A backup that separates the stores from their keys is worthless, and a
restore that re-applies on every restart turns a crash loop into repeated
data loss (enrahitu://027, enrahitu://032 §3.9). Both are designed out
here rather than documented around. The composer exists so that an app's
`main.rs` is one line and every cell has the same verbs.

## 2. Territory

The whole of `crates/rahi-ops` and `crates/rahi-cli` after this spec. First
boot, key generation, and supervision (031) are modules added to
`rahi-ops`.

## 3. Behavior

- **B-1 (Cell trait).** `trait Cell { fn manifest() -> &'static str; fn
  migrations() -> &'static [Migration]; fn routes(state: AppState) ->
  Router; fn operator_routes(state) -> Router { empty } }`. `rahi_cli::run
  (cell)` parses argv: `serve`, `preflight`, `migrate`, `backup`, `restore
  <archive>`, `ledger verify [--full]`, `ledger export <path>`, `supervise`
  (031), `first-boot` (031). Exit codes are `rahi_types::Error::exit_code`.
- **B-2 (serve).** Load config, open the store, open the ledger (fatal on
  integrity), boot the kernel with the manifest hash, build the edge with
  the idp routes and the cell's routes, listen on the configured address.
  `serve` refuses to start when `schema_version` is behind the cell's
  migrations (`Error::Stale`, exit 2) and prints the `migrate` command.
- **B-3 (preflight).** Checks, each reported pass or fail with a reason:
  config parses; `/data` writable; key files present with mode `0600`;
  the app's hiqlite opens and elects; the store's engine report
  (`StoreHandle::engine_report()`, spec 016) prints `extensions: none` and
  the configured `max_value_bytes`; rauthy answers on loopback; the ledger
  verifies at `Depth::Resident`; free disk above a threshold. Exit 1 on any
  failure; never mutates.
- **B-4 (migrate).** Runs `Store::migrate(cell.migrations())` on the
  leader, refuses on a follower with the leader named, takes a backup first
  when `--backup` is given, prints the report.
- **B-5 (backup).** Produces one archive `rahi-backup-<utc>.tar.age`
  containing: `app-hiqlite/` (the snapshot from `Store::backup()`),
  `rauthy/` (the snapshot fetched from rauthy's `POST /backup` then `GET
  /backup/local/<file>` through the loopback base with the admin token),
  `keys/` (the contents of `/data/keys`), and `manifest.json` (versions,
  hashes of each part, the manifest hash). The archive is encrypted with
  the deployment's backup key; `--to s3://` uploads it. A backup with any
  part missing is an error, not a partial archive.
- **B-6 (restore).** `restore <archive>` runs only when neither hiqlite
  node is running (it checks the lock files), verifies every part's hash
  against `manifest.json`, restores keys, the app snapshot (via hiqlite's
  restore path, purging the Raft log so peers rejoin by snapshot), and
  rauthy's snapshot (placed for rauthy's own restore on next start), then
  writes `/data/restore.marker` naming the archive. A marker naming the
  same archive makes a second `restore` a no-op with exit 0 and a message.
  There is no environment variable that triggers restore.
- **B-7 (ledger).** `ledger verify` runs spec 013's verification at
  `Depth::Resident` or `--full`; `ledger export` writes the resident chain
  plus segment references as attest-ledger JSON.

## 4. Functional requirements

- **FR-001.** A backup taken against a temp store and a stub rauthy backup
  endpoint contains all four parts; removing the stub makes `backup` fail
  with no archive written.
- **FR-002.** A restore of that archive into a fresh data directory yields
  a store whose ledger verifies and whose keys match; a second restore is a
  no-op; a tampered part is refused before anything is written.
- **FR-003.** `serve` against a store behind on migrations exits 2 and
  prints the migrate command.
- **FR-004.** `preflight` reports every check by name and exits 1 when the
  key file mode is wrong.
- **FR-005.** `rahi-cli` tests drive the binary with a fixture cell over
  argv and assert exit codes for each verb.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p rahi-ops --locked` and `cargo test -p rahi-cli
  --locked` pass.
- **AC-2.** `cargo run -p rahi-cli -- --help` lists exactly the verbs of
  B-1.

## 6. Out of scope

First boot, key generation, the supervisor (031); the backup schedule and
where archives land in a cluster (032).

## 7. Resolved decisions

- **D-1 (2026-09-07, corpus amendment; adds the engine report to B-3).**
  B-3's check list now names the store's engine report. Spec 016 built the
  two facts and their `Display` (`StoreHandle::engine_report()`, whose
  output is `extensions: none, max_value_bytes: <n>`) and made printing
  them from `preflight` its AC-2, but `preflight` is this spec's territory
  and B-3 did not list the check, so a diligent session building 030 would
  have closed every requirement it could read and still left 016's AC-2
  open. Spec 016's own `## 8. Status` surfaced that contradiction on
  2026-09-06 rather than resolving it, which is what the coherence guard
  asks of a build session; this entry is the human answer it was waiting
  for. The session that builds 030 adds the check, and flips spec 016 to
  `implementation: complete` in the same change. No `depends_on` edge is
  added: 016 is a built crate this spec calls a public method on, and
  declaring the edge would make 030 blocked by a spec that only 030 can
  unblock.

## Verification

```verify:cli
cargo test -p rahi-ops --locked
cargo test -p rahi-cli --locked
```
