---
id: "037-identity-recovery-and-live-proof"
title: "Identity recovery and the live proof: a backup a real rauthy answers, a restore that brings the users back, and CI that runs against the pinned release"
status: draft
kind: feature
domain: ops
created: "2026-09-11"
authors: ["Bartek Kus"]
implementation: pending
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
extends:
  - { spec: "011-store-hiqlite", unit: "crates/rahi-store/src/backup.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-ops/src/rauthy_api.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-ops/src/restore.rs", nature: additive }
  - { spec: "031-single-container-packaging", unit: "crates/rahi-ops/src/supervise.rs", nature: additive }
  - { spec: "033-dev-substrate-and-harness", unit: "crates/rahi-harness/src/boot.rs", nature: additive }
  - { spec: "034-hello-cell", unit: "apps/hello-cell/tests/e2e.rs", nature: additive }
  - { spec: "021-idp-proxy-and-discovery", unit: "crates/rahi-idp/tests/discovery.rs", nature: additive }
  - { spec: "032-cluster-topology", unit: "deploy/README.md", nature: additive }
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
- **B-2 (the app backup exists before it is reported).** `Store::backup`
  triggers hiqlite's backup and then waits, polling the local listing
  within a bound (default 120 seconds), for a file newer than the ones it
  listed before; it reports that file, or `Error::Upstream` naming the
  bound.
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
  when no file appears.
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

Still open: whether `live.yml` becomes a required check or stays advisory
beside `ci-gate`.

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
