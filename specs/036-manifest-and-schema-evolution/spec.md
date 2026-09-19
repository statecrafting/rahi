---
id: "036-manifest-and-schema-evolution"
title: "Manifest and schema evolution: a ledgered manifest transition, a checked migration history, and a restore that refuses what it cannot serve"
status: approved
kind: kernel
domain: kernel
created: "2026-09-11"
authors: ["Bartek Kus"]
implementation: complete
risk: critical
wave: 3
depends_on:
  - "010-workspace-and-core-types"
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
  - "crates/rahi-ops/tests/evolution.rs"
extends:
  - { spec: "013-ledger-decision-chain", unit: "crates/rahi-ledger/src/chain.rs", nature: additive }
  - { spec: "013-ledger-decision-chain", unit: "crates/rahi-ledger/src/verify.rs", nature: additive }
  - { spec: "013-ledger-decision-chain", unit: "crates/rahi-ledger/src/lib.rs", nature: additive }
  - { spec: "014-ledger-sealing-and-archive", unit: "crates/rahi-ledger/src/segment.rs", nature: additive }
  - { spec: "014-ledger-sealing-and-archive", unit: "crates/rahi-ledger/src/seal.rs", nature: additive }
  - { spec: "013-ledger-decision-chain", unit: "crates/rahi-ledger/tests/append.rs", nature: additive }
  - { spec: "015-kernel-manifest-and-adjudication", unit: "crates/rahi-kernel/src/lib.rs", nature: additive }
  - { spec: "015-kernel-manifest-and-adjudication", unit: "crates/rahi-kernel/src/manifest.rs", nature: additive }
  - { spec: "015-kernel-manifest-and-adjudication", unit: "crates/rahi-kernel/tests/adjudicate.rs", nature: additive }
  - { spec: "011-store-hiqlite", unit: "crates/rahi-store/src/migrate.rs", nature: additive }
  - { spec: "011-store-hiqlite", unit: "crates/rahi-store/src/lib.rs", nature: additive }
  - { spec: "011-store-hiqlite", unit: "crates/rahi-store/Cargo.toml", nature: additive }
  - { spec: "011-store-hiqlite", unit: "crates/rahi-store/tests/migrate.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-ops/src/migrate.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-ops/src/restore.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-ops/src/archive.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/src/lib.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/src/serve.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/tests/cli.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/src/verbs.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-ops/src/backup.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-ops/Cargo.toml", nature: additive }
  - { spec: "010-workspace-and-core-types", unit: "Cargo.toml", nature: additive }
  - { spec: "013-ledger-decision-chain", unit: "crates/rahi-ledger/Cargo.toml", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-ops/tests/common/mod.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-ops/tests/restore.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-ops/tests/backup.rs", nature: additive }
  - { spec: "037-identity-recovery-and-live-proof", unit: "crates/rahi-ops/tests/rauthy_restore.rs", nature: additive }
  - { spec: "032-cluster-topology", unit: "deploy/README.md", nature: additive }
  - { spec: "039-release-and-out-of-tree-packaging", unit: "CHANGELOG.md", nature: additive }
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
  JSON, the bytes its hash is computed over), `schema_version` (the
  store's, after the deploy's migrations), `binary` (the rahi version and
  the cell's `contract.version`), and `actor` (the operator's `sub`, or
  `system:deploy`). The manifest is retained whole or the adoption is
  refused: there is no truncated model, no overflow into a second field,
  and no hash-only record. D-6 states the serialization the size is
  measured over, the bound it is measured against
  (`ledger.max_record_bytes`), where the measurement happens, and what the
  refusal is. It is appended through `Ledger::append`, synchronously, and
  is CAS-protected like every record (013 B-3).
- **B-3 (adoption is a deploy step).** `rahi migrate --adopt-manifest`
  applies the cell's migrations and then, on the leader, appends a
  transition when the booted manifest differs from the chain's current
  one; when they are equal it appends nothing and says so. It prints the
  grants added and removed, or reports the diff **unavailable** when the
  previous ceiling's canonical text cannot be recovered through the
  supported read path; it never prints an empty diff in that case, and a
  read that fails is an error rather than an unavailable diff (D-14).
  `docker/entrypoint.sh` and the migration Job
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
  not given. Schema compatibility is **established before the destination
  is replaced, or the restore is refused**: an archive whose
  `manifest.json` records no history is judged on the history read out of
  the archived database itself, and refused with exit 2 when that evidence
  cannot be obtained or does not check (D-12). `--adopt` authorizes a
  manifest difference and nothing else: it never bypasses the schema check
  or an integrity check. With `--adopt` it restores a schema-compatible
  archive, and the next deploy step's `--adopt-manifest` appends the
  transition.

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
- **FR-006.** A ledger test builds the transition record for a manifest
  whose measured record is exactly `ledger.max_record_bytes` and asserts it
  is admitted, and for one a single byte over and asserts `Error::Validation`
  naming the measured size and the bound. An ops test runs
  `migrate --adopt-manifest` against an oversized manifest and asserts exit
  1, that `schema_version` did not move, and that the chain gained no
  record.
