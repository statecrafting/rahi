---
id: "046-named-migration-sets"
title: "Named migration sets: a linked library brings its own versioned, checksummed migration sequence, and never renumbers into the host's"
status: draft
kind: kernel
domain: store
created: "2026-09-24"
authors: ["Bartek Kus"]
implementation: pending
risk: critical
wave: 3
depends_on:
  - "011-store-hiqlite"
  - "012-store-coordination"
  - "030-operational-verbs"
  - "036-manifest-and-schema-evolution"
  - "045-store-receipts-and-work-claims"
establishes:
  - { kind: file, path: "crates/rahi-store/src/migration_set.rs", planned: true }
  - { kind: file, path: "crates/rahi-store/tests/migration_sets.rs", planned: true }
  - { kind: file, path: "crates/rahi-ops/tests/migration_sets.rs", planned: true }
extends:
  - { spec: "011-store-hiqlite", unit: "crates/rahi-store/src/migrate.rs", nature: additive }
  - { spec: "011-store-hiqlite", unit: "crates/rahi-store/src/lib.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/src/cell.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/src/lib.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/src/verbs.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-ops/src/migrate.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-ops/src/backup.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-ops/src/archive.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-ops/src/restore.rs", nature: additive }
refines:
  - { aspect: "the checksum is per (set, version); the unnamed history is the set named app", unit: { kind: symbol, id: "rahi_store::migrate::check_checksums" } }
  - { aspect: "the additive rule is applied per set", unit: { kind: symbol, id: "rahi_ops::migrate::check_ahead" } }
amends: ["045-store-receipts-and-work-claims"]
amends_sections: ["2-3-what-is-extended"]
references:
  - { unit: { kind: file, path: "docs/design/01-consumer-contract.md" }, role: context }
  - { unit: { kind: file, path: "CHANGELOG.md" }, role: context }
obligations:
  - id: "I-1"
    kind: invariant
    text: "A migration's identity is (set, version); versions are ordered only within their set, and no library's migration is ever renumbered into another set's sequence."
    anchor: "3-1-set-identity-and-naming"
  - id: "I-2"
    kind: invariant
    text: "The checksum recorded for (set, version) is sha256 over that migration's SQL, and a recorded checksum that differs from the binary's is refused before anything is applied, in any set."
    anchor: "3-4-checksums-and-the-additive-rule"
  - id: "I-3"
    kind: invariant
    text: "A store written before this spec keeps every schema_version row byte for byte; that history is the set named app, and no recorded checksum changes on upgrade."
    anchor: "3-5-upgrading-a-single-sequence-store"
  - id: "I-4"
    kind: invariant
    text: "migrate computes the whole cross-set plan, requirements included, before it applies any migration, and refuses an unsatisfiable or cyclic plan having applied nothing."
    anchor: "3-3-ordering-and-requirements-between-sets"
  - id: "I-5"
    kind: invariant
    text: "Schema compatibility is established per set before a restore replaces the destination, or the restore is refused."
    anchor: "3-6-backup-restore-and-first-boot"
  - id: "V-1"
    kind: verification
    text: "Set naming, planning, per-set checksums and the additive rule, the upgrade of a single-sequence store, and restore of archives with and without named sets are exercised against real stores."
    anchor: "verification"
    inputs:
      - "cargo test -p rahi-store --locked --test migration_sets"
      - "cargo test -p rahi-ops --locked --test migration_sets"
      - "cargo test -p rahi-store --locked --test migrate"
      - "cargo test -p rahi-ops --locked --test evolution"
