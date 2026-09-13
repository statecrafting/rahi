---
id: "036-manifest-and-schema-evolution"
title: "Manifest and schema evolution: a ledgered manifest transition, a checked migration history, and a restore that refuses what it cannot serve"
status: draft
kind: kernel
domain: kernel
created: "2026-09-11"
authors: ["Bartek Kus"]
implementation: pending
risk: critical
wave: 3
depends_on:
  - "011-store-hiqlite"
  - "013-ledger-decision-chain"
  - "014-ledger-sealing-and-archive"
  - "015-kernel-manifest-and-adjudication"
  - "030-operational-verbs"
  - "031-single-container-packaging"
  - "032-cluster-topology"
establishes:
  - "crates/rahi-ledger/src/transition.rs"
  - "crates/rahi-ledger/tests/transition.rs"
  - "crates/rahi-cli/tests/evolution.rs"
extends:
  - { spec: "013-ledger-decision-chain", unit: "crates/rahi-ledger/src/chain.rs", nature: additive }
  - { spec: "013-ledger-decision-chain", unit: "crates/rahi-ledger/src/verify.rs", nature: additive }
  - { spec: "013-ledger-decision-chain", unit: "crates/rahi-ledger/src/lib.rs", nature: additive }
  - { spec: "014-ledger-sealing-and-archive", unit: "crates/rahi-ledger/src/segment.rs", nature: additive }
  - { spec: "015-kernel-manifest-and-adjudication", unit: "crates/rahi-kernel/src/lib.rs", nature: additive }
  - { spec: "011-store-hiqlite", unit: "crates/rahi-store/src/migrate.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-ops/src/migrate.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-ops/src/restore.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-ops/src/archive.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/src/lib.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/src/serve.rs", nature: additive }
  - { spec: "031-single-container-packaging", unit: "docker/entrypoint.sh", nature: additive }
  - { spec: "032-cluster-topology", unit: "deploy/k8s/migrate-job.yaml", nature: additive }
references:
  - { unit: { kind: file, path: "docs/design/01-consumer-contract.md" }, role: context }
summary: >
  Today a cell's manifest is frozen at its first boot: the manifest hash is
  the chain's genesis parent, Ledger::open checks the first record against
  the booted manifest on every open, and any change to the manifest (one
  added grant, otel switched on) makes serve exit 1 with an integrity
  error on the existing volume. Spec 015 B-8 names the missing step, "a
  deploy genesis record", and nothing defines it. Migrations have the
  mirror problems: serve accepts a store ahead of the binary, a changed
  migration under an applied version is skipped silently, and restore
  checks no compatibility. This spec makes a manifest change a signed
  transition record appended at deploy, checks the booted manifest against
  the chain's current manifest, records a checksum per migration, lets an
  older binary serve a newer schema only across migrations declared
  additive, and makes restore refuse an archive it cannot serve. It carries
  the worked consumer example for an upgrade, a rollback, and a restore.
---

# 036: Manifest and schema evolution

## 1. Purpose

The chain commits to what the cell was permitted to do (015 §1). As built
it commits to the first manifest only, and refuses to boot under any
other. Reproduced on 2026-09-11 with an out-of-tree cell at `444bcf8`:
adding one `db.migrate` grant and rebuilding made `serve` and `ledger
verify` exit 1 with `integrity: ... record(s) unreachable from the genesis
parent`, while `migrate` exited 0; reverting the manifest made the volume
boot again. The operator is told the chain is broken when what changed is
the ceiling, and the only way to widen the ceiling while keeping the chain
is not to.

The migrations beside it are forward-only (constitution IX), and three
things about them are unchecked: a store ahead of the binary is served
without a word (`rahi_ops::migrate::check_current` refuses only behind);
`schema_version` records a version and a name, so a migration whose SQL
changed under an applied version is skipped as applied (reproduced: a
version 1 with different SQL never ran); and `restore` reads the archive's
manifest hash and versions and compares neither with the binary.

A consumer upgrading, rolling back, or restoring needs one procedure that
keeps the chain linear and verified (constitution XI), keeps migrations a
deploy step, and never lets a binary serve a ceiling or a schema the chain
and the store have not recorded.

