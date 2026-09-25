---
id: "030-operational-verbs"
title: "The verbs and the binary composer: preflight, migrate, backup, restore, ledger verify"
status: approved
kind: "kernel"
domain: "ops"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: complete
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
  - "crates/rahi-ops/tests/common/mod.rs"
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
  - { spec: "025-api-tokens-and-resource-server", unit: "crates/rahi-idp/tests/bearer.rs", nature: additive }
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
  (031), `first-boot` (031), `ledger reindex <archive>` (042). Exit codes are
  `rahi_types::Error::exit_code`. A verb annotated with an ordinal is
  established by that spec and enters the binary when that spec lands;
  `ledger reindex` is the only mutating verb under `ledger`, and D-9 records
  why it is a verb rather than a flag on `ledger verify`.
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
- **AC-2.** `cargo run -p rahi-cli -- --help` lists exactly the **required
  verb set**, and no other. The required set is derived from B-1's argv list
  and the corpus lifecycle, never from what the binary happens to parse: it
  is every entry B-1 carries with no ordinal annotation, plus every entry
  annotated with the ordinal of a spec whose `implementation` is `complete`,
  plus every entry annotated with the ordinal of the spec the change under
  test is implementing. An entry annotated with any other ordinal must be
  absent, and an entry B-1 does not carry at all must be absent. So the help
  is complete for the binary that prints it rather than for a corpus that has
  not shipped, while a verb that ought to be there and is not is a failure
  rather than a definition.

  `rahi_cli::VERBS` is the declaration of that required set and is what the
  test compares the help against. `VERBS` is therefore itself under test: a
  verb the required set contains and `VERBS` omits is an AC-2 failure, not a
  redefinition of the criterion, and the test must assert `VERBS` against the
  required set rather than take it as given. Extending `VERBS` is part of
  landing the spec that adds the verb (D-9).

  Today the required set has nine members: B-1's seven unannotated entries
  plus `supervise` and `first-boot`, whose spec 031 is `complete`. `ledger
  reindex` is annotated `(042)`, whose implementation is `pending`, so it is
  excluded today and joins as the tenth in the change that implements spec
  042, and not before.

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

- **D-2 (2026-09-09, build session; mechanises B-6).** `restore` resets
  the app node at the file level rather than through hiqlite's restore
  path, and `refuse_env_restore()` runs before every node open. hiqlite
  0.14's only reachable restore is the `HQL_BACKUP_RESTORE` variable read
  at node start (`backup::restore_backup` is `pub` inside a private module),
  and setting a variable in-process is `unsafe` in edition 2024, which the
  workspace forbids. What that path does is four removals and one copy
  (`state_machine/db`, `state_machine/snapshots`, `state_machine/lock`,
  `logs`, then the snapshot placed as `db/hiqlite.db`), so the verb does
  exactly that; an emptied log is what makes peers rejoin by snapshot. B-6's
  "no environment variable triggers restore" is thereby enforced rather than
  assumed: `serve`, `preflight`, and every verb that opens the node refuse
  while the variable is set, and `preflight` reports it as its own check.
  rauthy's snapshot is placed under `<data>/restore/rauthy/`, outside
  rauthy's directory (constitution VIII), and the marker names it; spec
  031's supervisor is where it is handed to rauthy exactly once, through the
  child's environment, which is a safe `Command::env`. Spec 031 must also
  point rauthy's hiqlite at `<data>/rauthy`, since that is the lock file
  B-6's "neither node is running" probes for existence. Rejected: re-exec
  of the binary with the variable set (the mechanism B-6 denies, one process
  removed); calling hiqlite's private function (not reachable).
- **D-3 (2026-09-09, build session; a hold on B-5 against a real rauthy).**
  B-5's rauthy snapshot is fetched with the admin token, the API key spec
  021 B-5 carries in `Authorization: API-Key <token>`, and `rauthy_api.rs`
  does exactly that; FR-001 holds against a stub that accepts it. Against
  the rauthy at this repository's sibling checkout (v0.36), all four
  `/auth/v1/backup*` handlers call `validate_admin_session()` only
  (`src/api/src/backup.rs`, `principal.rs:534`): an admin *session* cookie,
  never an API key, with MFA forced for admins by default. No mechanism
  available to this session honours B-5's text against that rauthy: an API
  key is refused outright, and a headless admin login is a rauthy
  configuration change (MFA off for a backup principal) that is not a build
  session's to make. The verb is built to the spec's text and the mismatch
  is recorded here and in `## 8. Status` rather than papered over with a
  read of rauthy's directory, which constitution VIII forbids. The options a
  human can choose between: a rauthy change accepting an API key on the
  backup routes (upstream, or a fork this repository does not want), or a
  dedicated backup admin whose session the verb establishes. Spec 031's
  smoke test and spec 034's e2e are where a real rauthy first meets this
  verb.
