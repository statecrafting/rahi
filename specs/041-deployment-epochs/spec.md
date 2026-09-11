---
id: "041-deployment-epochs"
title: "Deployment epochs: the chain records each deployment it serves under, appended at the deploy step, linked to external build and deployment records it never rewrites"
status: draft
kind: kernel
domain: ledger
created: "2026-09-11"
authors: ["Bartek Kus"]
implementation: pending
risk: critical
wave: 3
depends_on:
  - "013-ledger-decision-chain"
  - "014-ledger-sealing-and-archive"
  - "015-kernel-manifest-and-adjudication"
  - "030-operational-verbs"
  - "032-cluster-topology"
  - "036-manifest-and-schema-evolution"
  - "040-runtime-identity-and-binding-surface"
establishes:
  - "crates/rahi-ledger/src/epoch.rs"
  - "crates/rahi-ledger/tests/epoch.rs"
  - "crates/rahi-cli/tests/epochs.rs"
  - "crates/rahi-cli/testdata/binding/"
extends:
  - { spec: "013-ledger-decision-chain", unit: "crates/rahi-ledger/src/lib.rs", nature: additive }
  - { spec: "014-ledger-sealing-and-archive", unit: "crates/rahi-ledger/src/segment.rs", nature: additive }
  - { spec: "014-ledger-sealing-and-archive", unit: "crates/rahi-ledger/src/seal.rs", nature: additive }
  - { spec: "015-kernel-manifest-and-adjudication", unit: "crates/rahi-kernel/src/lib.rs", nature: additive }
  - { spec: "023-observability", unit: "crates/rahi-edge/src/obs/metrics.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-ops/src/migrate.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-ops/src/archive.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/src/lib.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/src/serve.rs", nature: additive }
  - { spec: "032-cluster-topology", unit: "deploy/k8s/migrate-job.yaml", nature: additive }
  - { spec: "032-cluster-topology", unit: "deploy/README.md", nature: additive }
  - { spec: "040-runtime-identity-and-binding-surface", unit: "crates/rahi-ops/src/binding.rs", nature: additive }
references:
  - { unit: { kind: file, path: "docs/design/01-consumer-contract.md" }, role: context }
summary: >
  The chain commits to the first manifest (genesis) and, with spec 036, to
  every manifest adopted since. It records nothing about which build a
  replica runs or which deployment put it there, so a consumer cannot join
  a decision to an artifact, an authority snapshot, or a rollout, and an
  upgrade, a rollback, and a restore look the same in the chain. This spec
  appends one epoch record at each deploy step that changes what is
  deployed: the manifest current after the step, the artifact (the binary
  measured by the deploy step, the image digest the deployer declares),
  and typed opaque references to the external build provenance, the
  deployment record, and the authority snapshot. The genesis and every
  existing record stay byte for byte; no history is rehashed; nothing is
  embedded in a built artifact; every reference points from the later
  record to the earlier one, so no hash cycle is possible. Replicas bind to
  the current epoch at boot and report a mismatch without refusing, every
  decision names its epoch and instance, and the chain becomes readable
  from a running cell.
---

# 041: Deployment epochs

## 1. Purpose

Spec 013 roots the chain at the booted manifest; spec 036 (draft) makes a
manifest change a transition record. Neither says which artifact served a
decision, which deployment put it there, or whether a later record was
made by an upgrade, a rollback to an older build, or a restore. Spec 040
(draft) makes a replica report what it runs; this spec makes the chain
record what was deployed, so that a consumer's verifier can join
authority, build, deployment, and runtime identifiers offline from records
each party already signs.