## 2. Territory

`rahi-ledger` gains `transition.rs`, the manifest transition record and
the current-manifest reader. Additive changes to the chain's open and
verification (013), the segment header (014), the kernel's boot check
(015), the store's migration history (011), the `migrate` and `restore`
verbs and the archive manifest (030), the serve path (030), the container
entrypoint (031), and the migration Job (032). Two new test files.

## 3. Behavior

### Manifests

- **B-1 (identity).** The manifest hash is unchanged (015 B-2). The
  chain's *current manifest* is its genesis parent until the first
  transition, then the `to` of its latest transition record.
- **B-2 (the transition record).** A decision of kind
  `manifest.transition` whose payload carries `from` (the current manifest
  hash), `to` (the adopted one), `model` (the adopted manifest's canonical
  JSON, the bytes its hash is computed over, when it fits
  `ledger.max_record_bytes`; otherwise its hash and the text travels in
  `note`), `schema_version` (the store's, after the deploy's migrations),
  `binary` (the rahi version and the cell's `contract.version`), and
  `actor` (the operator's `sub`, or `system:deploy`). It is appended
  through `Ledger::append`, synchronously, and is CAS-protected like every
  record (013 B-3).
- **B-3 (adoption is a deploy step).** `rahi migrate --adopt-manifest`
  applies the cell's migrations and then, on the leader, appends a
  transition when the booted manifest differs from the chain's current
  one; when they are equal it appends nothing and says so. It prints the
  grants added and removed. `docker/entrypoint.sh` and the migration Job
  pass the flag, so a deployment adopts its manifest in the same step that
  migrates it. A follower refuses as `migrate` does (exit 2).
- **B-4 (the boot check).** `Ledger::open` verifies the chain from its
  stored genesis record: the genesis parent is read from the chain, not
  supplied by the booted manifest, and signatures by the cell's ledger key
  remain the anchor (013 B-5). `Kernel::boot` then compares the booted
  manifest's hash with the chain's current manifest. A mismatch is
  `Error::Stale` (exit 2) naming both hashes and the command
  `rahi migrate --adopt-manifest`, never `Error::Integrity`: an unadopted
  manifest is a missing deploy step, not tampering. A broken link, a bad
  signature, or a fork stays `Error::Integrity` and fatal (constitution
  XI).
- **B-5 (sealing).** A sealed segment's header records the current
  manifest at its tail, so the current manifest is known at
  `Depth::Resident` after its transition record has been sealed away
  (014).
- **B-6 (provenance).** Unchanged: every denial's payload names the
  manifest hash it was judged under (015). Between a transition and the
  last old replica leaving, both hashes appear in the chain, each on the
  decisions its binary made.

### Migrations

- **B-7 (a checksum per migration).** `schema_version` gains a `checksum`
  column, sha256 over the migration's SQL, added by a store baseline
  migration. `migrate` and `serve` refuse a recorded version whose
  checksum differs from the binary's (`Error::Integrity`, naming the
  version). A row recorded before this spec has no checksum; the first
  `migrate` under this spec records the binary's, and checks from then on.
- **B-8 (additive migrations).** A migration declares whether it is
  additive (`Migration::additive()`: it only creates tables, indexes, or
  nullable or defaulted columns), and the declaration is recorded with
  it. `serve` accepts a store ahead of the binary only when every applied
  version above the binary's last is recorded additive; otherwise it
  refuses with exit 2, naming the first non-additive version. Behind stays
  exit 2 as today (030 B-2).

### Restore

- **B-9 (restore checks before it writes).** `restore` reads the running
  cell's manifest and migrations and the archive's `manifest.json`, whose
  `manifest_hash` names the chain's current manifest at backup time. It
  refuses with exit 2, writing nothing, when the archive's schema is
  ahead of the binary across a non-additive migration, or when the
  archive's current manifest differs from the binary's and `--adopt` was
  not given. With `--adopt` it restores, and the next deploy step's
  `--adopt-manifest` appends the transition.

### Rolling updates at N=3

- **B-10 (one transition per deploy).** The migration Job runs `migrate
  --adopt-manifest` once, on the leader, before the rollout (032 B-6).
  Replicas still running the old image keep serving and keep stamping the
  old hash on their decisions; a replica that restarts on the old image
  after the transition refuses to boot (B-4) instead of serving a ceiling
  the chain no longer names.

## 4. Functional requirements

- **FR-001.** A ledger test appends transitions H1 to H2 to H1 and asserts
  the chain verifies at both depths, the current manifest is H1, and a
  forged transition signed by another key is `Error::Integrity`.
- **FR-002.** A ledger test seals a window whose last record is a
  transition and asserts the current manifest survives at
  `Depth::Resident`.
- **FR-003.** `tests/evolution.rs` drives the worked example below end to
  end with two fixture cells that differ by one grant and one additive
  migration, asserting every exit code the example names.
- **FR-004.** A store test changes the SQL of an applied version and
  asserts `migrate` and `serve` refuse, naming the version.
- **FR-005.** A restore test refuses a newer non-additive archive and an
  unadopted manifest before writing anything, and accepts both with the
  conditions B-9 names.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p rahi-ledger --locked`, `cargo test -p
  rahi-store --locked`, `cargo test -p rahi-ops --locked`, and `cargo test
  -p rahi-cli --locked --test evolution` pass.
- **AC-2.** Spec 015 AC-2 and spec 013's boot verification hold: a chain
  with no transition verifies byte for byte as before this spec.
- **AC-3.** `docs/design/01-consumer-contract.md` section 7 is replaced by
  the procedure below, and `deploy/README.md` names the flag.

### The worked consumer example

A cell ships v1 with manifest H1. v2 adds a grant (`items-migrate`) and an
additive migration 2. The chain before: `genesis(parent H1)`, then
decisions stamped H1.

Upgrade, N=1 (the entrypoint) or N=3 (the Job, then the rollout):

```sh
rahi migrate --adopt-manifest   # applies 2 (additive), appends transition H1 -> H2, exit 0
rahi supervise                  # boots: current manifest H2 == booted H2
```

Chain after: `genesis(H1) ... transition(H1 -> H2, schema 2) ...`, then
decisions stamped H2.

Rollback to v1: the v1 binary against a store at schema 2. Migration 2 is
additive, so v1 may serve it (B-8), and the ceiling returns through the
same deploy step, run with the v1 image:

```sh
rahi migrate --adopt-manifest   # nothing to apply; appends transition H2 -> H1
rahi supervise                  # boots on H1
```

Had migration 2 not been additive, v1's `serve` would refuse with exit 2
naming version 2: the rollback is then a forward fix, never a down
migration.

Restore a v1 archive into a v2 deployment: `rahi restore <archive>`
refuses (the archive's current manifest is H1, the binary's H2);
`rahi restore <archive> --adopt` restores; the next `rahi migrate
--adopt-manifest` applies migration 2 and appends H1 -> H2.

Without this spec, every step above that changes the manifest ends in
`integrity`, exit 1.

## 6. Out of scope

- Changing the hash definition (015 B-2) or what a grant means.
- Down migrations (constitution IX).
- Key rotation for the ledger key.
- Manifest extraction from source (lineage: a later spec if a product
  needs it).
- Retiring a chain: a cell that wants a new chain still stands up a new
  volume.

## 7. Resolved decisions

None yet. Before approval a human decides:

- whether adoption is a flag of `migrate` (proposed, which keeps spec 030
  AC-2's exact verb list) or a verb of its own (which amends 030 B-1 and
  AC-2);
- whether the transition carries the whole canonical model or only its
  hash (proposed: the model when it fits);
- whether B-8's additive marking is right, or whether a store ahead of
  the binary is refused always, or accepted always as today;
- whether an old replica that restarts during an N=3 rollout may boot on
  the *previous* manifest for a bounded window instead of refusing (B-10);
- the genesis-parent change in B-4, which moves the anchor from the booted
  manifest to the stored genesis record plus the ledger key's signatures,
  and is a change to spec 013's verification a human approves explicitly.

## Verification

```verify:cli
cargo test -p rahi-ledger --locked
cargo test -p rahi-store --locked
cargo test -p rahi-ops --locked
cargo test -p rahi-cli --locked --test evolution
```