summary: >
  A store today has one migration sequence, owned by the cell. A library
  linked into a cell (aicortex inside travel-memory) numbers its own
  migrations from 1, and so do the chassis's own tables
  (coordination_migration, 045's receipts), so the host must renumber
  everyone into its list and every renumbering is a checksum nobody else can
  reproduce. This spec gives each linked library a named migration set: the
  store records version, checksum, and the additive declaration per (set,
  version); migrate plans across sets with declared minimum-version
  requirements between them; serve, backup, and restore apply 036's
  checksum-first and additive rules per set. A store written before this
  spec keeps its schema_version rows byte for byte as the implicit set app.
  045's receipt_migration becomes the chassis set rahi.receipts.
---

# 046: Named migration sets

## 1. Purpose

Spec 011 B-4 gives a store one ordered list of migrations and spec 036 B-7
and B-8 record a checksum and an additive declaration beside each version.
Both assume the cell owns every migration. That stops being true the moment
a library with its own tables is linked into a cell:

- travel-memory, the first consumer of 045, links aicortex as a library in
  the same cell (045 D-13). aicortex's 012 B-1 numbers its migrations from
  1, and its draft spec 050 records the gap as Q-8 ("a reserved version
  range, a host-supplied offset, or rahi support for more than one
  migration namespace per store"), noting that the last is a rahi spec.
- The chassis already ships migrations a cell must place in its own list:
  `coordination_migration(version)` (012 D-5) and, drafted, 045's
  `receipt_migration(version)`. The host picks each version, so two cells
  record the same SQL under different numbers.

A renumbering host breaks 036 B-7 for the library: the library cannot
reason about its own history, cannot declare that its v3 needs its v2, and
cannot ship an additive v4 that a host's older binary may serve, because
the version a store records is the host's, not the library's. This spec is
the rahi support Q-8 asks for, decided by the owner on 2026-09-24 (D-1).

## 2. Territory

### 2.1 What is new

- `crates/rahi-store/src/migration_set.rs`: `SetName`, `MigrationSet`,
  `SetRequirement`, the cross-set planner, and the per-set history read.
- One table, `schema_set_version`, created by the store's own baseline (as
  036 B-7 added its columns), never by a cell.
- Two test files: `crates/rahi-store/tests/migration_sets.rs` and
  `crates/rahi-ops/tests/migration_sets.rs`.

### 2.2 What is extended

- `rahi-store` (011): `StoreHandle::migrate_sets` and
  `recorded_set_migrations` beside the existing `migrate` and
  `recorded_migrations`, which keep their signatures and their meaning for
  the set `app`; `coordination_set()` beside `coordination_migration`, which
  is not changed; re-exports in `lib.rs`.
- The `Cell` trait (030): a defaulted `fn migration_sets() -> Vec<MigrationSet>`.
  `Cell::migrations()` is unchanged and is the set `app`.
- `rahi-ops` (030, as 036 left it): `migrate::run`, `check_current`,
  `check_ahead`, `adopt`, the backup's `ArchiveSchema`, and the restore's
  schema check read and write per set.
- 036's checksum and additive rules are refined per set (the `refines`
  edges on `check_checksums` and `check_ahead`), not restated or changed.

### 2.3 What is amended

045 (a draft) ships its tables as `receipt_migration(version)` for the host
to number. This spec replaces that with the set `rahi.receipts` (B-11), the
`amends` edge on 045's section 2.3. If 045 is approved after this spec, the
owner may instead fold B-11 into 045's text before approval.

No approved spec's text changes.

## 3. Behavior

### 3.1 Set identity and naming

- **B-1 (a set).** `MigrationSet { name: SetName, migrations:
  Vec<Migration>, requires: Vec<SetRequirement> }`. A migration's identity
  is `(set, version)`. Versions are strictly ascending within a set and
  start at 1; version 0 stays the store's baseline (011). Two sets may both
  have a version 3; they are unrelated.