The shape follows the family realignment of 2026-09-11 (handoff 02-rahi's
amendment; register B10, B12, and rahi's part of B30 and B32): a detached,
external build and deployment record plus an appended epoch linkage;
manifest-root semantics and the old genesis preserved; no in-place history
rewrite; no future runtime evidence inside a build; the TOML manifest
stays the ceiling and nothing extracted replaces it.

What exists today, verified at `444bcf8`:

- Records are `attest-ledger` envelopes with a sibling Ed25519 signature
  over `record_hash` (013 D-2); the payload is opaque to the published
  verifier, so a new decision kind verifies without a verifier change.
- `DecisionKind` is an open string; genesis is `genesis:<manifest hash>`
  with payload `{schema_version, genesis_parent}`.
- Every denial's payload names the manifest hash it was judged under.
- A backup's `manifest.json` records `versions {rahi, store_schema,
  ledger_schema}` and the manifest hash. `restore` writes the chain back
  as archived, restores the ledger key verbatim, and writes
  `/data/restore.marker` naming the archive and its sha256 (030 B-6).
- At N=3 the deploy step is the migration Job, a store client running the
  new image before the rollout (032 B-6); at N=1 it is the entrypoint's
  `migrate` before `supervise` (031).

## 2. Territory

`rahi-ledger` gains `epoch.rs`: the epoch record, its builder, and the
current-epoch reader. Additive changes to the ledger's exports (013), the
segment header and the seal that writes it (014), the kernel's decision
payload (015), `/metrics` (023), the `migrate` step, the archive manifest,
the verbs, and the serve path (030), the migration Job and the deploy
notes (032), and 040's binding document; `ledger verify` and `ledger
export` gain the attach path `migrate` and `backup` already have (030).
One ledger test, one end-to-end test, and the fixture templates it
composes.

## 3. Behavior

### The record

- **B-1 (an epoch).** An epoch is a stretch of the chain during which
  replicas serve one deployment: one artifact, one current manifest, one
  set of external references. Epoch 0 is the genesis record and needs no
  new record. A chain with no epoch record is at epoch 0, and every
  existing chain opens and verifies byte for byte as before: the genesis
  is unchanged, nothing is rehashed, nothing is backfilled.
- **B-2 (the epoch record).** A decision of kind `deployment.epoch`,
  outcome `allow`, actor the deployer's `sub` or `system:deploy`, id
  `epoch:<n>`, appended through `Ledger::append` synchronously (the CAS of
  013 B-3, never the denial queue). Its payload carries:
  - `epoch` (n) and `previous` (the record hash of epoch n-1, or of the
    genesis record when n is 1);
  - `cause`: `deploy` or `restore`;
  - `manifest`: the chain's current manifest after the step (036 B-1), and
    `transition`: the record hash of the 036 transition appended in the
    same step, or null;
  - `schema_version`: the store's, after the step's migrations;
  - `artifact`: `binary` (sha256 of the deploy step's own executable,
    measured, 040 B-2, with the `platform` it was built for; the step runs
    the image being deployed), `image` (the declared digest reference of
    040 B-5, or null), `rahi_version`, and `build_revision` (declared, or
    null);
  - `refs`: `build`, `deployment`, and `authority`, each null or a typed
    reference (B-3);
  - `restore`: null, or the marker's `{archive, sha256}` when `cause` is
    `restore`;
  - `wall_time`, from the deploy step's clock.
- **B-3 (references are typed and opaque).** The chassis records a
  reference and never fetches, parses, or trusts what it names. A
  reference is `{type, digest, id}`: `type` a URI naming the record's
  schema and version, `digest` `sha256:<64 hex>` over the record's
  original bytes, `id` an optional string the producer assigns. The deploy
  step reads them from `RAHI_DEPLOYMENT_REFS`, one JSON object with
  optional members `build`, `deployment`, and `authority`; an unknown
  member, a malformed digest, or a value over 4 KiB is `Error::Config`.
  Expected, not enforced: `build` names an in-toto Statement with an SLSA
  provenance predicate whose subjects include the image digest and the
  cell binary's sha256; `deployment` names the deployer's record for this
  rollout (Statecraft's, when Statecraft deploys); `authority` names the
  authority snapshot the build was accepted under (spec-spine's). The
  type URIs are agreed with those producers before approval (7).
- **B-4 (no cycle).** Every reference points from a later record to an
  earlier one: the authority snapshot is computed over source; the build
  provenance names the source, the snapshot, and the artifact it produced;
  the deployment record names the build and the artifact; the epoch record
  names all three and the artifact; a replica's binding (040) and each
  decision name the epoch. None of them is an input to a digest it is
  named by. A built artifact MUST NOT embed its own digest, a deployment
  id, an epoch, or an authority snapshot written after the build, and the
  chassis writes none of them into a built file. A deployment outcome a
  consumer records after the rollout may name the epoch record's hash,
  which keeps the direction.

### When an epoch is appended

- **B-5 (once per change, at the deploy step).** The deploy step (036
  B-3's `migrate --adopt-manifest`, run by the entrypoint at N=1 and by
  the migration Job at N=3) builds the candidate epoch after its
  migrations and any transition. When its manifest, binary, image, and
  refs equal the current epoch's, it appends nothing and prints the
  current epoch. Otherwise it appends epoch n+1 and prints it. A restore
  marker whose archive the current epoch does not name forces an epoch
  with `cause: restore` even when nothing else changed. A follower refuses
  as `migrate` does (exit 2).
- **B-6 (the current epoch).** `Ledger::current_epoch()` returns the
  newest epoch's number and record hash: from the resident chain, or, once
  the epoch record has been sealed away, from the newest segment header,
  which records the current epoch at its tail (014 B-2, additively, as 036
  B-5 does for the manifest); with neither, epoch 0 and the genesis
  record's hash.
- **B-7 (an epoch is its hash).** An epoch is identified by its record
  hash; its number orders it within one chain. A restore rewinds the
  chain, so the first epoch after a restore can reuse a number that the
  discarded timeline also used; the hash and `restore` tell them apart.
  A consumer that correlates by number alone conflates timelines.

### What a replica does with it

- **B-8 (bind at boot, report, do not refuse).** `serve` reads the current
  epoch after the ledger opens and before it listens, and 040's document
  gains `epoch {number, record_hash}` and `match`: `bound` when the
  measured binary, the declared image (when both sides declare one), and
  the booted manifest equal the epoch's; `mismatch` with the kinds that
  differ, from the closed set `binary`, `image`, `manifest`; `unbound` at
  epoch 0. The binary is compared only when the replica's platform equals
  the epoch's; a replica on another platform of a multi-architecture image
  reports the binary as `unknown` rather than a mismatch, and the
  consumer's join resolves it against the provenance, whose subjects name
  each platform's binary. A mismatch does not stop the boot: the ceiling
  is 036's to enforce and a binary is not a ceiling. It is one warning
  line at boot naming both values of each kind. `manifest` is reachable
  only when a deploy step appended 036's transition and failed before its
  epoch; it says the step did not finish.
- **B-9 (decisions name their epoch and instance).** Every decision the
  kernel emits from this spec on carries two scalar keys in its payload
  beside the `manifest` it already names: `epoch`, the replica's booted
  epoch number, and `instance`, the string `instance.id` of 040 B-4 (not
  040's whole `instance` object). Records already in a chain are untouched,
  and a payload without the keys was written before this spec. During an
  N=3 rollout, decisions from old and new replicas interleave, and each
  names its own epoch; position in the chain never assigns one.
- **B-10 (bounded signals).** `/metrics` gains `rahi_binding_epoch`, a
  gauge whose value is the booted epoch number with no label, and
  `rahi_binding_mismatch{kind}`, a gauge of 0 or 1 whose `kind` is B-8's
  closed set. No digest, reference, or instance id is a label.

### Backup, restore, and export

- **B-11 (the archive names its epoch).** The archive manifest (030 B-5)
  records the current epoch's number and record hash beside the manifest
  hash, so an operator knows which deployment a backup was taken under
  before restoring it.
- **B-12 (restore is an epoch).** `restore` still writes nothing to the
  chain (it runs on a stopped volume, 030 B-6). The next deploy step reads
  the marker and appends the restore epoch (B-5), whose `previous` is the
  archived current epoch. Records appended after the backup and before the
  restore are not in the restored chain and the chain does not pretend
  otherwise; a consumer that holds them holds a discarded timeline. The
  restore epoch also moves the head before a replica mints an id, so the
  N=1 path cannot re-mint ids from the discarded timeline; at N=3 that
  holds only when the Job runs before the replicas start, which the deploy
  notes say.
- **B-13 (the export states its coverage).** `ledger export <path>` also
  writes `<path>.coverage.json`: the depth exported, the resident range,
  the sealed segment keys referenced and not included, the verifying
  public key, the current epoch, and the standing gaps: allows are never
  recorded (015 D-5), and a denial can be lost under the causes spec 035
  B-4 names, whose counters are per process and not in the export. The
  JSONL bytes are unchanged, so every existing reader and the published
  verifier see what they saw before.
- **B-14 (the chain is readable from a running cell).** `ledger verify`
  and `ledger export` open the store as `migrate` and `backup` do
  (`Booted::open_or_attach`): attached to the replica's running node, or
  as a pure store client under `RAHI_STORE_CLIENT`. Today they call
  `Booted::open`, which starts a node on the data directory, so a live
  cell's epochs and denials can be read only after stopping a replica.
  Neither verb writes; an export taken while replicas append is a prefix
  of the chain, ending at the head it read.

## 4. Functional requirements

- **FR-001.** A ledger test appends epochs 1 to 3 with a transition
  between 1 and 2 and asserts: the chain verifies at both depths; each
  `previous` names its predecessor's record hash; `current_epoch()` is 3
  from the resident chain and still 3 at `Depth::Resident` after a seal
  moves epoch 3 into a segment; a chain with no epoch record reports epoch
  0 and the genesis hash.
- **FR-002.** 013's `testdata/chains/` fixtures and 014's seal fixtures
  verify unchanged without regeneration, and an export containing epoch
  records passes `attest-ledger verify` (the `attest-ledger` binary of the
  `attest-ledger-cli` package; skipped with a message when it is not
  installed, as 013 FR-004 is).
- **FR-003 (the composed consumer fixture).** `tests/epochs.rs` plays each
  producer in order from the templates in `testdata/binding/`: a fixture
  authority snapshot digest; a build provenance Statement whose subjects
  are a fixed image digest and the sha256 of the fixture cell's
  executable and whose materials name the snapshot; a deployment record
  naming the Statement's digest and the image. It runs the deploy step
  with `RAHI_DEPLOYMENT_REFS` naming all three, serves, provokes one
  denial, and joins with a reference join written in the test (the
  consumer's logic, not a chassis feature): snapshot to Statement
  materials; Statement subjects to the epoch's `artifact.binary` and
  `artifact.image`; deployment record digest to `refs.deployment`; the
  epoch record hash to `/binding`'s `epoch.record_hash`; the denial's
  `epoch` and `instance` to the binding. It asserts that each digest was
  computed over bytes containing only digests of records earlier in that
  order.
- **FR-004 (a mismatch is observable).** The same volume served by a
  second fixture binary with the same manifest reports `mismatch:
  [binary]` on `/binding` and `rahi_binding_mismatch{kind="binary"} 1`;
  served with a different `RAHI_ARTIFACT_IMAGE` it reports `image`; and
  FR-003's join fails, naming the subject, when the Statement names
  another binary.
- **FR-005 (idempotent, and restore).** Re-running the deploy step with
  the same inputs appends nothing. A backup taken at epoch 2, a chain
  advanced to epoch 4, a restore of that backup, and the next deploy step
  yield epoch 3 with `cause: restore`, `previous` equal to epoch 2's
  hash, and the marker's archive digest; the archive manifest named epoch
  2; a denial after it names the new epoch 3.
- **FR-006 (rollback is a new epoch).** v1 (epoch 1), v2 with one added
  grant (036 transition, epoch 2), then v1's binary again (036 transition
  back, epoch 3): epoch 3's artifact equals epoch 1's, its `previous` is
  epoch 2's hash, and epoch 1 is unchanged.
- **FR-007 (a live export).** With a fixture cell serving, `ledger
  export` run beside it attaches, exits 0, and the exported chain ends at
  or after the newest epoch record; the same holds as a store client
  under `RAHI_STORE_CLIENT`.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p rahi-ledger --locked --test epoch` and `cargo
  test -p rahi-cli --locked --test epochs` pass.
