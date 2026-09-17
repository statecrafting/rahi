---
id: "037-identity-recovery-and-live-proof"
title: "Identity recovery and the live proof: a backup a real rauthy answers, a restore that brings the users back, and CI that runs against the pinned release"
status: approved
kind: feature
domain: ops
created: "2026-09-11"
authors: ["Bartek Kus"]
implementation: in-progress
risk: high
wave: 3
depends_on:
  - "011-store-hiqlite"
  - "030-operational-verbs"
  - "031-single-container-packaging"
  - "033-dev-substrate-and-harness"
  - "034-hello-cell"
establishes:
  - ".github/workflows/live.yml"
  - "crates/rahi-ops/tests/rauthy_restore.rs"
  - "crates/rahi-ops/src/rauthy_session.rs"
  - "crates/rahi-store/tests/backup_freshness.rs"
  - "crates/rahi-ops/tests/rauthy_backup_admin.rs"
extends:
  - { spec: "011-store-hiqlite", unit: "crates/rahi-store/src/backup.rs", nature: additive }
  - { spec: "011-store-hiqlite", unit: "crates/rahi-store/Cargo.toml", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-ops/src/rauthy_api.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-ops/src/restore.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-ops/src/lib.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/src/lib.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/tests/cli.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-ops/tests/common/mod.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-ops/tests/backup.rs", nature: additive }
  - { spec: "031-single-container-packaging", unit: "crates/rahi-ops/tests/first_boot.rs", nature: additive }
  - { spec: "031-single-container-packaging", unit: "crates/rahi-ops/src/supervise.rs", nature: additive }
  - { spec: "031-single-container-packaging", unit: "crates/rahi-ops/src/first_boot.rs", nature: additive }
  - { spec: "033-dev-substrate-and-harness", unit: "crates/rahi-harness/src/boot.rs", nature: additive }
  - { spec: "033-dev-substrate-and-harness", unit: "crates/rahi-harness/tests/boot.rs", nature: additive }
  - { spec: "025-api-tokens-and-resource-server", unit: "crates/rahi-idp/tests/bearer.rs", nature: additive }
  - { spec: "034-hello-cell", unit: "apps/hello-cell/tests/e2e.rs", nature: additive }
  - { spec: "021-idp-proxy-and-discovery", unit: "crates/rahi-idp/tests/discovery.rs", nature: additive }
  - { spec: "032-cluster-topology", unit: "deploy/README.md", nature: additive }
  - { spec: "032-cluster-topology", unit: "deploy/k8s/secret.example.yaml", nature: additive }
  - { spec: "034-hello-cell", unit: "apps/hello-cell/README.md", nature: additive }
references:
  - { unit: { kind: file, path: "docs/design/01-consumer-contract.md" }, role: context }
  - { unit: { kind: file, path: "docs/design/02-operational-prerequisites.md" }, role: context }
summary: >
  Recovery of a cell is two state systems, and only one of them recovers
  today. Against a real rauthy the backup verb fails outright, because
  rauthy's backup routes accept only an admin session and the verb
  presents the admin API key (030 D-3, observed against 0.36.2). The app
  half races hiqlite's background backup and can report no file for a
  backup that lands a moment later (observed on two of four runs). A
  restore places rauthy's snapshot and nothing hands it to rauthy, so a
  restored cell comes up with no users, and every sub in its rows is
  unreachable. CI never runs a real rauthy. This spec resolves D-3 by the
  mechanism a human chooses, waits for the app backup to exist, applies
  rauthy's snapshot exactly once on the next start, and adds a CI job that
  runs every rauthy-gated test, and an end-to-end test through recovery
  with identity, against the pinned rauthy release.
---

# 037: Identity recovery and the live proof

## 1. Purpose

Constitution VII makes rauthy's `sub` the only principal id, so rauthy's
database is as much the cell's state as the app's store. Spec 030 built
one archive for both (B-5, B-6) and recorded, honestly, that the rauthy
half does not work against a real rauthy (D-3). Three more facts, verified
on 2026-09-11 at `444bcf8`:

- In the hello-cell image with the pinned rauthy 0.36.2, `rahi backup`
  inside the running container ends with `unauthorized: rauthy refused
  the admin token at .../auth/v1/backup (401 Unauthorized)` and writes no
  archive.
- hiqlite 0.14 documents that `Client::backup` returns before the backup
  file exists; `Store::backup` lists once, immediately after, and on two of
  four runs against that container answered `upstream: backup completed
  but no new file was listed` (exit 3) for a backup that then appeared.
- `restore` writes rauthy's snapshot to `/data/restore/rauthy/`. 030 D-2
  assigned the hand-off to spec 031's supervisor; `rauthy_command` starts
  rauthy from its rendered environment only, and nothing applies the
  snapshot. hello-cell's end-to-end test stubs rauthy's backup routes and
  reboots the restored volume without identity for exactly these reasons.

And CI runs `cargo test --workspace --locked` with no rauthy, so every
test gated on `RAHI_TEST_RAUTHY` prints `skipped` and passes; the only
live identity evidence is a hand-driven run against an unreleased rauthy
build (034 §8). This spec makes recovery real and makes the proof run
where regressions are caught.

## 2. Territory

Additive changes to the app backup (011), the rauthy client and the
restore marker (030), the supervisor (031), the harness (033), the
end-to-end test (034), and one dead test (021). A new workflow and a new
test file.

## 3. Behavior

- **B-1 (the rauthy backup answers).** `rahi backup` obtains rauthy's
  snapshot through the mechanism a human selects in §7 and nothing else.
  No mechanism reads rauthy's directory (constitution VIII). A refusal is
  still an error and still writes no archive (030 B-5). The selection
  (D-1) is a dedicated backup admin that completes rauthy's MFA with a
  software passkey custodied in the key set, built only after FR-005's
  spike proves it against the pinned release; the chassis never sets
  `ADMIN_FORCE_MFA=false`. If the spike fails, B-1 is not built and the
  rauthy half is the rehearsed operator-assisted procedure of D-1.
  Amended 2026-09-17 (D-4): what rauthy's half of the archive carries is a
  snapshot taken *for this backup*, under the same freshness rule B-2
  states, and never a file that was already there. rauthy's store is
  hiqlite too, so its backup route suppresses a request within sixty
  seconds of the last one and answers `204` regardless (D-3); the same
  wait, the same deadline, and the same refusal apply.
- **B-2 (the app backup exists before it is reported).** `Store::backup`
  triggers hiqlite's backup and then waits, polling the local listing
  within a bound (default 120 seconds), for a file newer than the ones it
  listed before; it reports that file, or `Error::Upstream` naming the
  bound. Amended 2026-09-17 (D-3, D-4): "newer" is not file age. hiqlite
  names a snapshot `backup_node_<id>_<ts>.sqlite` for the epoch second at
  which the request that produced it entered the log, and it silently
  ignores, while acknowledging, a request that arrives within sixty
  seconds of the one before. So: rahi serialises its own backup requests
  so two of its callers never race; it reads the newest existing
  snapshot's timestamp and waits out the remainder of the suppression
  window before it triggers; it accepts only a snapshot whose timestamp is
  at or after the second in which it triggered, which is a snapshot no
  earlier request can have produced; and when an outside request wins the
  window it waits and triggers again rather than reporting that snapshot,
  unless that snapshot is itself at or after its own trigger second. The
  whole sequence, waiting included, is bounded by one deadline, and
  running out of it is `Error::Upstream` naming the deadline. A stale file
  is never reported as a success. The freshness of the two stores is each
  store's own: nothing here makes the app's snapshot and rauthy's a
  coordinated pair, and no document may claim they are.
- **B-3 (rauthy's snapshot is applied once).** When `restore.marker` names
  a rauthy snapshot that is not yet applied, the supervisor starts rauthy
  once with its hiqlite restore source pointing at that file, waits for
  rauthy's health, and records the application in the marker. A later
  start sees the record and passes nothing. Every other start is exactly
  as today. The app never opens rauthy's store; it hands rauthy a file.
  This spec builds and exercises the hand-off at N=1; restore
  orchestration at N=3 is deferred (D-2).
- **B-4 (loud skips where it matters).** When `RAHI_REQUIRE_RAUTHY=1` is
  set, every test gated on `RAHI_TEST_RAUTHY` or `RAHI_TEST_RAUTHY_URL`
  fails instead of skipping when its variables are absent. The dead test
  at `crates/rahi-idp/tests/discovery.rs` line 307, which never boots
  anything, either drives the image as spec 031 FR-005 describes or is
  deleted with a line in 021 saying where FR-005 is proven.
- **B-5 (the live job).** `.github/workflows/live.yml` extracts the rauthy
  binary from the image `docker/Dockerfile` pins, by the same digest, and
  runs `cargo test --workspace --locked` with `RAHI_TEST_RAUTHY` naming it
  and `RAHI_REQUIRE_RAUTHY=1`; it also starts the pinned image and runs
  spec 025's live bearer test against it. It runs on every pull request
  that touches `crates/`, `apps/`, `docker/`, or `deploy/`, and nightly on
  `main`.
- **B-6 (the proof goes through recovery with identity).** hello-cell's
  end-to-end test, on the rauthy path, backs up against the real rauthy
  (no stub), restores into a fresh volume, boots it *with* identity on the
  same ports, logs the same user in, and asserts the same `sub`, the
  surviving note readable by that user, and the same ledger head. It also
  restarts a cell without a restore and asserts the session renews and the
  head is unchanged.

## 4. Functional requirements

- **FR-001.** `tests/rauthy_restore.rs` restores an archive into a fresh
  volume, runs the supervisor against a stub rauthy that records its
  environment, and asserts the restore source was passed on the first
  start and not on the second.
- **FR-002.** A store test delays the backup file behind a slow listing
  and asserts `Store::backup` reports the file, and reports the bound
  when no file appears. Amended 2026-09-17 (D-4): the delay that matters
  is hiqlite's own suppression window, so the test takes two backups
  inside sixty seconds with a row written between them, opens both
  snapshots, and asserts the second is a different file whose contents
  hold the row the first cannot; and that a deadline shorter than the
  remaining window is `Error::Upstream` naming it, with no snapshot
  reported.
- **FR-006.** Added 2026-09-17 (D-4). The same proof for rauthy's store,
  against the pinned release: two `rahi backup` runs inside sixty seconds
  carry two different rauthy snapshots, the second taken after the first,
  and neither run reports a snapshot that was already on disk when it
  started.
- **FR-003.** The end-to-end test of B-6 passes locally against the
  pinned release and in `live.yml`.
- **FR-004.** With `RAHI_REQUIRE_RAUTHY=1` and no `RAHI_TEST_RAUTHY`, `cargo
  test -p hello-cell --locked` fails naming the variable.
- **FR-005 (the spike, first).** Added 2026-09-12 (D-1). Before any B-1
  code lands, a bounded experiment against the pinned rauthy with
  `ADMIN_FORCE_MFA` on: a dedicated backup admin holds a software passkey,
  a WebAuthn client in `rahi-ops` completes the assertion without a
  browser, the resulting session is MFA-satisfied, `POST /auth/v1/backup`
  succeeds, and the snapshot downloads. Its outcome, pass or fail, with the
  rauthy version, is recorded as a dated decision in this spec, and it
  decides which branch of D-1 the build takes.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p rahi-store --locked`, `cargo test -p rahi-ops
  --locked`, and `cargo test -p hello-cell --locked` pass without rauthy
  (the rauthy steps skipped and named).
- **AC-2.** `live.yml` passes on the pull request that lands this spec.
  This is an operator-visible check, not a `verify:cli` command, because
  it needs the pinned image.
- **AC-3.** `deploy/README.md` loses its restore gap paragraph and its D-3
  hold paragraph, or keeps whichever of them this spec did not close,
  with the reason.
- **AC-4.** Added 2026-09-12 (D-1). No document in this repository claims
  unattended recovery until B-6 passes with the mechanism B-1 built. If
  FR-005 fails, `deploy/README.md` carries the operator-assisted backup and
  restore procedure, states that unattended recovery is not supported, and
  names the date and rauthy version of the rehearsal that proved the
  procedure.

## 6. Out of scope

- Rotating rauthy's keys or the backup key.
- Restoring one store without the other: the archive stays one artifact
  (constitution XII).
- Backup scheduling and retention (032).
- A rauthy fork: rahi consumes released rauthy.
- Restore orchestration at N=3 (D-2), and turning rauthy's admin MFA
  enforcement off (D-1).

## 7. Resolved decisions

As drafted on 2026-09-11, this section asked a human to choose B-1's
mechanism between a rauthy change upstream (an API key with backup access on
`/auth/v1/backup*`, then waiting on a release that carries it) and a
dedicated backup admin session for a user whose MFA requirement is off. The
evidence below showed the second does not exist as worded in 0.36.2, and P-1
named a third. The owner decided on 2026-09-12 (decisions RH-02 and RH-03 of
the revision-3 register). The spec stays `draft` until a human flips it.

- **D-1 (2026-09-12, owner decision RH-02; B-1's mechanism).** A software
  passkey feasibility spike first (FR-005), then the implementation of the
  mechanism it proves: a dedicated backup admin whose passkey private key
  is custodied in the key set, so `rahi backup` completes rauthy's MFA and
  `ADMIN_FORCE_MFA` stays on. The owner called this a bounded engineering
  experiment, not another product decision. Two routes are refused: waiting
  for an upstream API-key route (asking upstream is allowed and is not a
  dependency of this spec), and turning MFA off instance-wide. If
  automation fails, the only thing kept is a rehearsed operator-assisted
  backup and restore procedure, and every unattended recovery claim is
  blocked (AC-4). N=1 engineering work on B-2 to B-6 continues whichever
  branch the spike takes. No hosted pilot may claim unattended recovery
  until B-6 passes with the built mechanism.
- **D-2 (2026-09-12, owner decision RH-03; B-3 at N=3).** Restore
  orchestration at N=3 is deferred; the first pilot makes no N=3 promise.
  The candidate design is recorded and not adopted: one node restores
  (hiqlite restores only on node 1), the others rejoin, then the whole is
  exercised. It is reopened only when N=3 is requested and identity,
  rollout, and backup can be exercised together. P-2 is therefore not
  adopted; its text stays as the candidate's detail.

- **D-3 (2026-09-17, FR-005's outcome; the spike passes, passkey only).**
  Ratified by the owner on 2026-09-17 together with the flip of this spec
  from `draft` to `approved`: D-1's branch A is confirmed and built. The
  spike ran against the pinned release, rauthy `0.36.2` from image digest
  `f7d3c501402165e023edbd958b032b41c9cfdac5ea7f8ca7d62217327145577e`, with
  `ADMIN_FORCE_MFA` at its default (on), in an isolated scratch volume.
  Both recorded facts reproduced first: the admin API key is `401` on
  `POST /auth/v1/backup`, and a password-only admin session is `406`
  `MfaRequired`. Then a software authenticator (ES256 over P-256, a
  hand-rolled CBOR `none` attestation) drove rauthy's own ceremonies over
  HTTP and `POST /auth/v1/backup` answered `204`, and the snapshot
  downloaded (675,840 bytes, `SQLite format 3`).

  The mechanism built is **passkey only**, spiked on the owner's
  instruction before any password variant: the dedicated backup admin is
  created by the admin API key, given a transient password for the length
  of one registration because `User::convert_to_passkey` refuses any
  account type but `Password`, registers the custodied passkey with user
  verification, and is then converted by `POST
  /users/{id}/self/convert_passkey`, which sets both `password` and
  `password_expires` to null. The transient password is never written
  anywhere. Measured: `account_type` `passkey`, `password_expires` null;
  three consecutive fresh processes holding only the custodied private key
  and credential id authenticated with no password at all
  (`POST /oidc/authorize` with no `password` field, then the assertion) and
  backed up; and the same after a rauthy restart. Password expiry is
  therefore not a hazard of this mechanism rather than a hazard managed by
  it, which is why the proven password-plus-passkey variant of the spike is
  not built.

  Three facts the live run produced that only a live run would have:

  - **The signature counter is custody state.** Reloading the key with a
    reset counter is `401` "The credential may have be compromised".
    A software authenticator must report zero permanently (WebAuthn 6.1.1:
    zero means "not supported"), which is what is built.
  - **hiqlite silently suppresses a backup within sixty seconds of the
    last one and acknowledges it anyway** (`hiqlite` 0.14
    `writer.rs`, `WriterRequest::Backup`), so four `POST /backup` calls
    produced one file and three `204`s. `ts_last_backup` is process-local,
    so the first request after a restart is never suppressed. This is what
    B-1 and B-2 are amended for (D-4).
  - **rauthy reports errors that are not credential errors as "Invalid
    user credentials"** (a wrong `redirect_uri`, a missing
    `code_challenge`); only its own log says why. The client built here
    says so in its error text.

  One operational hazard, found by causing it: rauthy blacklists the
  calling address for hours on a request to a path its scanner list knows
  (`/docs/v1/api-doc/openapi.json` here), and the blacklist survives a
  restart. Every route this chassis calls is a route read from rauthy's
  source; nothing probes.

- **D-4 (2026-09-17, owner instruction; what "fresh" means).** The owner
  directed that a backup must produce a fresh snapshot, that the
  suppression window be accounted for before triggering under one bounded
  deadline with a clear failure, that file age alone is not proof that no
  concurrent request occurred, that rahi serialise its own requests and
  handle outside interference without ever reporting a stale file as
  success, and that freshness be verified independently for both stores.
  B-1, B-2, and FR-002 are amended accordingly and FR-006 is added. The
  rule built is the one the file name affords: hiqlite derives
  `backup_node_<id>_<ts>.sqlite` from the epoch second the request was
  issued at, and the writer applies the log in order, so a snapshot whose
  `<ts>` is at or after the second in which this process triggered is a
  snapshot of a state at or after that trigger, whoever issued it. Age on
  disk proves nothing of the sort and is not used. The owner also directed
  that no claim of coordinated evidence-head consistency between the two
  stores be drawn from freshness; B-2 says so and `deploy/README.md`
  repeats it.

- **D-5 (2026-09-17; the edges this build declares).** The owner
  authorised the edges on `crates/rahi-ops/src/first_boot.rs` (031) and
  `crates/rahi-ops/src/lib.rs` (030) with the approval. Three more were
  needed and are declared rather than worked around: `crates/rahi-cli/src/lib.rs`
  (030), because the composer is the only place the credential can be
  handed to the backup verb and the provisioning step to `supervise`'s
  readiness; and `deploy/k8s/secret.example.yaml` (032), which carries the
  same documented command as the `deploy/README.md` error D-6 names. The
  passkey is minted in `first_boot.rs` and not in `keys.rs`, which 031
  calls the only place a key is minted, for a reason that is not
  convenience: `first-boot --export` renders a key set for a read-only
  Secret mount, a mounted key set can never be written to later, and both
  the minting call sites (`run` and `export`) are in `first_boot.rs`.

- **D-6 (2026-09-17; the documented export command does not work).** The
  owner asked for a verified entrypoint error in the deployment
  documentation to be fixed if present. It is present, in
  `deploy/README.md` and in `deploy/k8s/secret.example.yaml`: both
  document `docker run ... ghcr.io/statecrafting/rahi:<v> first-boot
  --export > rahi-keys.yaml`. The image's `ENTRYPOINT` is
  `entrypoint.sh`, which takes no arguments and runs `first-boot`,
  `migrate`, `exec supervise`; the arguments are discarded and the command
  starts a cell instead of rendering a Secret. Verified by running
  `docker/entrypoint.sh first-boot --export` against a stub `rahi` on the
  path: it invoked `first-boot`, `migrate`, `supervise`, and never the
  export. Both documents now pass `--entrypoint rahi`. The entrypoint
  itself is not changed: taking arguments would make the container's one
  lifetime depend on what an operator typed, which is what spec 031 B-3
  removed.

Still open: whether `live.yml` becomes a required check or stays advisory
beside `ci-gate`. Decided for now by the owner on 2026-09-17: advisory,
beside `ci-gate`, and not a required check.

### Evidence and proposals (2026-09-12)

Recorded by the operational-prerequisites session against the pinned
release: the rauthy binary byte-identical to `/app/rauthy` in
`ghcr.io/sebadob/rauthy:0.36.2@sha256:f7d3c501...`, reporting `0.36.2`, and
the hello-cell release binary from the image `docker/Dockerfile` builds at
`c13cc70`. Method and output are in
`docs/design/02-operational-prerequisites.md` section 4.

- **The verb.** `rahi backup` against the running rauthy exits `1` with
  `unauthorized: rauthy refused the admin token ... (401 Unauthorized)` and
  writes no archive.
- **An admin session under rauthy's defaults.** The bootstrap admin logs in
  through the cell's origin; `POST /auth/v1/backup` with that session answers
  `406` `MfaRequired`, "Rauthy admin access only allowed with MFA active".
- **The second option as worded above does not exist in 0.36.2.** rauthy's
  admin MFA rule is one instance-wide setting, `ADMIN_FORCE_MFA`
  (`mfa.admin_force_mfa`, checked in `validate_admin_session`); there is no
  per-user exemption. With `ADMIN_FORCE_MFA=false` the same session backs up
  (`204`), lists `backup_node_1_<ts>.sqlite`, and downloads it (675,840
  bytes). So the session route turns admin MFA enforcement off for every
  rauthy admin of the cell, not for one backup principal.
- **Upstream.** rauthy's `main` on 2026-09-12 still calls
  `validate_admin_session()` in all four backup handlers; no release accepts
  an API key there.
- **Restore without a hand-off, as today.** A rahi archive carrying that
  real snapshot (relayed to the unchanged verb by a stub on rauthy's port)
  restores into a fresh volume, and the volume boots with the pinned rauthy.
  The user is absent from rauthy, her login is refused, and the app's notes
  answer `401`: the rows survive and nobody can reach them.
- **The B-3 mechanism, by hand.** The same volume started once with
  `HQL_BACKUP_RESTORE=file:<the placed snapshot>` in rauthy's environment
  (injected by a test wrapper, since the supervisor clears the environment):
  the user is back with her original `sub`, logs in with her original
  password, reads her note, and the ledger head equals the head before the
  backup. A second start without the variable keeps all of it. hiqlite
  restores only on node 1; nodes 2 and 3 given the variable delete their data
  and rejoin (`hiqlite/src/backup.rs`, `restore_backup_start`), so at N=3 the
  hand-off is node 1's and was not exercised.

Proposals, kept as written. P-1's passkey spike is D-1, with its
upstream-first ordering and fallback date refused; P-2 is D-2's deferred
candidate; P-3 is not decided:

- **P-1 (B-1's mechanism).** Ask upstream for an API key access group that
  covers `/auth/v1/backup*` (the first option, still recommended), with a
  date after which the pilot falls back. Name a third option beside the two
  above: a dedicated backup admin holding a software passkey whose private
  key is custodied in the key set, so the verb completes rauthy's MFA and
  `ADMIN_FORCE_MFA` stays on. It is unverified and costs a WebAuthn client in
  `rahi-ops`; spike it before choosing it. Do not choose the session route
  with `ADMIN_FORCE_MFA=false` for a cell with human admins; if a pilot takes
  it, the preflight should say that admin MFA is off.
- **P-2 (B-3 at N=3).** State in B-3 that the supervisor passes the restore
  source to every replica's first start after a restore, that node 1
  applies it and the others rejoin empty, and that the marker records the
  application per volume; exercise it with three processes as the 035 note
  does before claiming N=3 recovery.
- **P-3 (clients).** rauthy's `POST /auth/v1/oidc/authorize` refuses a
  request with an empty `User-Agent` and reports it to the caller as
  "Invalid user credentials" (its log says "Empty User-Agent not allowed").
  The harness sets one; `rahi-ops` and `rahi-idp` set none, and their calls
  (token exchange, admin API, backup) were not refused for it in these runs.
  B-6's test and any native client flow (038) send one, and whether the
  device endpoints apply the same check is unverified.

## Verification

```verify:cli
cargo test -p rahi-store --locked
cargo test -p rahi-ops --locked
cargo test -p hello-cell --locked
```