- **B-2 (names).** A `SetName` matches `^[a-z][a-z0-9-]*(\.[a-z][a-z0-9-]*)*$`
  and is at most 64 bytes. The name is the library's, chosen by the library
  and not by the host: aicortex ships the set `aicortex`, and a host cannot
  rename it, because a renamed set is a different set with no history. Two
  names are reserved:
  - `app`: the host cell's own migrations, which are `Cell::migrations()`.
    No library may use it.
  - the prefix `rahi.`: the chassis's own sets (`rahi.coordination`,
    `rahi.receipts`). No library or cell may use it.
  A cell whose sets repeat a name, or use a reserved name they do not own,
  is `Error::Validation` before anything is read.
- **B-3 (a set is renamed never).** Renaming a set, or moving a migration
  from one set to another, is a new set plus a migration in it; the old
  set's history stays recorded. A library that splits into two crates keeps
  its set name in one of them.

### 3.2 What the store records

- **B-4 (the table).** `schema_set_version(set_name TEXT NOT NULL, version
  INTEGER NOT NULL, name TEXT NOT NULL, checksum TEXT NOT NULL, additive
  INTEGER NOT NULL, PRIMARY KEY (set_name, version))`. Every named set's
  migration is recorded here, in the same `txn` that applies it, exactly as
  011 records a version today. Unlike a pre-036 `schema_version` row, a
  named-set row always carries a checksum and an additive flag, so there is
  no backfill path for it.
- **B-5 (the set app stays where it is).** The set `app` is recorded in
  `schema_version` as today (B-13). `recorded_set_migrations()` answers one
  history per set, reading `app` from `schema_version` and every other set
  from `schema_set_version`.

### 3.3 Ordering and requirements between sets

- **B-6 (requirements).** A `SetRequirement { set: SetName, min_version: u32 }`
  says this set needs the named set at or above that version. It is
  declared on the set (`MigrationSet::requires`) or, when only some
  versions need it, on a migration (`Migration::requires(set, min)`),
  which applies to that migration and every later one in its set. aicortex
  declares `requires rahi.receipts >= 1` and `rahi.coordination >= 1`.
- **B-7 (the plan).** `migrate` first reads every set's history with
  `query_consistent`, runs every checksum check (B-9), then computes the
  whole plan: the pending migrations of every set, ordered so that each
  migration runs after every requirement it carries is met by a recorded
  or earlier planned version. Among migrations that are ready together the
  order is chassis sets first, then libraries by name, then `app`, so that
  a host's migration can rely on every library it links. The plan is a
  pure function of the sets and the histories and is printed by `migrate
  --plan` without applying anything.
- **B-8 (refusal before any write).** A requirement naming a set the cell
  does not carry, a `min_version` above that set's last migration, or a
  cycle among requirements is `Error::Validation` naming the sets involved,
  with nothing applied (I-4). This follows 036 D-6's rule that the deploy
  step measures before it writes. Each migration then applies in its own
  `txn` with its record, as 011 does; a failure stops the run with every
  earlier migration applied and recorded, and the next run resumes the plan.

### 3.4 Checksums and the additive rule

- **B-9 (checksum per set and version).** 036 B-7 applies per `(set,
  version)`: every recorded row of every set the binary carries is compared
  with the binary's SQL for that set and version, and a mismatch is
  `Error::Integrity` naming the set and version. 036 D-13's order holds
  across sets: integrity is checked for every set before staleness is
  reported for any.
- **B-10 (additive per set).** 036 B-8 applies per set, and the additive
  declaration that 0.2.0 introduced travels with each migration of each
  set. `serve` refuses a store behind the binary in any set (`Error::Stale`
  naming the set and the command), and accepts a store ahead of the binary
  in a set only when every recorded version of that set above the binary's
  last is additive. A set recorded in the store that the binary does not
  carry at all is ahead from version 1 and follows the same rule.

### 3.5 Upgrading a single-sequence store