- **AC-2.** `cargo test -p rahi-ledger --locked --test verify` and `--test
  seal` pass with 013's and 014's fixtures as committed: a chain with no
  epoch verifies byte for byte as before this spec.
- **AC-3.** Spec 015 AC-2 still holds, and `docs/design/01-consumer-contract.md`
  section 11 is replaced by the procedure as built.

### The worked consumer example

A cell's v1 has manifest H1, binary X1, image I1; v2 adds a grant (H2),
binary X2, image I2. The deploy step runs with `RAHI_ARTIFACT_IMAGE` and
`RAHI_DEPLOYMENT_REFS` from the rollout (the Job at N=3, the entrypoint
at N=1).

```
genesis(H1)                                  epoch 0, as today
epoch:1 {H1, X1, I1, deployment D1, previous = hash(genesis)}
  ... denials {manifest H1, epoch 1, instance ...}
transition(H1 -> H2)                         036, same deploy step
epoch:2 {H2, X2, I2, D2, transition = hash(transition), previous = hash(epoch:1)}
  ... during the rollout: denials {H1, epoch 1} and {H2, epoch 2} interleave
rollback to v1's image:
transition(H2 -> H1)
epoch:3 {H1, X1, I1, D3, previous = hash(epoch:2)}   an old artifact, a new epoch
backup at epoch 3                            manifest.json names epoch 3
epoch:4 ...                                  a later deploy
restore of that backup, then the deploy step:
epoch:4 {cause restore, restore = {archive, sha256}, previous = hash(epoch:3)}
                                             a different record from the lost epoch:4
```