- **FR-007 (D-11).** A ledger test appends H1 to H2, verifies, then makes
  the resident read answer nothing and asserts `current_manifest` is
  `Error::Integrity` rather than the genesis parent, so an H1 image cannot
  boot on it; a second does the same for a chain whose transition has been
  sealed and whose segment read answers nothing. Unit tests over the census
  assert that a missing witness row and a row count below the witnessed
  total are both refused, and that a witnessed empty relation is accepted.
- **FR-008 (D-12).** Restore tests cover a legacy archive whose
  `manifest.json` records no schema: one whose archived database proves the
  baseline and restores, one whose archived database is ahead across a
  non-additive migration and is refused with `Error::Stale`, and one whose
  payload yields no usable evidence and is refused with `Error::Stale`. Each
  is run with `--adopt` both set and unset, and the destination is asserted
  untouched on every refusal.
- **FR-009 (D-13).** An ops test applies a migration, alters its SQL, adds a
  further pending migration, and asserts `check_current` is
  `Error::Integrity` naming the altered version rather than `Error::Stale`;
  a companion asserts the behind-version `Error::Stale` is unchanged when
  every checksum agrees.
- **FR-010 (D-14).** An ops test asserts that an adoption whose previous
  model cannot be recovered reports the diff unavailable, and that a
  recovery path whose read does not answer is an error rather than an
  unavailable diff.
- **FR-011 (D-15).** Ledger tests commit a real seal at the one instant the
  chain read is vulnerable to it, through the production read path and with
  no reliance on timing, and assert that `current_manifest` and
  `current_manifest_model` answer from evidence on one side of it: once
  where the crossing is from nothing sealed to a first segment, once where it
  is from a segment naming an older manifest to a newer one, and once for the
  model, which must not report a chain's retained manifest text as absent. A
  kernel test asserts the consequence the reads exist to prevent: after a
  chain adopts H1 to H2, a boot on the H1 image is refused with
  `Error::Stale` naming both hashes and the deploy step.