- **D-4 (2026-09-09, build session; fixes the key file contract).** The
  key set is five files under `keys/`, named in `rahi_ops::KeySet`:
  `ledger.key` (base64 Ed25519 seed, spec 013's loader), `session.key`
  (raw HMAC bytes, spec 022's loader), `hiqlite.json` (a JSON
  `rahi_store::StoreSecrets`), `backup.key` (one age X25519 identity;
  the recipient is derived, so a backup and a restore need nothing else),
  and `rauthy_admin_token`; `rauthy_client_secret` is the sixth, minted at
  bootstrap and not required before it. Files are `0600`, the directory
  `0700`, and `preflight` checks the modes. Spec 031's `first-boot` writes
  these names and no others.
- **D-5 (2026-09-09, build session; refines B-1).** `Cell` gains two
  defaulted methods beyond B-1's four, `exposed() -> Vec<Route>` and
  `static_dir() -> Option<PathBuf>`, because spec 034's cell serves a page
  (020 B-7) and names public routes inside its root merge (024 B-3), and a
  trait with no way to say either would force every app to reach around the
  composer. `routes` is merged at the root and classified authenticated;
  `operator_routes` is nested under `/operator` behind the role gate. The
  `rahi` binary is `EmptyCell`, the chassis with no app, so AC-2 and FR-005
  have a subject and an operator can preflight a volume with the binary
  that will serve it.
- **D-6 (2026-09-09, build session; three environment variables B-2 needs
  and spec 010 does not carry).** `RAHI_LISTEN_ADDR` (default
  `0.0.0.0:8443`, the port spec 031 exposes) is where `serve` binds;
  `Config` has no listen address because spec 010 derives everything from
  the public URL, and a bind address is not derivable from it.
  `RAHI_RAUTHY_MODE=none` serves without identity, which spec 033 B-2 needs
  for a harness with no rauthy; the default `required` mounts the proxy,
  the session routes, and the resource metadata and refuses to serve without
  discovery. `RAHI_LEDGER_ARCHIVE_DIR` (default `<data>/ledger-archive`) is
  the filesystem archive `ledger verify --full` fetches segment bodies from;
  an object-store archive is spec 032's to configure.
- **D-7 (2026-09-09, build session; reads B-5's `--to`).** `--to` is a
  directory (the archive is written to `<name>.partial` and renamed) or
  `s3://bucket/prefix`, uploaded through `rahi_ledger::S3Archive` with
  credentials from `RAHI_BACKUP_S3_{ENDPOINT,REGION,ACCESS_KEY,SECRET_KEY,
  PATH_STYLE}`; spec 010 B-7 keeps credentials out of `Config` and spec 032
  provisions them. The default is `<data>/backups`. Reusing the ledger's
  bucket client adds no second S3 implementation.
- **D-8 (2026-09-09, build session; reads B-3 and B-4).** Three readings.
  `preflight` verifies the chain only when `kernel_decisions` exists and
  holds a record, because spec 013's `open` writes genesis on an empty
  table and B-3 says never mutates; an empty chain is reported as a pass
  that names what `serve` will do. The disk floor is 512 MiB, read through
  `fs4` because std has no free-space call and the workspace forbids
  `unsafe`. B-4's "refuses on a follower with the leader named" names the
  declared peers, because `rahi-store` exposes `is_leader()` and no leader
  accessor; adding one is spec 011's territory. The refusal is
  `Error::Stale` (exit 2) on purpose: like a store behind on migrations,
  a follower is a node that cannot proceed from where it is, and the
  message names where to go.

- **D-9 (2026-09-18, owner decision; amends B-1's argv list and AC-2).**
  B-1's list gains `ledger reindex <archive>`, established by spec 042 B-8,
  and AC-2 is reworded so that `--help` is judged complete for the binary
  that prints it rather than for a corpus that has not shipped.

  The alternative was to express the operation as `ledger verify --reindex
  <archive>`, a flag on a verb B-1 already names, which would have amended
  nobody and is the resolution the coherence guard prefers when a mechanism
  honors both texts. The owner rejects it: `ledger verify` is the read-only
  check an operator reaches for while diagnosing an incident, `reindex`
  writes to the store, and a mutating repair must not be reachable by
  mistyping a flag on a diagnostic. The cost is this amendment to a
  `complete` spec, taken by the owner rather than by a build session, and
  taken narrowly: no behavior of any existing verb changes, no functional
  requirement changes, AC-1 is untouched, and this spec's implementation
  stays complete.

  The AC-2 rewording repairs a drift this spec already carried rather than
  one 042 introduced. B-1 has annotated `supervise` and `first-boot` with
  `(031)` since this spec was written, so "exactly the verbs of B-1" was
  already a claim about a set the binary did not hold until 031 landed,
  while the test compared the help against `rahi_cli::VERBS`.

  The rewording derives the expected set from B-1's text and each annotated
  spec's `implementation` field, plus the spec the change under test is
  implementing, and never from the binary. That ordering matters: an earlier
  draft of this decision said the help lists the verbs of B-1 "that the
  binary implements", which would have made the criterion unfalsifiable,
  since a verb accidentally left out of the parser would have left itself out
  of the expected set too. `VERBS` is the declaration of the required set and
  is asserted against it, so it can no longer define a missing verb away. The
  arithmetic is unchanged by the tightening: the required set is nine today,
  because 031 is `complete` and 042 is `pending`, and becomes ten in the
  change that implements 042. `--help` lists nine today and AC-2 holds, as it
  held before this amendment.

  Recorded on the same day and in the same change as spec 042's D-1, which
  states the owner's reasoning from 042's side. Spec 042 is approved in the same
  change and holds at `implementation: pending`; this amendment authorizes no
  implementation and changes nothing this spec requires.
- **D-10 (2026-09-24, correction session, owner-authorized work order; reads
  no B-n: a test-support defect).** `free_addr` in
  `crates/rahi-ops/tests/common/mod.rs` and `free_port` in
  `crates/rahi-cli/tests/cli.rs` bound `127.0.0.1:0`, read the port, and
  dropped the listener before hiqlite (or the spawned cell) bound it. That
  is a time-of-check race: under parallel test binaries another process
  could take the port in the gap, and hiqlite then panicked in its
  `start.rs` with `valid RPC socket address: AddrInUse`. hiqlite binds the
  address itself and accepts no listener, so holding the socket until the
  bind is not available. The allocator now hands out ports from
  20000..32000, below the Linux (32768..60999) and macOS (49152..65535)
  ephemeral ranges so no outbound connection is given one, starting at an
  offset derived from the process id and advanced by an in-process atomic
  counter. Each candidate is claimed by an exclusive `File::try_lock` on
  `<temp>/rahi-test-ports/<port>.lock`, held until the process exits, and
  then probed with a bind; a held lock or a failed bind moves on to the next
  candidate. Every copy of the allocator in the workspace (the store,
  ledger, kernel, edge, idp, ops and cli tests and the harness) uses the
  same range and lock directory, so two concurrently running test processes,
  from this checkout or another, never hand out the same port. This applies
  to the verb tests. The helpers keep their signatures, so no caller
  changed. Rejected: a shared helper crate or a dev-dependency on
  `rahi-harness`, which would add a dependency edge from a lower-numbered
  spec to a higher one; and retrying on `AddrInUse`, which hiqlite reports
  as a panic rather than an error.

## 8. Status

- **2026-09-09.** B-1 to B-7, FR-001 to FR-005, AC-1, and AC-2 hold:
  `cargo test -p rahi-ops --locked` (10 tests) and `cargo test -p rahi-cli
  --locked` (11 tests) pass, `cargo run -p rahi-cli -- --help` lists the
  nine verbs of B-1 and no other, and a hand-driven `migrate`, `serve`,
  probes, SIGTERM, `ledger verify` round trip is clean. Spec 016's AC-2
  holds through this spec's preflight (`PASS engine: extensions: none,
  max_value_bytes: 1048576`) and 016 is flipped complete in this change per
  D-1. One thing is known and not closed: D-3, the backup verb's admin
  token against a real rauthy's backup routes. It needs a human choice
  between a rauthy-side change and a dedicated backup session, and it is
  first exercised by spec 031's smoke test.

## Verification

```verify:cli
cargo test -p rahi-ops --locked
cargo test -p rahi-cli --locked
```
