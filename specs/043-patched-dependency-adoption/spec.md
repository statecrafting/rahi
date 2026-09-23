---
id: "043-patched-dependency-adoption"
title: "Patched dependency adoption: the N=1 cell runs the published, provenance-pinned hiqlite and Rauthy builds, crosses their cache boundary only with consent, keeps revocations through it, and stops within a measured bound"
status: draft
kind: feature
domain: ops
created: "2026-09-23"
authors: ["Bartek Kus"]
implementation: pending
risk: critical
wave: 3
depends_on:
  - "010-workspace-and-core-types"
  - "011-store-hiqlite"
  - "012-store-coordination"
  - "016-store-binary-values-and-extensions"
  - "025-api-tokens-and-resource-server"
  - "026-streaming-responses"
  - "030-operational-verbs"
  - "031-single-container-packaging"
  - "032-cluster-topology"
  - "035-denials-survive-shutdown"
  - "037-identity-recovery-and-live-proof"
  - "038-native-clients-and-bearer-revocation"
  - "039-release-and-out-of-tree-packaging"
establishes:
  - "crates/rahi-ops/src/upgrade.rs"
  - "crates/rahi-ops/tests/upgrade.rs"
  - "crates/rahi-cli/tests/terminal.rs"
  - "crates/rahi-cli/tests/stop_budget.rs"
  - "crates/rahi-idp/tests/revocation_durable.rs"
extends:
  - { spec: "010-workspace-and-core-types", unit: "Cargo.toml", nature: amending }
  - { spec: "010-workspace-and-core-types", unit: "deny.toml", nature: amending }
  - { spec: "011-store-hiqlite", unit: "crates/rahi-store/src/store.rs", nature: amending }
  - { spec: "011-store-hiqlite", unit: "crates/rahi-store/src/error.rs", nature: amending }
  - { spec: "016-store-binary-values-and-extensions", unit: "crates/rahi-store/tests/blob.rs", nature: amending }
  - { spec: "025-api-tokens-and-resource-server", unit: "crates/rahi-idp/src/bearer.rs", nature: amending }
  - { spec: "038-native-clients-and-bearer-revocation", unit: "crates/rahi-idp/src/revoke.rs", nature: amending }
  - { spec: "030-operational-verbs", unit: "crates/rahi-ops/src/lib.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-ops/src/restore.rs", nature: amending }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/src/lib.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/src/serve.rs", nature: amending }
  - { spec: "031-single-container-packaging", unit: "crates/rahi-ops/src/supervise.rs", nature: amending }
  - { spec: "031-single-container-packaging", unit: "docker/Dockerfile", nature: amending }
  - { spec: "031-single-container-packaging", unit: ".github/workflows/image.yml", nature: additive }
  - { spec: "039-release-and-out-of-tree-packaging", unit: "docker/runtime.Dockerfile", nature: amending }
  - { spec: "032-cluster-topology", unit: "deploy/README.md", nature: amending }
  - { spec: "032-cluster-topology", unit: "deploy/k8s/statefulset.yaml", nature: amending }
  - { spec: "037-identity-recovery-and-live-proof", unit: ".github/workflows/live.yml", nature: additive }
references:
  - { unit: { kind: file, path: "docs/design/01-consumer-contract.md" }, role: context }
  - { unit: { kind: file, path: "docs/design/02-operational-prerequisites.md" }, role: context }
summary: >
  rahi's application store runs hiqlite 0.14.0 through a root git patch that
  registry consumers never inherit, and its co-deployed identity provider
  runs upstream Rauthy 0.36.2. Published downstream builds now exist for
  both: hiqlite-patched 0.15.0-patched.1 and rauthy-patched
  0.36.2-patched.2, each qualified by its producer at N=1. This spec adopts
  exactly those two artifacts for the N=1 cell under a narrow,
  provenance-pinned exception to 011's no-fork rule, makes the one
  incompatible boundary between them (the cache raft log) crossable only
  with an explicit, resumable, backed-up operator consent, moves bearer
  revocation out of the cache so that crossing it, a restore, or a
  disaster recovery does not resurrect a revoked token, turns a proven
  terminal storage failure into a bounded exit instead of an indefinitely
  erroring process, and replaces the stacked shutdown numbers with a
  measured bound that distinguishes graceful completion from forced
  termination. It changes no topology and claims no N=3 support.