An artifact never names an epoch; an epoch names an artifact, and the
same artifact can appear in many epochs. A deploy step that changes
nothing appends nothing. A manifest change without its epoch, which only
a failed deploy step leaves, shows as `manifest` mismatch until the step
is re-run.

## 6. Out of scope

- Evaluating a reference: fetching, verifying, or trusting the provenance,
  the deployment record, or the snapshot, and deciding whether a
  deployment was authorized. That is the consumer's verifier and broker.
- A central proof graph, runtime assertion evaluation, and drift scoring.
- Ledgering replica boots or mismatches: a boot appends nothing (the chain
  is not the request log, 015 D-5); a later spec if a consumer needs
  tamper-evident instance history.
- Changing the genesis, the manifest hash, the record envelope, or the
  ledger key; key rotation.
- Producing build provenance for this repository's images (039 leaves
  signing to a later supply-chain spec).
- Trust-window evaluation (015 §6). rahi's part of a validity predicate
  revalidated at effect time is the epoch's record hash as a freshness
  input: a broker that authorized an effect against one epoch sees a
  newer one on `/binding` and in the chain.
- Changing the denial queue's guarantees (015 B-6, D-7; 035).

## 7. Resolved decisions

None yet. Before approval a human decides, and the first item needs the
other repositories:

- the reference type URIs and the three member names, agreed with
  Statecraft (the deployment record and the composing envelope),
  spec-spine (the authority snapshot), and the CLI (the neutral verifier),
  with the in-toto Statement and SLSA provenance versions pinned; the
  handoff's exit criterion is that no side implements a parallel format
  first;
- two record kinds (036's transition and this spec's epoch, proposed, so
  036 can land without waiting for the cross-repository agreement) or one
  (the epoch carrying the transition), which changes 036 before either is
  approved;
- 040's document schema version with this spec's additions in view: B-8
  adds `epoch` and `match` to the document 040 names, so 040's version
  either reserves them or this spec bumps it;
- whether a mismatch stays a signal (proposed) or may refuse the boot
  under a manifest option;
- whether the ledger's `schema_version` moves when payloads gain keys
  (proposed: no; the envelope is unchanged and the payload is additive);
- sealed segment keys after a restore: an id the discarded timeline
  minted and sealed can be minted again after a restore, and a segment key
  built from it would be refused `Error::Conflict` (014 B-5). B-12 removes
  the reuse on the N=1 path; the N=3 ordering and the key scheme are
  inferred from the code, not reproduced, and want a maintainer's look.

## Verification

```verify:cli
cargo test -p rahi-ledger --locked --test epoch
cargo test -p rahi-cli --locked --test epochs
cargo test -p rahi-ledger --locked --test verify
cargo test -p rahi-ledger --locked --test seal
```
