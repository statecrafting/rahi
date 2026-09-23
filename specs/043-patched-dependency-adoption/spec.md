---
id: "043-patched-dependency-adoption"
title: "Patched dependency adoption: the N=1 cell runs the published, provenance-pinned hiqlite and Rauthy builds, crosses their cache boundary only through a resumable, excluded, backed-up transition, keeps every accepted revocation through it, and stops within a validated budget"
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
  - "021-idp-proxy-and-discovery"
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
  - "crates/rahi-ops/src/stop.rs"
  - "crates/rahi-ops/tests/upgrade.rs"
  - "crates/rahi-cli/tests/terminal.rs"
  - "crates/rahi-cli/tests/stop_budget.rs"
  - "crates/rahi-idp/tests/revocation_durable.rs"
  - "crates/rahi-store/tests/dependency_identity.rs"
extends:
  - { spec: "010-workspace-and-core-types", unit: "Cargo.toml", nature: amending }
  - { spec: "010-workspace-and-core-types", unit: "deny.toml", nature: amending }
  - { spec: "011-store-hiqlite", unit: "crates/rahi-store/src/store.rs", nature: amending }
  - { spec: "011-store-hiqlite", unit: "crates/rahi-store/src/error.rs", nature: amending }
  - { spec: "011-store-hiqlite", unit: "crates/rahi-store/src/lib.rs", nature: additive }
  - { spec: "016-store-binary-values-and-extensions", unit: "crates/rahi-store/tests/blob.rs", nature: amending }
  - { spec: "025-api-tokens-and-resource-server", unit: "crates/rahi-idp/src/bearer.rs", nature: amending }
  - { spec: "038-native-clients-and-bearer-revocation", unit: "crates/rahi-idp/src/revoke.rs", nature: amending }
  - { spec: "021-idp-proxy-and-discovery", unit: "crates/rahi-idp/src/lib.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-ops/src/lib.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-ops/src/preflight.rs", nature: amending }
  - { spec: "030-operational-verbs", unit: "crates/rahi-ops/src/restore.rs", nature: amending }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/src/lib.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/src/verbs.rs", nature: amending }
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
  runs upstream Rauthy 0.36.2. Published downstream builds exist for both,
  hiqlite-patched 0.15.0-patched.1 and rauthy-patched 0.36.2-patched.2, each
  qualified by its producer at N=1. This spec adopts exactly those two
  artifacts for the N=1 cell under a narrow recorded exception to 011's
  no-fork rule. It crosses the one incompatible on-disk boundary (the cache
  raft log) only through a resumable transition that proves no old or new
  process holds the stores, requires a verified pre-upgrade archive, and
  lets Rauthy move its own cache. It keeps every revocation accepted before
  the upgrade effective for the full accepted-token validity window, moves
  future revocations to SQL, states the restore boundary, turns a proven
  terminal storage failure into a bounded exit, and validates the shutdown
  budget from its phases and from a measured workload. It changes no
  topology and claims no N=3 support.
---

# 043: Patched dependency adoption

## 1. Purpose