---

# 043: Patched dependency adoption

## 1. Purpose

Constitution IX puts nothing durable in the cache group, and 011 B-6
requires the released hiqlite from the registry, never a fork or a patch.
Two facts now pull against the second rule. The lock-handler fix rahi
needs (upstream #352) is still unreleased upstream, so 011 D-12 carries it
as a root `[patch.crates-io]` that no registry consumer inherits. And a
downstream line, `bartekus/hiqlite`, has published `hiqlite-patched
0.15.0-patched.1` with that fix plus durability, startup and restore
repairs rahi's volume would otherwise go without; `bartekus/rauthy` has
published a Rauthy 0.36.2 build on it. Both producers qualify N=1 only.

This spec is the governed route to those artifacts for the N=1 cell: which
identities are admitted and how they are pinned (B-1, B-2), how the one
incompatible on-disk boundary is crossed and rolled back (B-3 to B-5), which
promises survive the cache loss that crossing it causes (B-6), what a
terminal storage failure does (B-7, B-8), and how long a stop may take
(B-9). A later N=3 composition (spec 044, draft) consumes whatever release
is qualified at N=3; this spec does not.

## 2. Territory

The workspace dependency declaration and the supply-chain policy (010's
`Cargo.toml` and `deny.toml`, amending); the store's startup and error
mapping (011, amending); one 016 test whose assertion encodes the old
engine's error swallowing (amending); the bearer deny-list and the
subject-wide revocation instant (025, 038, amending); a new operator verb
`upgrade-cache` in `rahi-ops` (`src/upgrade.rs`, new) and its CLI wiring
(030, additive); restore's reset list and its cache-loss record (030,
amending); `serve`'s terminal-failure exit and stop sequence (030, 035,
amending); the supervisor's Rauthy environment and exit policy (031,
amending); both Dockerfiles' Rauthy pin (031, 039, amending); the
operator README and the StatefulSet grace (032, amending); the live
workflow's upgrade leg (037, additive).

## 3. Behavior

### Identities

- **B-1 (the hiqlite identity).** The workspace declares `hiqlite = {
  package = "hiqlite-patched", version = "=0.15.0-patched.1",
  default-features = false, features = [the 011 B-6 set] }`. The dependency
  key stays `hiqlite`, so no source path changes. The workspace root
  carries no `[patch]` section for any hiqlite package, `deny.toml` carries
  no allow-git entry for hiqlite, and `Cargo.lock` resolves `hiqlite-patched`
  and `hiqlite-wal-patched` from crates.io with the checksums recorded in
  D-P1, no package named `hiqlite` or `hiqlite-wal`, and no git source.
  `hiqlite-derive-patched` is absent while rahi does not enable `macros`.
  The version is exact until an owner decision relaxes it.
- **B-2 (the Rauthy identity).** `docker/Dockerfile` and
  `docker/runtime.Dockerfile` carry the identical
  `ARG RAUTHY_IMAGE=ghcr.io/bartekus/rauthy-patched:0.36.2-patched.2@sha256:ea114a8bb743d578dea6d7800916ee43550939c749a2cf586f9abdc0d0c52478`,
  and the comment above it names the downstream build, its upstream base
  `v0.36.2` (`dd61ac3c`), its source commit `17132b94`, and N=1 as its
  supported topology. `0.36.2-patched.1` is never pinned. The image build
  asserts `rauthy --version` prints `rauthy 0.36.2-patched.2`.

### Crossing the cache boundary

- **B-3 (refusal is the default).** A boot of either store over a cache raft
  log written by hiqlite 0.14 is refused by hiqlite with its own message
  and a non-zero exit; rahi adds nothing that bypasses it. The supervisor
  never forwards `HQL_CACHE_LEGACY_MOVE_ASIDE` from the operator's
  environment to Rauthy, and `serve` ignores it for the app store: a value
  present in rahi's environment is a startup error naming B-4's verb, so
  the hiqlite variable never becomes part of rahi's operator contract.