- **FR-012 (D-16).** An ops test builds an archive whose database carries a
  `schema_version` table in the shape spec 011 created, with neither checksum
  nor additive column, and asserts it is refused with `Error::Stale`, exit 2,
  naming the evidence that could not be read, with and without `--adopt`.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p rahi-ledger --locked`, `cargo test -p
  rahi-store --locked`, `cargo test -p rahi-ops --locked`, and `cargo test
  -p rahi-cli --locked --test evolution` pass.
- **AC-2.** Spec 015 AC-2 and spec 013's boot verification hold: a chain
  with no transition verifies byte for byte as before this spec.
- **AC-3.** `docs/design/01-consumer-contract.md` section 7 is replaced by
  the procedure below, and `deploy/README.md` names the flag.
- **AC-4 (the size boundary, D-6).** A manifest whose measured transition
  record is at most `ledger.max_record_bytes` is adopted with its `model`
  intact, and the appended record's stored bytes are no longer than the
  measure the preflight took. A manifest one byte over is refused with
  `Error::Validation` (exit 1) whose message names the measured size, the
  bound, and `ledger.max_record_bytes` as the setting that carries it; the
  run applies no migration, appends no record, and leaves the chain's
  current manifest where it was.
- **AC-5 (the corrections, D-11 to D-14).** `current_manifest` never
  answers an older manifest from a read that did not answer; `restore`
  applies nothing whose schema compatibility has not been established;
  `check_current` reports an altered applied migration as
  `Error::Integrity` even when the store is also behind; and an adoption
  reports an unavailable grant diff only when the chain genuinely holds no
  earlier manifest text. FR-007 to FR-010 are the tests that hold these.

- **AC-6 (the concurrency correction and the legacy boundary, D-15, D-16).**
  The manifest hash and the manifest model are answered from sealed and
  resident evidence taken together, so a seal committing while the chain is
  read cannot produce an answer older than the chain's, and an incoherent
  pair is refused rather than answered. The support a legacy archive actually
  has is stated as what the code does: an archived database with no
  `schema_version` table restores on the baseline it proves, and one whose
  table predates spec 036's columns is refused. FR-011 and FR-012 are the
  tests that hold these, and no criterion above is relaxed to make them pass.

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

The owner decided all five open questions on 2026-09-17 and flipped this
spec to `approved`. Each entry names the question as it stood, so the
record shows what was asked as well as what was answered.

- **D-1 (2026-09-17, owner decision; adoption is a flag).** Adoption is
  `rahi migrate --adopt-manifest`, the flag B-3 proposes, and not a verb of
  its own. This keeps spec 030 AC-2's exact verb list true and amends
  neither 030 B-1 nor AC-2. Alternative rejected: a separate verb, which
  buys a clearer name at the cost of amending a `complete` spec's
  acceptance criterion.
- **D-2 (2026-09-17, owner decision; the transition carries the model).**
  The transition record retains the canonical manifest content when it fits
  `ledger.max_record_bytes`, as B-2 proposes. Oversized content does not
  bypass that limit: before implementation this spec states the bounded
  reference or failure path it takes instead, and the build implements the
  stated one rather than deciding it at the keyboard. Alternative rejected:
  the hash alone always, which makes a transition cheap to append and
  useless to audit without the deploy's artefacts.
- **D-3 (2026-09-17, owner decision; additive marking stands).** B-8's
  declaration is right: an older binary serves a store ahead of it only
  across migrations explicitly recorded additive, and refuses otherwise.
  Incompatible rollbacks are refused; migrations stay forward-only.
  Alternatives rejected: refusing a store ahead of the binary always, which
  makes every rollout a hard cutover, and accepting one always, which is
  today's behavior and the reason an incompatible rollback can serve a
  schema it cannot read.
- **D-4 (2026-09-17, owner decision; an old replica refuses after
  adoption).** B-10 stands as written. A replica that restarts on the old
  image after the transition refuses to boot. An old replica already
  running may finish the rollout under the manifest its decisions record.
  Alternative rejected: a bounded window in which a restarting old replica
  boots on the previous manifest, which would let a ceiling the chain no
  longer names adjudicate live requests for the length of the window.
- **D-5 (2026-09-17, owner decision, explicit; the verification anchor
  moves).** B-4 is approved explicitly, as the spec reserved it. `Ledger::
  open` verifies the chain from its stored genesis record, reading the
  genesis parent from the chain rather than from the booted manifest, with
  the cell's ledger key's signatures as the anchor (013 B-5). `Kernel::
  boot` then checks the booted manifest against the chain's current
  manifest as a separate step, and a mismatch is `Error::Stale`, not
  `Error::Integrity`. This is the material change this spec asked a human
  to approve, and it is approved. What does **not** change: a broken link,
  a bad signature, or a fork remains `Error::Integrity` and remains fatal
  at boot (constitution XI). Verification is being re-anchored, never
  relaxed.

- **D-6 (2026-09-18, owner decision; the oversized manifest is refused,
  never reduced).** D-2 reserved the bounded reference or failure path for
  a manifest whose transition record does not fit, and required this spec
  to state it before implementation rather than decide it at the keyboard.
  The owner decides a bounded **failure** policy, and B-2 is reconciled
  with it above: the transition retains the canonical manifest when the
  complete serialized record fits `ledger.max_record_bytes`, and otherwise
  adoption is refused with `Error::Validation`, a diagnostic naming the
  size and the bound, and no transition appended. The manifest is never
  truncated, never moved into a second field, never written to storage
  outside the chain, and never silently reduced to hash-only evidence. A
  cell that outgrows its own `ledger.max_record_bytes` raises that bound in
  its manifest, which is itself a manifest change and so is adopted through
  the same step. Alternatives rejected: a `note` overflow field, which
  splits one record's evidence across two places an auditor must reassemble;
  external storage, which puts the evidence somewhere the chain cannot
  commit to; and a silent hash-only fallback, which is the outcome D-2
  already rejected, arrived at by accident instead of by choice.

  *The serialization the size is measured over.* The measure is the byte
  length of the record's canonical JSON, `SignedRecord::to_canonical_json`:
  the whole signed record, envelope and signature and public key together,
  key-sorted at every depth. That string is exactly what the `record`
  column stores and exactly what one line of `ledger export` carries, so
  the bound is measured over the bytes the chain actually holds rather than
  over the payload alone. `model` inside it is the manifest's own canonical
  JSON, the first half of what `Manifest::hash` digests (015 B-2), carried
  as a JSON string so that the model the record retains and the bytes the
  hash was taken over are the same bytes.

  *What is measured before the append, and how it is made deterministic.*
  Two fields of the record are not final until `Ledger::append` chains it:
  `previous_record_hash`, whose length is fixed at 71 characters for every
  hash this chain admits (013 B-1), and the envelope's `timestamp`, which
  is `revision:<n>`. The measurement therefore runs against a candidate
  record built with a placeholder parent of that fixed length and with `n`
  widened to the largest `u64`, so the measure is an upper bound that can
  exceed the appended record only by the decimal digits the revision did
  not need. A manifest within a few bytes of the bound can be refused when
  the record would in fact have fitted; that direction is the safe one and
  is the price of a preflight that never passes something the append then
  refuses.

  *Where it runs, and what that guarantees.* `migrate --adopt-manifest`
  takes the measure **before** it applies any migration, because every
  field the record carries is known at that point: `from` and `to` from the
  chain and the booted manifest, `model` from the booted manifest,
  `schema_version` from the cell's own migration list (the value the
  store's `schema_version` will hold once this run has applied them),
  `binary` from the build, and `actor` from the invocation. So the
  guarantee is exact: when the manifest is oversized, the run exits 1 with
  nothing applied and nothing appended, and the store and the chain are
  byte-for-byte as they were. When the preflight passes, the append cannot
  afterwards be refused for size.

  What the preflight does not guarantee is that the append succeeds. It can
  still lose the compare-and-swap past `APPEND_ATTEMPTS` (013 B-3), meet a
  store failure, or find the chain damaged, and each of those keeps its own
  error and its own exit code. It can also find, at the moment it reads the
  head, that another deploy step already adopted the same manifest, which
  B-3 answers by appending nothing and saying so. The size check is
  additionally kept on the append path itself, so a caller that builds a
  transition without going through the verb still cannot write a record
  past the bound.

- **D-7 (2026-09-18, build session; the transition's decision id).** B-2
  fixes the payload and leaves the id open. The id is
  `manifest:<head>:<to>`, each hash abbreviated to its last sixteen hex
  characters, which is the convention spec 015 D-8 already set for the boot
  nonce. It has to be unique for the life of the chain and it has to be
  fixed before the append, because spec 013 B-3 keeps the id across every
  compare-and-swap retry; naming the head the transition was built against
  gives both, and it distinguishes the four records a cell writes going H1
  to H2 to H1 to H2, which `manifest:<from>:<to>` would collide on.
  Alternatives rejected: an ordinal count of transitions, which is not
  answerable once a transition has been sealed away without adding a second
  counter to the segment header that spec 042 would then have to carry; and
  the target hash alone, which collides on any readoption.
- **D-8 (2026-09-18, build session; the two absences B-7 and B-8 create).**
  Both new facts are optional in their types and neither is ever inferred.
  `Migration::new` produces a migration that is **not** additive and
  `Migration::additive()` is the declaration B-8 names, so a migration that
  has not said it is safe to serve from an older binary has not said it;
  `RecordedMigration::additive` is `None` on a row written before this spec
  and `is_additive()` reads that as false for the same reason. A
  `SegmentHeader` sealed before this spec carries `current_manifest: None`,
  which `Ledger::current_manifest` reads as "this segment names none" and
  falls back through, rather than as a manifest. Absence is never
  permission (constitution) and never evidence. Alternative rejected:
  defaulting an undeclared migration to additive, which would make every
  pre-036 store look rollback-safe on no evidence at all.

- **D-9 (2026-09-18, build session; two absences the record cannot invent).**
  B-3 says the adoption step prints the grants added and removed, and B-9
  makes `restore` check the archive's schema. Both need a fact an older
  artefact may simply not carry, and in both cases the verb says so instead
  of guessing.

  The grant diff is against the manifest the chain previously named, whose
  *text* lives only in the previous transition record's `model`. A chain
  that has never transitioned holds no earlier manifest text (the genesis
  record carries the hash, not the model), and one whose last transition has
  been sealed away holds it in the archive, which a deploy step does not
  fetch. `Adoption::diffed` is false in both cases and the verb prints
  "grants added and removed: not shown; the chain holds no earlier manifest
  to diff against" rather than an empty diff, which would read as "nothing
  changed". Alternative rejected: fetching archived segment bodies from a
  deploy step, which would make the cost of adopting a manifest depend on
  how much history the cell has.

  An archive written before this spec records no migration history at all.
  `restore` still makes B-9's manifest check against it, because the
  archive's `manifest_hash` predates this spec too, and reports that the
  schema could not be checked (`restore::schema_checked`) rather than
  letting silence read as a check that passed. Alternative rejected:
  refusing every pre-036 archive, which would strand exactly the archives an
  upgrading consumer has.

- **D-10 (2026-09-18, build session; an absent root is not an empty chain).**
  B-4 says the genesis parent is read from the chain and does not say what an
  absent answer means. It has two possible causes with opposite consequences:
  there is no chain yet, or the read did not answer, which a local read
  reports as no rows rather than as an error (spec 016 D-2). Treating the
  second as the first writes a genesis record that is a valid
  compare-and-swap onto the real head, so it lands, persists, and leaves a
  `ledger.genesis` record in the middle of an audit chain. `Ledger::open`
  therefore cross-checks the resident and sealed counts before it writes
  genesis, and refuses with `Error::Integrity` when either is non-zero while
  no root was found. Constitution XI settles the direction: under doubt the
  ledger stops rather than guesses, and the refusal writes nothing.

  The same shape predates this spec (the previous `open` asked two reads and
  wrote genesis when both came back empty), and the same hazard is answered
  the same way twice more in this change, in the two column probes that
  refuse an empty answer from a table they have just created. The reads this
  spec adds that are **not** guarded fail closed on their own and are marked
  so in place: `Ledger::current_manifest` walking back to an older answer
  makes `Kernel::boot` refuse a manifest that was in fact adopted, and writes
  nothing. Alternative rejected: retrying the probe, which cannot tell a
  retry that succeeded from one that dropped its rows again.

- **D-11 (2026-09-18, owner decision; the manifest read establishes its own
  evidence, and D-10's safety note was wrong).** D-10 marked
  `Ledger::current_manifest` as a read that "fails closed on its own",
  reasoning that walking back to an older answer can only make
  `Kernel::boot` refuse a manifest that was in fact adopted. That reasoning
  is wrong and is corrected here; D-10's text is left standing so the record
  shows what was believed as well as what was found.

  The older answer is not safe, because an older *image* can match it. A
  cell that adopts H1 to H2 and then restarts a replica on the H1 image is
  exactly the case B-10 and D-4 refuse: the H1 replica must not boot. If the
  resident read comes back empty after a successful verification, the walk
  back reaches the genesis parent, which on that chain **is** H1, the booted
  manifest agrees with it, and the H1 image boots and adjudicates under a
  ceiling the chain no longer names. The sealed-history case has the same
  shape: the transition's segment header carries H2, and a segment read that
  comes back empty falls back to the same genesis parent with the same
  result. An absent answer never proves absence, and this read was relying on
  it twice.

  The correction is that the evidence is made to witness itself, and no
  fallback is taken past evidence that contradicts it:

  - **Each read carries its own census, from one snapshot.** The resident
    chain and the sealed headers are each read through one compound
    statement that always yields a witness row carrying that relation's
    `COUNT(*)` evaluated in the same statement, followed by the rows
    themselves. A missing witness row is a read that did not answer, and a
    row count below the witnessed total is a read that dropped rows; both are
    `Error::Integrity` and nothing is written. A separate `COUNT(*)` issued
    as its own statement is deliberately **not** what is used: two statements
    are two snapshots, and a legitimate append between them is
    indistinguishable from a lost row.
  - **What `open` established is carried forward, because it only grows.**
    `Ledger::open` verifies the chain before it returns, so it knows whether
    anything was resident and whether anything was sealed. Neither fact can
    become false afterwards: records are appended and segments are added,
    and a seal that empties the hot window adds the segment in the same
    transaction. `current_manifest` therefore refuses, with
    `Error::Integrity`, to read an empty resident answer as "no transition"
    when `open` saw records, or an empty segment answer as "nothing sealed"
    when `open` saw segments, instead of walking back to an older manifest.

  What this does not claim: a store that answers every statement
  consistently wrong is outside what reading that store can detect, and the
  guarantee is stated at that bound rather than beyond it. **D-15 corrects
  one thing this entry did claim.** "Each read carries its own census, from
  one snapshot" is true of each read and was taken to make the pair of them
  sound, which it does not: two statements are two snapshots of each other
  even when neither loses a row. What is preserved:
  a fresh chain with no transition still answers its genesis parent, a
  segment sealed before this spec still names no manifest and still falls
  through (D-8), and nothing on either path writes.

- **D-12 (2026-09-18, owner decision; unknown compatibility does not
  authorize a restore).** D-9's second paragraph let `restore` apply an
  archive whose `manifest.json` records no migration history, reporting
  through `restore::schema_checked` that the schema could not be checked.
  The owner does not approve restoring on that basis. That exception is
  superseded: D-9's first paragraph (the grant diff) stands, its second is
  replaced by this entry.

  The policy is: schema compatibility is established before the destination
  is replaced, or the restore refuses with `Error::Stale` (exit 2), naming
  the evidence that is missing and leaving the destination untouched. There
  is no unchecked-restore bypass and no flag that buys one. `--adopt`
  authorizes a manifest difference only; it never bypasses the schema check
  or an integrity check.

  A legacy archive is not stranded, because the evidence it lacks in its
  metadata it still carries in its payload. The archive's app part is the
  hiqlite snapshot, written byte for byte as the destination's database
  (`restore::reset_app_node`), so it is an ordinary SQLite database and its
  `schema_version` table is the same recorded history a live store answers
  from. `restore` reads that table out of the archived database and checks it
  exactly as it checks a recorded `ArchiveSchema`. So a supported legacy
  success path demonstrates its evidence: the history came from the archive
  itself. An archive whose payload cannot be opened, or that holds no
  `schema_version` table when its recorded version is above the baseline, or
  whose history is ahead of this binary across a non-additive migration, is
  refused. An archived database that holds no `schema_version` table at all
  has provably applied no migration, which is the baseline, and that is
  evidence rather than silence. `restore::schema_checked` therefore no longer
  reports a restore that happened without a check, because no such restore
  happens.

- **D-13 (2026-09-18, owner decision; integrity outranks staleness when the
  store is also behind).** B-7 says `migrate` and `serve` refuse a recorded
  version whose checksum differs from the binary's, and B-2 of spec 030 says
  a store behind the binary is `Error::Stale`. The spec was silent on which
  answers first when both are true, and `migrate::check_current` returned
  `Error::Stale` before it ever compared a checksum, so a store with an
  altered applied migration **and** a pending one reported only that it
  needed migrating. Running the named command would then apply the pending
  migration on top of a history the binary cannot vouch for.

  The checksum check runs first. A recorded version whose SQL differs from
  this binary's is `Error::Integrity` (exit 1) naming that version, whether or
  not the store is also behind, because it is a statement about what already
  ran rather than about what has yet to run. The ordinary behind-version
  refusal is unchanged when every recorded checksum agrees. The `migrate`
  path already had this order, since `StoreHandle::migrate` compares
  checksums before it applies anything; this makes `serve` agree with it.
  Alternative rejected: reporting staleness first because it is the more
  common case, which is how an altered migration came to be reported as
  routine work.

- **D-14 (2026-09-18, owner decision; the grant diff's bounded
  unavailability, and what is not an unavailability).** D-9's first paragraph
  stands and B-3 now carries it: the grant diff is reported **unavailable**
  when the previous ceiling's canonical text cannot be recovered through the
  supported read path, which is a chain that has never transitioned and one
  whose last transition has been sealed away. An empty diff is never printed
  in that case, because it would read as "no grants changed".

  What this entry adds is the distinction the implementation must keep. A
  read that fails is not an unavailability. The previous model is recovered
  through `Ledger::records`, which D-11 makes self-witnessing, so a read that
  does not answer is `Error::Integrity` and the adoption stops; only a chain
  that genuinely holds no earlier manifest text reports the diff unavailable.
  Alternative rejected: treating any `None` from the recovery path as an
  unavailable diff, which is what turned a failed read into an ordinary
  report.

- **D-15 (2026-09-19, owner-authorized correction; D-11's census is per
  read, and the answer is not).** D-11 made each of the two reads account for its own
  rows and called the pair sound. It is not. A seal commits the new segment
  and the deletion of the records it archived in one transaction (spec 014
  B-2), so the two reads can straddle it:

  1. the segment headers are read, and the newest one names H1;
  2. a seal moves the resident H1 to H2 transition into a new segment,
     adding that header and deleting those records atomically;
  3. the resident records are read, and no longer hold the transition;
  4. the answer combines the older headers with the newer records and is H1.

  Every guard D-11 added passes: each census accounts for its own rows,
  neither relation reads back empty, and `OpenedChain`'s two booleans are
  both still true. The result is the failure D-11 exists to prevent, reached
  by a different route: H1 is what an old image carries, so that image boots
  under a ceiling the chain no longer names (B-10, D-4). Reproduced on
  2026-09-19 against `757f647`, deterministically, by committing a real seal
  at that instant through the production read path;
  `crates/rahi-ledger/tests/transition.rs` holds the three regressions and
  all three fail without the fix below.

  **The mechanism.** Both relations are read in **one** statement, with both
  `COUNT(*)` witnesses evaluated in it, so one snapshot carries the whole
  answer and there is no instant between them for a seal to occupy. The
  snapshot is then made to check itself: the resident chain is ordered
  against the root *it* names, which is the last sealed segment's terminal
  hash or the genesis parent (spec 014 B-4), so evidence from two moments
  cannot satisfy the seam and an incoherent pair is `Error::Integrity`
  rather than an answer. There is no retry, bounded or otherwise: one atomic
  read has nothing to re-establish, and a store answering incoherently is the
  bound D-11 already states rather than something a second read of that store
  could settle. Nothing process-local is relied on, which matters because a
  seal runs on whichever replica is leading and a lock in this process would
  say nothing about that. Alternatives rejected: re-reading the segments
  after the records and comparing, which turns a legitimate concurrent seal
  into either a retry loop or a refusal and still leaves a window; and a
  process-local lock around the pair, which cannot see another replica's
  seal at all.

  **What is not changed, and why that is safe.** `Ledger::records` and
  `Ledger::verify_chain` still take more than one statement: the records and
  the root they are ordered against, and for verification the segment headers
  too. A seal crossing those reads cannot produce a wrong answer, because the
  root a later read names is the one the oldest resident record must link and
  a pair from two moments cannot satisfy it: the crossing surfaces as
  `Error::Integrity` and the caller fails closed. That is a refusal where a
  single snapshot would have answered, which is the safe direction and not
  the failure this entry corrects. Folding them onto the same snapshot is a
  legitimate later change and is deliberately not made here, because it moves
  code spec 013 and spec 014 own for a reason this correction does not need.

  **The seam this needs, and its cost.** A deterministic test has to commit
  the seal at exactly that instant, and the reads are inside one private
  function, so `rahi-ledger` gains a `read-interleave` feature carrying a
  hook that is run once inside the chain read. Nothing enables the feature
  but this crate's own dev-dependency on itself, so `cargo test -p
  rahi-ledger` exercises it and no build a consumer makes contains the
  field, the call, or any way to install one. The alternative was a
  timing-dependent race, which is not a regression.

- **D-16 (2026-09-19, owner-authorized correction; what legacy restore
  actually supports).** D-12 says a legacy archive "is not stranded, because the
  evidence it lacks in its metadata it still carries in its payload". That
  is true of an archived database that has applied nothing and false of one
  that has applied something: `restore::read_schema_version` selects
  `version, name, checksum, additive`, and a `schema_version` table written
  before this spec has only the first two (B-7 and B-8 added the others), so
  the read fails and the archive is refused with `Error::Stale`, exit 2,
  nothing written. The boundary is the table's shape rather than its
  contents: an empty pre-036 table is refused for the same reason.

  The refusal stands. It is the authorized direction under the constitution:
  the evidence cannot be read, so compatibility is not established, so the
  destination is not replaced (D-12). What is corrected here is the claim,
  not the code, and FR-012 is the representative fixture that holds it, so
  the supported set is what a test demonstrates rather than what a sentence
  asserts. Widening it (reading the two columns a pre-036 table does have,
  and treating every recorded version as neither checksummed nor additive)
  is a change to what `restore` accepts and belongs to a spec that states
  it, not to a correction of this one. Alternative rejected: leaving D-12's
  sentence standing, which reads as support that the only legacy fixture in
  the corpus, a four-column table no pre-036 store ever wrote, does not
  demonstrate.

## 8. Status

- **2026-09-18.** B-1 to B-10, FR-001 to FR-006, and AC-1 to AC-4 hold.
  `spec-spine verify 036-manifest-and-schema-evolution` passes all four
  declared commands, and `make ci` is green (gate, k8s, build, the whole
  workspace suite, clippy with warnings denied, fmt, and cargo-deny).

  AC-2's two halves, checked rather than assumed: `cargo tree -p rahi-kernel`
  still shows only `rahi-ledger`, `rahi-store`, and `rahi-types` as workspace
  dependencies (spec 015 AC-2), and a chain with no transition is byte for
  byte what it was. No record gained a field; the only shape this spec could
  have moved is the archived segment body, and a header that names no
  manifest is absent from the serialized form, so a pre-036 body round-trips
  unchanged and the committed fixture chains under
  `crates/rahi-ledger/testdata/chains/` keep verifying against the same
  bytes (`tests/transition.rs`,
  `a_chain_with_no_transition_is_unchanged_by_this_spec`).

  Three tests that asserted the behavior this spec replaces were rewritten
  to assert the new one, each keeping a note of what it used to say:
  the ledger's `a_ledger_booted_against_another_manifest_refuses_the_chain`
  (now reports the chain's own root), the kernel's
  `a_ledger_rooted_at_another_manifest_will_not_boot` (now `Error::Stale`
  naming the deploy step), and the store's
  `recorded_versions_are_skipped_even_when_their_sql_changed` (now the
  checksum refusal, still asserting that an unchanged applied version is
  skipped). Each is one of the three defects section 1 reproduces.

  Two things are deliberately unchanged and named so no reader infers them.
  `preflight` reports no unadopted manifest: this spec's Territory does not
  name it and spec 030 B-3's check list does not ask for it, so `serve` and
  `migrate --adopt-manifest` remain where that question is answered. And
  every check here lives in a binary that carries this spec: an older binary
  that carries it honors the additive rule, a binary built before it has no
  such check and cannot be given one, and a replica already running
  re-evaluates nothing after boot (B-10, D-4).

- **2026-09-18 (the corrections).** Four code-level findings were reproduced
  against the merged implementation (`ae12c69`) and fixed, each with a
  regression that fails without the fix. D-11 to D-14 record the decisions;
  AC-5 and FR-007 to FR-010 are what hold them.

  1. **`Ledger::current_manifest` failed open, not closed.** D-10 reasoned
     that walking back to an older answer could only refuse a manifest that
     was adopted. It can also *admit* an older image: on a chain that went
     H1 to H2, the genesis parent is H1, so an H1 replica agrees with the
     stale answer and boots under a superseded ceiling (B-10, D-4). The
     sealed-history path had the same shape through the segment headers. The
     resident and sealed reads now each carry their own `COUNT(*)` from the
     same statement, and neither falls back past what `open` verified.
     `crates/rahi-ledger/tests/transition.rs` reproduces both, and both fail
     when the guards are removed.
  2. **An unknown archive schema no longer authorizes a restore.** D-9's
     legacy exception is superseded by D-12. The history is read out of the
     archived database's own `schema_version` table, which is the same table
     a live store answers from because the app part *is* the destination's
     database. An archive that yields no such evidence is `Error::Stale`,
     exit 2, destination untouched. `restore::schema_checked` is gone;
     `restore::SchemaEvidence` names what each restore was judged on.
  3. **Integrity outranks staleness.** `migrate::check_current` returned
     `Error::Stale` before it compared a checksum, so an altered applied
     migration plus a pending one reported only that the store was behind.
     The checksum check runs first now (D-13). The `migrate` path already had
     this order through `StoreHandle::migrate`, and the regression asserts
     both paths.
  4. **The grant diff's limitation is explicit.** B-3 and D-14 carry it: the
     diff is reported unavailable only when the chain genuinely holds no
     earlier manifest text, and a read that does not answer is an error
     instead. `Ledger::current_manifest_model` is the supported recovery
     path and shares the guards from finding 1.

  Preserved and re-checked, not assumed: the oversized-manifest policy (D-6,
  AC-4), spec 030's independent command-set test, spec 015 AC-2's dependency
  shape, the fresh-chain and pre-036 fallbacks (D-8), and every existing
  integrity guarantee. Consumer-visible API movement is recorded in
  `CHANGELOG.md` under the unreleased 0.2.0 section.

- **2026-09-19 (the concurrency correction).** A fifth finding was reproduced
  against `757f647` and fixed. `Ledger::witnessed_chain` read the sealed
  headers and the resident records as two statements, so a seal committing
  between them could leave the transition in neither answer while both
  accounted for their own rows: `current_manifest` then named H1 on a chain
  that names H2, which is the manifest an old image carries (B-10, D-4), and
  `current_manifest_model` reported the retained text absent, which D-14
  reserves for a chain that genuinely holds none. Both relations now come
  from one statement and the snapshot is ordered against the root it names,
  so the seam between resident and sealed history is checked rather than
  assumed (D-15). The three regressions in
  `crates/rahi-ledger/tests/transition.rs` commit a real seal at that instant
  through the production read path, with no sleep and no race, and all three
  fail without the fix; `crates/rahi-kernel/tests/adjudicate.rs` holds the
  consequence, an H1 image refused after the chain adopted H2.

  D-16 records the second half: the legacy-restore support D-12 described is
  narrower than its sentence, because the archived history read selects two
  columns a pre-036 `schema_version` table does not have. The behavior is
  unchanged and correct (`Error::Stale`, exit 2, nothing written); the claim
  is corrected and FR-012 is the representative pre-036 fixture that holds
  it.

  Preserved and re-checked rather than assumed: the same-statement witness
  checks and the `OpenedChain` guards (D-11, FR-007), checksum precedence
  (D-13), the restore compatibility policy (D-12), the oversized-manifest
  policy (D-6, AC-4), spec 030's independent command-set test, the
  fresh-chain and pre-036 fallbacks (D-8), and the missing-row regressions.
  No acceptance criterion was relaxed; AC-6 adds to them.

## Verification

```verify:cli
cargo test -p rahi-ledger --locked
cargo test -p rahi-store --locked
cargo test -p rahi-ops --locked
cargo test -p rahi-cli --locked --test evolution
```