Constitution IX puts nothing durable in the cache group, and 011 B-6
requires the released hiqlite from the registry, never a fork or a patch.
The lock-handler fix rahi needs (upstream #352) is still unreleased
upstream, so 011 D-12 carries it as a root `[patch.crates-io]` that no
registry consumer inherits. A downstream line, `bartekus/hiqlite`, has
published `hiqlite-patched 0.15.0-patched.1` with that fix plus durability,
startup and restore repairs, and `bartekus/rauthy` has published a Rauthy
0.36.2 build on it. Both producers qualify N=1 only.

This spec is the governed route to those artifacts for the N=1 cell: which
identities are admitted (B-1, B-2); how the incompatible cache boundary is
crossed, excluded, resumed and rolled back (B-3 to B-5a); which
revocations survive it and every other cache loss, and where that stops
(B-6 to B-6c); what a terminal storage failure does (B-7, B-8); and how
long a stop may take and how its outcome is known (B-9, B-10). An N=3
composition (spec 044, draft) is separate and consumes whatever release is
later qualified at N=3.

## 2. Territory

The workspace dependency declaration and supply-chain policy (010,
amending); the store's error mapping and the SQL tables this spec adds
(011, amending and additive); one 016 test that encoded the old engine's
error swallowing (amending); the bearer check and the revocation store
(025, 038, amending; 021's crate root, additive); the new transition verb
and the stop record in `rahi-ops` (`src/upgrade.rs`, `src/stop.rs`, new)
with CLI wiring, including the new verb in 030's argv parser
(`verbs.rs`, amending: per 030 D-9, extending `VERBS` belongs to the spec
that adds the verb, and 030 AC-2's required set is derived from it); preflight's accepted-validity figure and
restore's floor (030, amending); `serve`'s gating, terminal exit and stop
sequence (030, 035, amending); the supervisor's cell lock, Rauthy consent
and stop recording (031, amending); both Dockerfiles' Rauthy pin (031, 039,
amending); the operator README and StatefulSet grace (032, amending); the
live workflow's upgrade leg (037, additive).

## 3. Behavior

Terms. **L** is the access-token lifetime Rauthy is applying, read back
from Rauthy the way 038 B-6's `preflight` reads it (never the manifest's
default and never Rauthy's documented default). **V**, the accepted
validity, is `L + 2 * LEEWAY_SECONDS` = `L + 120` seconds (038 D-7: `exp`
and `nbf` are each accepted with 60 seconds of leeway; `denylist_ttl` in
`revoke.rs`). With the manifest default `L = 600`, `V = 720`. Every
revocation window in this spec (preflight's bound, pruning, the transition
floor, the restore floor, and every test) uses V computed from the same L
by the same function.

### Identities

- **B-1 (hiqlite).** The workspace declares `hiqlite = { package =
  "hiqlite-patched", version = "=0.15.0-patched.1", default-features =
  false, features = [the 011 B-6 set] }`. No `[patch]` section names a
  hiqlite package; `deny.toml` has no hiqlite allow-git entry; `Cargo.lock`
  resolves `hiqlite-patched` and `hiqlite-wal-patched` from crates.io with
  D-P1's checksums, no package named `hiqlite` or `hiqlite-wal`, and no git
  source, identically for `x86_64-unknown-linux-gnu` and
  `aarch64-unknown-linux-gnu`. `hiqlite-derive-patched` is absent while
  `macros` is off. The version stays exact until an owner decision relaxes
  it.
- **B-2 (Rauthy).** Both Dockerfiles carry the identical
  `ARG RAUTHY_IMAGE=ghcr.io/bartekus/rauthy-patched:0.36.2-patched.2@sha256:ea114a8bb743d578dea6d7800916ee43550939c749a2cf586f9abdc0d0c52478`
  with a comment naming the downstream build, upstream base `v0.36.2`
  (`dd61ac3c`), source `17132b94` and N=1. `0.36.2-patched.1` is never
  pinned. The image build asserts `rauthy --version` prints
  `rauthy 0.36.2-patched.2` and that `/usr/local/bin/rauthy` hashes to the
  platform binary in D-P1.

### Crossing the cache boundary

- **B-3 (refusal is the default).** A boot over a 0.14 cache log is refused
  by hiqlite and rahi adds nothing that bypasses it.
  `HQL_CACHE_LEGACY_MOVE_ASIDE` present in rahi's own environment is a
  startup error naming B-4's verb; the supervisor never forwards an
  operator's value to Rauthy.
- **B-4 (the transition).** `rahi upgrade-cache --backup <archive>` is the
  only supported crossing. It is a state machine persisted in
  `<data>/upgrade-cache.json`, written by write-to-temporary, fsync, rename,
  fsync of `<data>`. Each step records its **intent** before acting and its
  **completion** after; recovery reads the last record.

  | step | intent recorded | action | completion recorded | recovery if interrupted after the intent |
  |---|---|---|---|---|
  | T0 lock | none | acquire non-blocking, and hold until exit: `<data>/cell.lock` (B-5), the app store's `hiqlite-owner.lock`, `logs/lock.hql` and `logs_cache/lock.hql` (a lock file a clean 0.14 stop removed is created empty in the app store to be locked); nothing under Rauthy's directory | none; the locks are held, not recorded | a lock held by another process: refuse and change nothing |
  | T1 begin | `begin{id, instant, archive}`; `instant` is the wall clock read after T0 | refuse if the app store holds a `state_machine/lock` that is not this transition's own marker (0.14's unclean-stop marker); verify the archive with 030's read-only verification | `verified{digest}` | rerun T1; nothing on disk changed |
  | T2 move | `moving{target}` with `target` = `<app store>/pre-upgrade-<instant>/` | rename `logs_cache` and `state_machine_cache` into `target`; fsync `target` and the store directory | `moved` | per directory: already in `target` is done, still in place is renamed; a completed move is never reversed |
  | T3 floor | `flooring` | open the app store in-process with 0.15 and no listener; write in one `txn` the transition row and the revocation floor (B-6b) with `before = instant`; shut the store down and require `Ok` (B-10's confirmed completion) | `floored` | rerun T3: both rows are keyed by `id` and written by upsert, so a repeat is a no-op |
  | T4 arm | `arming` | write the app store's `state_machine/lock` with content `rahi-upgrade-cache <id>` (the guard, B-5), fsync it and its directory | `armed` | rewrite the guard; it is idempotent |
  | T5 Rauthy | written by `supervise` | on the next boot, while the state is `armed`, `supervise` adds `HQL_CACHE_LEGACY_MOVE_ASIDE=true` to the Rauthy child's environment and Rauthy moves its own cache | `rauthy-done`, once Rauthy answers ready | a boot while still `armed` passes the consent again; once Rauthy's own format marker exists the variable is a no-op (hiqlite source and probe; for Rauthy, a source finding until AC-4 runs it) |
  | T6 serve | written by `serve` | before opening the store, and only when the state is `rauthy-done` and the guard's content names this transition, `serve` removes the guard and fsyncs; then it starts normally | `done`, after the first `/readyz` 200; the file is kept as history | a guard still present with state `rauthy-done` is removed at the next start |

  `serve` refuses to start, before opening the store, while the file is in
  any state before `rauthy-done`, and `supervise` refuses to spawn Rauthy
  while the file is in any state before `armed`. So normal serving never
  starts between the cache move and the floor write. The verb never opens,
  locks, renames, reads, copies or writes anything under Rauthy's data
  directory (011 B-7, spec 000 `store-separation`). Rauthy's own transition
  is the producer's (its leg J).
- **B-5 (exclusion).** Two windows, two mechanisms (D-P3, D-P5).
  *During the verb:* the proof that no process holds the app store is the
  set of locks T0 takes. A running hiqlite 0.14 node holds `flock` on
  `logs/lock.hql` and `logs_cache/lock.hql` and **not** on
  `hiqlite-owner.lock`; a 0.15 node holds `hiqlite-owner.lock`. While the
  verb holds all three, a 0.14 start fails on the WAL lock and a 0.15 start
  fails with `StorageInUse`, both before any cache move, with or without
  consent. *From `armed` until `serve` starts:* the verb has exited, and the
  guard T4 wrote is what excludes. A 0.14 serve (a v0.2.0 image) refuses to
  start while `state_machine/lock` exists and changes no file; its
  supervisor then ends its Rauthy (031 B-3's serve-failure path). A
  new-image cell is excluded by `cell.lock`, which `supervise` takes as its
  first act and holds for its life, and by the state file, which `serve`
  and `supervise` read before opening anything; a second run of the verb
  reads the state and resumes (from `armed` it has nothing left to do).
  *What rahi cannot prove:* that no old Rauthy process is live on Rauthy's
  directory, because that would mean opening a path under it. The verb does
  not need it: an old Rauthy on its own, still 0.14, directory is unharmed
  by the verb. The one hazard is a new Rauthy started with T5's consent
  while an old Rauthy is live on the same directory, since hiqlite 0.15
  performs the consent move before it detects the live 0.14 node (D-P3).
  That requires two cells running on one volume at once, which the
  one-deployment-unit invariant already excludes; the README states it as
  an operator precondition (the old container is stopped and removed before
  the new image first starts), and a hiqlite producer request asks for the
  exclusion check to precede the move.
- **B-5a (rollback).** The supported rollback is restoring B-4's verified
  pre-upgrade archive into a fresh volume and starting the old image (030
  restore, single-shot). `upgrade-cache --rollback` exists only for the app
  store: under T0's locks it moves the app store's current cache aside and
  never deletes; Rauthy's cache is Rauthy's, and the README gives the
  producer's manual step. The README states that an old image started over
  a 0.15 cache without the move can destroy the SQLite raft metadata
  (D-P2), after which the volume is not trusted and the archive is the
  recovery.

### Revocation across cache loss

- **B-6 (durable revocation).** Revocations taken by this version are
  written to SQL (a `jti` table and a subject table, each row carrying its
  expiry `revoked_at + V`) in the same `txn` as the revocation's ledger
  decision, and read on every bearer check. An in-process lookaside may
  accelerate reads; a miss falls through to SQL and never admits. Rows are
  pruned only after their expiry. A cache loss of any cause (this upgrade,
  a lost cache directory, a restart) removes nothing a check relies on.
- **B-6a (revocations that exist only in the old cache).** They cannot be
  carried: the 0.14 cache is unreadable by 0.15 by design, no backup holds
  it, and 0.2.0 has no export. B-6b replaces them conservatively.
- **B-6b (the transition floor).** T3 writes one durable floor with
  `before = instant`. While it is active, every bearer token whose `iat` is
  at or before `before` is refused `401` with 025 B-6's challenge, and a
  token with no `iat` is refused: the comparison 038 B-5 already applies
  per subject, applied to every subject. `instant` is read after T0 proved
  no rahi process holds the app store, so no old revocation can be
  recorded after it; a token Rauthy issues after `instant` is a new token
  that no lost revocation named (a subject revocation also ended the
  subject's Rauthy sessions, 038 D-8), so the floor needs no claim about
  Rauthy's process. The floor is active until `before + V`, with V
  from L read back from Rauthy at the first boot after the transition;
  until that read succeeds the floor stays active (fail closed). It lives
  in SQL, so a restart inside the window keeps it. Consequence, stated in
  the README and the release notes: every bearer access token issued
  before the upgrade is refused for up to V after the transition instant; a
  native client must refresh; a refresh presented before the refresh
  token's own `nbf` (Rauthy sets it to `issued_at + L - 60`) makes Rauthy
  end that user's sessions and tokens, so the user logs in again; browser
  sessions keep their Rauthy session and pay 022's renewal round trip.
  Revocations Rauthy enforces itself (038 D-8's session end) are in
  Rauthy's database and survive the move-aside.
- **B-6c (the restore boundary).** A restore returns both databases to the
  archive's instant, so revocations taken after that instant are absent
  from the restored SQL and from Rauthy's restored database. Proposed
  (P-7): the restore verb writes the same floor with `before` = the restore
  instant, so every access token issued before the restore is refused for
  V, which covers every such revocation of an access token. What no floor
  covers, stated as the boundary: a Rauthy-side revocation (a session or
  refresh token ended after the archive instant) comes back with Rauthy's
  restore, and that refresh token works again until it expires or is
  revoked again.

### Terminal storage failure

- **B-7 (the app store).** `rahi-store` maps hiqlite `Error::NodeFailed` to
  a distinct terminal error. On the first one, `serve` fails `/readyz`,
  runs its ordinary stop (B-9), records the stop reason `storage_terminal`
  (B-10), and exits `3`, the existing infrastructure code of the four-code
  contract (010), which is not widened.
- **B-8 (Rauthy under the N=1 supervisor).** 031 B-3 stands: Rauthy exits,
  the cell exits. A Rauthy that is up and unready, transient or not, fails
  rahi's readiness and never ends the process; this release adds no
  persistent-unready restart heuristic (D-5). A machine-readable terminal
  signal from Rauthy, when its producer ships one, is adopted by a later
  change; nothing in this spec waits for it.

### Stopping

- **B-9 (the budget, composed and measured).** The stop phases and their
  configured bounds are: stream drain S (026 B-7, default 10 s),
  connection drain C (`DRAIN_BUDGET`, 10 s), denial drain D (035, 5 s),
  store shutdown H (hiqlite's caller-side wait, 15 s, after which
  `Client::shutdown` returns `Error::Timeout`), and Rauthy's stop R
  (`SHUTDOWN_TERM_GRACE`, 10 s). The **composition check** is a test over
  the configured values: `SERVE_GRACE >= S + C + D + H`, and the pod's
  `terminationGracePeriodSeconds` and the documented `docker stop -t` are
  each `>= SERVE_GRACE + R`. With today's defaults the sums are 40 s and
  50 s, so both graces rise to those values (from 15 s and 30 s). The
  **workload check** is AC-7's measurement: under the declared workload,
  the largest measured time from SIGTERM to process exit, multiplied by 1.5
  (D-6), must not exceed the configured grace. Both checks must hold;
  neither alone is a bound, and a measured maximum is evidence about the
  declared workload only. The chassis never wraps `Client::shutdown` in a
  shorter timeout and then treats the node as stopped.
- **B-10 (three outcomes, and who records them).** Every stop is exactly
  one of: **confirmed completion** (every phase finished inside its bound,
  every queued denial ledgered, `Client::shutdown` returned `Ok`, Rauthy
  exited `0`, both owner locks released, exit `0`); **unconfirmed
  completion** (the process exited by itself, but a phase overran or
  `Client::shutdown` returned `Timeout` or an error, so the store's stop is
  not confirmed; exit non-zero); **forced exit** (SIGKILL). The process
  records what it can: on SIGTERM it writes `<data>/stop.json` with
  `{boot, received_at}`, and on exit rewrites it with the outcome and each
  phase's duration. A SIGKILLed process cannot record, so the **next boot**
  classifies the previous stop: a record with an outcome is taken as
  written; a record with `received_at` and no outcome is a forced exit
  after SIGTERM; no record for the previous boot is an unannounced kill or
  a crash. The next boot logs the classification and exports it as
  `rahi_previous_stop{outcome}`. The supervisor records Rauthy's SIGKILL
  itself, since it sends it. In tests, the harness that sends a SIGKILL
  records it.

## 4. Functional requirements

- **FR-001.** `tests/dependency_identity.rs` reads `cargo metadata --locked`
  for both Linux targets and fails on any hiqlite package other than B-1's
  two, any git source, or a checksum different from D-P1.
- **FR-002.** The image build fails when the two `RAUTHY_IMAGE` values
  differ, when `rauthy --version` differs, or when the binary hash differs.
- **FR-003.** The transition is a library function with an injectable fault
  point after every intent and every action of B-4, run against real
  stores in temporary directories.
- **FR-004.** The bearer check reads revocation state through the store
  handle, so a test can delete the cache directories and observe refusals
  hold.
- **FR-005.** `NodeFailed` is produced in tests by a real storage fault
  (the data directory made unwritable under a running node, or a torn WAL
  record), never by a mocked client.
- **FR-006.** V has one implementation (`denylist_ttl`), used by preflight,
  pruning, both floors and every test.
- **FR-007.** The composition check of B-9 is a unit test over the
  constants and the shipped manifests.

## 5. Acceptance criteria

Every criterion runs against the real hiqlite and, where named, the real
patched Rauthy image; none substitutes a mock for storage or identity.

- **AC-1 (graph).** FR-001 passes; `cargo deny check` passes with no
  hiqlite allow-git entry and no new duplicate; `cargo test --workspace
  --locked` passes, with 016's extension test asserting the refusal as an
  error.
- **AC-2 (image).** FR-002 passes on `linux/amd64` and `linux/arm64`.
- **AC-3 (transition).** On a volume and an archive produced by the v0.2.0
  image: the new image without the verb refuses and moves nothing; the verb
  without a verifying archive refuses; with it the state reaches `armed`,
  the next `supervise` reaches `done`, both caches are aside (the app
  store's by the verb, Rauthy's by Rauthy), and a second boot needs
  nothing; app rows, a logged-in principal's `sub`, `ledger verify --full`
  and the key set are unchanged.
- **AC-4 (interruption).** With a fault injected after each intent and
  each action of B-4 in turn (T1 to T6), a rerun, or the next boot for T5
  and T6, reaches AC-3's end state, and at no point does `serve` answer a
  request before `floored`.
- **AC-5 (exclusion, against the real old version).** With a live v0.2.0
  cell the verb refuses at T0 and changes nothing; with the verb holding
  T0, a v0.2.0 cell and a new-image cell each fail to start and change
  nothing in the app store; after the verb exits at `armed`, a v0.2.0 cell
  fails to start, changes no file in the app store, and ends its Rauthy,
  and a second run of the verb changes nothing; a foreign
  `state_machine/lock` refuses at T1.
- **AC-6 (revocation, not weakened).** A token revoked by `jti` and one by
  subject: (a) revoked before the upgrade, in the 0.14 cache, are refused
  after it by the floor for V from `instant`; (b) revoked after the
  upgrade, are refused across a restart, across deletion of both cache
  directories, and until `revoked_at + V`; (c) the boundaries hold:
  `iat == before` refused, `iat == before + 1` accepted, no `iat` refused
  while the floor is active, the floor lifts at `before + V` with V from
  the read-back L (a test sets L at Rauthy to a non-default value), a
  restart inside the window keeps refusing, and a floor whose L cannot be
  read stays active; (d) if P-7 is accepted, a token revoked after a backup
  point is refused after restoring that backup. B-6c's Rauthy-side boundary
  is documented, not tested as closed.
- **AC-7 (stop, graceful).** FR-007 passes. Then a bounded series under the
  declared workload (streams open up to 026's per-identity limit, a
  200-denial backlog in flight, Rauthy running) records per run each
  phase's duration, the outcome, time to exit, and time to both locks'
  release. **Every run of this series must be a confirmed completion**; a
  series with any unconfirmed completion or forced exit fails this
  criterion, and so does one whose measured maximum times 1.5 exceeds the
  configured grace.
- **AC-7a (stop, unconfirmed and forced).** Separately: with the store's
  shutdown made to overrun, the stop is an unconfirmed completion with a
  non-zero exit; with a grace set below the workload's need, the harness
  SIGKILLs the process and the next boot classifies a forced exit. Both
  next boots succeed with no manual step. These runs never count toward
  AC-7.
- **AC-8 (terminal).** FR-005's fault makes `serve` fail `/readyz`, record
  `storage_terminal`, and exit `3` within B-9's bound; denials queued at the
  fault are ledgered or counted abandoned; a boot after the fault is
  cleared is ready. A Rauthy held unready does not end the process.
- **AC-9 (live).** The live workflow (037) passes with
  `RAHI_REQUIRE_RAUTHY=1` against the image built from this change and
  reports zero skips; an unexecuted required leg fails the job.

## 6. Out of scope

N=3 in any layout (spec 044, and a later adoption of an N=3-qualified
release); any change to hiqlite or Rauthy source; enabling hiqlite's
`auto-heal`; S3 upload outcome reporting; replacing 030 D-2's file-level
restore with hiqlite's repaired restore; a Rauthy terminal-storage signal,
which is a producer request and not a prerequisite.

## 7. Resolved decisions

- **D-1 (2026-09-23, owner direction; the dependency exception).** Adopt
  exactly the two published, provenance-pinned artifacts of B-1 and B-2
  through a narrow recorded exception to the existing dependency
  restrictions. 011 B-6, 011 D-10 and 037 section 6 stand as written and are
  suspended for these two artifacts only; at approval the same entry is
  recorded in 011 (D-13) and cross-referenced in 037. Rahi authors no
  change to either and maintains no fork; a later version is its own
  governed adoption; 011 D-12's patch and allow-git entry end here.
- **D-2 (2026-09-23, owner direction; scope).** Deliver and qualify N=1
  first, with no N=3 support claim. `deploy/n3`, 032's co-located overlay,
  never run on three pods, is labelled unqualified in `deploy/README.md`,
  the consumer contract and the release notes.
- **D-3 (2026-09-23, owner direction; revocation).** Durable SQL revocation
  for future revocations, retaining the full accepted-token validity window
  V. B-6 and B-6b are the mechanism.
- **D-4 (2026-09-23, owner direction; consent).** Explicit, resumable
  upgrade consent and a verified pre-upgrade backup, preserving store
  separation. B-4 and B-5 are the mechanism.
- **D-5 (2026-09-23, owner direction; Rauthy readiness).** Transient
  unready responses stay distinct from proven terminal failure, and this
  release adds no persistent-unready restart heuristic.
- **D-6 (2026-09-23, owner direction; headroom).** The 50 percent margin is
  qualification headroom over the measured workload, alongside a validated
  composition of the configured phase budgets (B-9).

These record direction. Approval of this text is a separate owner act.

Still open before approval: P-7.

### Proposals (2026-09-23)

- **P-6 (withdrawn 2026-09-23, after independent review).** An earlier
  draft had T0 lock Rauthy's WAL lock files, which means opening, and on a
  cleanly stopped volume creating, files under Rauthy's directory; that
  contradicts the plain words of spec 000's frozen `store-separation`
  anchor ("separate storage that app code never opens") and 011 B-7, and no
  ordinary approval can grant an exception to a frozen anchor. B-5 now
  excludes without touching Rauthy's directory and states the residual
  case.
- **P-7 (the restore floor).** B-6c's floor is written by every restore, so
  access-token revocations taken after the archive instant are not lost.
  Cost: every access token issued before the restore is refused for V,
  with B-6b's refresh consequence.

### Evidence recorded for the decisions

- **D-P1 (identities, verified 2026-09-23).** crates.io:
  `hiqlite-patched` `456c1c117e5c581f6638572f26d9ef7cd567738c578e42ff0c8e09300534e7ca`,
  `hiqlite-wal-patched` `024992a08719a870bcef79192ed392cbef758b39caf0a60167541df379a05a9b`,
  `hiqlite-derive-patched` `e2380bba9eb80f5d7ecb5a59097b91bed1a3600f9d6e659ad37362e6cb2df077`,
  source `bartekus/hiqlite@3392c120`. Rauthy: tag `v0.36.2-patched.2` at
  `17132b94`; an anonymous registry read returns index
  `sha256:ea114a8b...c52478` listing `linux/amd64`
  `sha256:6774d28c9f611777dc4ad2243a8f4cb1fa0df3d94caca1aeaf052cc10d9e5658`
  and `linux/arm64`
  `sha256:6ae9225a9243a7e660f6c407d07f81258d06f456470dfb4b6c899a6db13146f8`;
  `/app/rauthy` hashes `742b18ba3717a92577a2ae0d517546a64ef6967c86e2847b50b10a22ab8dfc59`
  (amd64) and `5e498c31ef23ebc27a6d2dbdbf73f6d6f48129f541fced92d48a53b87d61e312`
  (arm64) and prints `rauthy 0.36.2-patched.2`. The GitHub release has no
  assets although its ledger lists eight; the values are the registry's and
  the ledger's (section 7 on `patched/0.36.2`), which agree. The graph on
  `b815b18` with only B-1 changed resolves the same hiqlite packages for
  both Linux targets, single versions of rusqlite, reqwest, axum, tokio,
  ring and rustls, and the same 27 duplicate-crate warnings as `main`.
- **D-P2 (bounded trial and cache probe, 2026-09-23).** Build, clippy, fmt
  and deny pass; tests pass 605, fail 1 (016's extension test, now an error
  where 0.14 returned no rows), ignore 1 (a pre-existing doc test). On real
  hiqlite 0.14 (`8f3b9bd`) and 0.15 with rahi's features: 0.15 refuses a
  0.14 cache without moving it (it does create `hiqlite-owner.lock`);
  consent keeps SQL rows and empties the cache, including a stand-in
  revoked `jti`; later starts are idempotent. An old build over a
  0.15-written cache panicked in both of two runs, and in one left the
  SQLite raft's `logs/meta.hql` at zero bytes so that neither version could
  start the volume.
- **D-P3 (exclusion probe, 2026-09-23).** A live 0.14 node holds `flock` on
  `logs/lock.hql` and `logs_cache/lock.hql` and not on `hiqlite-owner.lock`,
  so the new lock alone does not exclude the old version. A 0.15 start with
  consent against a live 0.14 node moved the live node's cache aside
  **before** failing on the WAL lock; the 0.14 node then panicked at stop
  and the volume needed manual repair. With the owner lock and both WAL
  locks held by another process, a 0.14 start and a 0.15 start with and
  without consent each failed before any change. Both versions refuse to
  start while `state_machine/lock` exists; 0.15 does so by panicking, and
  only after its consent move has run. The real upstream
  `ghcr.io/sebadob/rauthy:0.36.2` image (the 0.2.0 cell's Rauthy), probed
  from a second container on the same volume: while live it holds `flock`
  on `logs/lock.hql` and `logs_cache/lock.hql` and has no
  `hiqlite-owner.lock`; after `docker stop` (exit 0) both lock files are
  gone and `state_machine/lock` is removed. AC-5 still runs this against a
  whole v0.2.0 cell.
- **D-P5 (the guard, 2026-09-23).** After a 0.14 write and a 0.15
  transition on a disposable store, `state_machine/lock` was written with
  this spec's content. A 0.14 start panicked on it ("Lock file already
  exists") and **no file changed** (content hashes of every file before and
  after). A 0.15 start also refused, but only after rewriting
  `hiqlite-owner.lock` and creating `logs/lock.hql` and `logs/meta.hql~`,
  which is why `serve` checks the state and removes the guard before it
  opens the store. `SHUTDOWN_WAIT` (15 s, B-9's H) is
  `hiqlite-patched-0.15.0-patched.1/src/client/mgmt.rs:592`, applied by
  `Client::shutdown` at `:278`, which returns an error when it elapses.
- **D-P4 (stop probe, 2026-09-23).** Five runs of a real single-node
  `serve` on the patched graph, SIGTERM during a concurrent 200-denial
  burst, no stream, no Rauthy: exit `0` every run, 0.37 to 0.73 s from
  SIGTERM to exit, the owner lock free at the first check (0.46 to 1.31 s).
  `Client::shutdown`'s own result was not observed separately. This is not
  AC-7's workload and bounds nothing.

## Verification

```verify:cli
cargo test -p rahi-store --locked --test dependency_identity
cargo test -p rahi-store --locked --test blob
cargo test -p rahi-ops --locked --test upgrade
cargo test -p rahi-idp --locked --test revocation_durable
cargo test -p rahi-cli --locked --test terminal
cargo test -p rahi-cli --locked --test stop_budget
```