- **B-4 (one consent, one verb).** `rahi upgrade-cache` runs with the cell
  stopped and is the only supported crossing. In order, each step recorded
  in `<data>/upgrade-cache.state` (fsynced) before the next begins:
  (1) refuse unless no process holds either data directory's
  `hiqlite-owner.lock` and 0.14 cache logs are present; (2) take a local
  pre-upgrade backup of both stores with the key set, as 030 B-5's archive
  (the backup runs the old on-disk format and is restorable by the old
  image); (3) move `logs_cache` and `state_machine_cache` of the app store
  and of Rauthy's store into `pre-upgrade-<unix seconds>/` beside each, with
  fsync of both parent directories; (4) write the transition row B-6
  consumes; (5) mark the state `done`. A rerun after any interruption
  resumes from the recorded step and never reverses a completed move. The
  verb touches Rauthy's cache directories as files on the cell volume the
  chassis owns (031 B-1); it never opens Rauthy's database (011 B-7).
- **B-5 (rollback).** `rahi upgrade-cache --rollback` moves both stores'
  current cache directories aside (it never deletes) so the old image
  starts on an empty cache, and prints the pre-upgrade backup's path. The
  README states: an old image started over a 0.15 cache without this step
  can destroy the SQLite raft metadata (D-P2's evidence), and the recovery
  is restoring the pre-upgrade backup, not reusing the volume.

### What survives cache loss

- **B-6 (revocation is durable).** *Proposed, owner decision pending
  (P-2).* The `jti` deny-list (025 B-5) and the subject-wide revocation
  instant (038 B-5) live in the SQL group, written in the same `txn` as the
  revocation's ledger decision, read on every bearer check. An in-process
  lookaside cache may sit in front as an accelerator; a miss falls through
  to SQL and never admits. Expired rows are pruned by the existing TTL
  bound. Consequence: a revocation taken before a cache loss (this spec's
  upgrade, a restore, a disaster recovery, a lost cache directory) still
  refuses the token afterwards; a revocation taken after the backup a
  restore uses is lost with that backup's RPO and no more. Sessions (022),
  rate-limit counters (020, 025) and coordination leases (012) stay in the
  cache; their loss is the renewal round trip, a counter reset, and a
  released lease respectively, each already accepted by its spec, and the
  fencing sequence stays in SQL (012 D-1).
- **B-6a (Rauthy's cache state).** What Rauthy loses with its cache is
  Rauthy's to enumerate. Known from the producer's handoff: in-flight
  authorization codes, device codes, WebAuthn challenges, rate-limit
  counters and IP blacklist entries; sessions survive. The README states
  this for the upgrade, every restore, and every migration, because none of
  them carries either cache.

### Terminal storage failure

- **B-7 (the app store).** `rahi-store` maps hiqlite `Error::NodeFailed`
  to a distinct terminal error. On the first one, `serve` fails `/readyz`,
  runs its ordinary stop (026, 035, B-9), and exits with a new named exit
  code, distinct from rauthy's codes and from 030's existing codes. This
  holds in every profile: the store is this process's own.
- **B-8 (Rauthy under the N=1 supervisor).** 031 B-3 stands: Rauthy exits,
  the cell exits. A Rauthy that stays up while unready fails rahi's
  readiness and never ends the process by itself. The supervisor restarts
  the cell for a Rauthy storage failure only on a **proven** terminal
  signal from Rauthy; none exists today (`/auth/v1/ready` answers `503` for
  both a transient sample and a terminal failure). Whether a
  persistent-unready bound is added as a fallback is P-3. This behavior is
  the N=1 profile's only; spec 044 governs N=3, where rahi never exits and
  never fails liveness because Rauthy is unreachable or unready.

### Stopping

- **B-9 (two outcomes, one measured bound).** A stop ends in exactly one of:
  *graceful completion*: streams closed or drained, every queued denial
  ledgered, `Client::shutdown` returned `Ok`, Rauthy exited `0`, both
  `hiqlite-owner.lock` flocks released, exit `0`; or *bounded forced
  termination*: a stage exceeded its bound, `Client::shutdown` returned
  `Error::Timeout` (hiqlite's 15 s `SHUTDOWN_WAIT`) or another error, or the
  outer grace sent SIGKILL. A forced termination is logged with the stage
  that overran, counts every unledgered denial as abandoned (035 D-5), exits
  non-zero, and the next start succeeds with no manual step. `SERVE_GRACE`,
  `SHUTDOWN_TERM_GRACE` and `terminationGracePeriodSeconds` are set from
  the measurement AC-7 records, at its maximum plus the margin P-4 fixes;
  until then they are unchanged.

## 4. Functional requirements

- **FR-001.** A lockfile check (a test reading `cargo metadata --locked`)
  fails on any hiqlite package name other than the two of B-1, any git
  source, or a checksum that differs from D-P1.
- **FR-002.** The image build fails when the two Dockerfiles' `RAUTHY_IMAGE`
  differ or when the built image's `rauthy --version` is not the pinned
  version (031 B-6's existing guard, extended).
- **FR-003.** `upgrade-cache` is testable against real stores in temp
  directories, with an injected fault point after each B-4 step.
- **FR-004.** The bearer check reads the deny state through the store
  handle, so a test can wipe the cache group and observe the refusal hold.
- **FR-005.** `NodeFailed` is injectable in a test by a real storage fault
  (for example the data directory made unwritable under a running node, or
  a torn WAL record), never by a mocked client.
- **FR-006.** The stop measurement is a test that runs the real binary with
  an open stream and a queued denial backlog, and records wall time to
  process exit and to flock release per run.

## 5. Acceptance criteria

Every criterion runs against the real hiqlite and, where named, the real
patched Rauthy image; none substitutes a mock for storage or identity.

- **AC-1 (graph).** FR-001 passes; `cargo deny check` passes with no hiqlite
  allow-git entry; `cargo test --workspace --locked` passes, including 016's
  extension test asserting the refusal as an error (the engine refuses;
  0.15 reports what 0.14 swallowed).
- **AC-2 (image).** FR-002 passes on `linux/amd64` and `linux/arm64`; the
  image's `/usr/local/bin/rauthy` hashes to the platform binary recorded in
  D-P1.
- **AC-3 (upgrade, disposable volumes).** A volume written by the v0.2.0
  image: the new image without consent exits non-zero and moves nothing;
  `upgrade-cache` produces the pre-upgrade backup and moves all four cache
  directories; the first start is ready; a second start is ready with no
  consent; app rows, the principal `sub` of a logged-in user, the ledger
  chain (`ledger verify --full`) and the key set are unchanged.
- **AC-4 (interruption).** With a fault injected after each of B-4's steps
  in turn, a rerun completes the transition and AC-3's end state holds.
- **AC-5 (rollback).** `upgrade-cache --rollback` then the v0.2.0 image
  boots and serves the pre-upgrade data; restoring the pre-upgrade backup
  into a fresh volume boots the v0.2.0 image.
- **AC-6 (revocation).** A bearer token revoked by `jti`, and another
  revoked by subject, before (a) the upgrade, (b) a restart, (c) a restore
  from a backup taken after the revocation, and (d) a deletion of the cache
  directories, are refused afterwards; a token revoked after the restore's
  backup point is documented, not tested, as lost.
- **AC-7 (stop).** FR-006 records a bounded series of stops; each run's
  outcome is classified per B-9; graceful runs release both flocks; forced
  runs restart without a manual step; the graces are then set as B-9 says.
- **AC-8 (terminal).** FR-005's fault makes `serve` exit with B-7's code
  within B-9's bound; a restart after the fault is cleared is ready; denials
  queued at the fault are ledgered or counted abandoned.
- **AC-9 (live).** The live workflow (037) passes with `RAHI_REQUIRE_RAUTHY=1`
  against the image built from this change, and records zero skips; an
  unexecuted required leg fails the job.

## 6. Out of scope

N=3 in any layout (spec 044 and a later adoption of an N=3-qualified
release); any change to hiqlite or Rauthy source; enabling hiqlite's
`auto-heal` (a data-integrity policy change that needs its own decision);
S3 upload outcome reporting (hiqlite reports it only in its log); replacing
030 D-2's file-level restore with hiqlite's repaired restore (proposal
R-b, its own change).

## 7. Resolved decisions

None. Every entry below is proposed and awaits the owner.

Still open before approval:

- P-1 to P-5 below;
- the verb name and exit-code value of B-4 and B-7;
- whether AC-9's live leg runs on both architectures or amd64 only.

### Proposals (2026-09-23)

- **P-1 (the dependency exception; would be recorded as 011 D-13 with a
  cross-reference in 037).** A narrow exception in D-12's form: 011 B-6,
  011 D-10 and 037 section 6 ("A rauthy fork: rahi consumes released
  rauthy") stand as written and are suspended, not amended, for exactly
  the two artifacts of B-1 and B-2. Rahi authors no change to either and
  maintains no fork. A later version of either is its own governed
  adoption citing its release provenance. 011 D-12's patch and its allow-git
  entry end with this change. Supported topology: N=1.
- **P-2 (revocation durability).** B-6 as written, amending 025 B-5 and
  038 B-5 through this spec's `amending` edges and a dated entry in each.
  Alternative kept on record: a boot-time `iat` floor, acceptable only once
  its persistence (SQL, written before the caches move), missing-`iat`
  refusal, `<=` boundary, clock leeway, window end (the manifest's current
  maximum access lifetime plus leeway), restart-inside-the-window
  enforcement and restore/DR application are specified and each tested; it
  also refuses every legitimate pre-upgrade token for the window.
- **P-3 (N=1 Rauthy fallback).** None (recommended until Rauthy exposes a
  terminal indicator), or a persistent-unready bound with a value, recorded
  as a heuristic and never applied at N=3.
- **P-4 (grace margin).** Fifty percent over AC-7's measured maximum.
- **P-5 (release scope).** The release carrying this spec supports N=1
  only; `deploy/n3` (the co-located overlay of 032, never run on three pods)
  is labelled unqualified in `deploy/README.md`, the consumer contract and
  the release notes.

### Evidence recorded for the proposals

- **D-P1 (identities, verified 2026-09-23).** crates.io:
  `hiqlite-patched` `456c1c117e5c581f6638572f26d9ef7cd567738c578e42ff0c8e09300534e7ca`,
  `hiqlite-wal-patched` `024992a08719a870bcef79192ed392cbef758b39caf0a60167541df379a05a9b`,
  `hiqlite-derive-patched` `e2380bba9eb80f5d7ecb5a59097b91bed1a3600f9d6e659ad37362e6cb2df077`,
  source `bartekus/hiqlite@3392c120`. Rauthy: tag `v0.36.2-patched.2` at
  `17132b94`; an anonymous registry read of the tag returns index
  `sha256:ea114a8b...c52478`, listing `linux/amd64`
  `sha256:6774d28c9f611777dc4ad2243a8f4cb1fa0df3d94caca1aeaf052cc10d9e5658`
  and `linux/arm64`
  `sha256:6ae9225a9243a7e660f6c407d07f81258d06f456470dfb4b6c899a6db13146f8`;
  `/app/rauthy` hashes `742b18ba3717a92577a2ae0d517546a64ef6967c86e2847b50b10a22ab8dfc59`
  (amd64) and `5e498c31ef23ebc27a6d2dbdbf73f6d6f48129f541fced92d48a53b87d61e312`
  (arm64), and prints `rauthy 0.36.2-patched.2`. The GitHub release has no
  assets although its ledger lists eight; the values above are the
  registry's and the ledger's (section 7 on `patched/0.36.2`), which agree.
- **D-P2 (bounded trial, spike only, 2026-09-23).** On `b815b18` with only
  B-1's declaration changed: build, clippy and fmt pass; deny passes with
  the unused hiqlite allow-git entry reported; the workspace tests pass
  605, fail 1 (016's extension test, as AC-1 describes), ignore 1 (the
  pre-existing harness doc test). A probe on real hiqlite 0.14 (at
  `8f3b9bd`) and 0.15 with rahi's feature set and default cache storage:
  0.15 refuses a 0.14 cache and moves nothing (it does create
  `hiqlite-owner.lock`); consent keeps SQL rows and empties the cache,
  including a stand-in revoked `jti` and a counter; later starts are
  idempotent. An old build started over a 0.15-written cache panicked in
  both of two runs, and in one it left the SQLite raft's `logs/meta.hql`
  at zero bytes so that neither version could start the volume.

## Verification

```verify:cli
cargo test -p rahi-ops --locked --test upgrade
cargo test -p rahi-idp --locked --test revocation_durable
cargo test -p rahi-cli --locked --test terminal
cargo test -p rahi-cli --locked --test stop_budget
cargo test -p rahi-store --locked --test blob
```