- **B-11 (045's receipts are a named set).** 045's tables ship as
  `rahi_store::receipt_set() -> MigrationSet` named `rahi.receipts`,
  versioned by rahi from 1 and requiring `rahi.coordination >= 1` (the
  claim sweep takes a lease, 045 B-19). `receipt_migration(version)` is not
  shipped. 045 is unbuilt, so no store has recorded its tables under any
  other name.
- **B-12 (the coordination set).** `coordination_set()` is the set
  `rahi.coordination`, whose version 1 is byte for byte
  `coordination_migration`'s SQL. A new cell carries the set. A cell that
  already recorded `coordination_migration` in its own list keeps it there:
  that row is part of `app`'s history and its checksum is unchanged. Such a
  cell may add the set later; its version 1 is `CREATE TABLE IF NOT
  EXISTS`, so it applies as a no-op and is recorded, and from then on the
  set owns every later coordination version.
- **B-13 (the implicit set).** On a store written before this spec,
  `schema_version` is the history of the set `app`: every row, name,
  checksum, and additive flag stays byte for byte, no row is moved into
  `schema_set_version`, and no checksum is recomputed (I-3). The only write
  this spec makes to an existing store's schema is creating
  `schema_set_version` in the baseline step. A cell that declares no named
  set behaves exactly as before this spec.

### 3.6 Backup, restore, and first boot

- **B-14 (backup).** `ArchiveSchema` gains `sets: BTreeMap<SetName,
  Vec<RecordedMigration>>`, every named set's history at backup time, and
  keeps `version` and `migrations` as the history of `app`. An archive of a
  store with no named set writes no `sets` key and keeps format 1.
- **B-15 (format, so an older binary fails closed).** An archive that
  records any named set is written with archive format 2. A 0.2.x binary
  refuses a format it does not read (`archive.rs` checks `format`
  exactly), so it cannot restore a store whose library schemas it would
  not check. A binary with this spec reads both formats.
- **B-16 (restore checks every set).** `restore` applies 036 B-9 and D-12
  per set before the destination is replaced (I-5): each set's archived
  history is checked against the running cell's sets for checksum and the
  additive rule, and a set the archive carries that the cell does not is
  judged ahead from version 1. When `manifest.json` records no `sets`, the
  history is read out of the archived database itself, as D-12 reads
  `schema_version`: a database with no `schema_set_version` table has
  provably applied no named set, which is evidence, not silence.
- **B-17 (first boot).** `first-boot` creates no schema and
  `first-boot --export` renders a key set and touches no store (032 B-2,
  037), so neither reads or writes a migration set. The entrypoint's
  `rahi migrate`, run between `first-boot` and `supervise` (031), and the
  migration Job at N=3 (032 B-6) apply every set's plan; `serve` checks
  every set at boot as B-10 states.

## 4. Functional requirements

- **FR-001.** A cell with sets `app`, `aicortex` (versions 1 to 3), and
  `rahi.receipts` (version 1) migrates a fresh store; each set's history
  reads back with its own versions and checksums; a second run applies
  nothing.
- **FR-002.** aicortex version 2 requiring `rahi.receipts >= 2` against a
  binary whose `rahi.receipts` ends at 1 is `Error::Validation` and the
  store is unchanged; a cycle between two sets is refused the same way.
- **FR-003.** An edited SQL under `(aicortex, 2)` is `Error::Integrity`
  naming the set and version, from `migrate` and from `serve`, even when
  `app` is also behind.
- **FR-004.** A store ahead of the binary in `aicortex` by one additive
  version serves; by one non-additive version it refuses with exit 2
  naming `aicortex` and the version.
- **FR-005.** A store written by the 0.2.0 migrate path, whose list held
  `coordination_migration(1)` and two app migrations, upgrades: its
  `schema_version` rows compare byte for byte before and after, and adding
  `rahi.coordination` records version 1 without changing a table.
- **FR-006.** A backup of a store with named sets is format 2 and restores
  under this binary; a backup with none is format 1 and byte-compatible
  with what 0.2.0 wrote; a format-2 archive whose `aicortex` history is
  ahead across a non-additive version is refused with nothing replaced.
- **FR-007.** `SetName` refuses an empty name, uppercase, a leading digit,
  more than 64 bytes, `app` from a library, and the `rahi.` prefix from a
  cell.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p rahi-store --locked --test migration_sets` and
  `cargo test -p rahi-ops --locked --test migration_sets` pass.
- **AC-2.** `cargo test -p rahi-store --locked --test migrate` and `cargo
  test -p rahi-ops --locked --test evolution` pass with their existing
  assertions: a cell with no named set behaves as before this spec.
- **AC-3.** `CHANGELOG.md` records the `Cell` trait method and archive
  format 2 under the next minor version, since both are consumer contract
  (039 B-2).

## 6. Out of scope

- Migrating aicortex, travel-memory, or statecraft onto sets. aicortex's
  050 Q-8 is answered by this spec; adopting it is aicortex's change.
- Down migrations, which 036 already refuses (the repair is forward).
- A set loaded at runtime or discovered from a dependency graph: the cell
  lists its sets in code, as it lists its migrations.
- A set spanning two stores. rauthy's store is never opened by the chassis.

## 7. Resolved decisions

- **D-1 (2026-09-24, owner decision).** Each linked library registers a
  named migration set in the host cell. The store tracks each set's
  version and checksum independently. Libraries never renumber into the
  host sequence. The spec is to cover set identity and naming, ordering
  and dependency between sets, the additive declaration carried per set,
  the checksum per (set, version), the upgrade of existing single-sequence
  stores with their existing migrations as an implicit named set whose
  checksums do not change, the restore and first-boot interaction, and
  045's receipt migration as a named set.
- **D-2 (2026-09-24, owner decision).** Approved specs are related by
  typed edges (`extends`, `refines`, `amends`), and their text is not
  edited. This draft is born `status: draft`; approval is a separate human
  act.
- **D-3 (2026-09-24, branch).** The spec is stacked on
  `spec/idempotent-receipts` (PR #79) rather than cut from `main`: it
  depends on and amends 045, which exists only on that branch, and a
  `depends_on` naming an id the registry does not hold would not compile.
  It takes ordinal 046 for the same reason 045 took 045.

### Open questions for the owner

1. **Where the named sets are recorded (B-4, B-5).** Proposed: a new
   `schema_set_version` table, leaving `schema_version` as the set `app`
   untouched. The alternative, a `set_name` column on `schema_version`,
   rewrites the table every 0.2.x binary reads.
2. **Rolling a binary back below this spec.** A 0.2.x binary reads only
   `schema_version`, so it serves a store whose library sets are ahead
   without checking them. B-15 makes its restore fail closed; its `serve`
   cannot be made to. Accept and document (proposed), or also record a
   non-additive sentinel in the set `app` so 036 B-8 refuses the rollback
   (which changes `app`'s history, against B-13)?
3. **Requirement granularity (B-6).** Per set and per migration
   (proposed), or per set only?
4. **Ready-together order (B-7).** Chassis sets, then libraries by name,
   then `app` (proposed), or a host-declared order?
5. **A set the binary no longer carries (B-10, B-16).** Judged ahead from
   version 1 (proposed, so an additive-only library can be unlinked), or
   always refused?
6. **Coordination adoption (B-12).** Leave existing placements in `app`
   forever (proposed), or require every cell to adopt `rahi.coordination`
   at the next minor?
7. **Archive format 2 only when a named set exists (B-15)**, or always,
   so every archive a new binary writes is refused by 0.2.x?
8. **Naming (B-2).** Is the grammar, the 64-byte bound, and the reserved
   `app` and `rahi.` enough, or should a library set carry its crate name
   exactly?

## Verification

Planned: the two new test files do not exist until the spec is built.

```verify:cli
cargo test -p rahi-store --locked --test migration_sets
cargo test -p rahi-ops --locked --test migration_sets
cargo test -p rahi-store --locked --test migrate
cargo test -p rahi-ops --locked --test evolution
```
