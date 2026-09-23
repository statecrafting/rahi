---
id: "043-patched-dependency-adoption"
title: "Patched dependency adoption: the N=1 cell runs the published, provenance-pinned hiqlite and Rauthy builds, crosses their cache boundary only through a fenced, resumable, backed-up transition, keeps every accepted revocation through it, and stops within a validated budget with an honest outcome"
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
  - "crates/rahi-ops/src/cell_lock.rs"
  - "crates/rahi-ops/src/stop.rs"
  - "crates/rahi-ops/tests/upgrade.rs"
  - "crates/rahi-ops/tests/cell_lock.rs"
  - "crates/rahi-cli/tests/terminal.rs"
  - "crates/rahi-cli/tests/stop_budget.rs"
  - "crates/rahi-cli/tests/stop_outcome.rs"
  - "crates/rahi-idp/tests/revocation_durable.rs"
  - "crates/rahi-store/tests/dependency_identity.rs"
extends:
  - { spec: "010-workspace-and-core-types", unit: "Cargo.toml", nature: amending }
  - { spec: "010-workspace-and-core-types", unit: "deny.toml", nature: amending }
  - { spec: "010-workspace-and-core-types", unit: "crates/rahi-types/src/config.rs", nature: amending }
  - { spec: "011-store-hiqlite", unit: "crates/rahi-store/src/store.rs", nature: amending }
  - { spec: "011-store-hiqlite", unit: "crates/rahi-store/src/error.rs", nature: amending }
  - { spec: "011-store-hiqlite", unit: "crates/rahi-store/src/lib.rs", nature: additive }
  - { spec: "016-store-binary-values-and-extensions", unit: "crates/rahi-store/tests/blob.rs", nature: amending }
  - { spec: "025-api-tokens-and-resource-server", unit: "crates/rahi-idp/src/bearer.rs", nature: amending }
  - { spec: "038-native-clients-and-bearer-revocation", unit: "crates/rahi-idp/src/revoke.rs", nature: amending }
  - { spec: "021-idp-proxy-and-discovery", unit: "crates/rahi-idp/src/lib.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-ops/src/lib.rs", nature: amending }
  - { spec: "030-operational-verbs", unit: "crates/rahi-ops/src/preflight.rs", nature: amending }
  - { spec: "030-operational-verbs", unit: "crates/rahi-ops/src/restore.rs", nature: amending }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/src/lib.rs", nature: amending }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/src/verbs.rs", nature: amending }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/src/serve.rs", nature: amending }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/tests/cli.rs", nature: amending }
  - { spec: "035-denials-survive-shutdown", unit: "crates/rahi-cli/tests/shutdown.rs", nature: amending }
  - { spec: "031-single-container-packaging", unit: "crates/rahi-ops/src/supervise.rs", nature: amending }
  - { spec: "031-single-container-packaging", unit: "crates/rahi-ops/src/first_boot.rs", nature: amending }
  - { spec: "031-single-container-packaging", unit: "crates/rahi-ops/src/rauthy_env.rs", nature: amending }
  - { spec: "031-single-container-packaging", unit: "docker/Dockerfile", nature: amending }
  - { spec: "031-single-container-packaging", unit: ".github/workflows/image.yml", nature: additive }
  - { spec: "039-release-and-out-of-tree-packaging", unit: "docker/runtime.Dockerfile", nature: amending }
  - { spec: "032-cluster-topology", unit: "deploy/README.md", nature: amending }
  - { spec: "032-cluster-topology", unit: "deploy/k8s/statefulset.yaml", nature: amending }
  - { spec: "037-identity-recovery-and-live-proof", unit: ".github/workflows/live.yml", nature: additive }
references:
  - { unit: { kind: file, path: "docs/design/01-consumer-contract.md" }, role: context }
  - { unit: { kind: file, path: "docs/design/02-operational-prerequisites.md" }, role: context }
  - { unit: { kind: file, path: "docs/design/04-patched-adoption-producer-requests.md" }, role: context }
  - { unit: { kind: file, path: "docs/design/05-patched-adoption-delivery-plan.md" }, role: context }
  - { unit: { kind: file, path: "docs/design/06-owner-decision-packet-2026-09-23.md" }, role: context }
  - { unit: { kind: file, path: "docs/design/07-owner-decision-packet-rev3-2026-09-23.md" }, role: context }
summary: >
  rahi's application store runs hiqlite 0.14.0 through a root git patch that
  registry consumers never inherit, and its co-deployed identity provider
  runs upstream Rauthy 0.36.2. Published downstream builds exist for both,
  hiqlite-patched 0.15.0-patched.1 and rauthy-patched 0.36.2-patched.2, each
  qualified by its producer at N=1. This spec adopts exactly those two
  artifacts for the N=1 cell under a narrow recorded exception to 011's
  no-fork rule. It crosses the one incompatible on-disk boundary (the cache
  raft log) by relocating the app store behind a permanent fence that every
  pre-043 binary refuses at its old path, installed whole and verified
  quiescent before anything moves, and a second fence at rahi's rendered
  Rauthy environment that stops every published pre-043 supervisor before
  it spawns Rauthy; every new-binary entry point takes its locks before it
  reads the state file; Rauthy moves its own cache under consent, and the
  resumable upgrade is released only on a Rauthy build whose interrupted
  move is repaired. It keeps every revocation accepted before the upgrade
  refused by a permanent issued-at floor that needs no historical lifetime,
  bounds every admitted token by an enforced lifetime ceiling, prunes
  durable revocations against the manifest's hard lifetime maximum, states
  the restore boundary, turns a proven terminal storage failure into a
  bounded exit, and reports every stop's observed outcome with a non-zero
  exit when completion is not confirmed. It changes no topology and claims
  no N=3 support.
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
crossed, fenced, resumed and abandoned (B-3 to B-5a); how every
new-binary entry point is excluded and gated (B-4a); which revocations
survive it and every other cache loss, and where that stops (B-6 to B-6c);
what a terminal storage failure does (B-7, B-8); and how long a stop may
take and how its outcome is observed and reported (B-9, B-10). An N=3
composition (spec 044, draft) is separate and consumes whatever release is
later qualified at N=3.

This is revision 3 of the draft. Revision 1 (`5707f60`, draft PR #75)
held hiqlite's own lock files in the transition verb and armed an
old-version guard only after the cache move; section 7.3 records why both
were wrong. Revision 2 (`95622f1`) could proceed while a stopping pre-043
node still had its database open, trusted a destination's existence as
proof of a move, read its state before taking its lock, pruned revocations
by a lifetime a later deploy could raise, and left a directly started
pre-043 supervisor free to spawn Rauthy; section 7.4 records those and what
replaced them.

## 2. Territory

The workspace dependency declaration and supply-chain policy (010,
amending); the app store's directory in the core configuration type (010,
amending: `Config::hiqlite_dir` moves, B-4); the store's error mapping and
the SQL tables this spec adds (011, amending and additive); one 016 test
that encoded the old engine's error swallowing (amending); the bearer check
and the revocation store (025, 038, amending; 021's crate root, additive);
the new transition verb, the cell lock and the stop record in `rahi-ops`
(`src/upgrade.rs`, `src/cell_lock.rs`, `src/stop.rs`, new) with CLI wiring,
the rendered Rauthy environment's path (031, amending, B-5),
including the new verb in 030's argv parser (`verbs.rs`, amending: per 030
D-9, extending `VERBS` belongs to the spec that adds the verb, and 030
AC-2's required set is derived from it) and the one gate every store-opening
verb passes through (`rahi-cli/src/lib.rs`, `serve.rs`, amending);
`rahi-ops/src/lib.rs`'s lock and directory helpers (030, amending);
preflight's reported bound and restore's floor and fence (030, amending);
`serve`'s gating, terminal exit, stop record and exit status (030, 035,
amending); the supervisor's cell lock, Rauthy consent, stop recording and
exit status, and `first-boot`'s fence on a fresh volume (031, amending); both Dockerfiles' Rauthy pin (031, 039,
amending); the operator README and StatefulSet grace (032, amending); the
live workflow's upgrade and old-version legs (037, additive).

## 3. Behavior

Terms.

- **L** is the access-token lifetime ceiling the manifest declares
  (`access_token_lifetime_secs`, 038 B-6). 038's preflight reads each
  declared client's lifetime back from Rauthy and fails when one exceeds
  L; B-6 below makes the bearer check enforce it too, so L bounds every
  token the cell admits, whatever Rauthy is configured to issue.
- **V**, the accepted validity, is `denylist_ttl(L)` = `L + 2 *
  LEEWAY_SECONDS` = `L + 120` seconds (038 D-7). With the manifest default
  `L = 600`, `V = 720`. `denylist_ttl` is the only implementation of V
  (FR-006).
- **L_max** is `MAX_ACCESS_TOKEN_LIFETIME_SECS` (86,400), the upper bound
  the manifest's validation enforces on L (`rahi-kernel` `manifest.rs`,
  rauthy's own client limit). No build of this version admits a token
  whose `exp - iat` exceeds it. **The prune horizon** is `V(L_max)` =
  86,520 seconds, kept in SQL and raised (never lowered) at each boot to
  the running build's value.
- **The legacy path** is `<data>/hiqlite`, where every pre-043 binary
  (v0.1.0, v0.2.0) opens the app store. **The app store** is
  `<data>/app-store`, where this version opens it. **The fence** is the
  legacy path reduced to one file, `<data>/hiqlite/state_machine/lock`,
  whose content names why it exists (`rahi-upgrade-cache <id>` or
  `rahi-fence <version>`). **Debris** is anything else a refused pre-043
  start leaves under the legacy path (D-P14: directories, `lock.hql`, and
  non-empty WAL and `meta.hql` files).
- **The supervisor fence** is `<data>/rauthy/rauthy.env`, the path where
  every pre-043 `supervise` reads the Rauthy environment `first-boot`
  rendered (031 B-1), turned into a directory holding one file, `FENCE`.
  This version renders that environment at `<data>/rauthy-env/rauthy.env`
  instead (B-5, P-10).
- **Identity** of a file or directory is its `(st_dev, st_ino)` pair.

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
  by hiqlite, and rahi adds nothing that bypasses it.
  `HQL_CACHE_LEGACY_MOVE_ASIDE` present in rahi's own environment is a
  startup error naming B-4's verb; the supervisor never forwards an
  operator's value to Rauthy. This version never opens a store at the
  legacy path: a volume whose legacy path holds anything other than the
  fence and debris is refused by every entry point with an error naming
  B-4's verb, before anything is opened, written or spawned.
- **B-4 (the transition).** `rahi upgrade-cache --backup <archive>` is the
  only supported crossing. It is a state machine persisted in
  `<data>/upgrade-cache.json`, written by write-to-temporary, fsync, rename,
  fsync of `<data>`. Each step records its **intent** before acting and its
  **completion** after; recovery reads the last record. The verb never
  holds a hiqlite lock file through its own descriptor while it opens
  hiqlite in the same process (D-P6: hiqlite's advisory locks are per open
  file description, so the verb would contend with itself). No step
  replaces an existing path: every rename is a no-replace rename
  (`renameat2(RENAME_NOREPLACE)` on Linux, `renamex_np(RENAME_EXCL)` on
  macOS; `rustix` 1.x, already in the graph, exposes both), and a
  filesystem that refuses the flag refuses the step; there is no fallback
  to a plain `rename` (FR-011). The data directory is a mounted volume
  (031, 032), never the container's overlay layer.

  | step | intent recorded | action | completion recorded | recovery if interrupted after the intent |
  |---|---|---|---|---|
  | T0 locks | none | B-4a's gate: take `<data>/cell.lock` and `<data>/transition.lock` exclusively, non-blocking, and hold both until exit; only then read the state file and inspect the layout | none | a held lock: refuse, change nothing |
  | T1 guard | `begin{id}`, `id` 128 random bits | (a) refuse if `upgrade-cache.json` names another unfinished transition. (b) Write `rahi-upgrade-cache <id>` to `<legacy>/state_machine/.rahi-guard-<id>`, fsync it, and `link(2)` it to `<legacy>/state_machine/lock`, then fsync the directory and unlink the temporary: the marker appears whole or not at all. `EEXIST` refuses: a pre-043 node is live or stopped uncleanly (the message says which from (c)'s probe); the existing marker is never modified. (c) **Quiescence**, after the marker is durable: `<legacy>/logs/lock.hql` and `<legacy>/logs_cache/lock.hql` are not held (a non-blocking probe that opens existing files only and creates none; an absent file reads as not held), and `<legacy>/state_machine/db/` holds no `-wal` and no `-shm` file (a pre-043 node closes SQLite after it releases both WAL locks and removes its marker, D-P12; SQLite removes them only when the last connection closes, the writer's and every connection of hiqlite's read pool, `state_machine.rs:117,317-330`, so their absence covers the whole pool). (d) The marker still has the identity `link` gave it and its content is this `id` (a pre-043 start truncates the marker path in place with `File::create`, and a pre-043 clean stop unlinks it, D-P11). A failure of (c) or (d) refuses and **leaves the marker where it is**: while it carries this `id` a pre-043 start panics on it, and a marker a pre-043 node has truncated is that node's own. (e) Install the supervisor fence (B-5). (f) Read `instant` from the wall clock | `guarded{instant, marker identity}` | a marker with this `id`: resume at (c) (the content proves provenance; `link` never publishes a partial file); no marker: redo (b); a marker with other content, including empty: refuse as in (b), and name the state as interrupted by a pre-043 node |
  | T1a verify | `verifying` | verify the archive with 030's read-only verification | `verified{digest}` | rerun; nothing on disk changed |
  | T2 relocate | `relocating{target, plan}` with `target` = `<app store>/pre-upgrade-<instant>/` and `plan` = every entry to move with its source path, destination path and identity, listed once | refuse, before recording the intent, when: the app store exists and is not an empty directory (`first-boot`'s layout creates it empty); any planned entry, or `<legacy>`, `<legacy>/state_machine` or `<data>`, is a symbolic link; any planned entry's `st_dev` differs from `<legacy>`'s; or `<legacy>` and `<data>` are on different devices. Then move, in the plan's order, every child of `<legacy>` except `state_machine`, and every child of `<legacy>/state_machine` except `lock`, into the app store at the same relative path, except `logs_cache` and `state_machine_cache`, which go into `target`, `state_machine_cache` before `logs_cache` (hiqlite F-130); fsync both parents after each rename. Debris that appears under `<legacy>` after the plan was recorded is moved to `<data>/upgrade-cache/evidence/<id>/` once every planned entry has moved | `relocated`; `<legacy>` is now the fence | per planned entry, by identity and never by existence: the planned identity at the source means not moved (move it); at the destination means moved; a source path holding another identity beside a moved destination is debris (to evidence, never deleted, never merged); a destination holding an identity the plan does not name, or a planned identity found at neither path, refuses and changes nothing (the volume was modified outside this verb) |
  | T3 floor | `flooring` | open the app store in-process with 0.15, which binds its configured loopback addresses since hiqlite has no start mode without listeners (the verb holds no hiqlite lock of its own; `transition.lock` keeps every attaching verb away, B-4a); in one `txn` upsert the transition row, raise the floor (B-6b) to `before = floor(instant)` in whole seconds, and raise the prune horizon; shut the store down and require `Ok` | `floored` | an empty `<app store>/state_machine/lock` is this step's own unclean stop, because only a process holding `cell.lock` and `transition.lock` while the state is `flooring` can have opened the app store (B-4a): the verb requires the app store's `hiqlite-owner.lock` to be free, moves the marker into `<data>/upgrade-cache/evidence/<id>/t3-marker-<n>` and reruns T3; a marker with any other content is refused as foreign (a defensive branch: hiqlite writes only empty markers and T2 never relocates the guard, so only AC-5's synthetic case reaches it) |
  | T4 Rauthy | written by `supervise` | while the state is `floored`, `supervise` adds `HQL_CACHE_LEGACY_MOVE_ASIDE=true` to the Rauthy child's environment and Rauthy moves its own cache | `rauthy-done`, once Rauthy answers ready | a boot while still `floored` passes the consent again; once Rauthy's own format marker exists the variable is a no-op. An interruption inside Rauthy's own two renames is on this transition's normal crash path: with `hiqlite-patched 0.15.0-patched.1`, which renames `logs_cache` before `state_machine_cache`, it can leave a 0.14 cache snapshot that the next start restores without refusal (hiqlite F-130, committed at `8e4ec4b`; repair contract hiqlite 035 B-5). This spec does not repair it; it gates the release on it (B-4b) |
  | T5 serve | written by `serve` | at `floored` or `rauthy-done`, `serve` starts normally | `done`, after the first `/readyz` 200; the file is kept as history | none needed; `serve` is idempotent here |

  The fence and the supervisor fence are never removed by this version
  except by `--abort` before `flooring` (B-5a). A second run of the verb
  after `floored` changes nothing and exits `0` naming the state. The verb
  never opens, locks, renames, reads, copies or writes Rauthy's storage
  (its `logs`, `logs_cache`, `state_machine`, `state_machine_cache` and
  every other entry hiqlite or Rauthy writes under `<data>/rauthy`); the
  only path under `<data>/rauthy` it touches is the file `first-boot`
  renders there, `rauthy.env` (031 B-1, B-5). Rauthy's own transition is
  the producer's (its leg J).
- **B-4a (entry points, locks and gating).** Every entry point of this
  version that can open, attach to or reset the app store, or spawn Rauthy,
  passes one gate in this order, before it opens, writes or spawns
  anything: (1) refuse `HQL_CACHE_LEGACY_MOVE_ASIDE` in its environment;
  (2) take its locks as the table says, non-blocking; (3) only then read
  `upgrade-cache.json` and inspect the legacy path, the supervisor fence
  and the app store, so every decision is made on a layout no other
  entry point of this version can change until the process exits; (4) when
  the legacy path is absent and there is no state file, create both fences
  (each by `link(2)` of a whole temporary, B-4 T1 (b)) before anything
  creates or opens the app store. Locks are taken once per process through
  one descriptor each, opened close-on-exec so Rauthy never inherits them,
  and held for the process's life; ownership passes inward and nothing in
  the same process opens them a second time.

  Every lock is taken non-blocking and a process that cannot take one
  refuses rather than waits, so no two entry points can deadlock. Two
  locks, because two questions: `cell.lock` says who owns the app
  store's node, and `transition.lock` says whether the layout may change.
  Only `upgrade-cache`, `--abort` and `restore` change the layout and take
  `transition.lock` exclusively; every other entry point takes it shared,
  so it cannot overlap them, and several may hold it together.

  | entry point | cell.lock | transition.lock | transition state it accepts | legacy path it accepts | notes |
  |---|---|---|---|---|---|
  | `upgrade-cache` | exclusive, to exit | exclusive, to exit | any; resumes | legacy store, partial, or fence | the only writer of T0 to T3 |
  | `upgrade-cache --abort` | exclusive, to exit | exclusive, to exit | `begin` to `relocated` only | legacy store, partial, or fence | B-5a; refused from `flooring` on |
  | `supervise` (and so the image) | exclusive, for its life; its in-process `serve` uses that ownership | shared, for its life | `floored`, `rauthy-done`, `done`, or no file | fence with debris, or absent (the gate creates both fences) | refuses before spawning Rauthy otherwise; reads Rauthy's environment only at the new path |
  | `serve` alone | exclusive, for its life | shared, for its life | as `supervise` | as `supervise` | Rauthy's consent is then the operator's |
  | `first-boot` | exclusive, to exit | shared, to exit | any | any | on a volume whose legacy path is absent and which has no state file, creates both fences before its layout creates the app store; otherwise writes only keys, rendered files and directories that are absent (031 B-1, B-2) and opens neither store. It takes the locks because it creates the fences and the app store's directory, and a concurrent `restore` or `upgrade-cache` must not see a half-built layout; in the image it runs alone, so the locks cost nothing |
  | `migrate`, `preflight`, `backup`, `ledger`, and every other verb that opens the store | exclusive when free; when another process holds it and the verb supports attaching, attaches without it | shared, for its whole attached life | `done` or no file | fence with debris, or absent (the gate creates both fences) | refuses at every other state, before attaching; since the lock is shared for the verb's life, no transition or restore begins while it is attached |
  | `restore` | exclusive, to exit | exclusive, to exit | `done` or no file | fence with debris, or absent | creates both fences before it resets the app store; refuses a volume whose legacy path holds a store |
  | a second instance of any of these | refused at a lock it cannot take (or attaches, as above) | | | | concurrent-start refusal, AC-5 |

  **The legacy path at a normal start.** An entry point that accepts the
  fence accepts debris beside it and changes none of it: removing files
  under a path a pre-043 process may be refusing on right now is its own
  race, and debris holds nothing this version reads. It reports each
  entry at start and exports `rahi_legacy_path_debris{entries}`. It
  refuses, naming B-4's verb and the path, when the marker is absent or
  its content is not one of the fence contents, or when
  `<legacy>/state_machine/db/` holds any file (a store was placed at the
  legacy path, for instance by a pre-043 `restore` after an operator
  removed the fence). `upgrade-cache` moves debris to evidence at T2 and at
  no later step.

  Hiqlite's own locks are hiqlite's: the app store's `hiqlite-owner.lock`
  and WAL locks are held by the node that has it open and by nothing else.
  The ownership handoff inside the verb is therefore: `cell.lock` and
  `transition.lock` held from T0 to exit; no hiqlite lock held by the
  verb's own descriptors at any time; at T3 hiqlite takes its own locks at
  open and releases them at `Ok` shutdown. Between T2 and T3 nothing
  excludes pre-043 binaries except the two fences, and nothing needs to:
  they never look at the app store's path, at the legacy path they meet
  the guard, and their supervisor meets the supervisor fence (D-P7, D-P8,
  D-P14). A new `logs_cache` created at T3 is created inside the app store
  by the node that owns it; no lock on the old `logs_cache` inode is relied
  on for it.
- **B-4b (the release gate, P-12).** AC-4 requires every interruption of
  the transition, T4 included, to resume to AC-3's end state. The pinned
  Rauthy build cannot meet that at the interruption inside its own cache
  move (T4, hiqlite F-130). Implementation of this spec proceeds on the
  pins of B-1 and B-2. This spec is not flipped `complete`, and no release
  carrying it is qualified, until a hiqlite-patched release carrying
  hiqlite 035 B-5's move order (or an equivalent refusal of a snapshot
  without its log) is published, a Rauthy image built on it is published
  with its own provenance, and an owner decision amends B-1 and B-2 to
  those exact artifacts with the evidence of D-P1's kind. Nothing here
  selects those versions; the owner direction to adopt patched builds is
  not that decision. The N=3 work (spec 044) is not part of this gate.
- **B-5 (exclusion).** Four populations, four mechanisms.
  *Pre-043 binaries against the app store:* the guard written at T1, whole
  and verified quiescent before any change, and kept as the fence. A
  pre-043 hiqlite panics on it, never removes it (its clean stop removes
  only the marker it created itself, and B-4 T1 (d) refuses any T1 that
  overlapped one), and never reaches the app store, which is not at its
  path (D-P7, D-P14); its racing WAL task can leave debris under the fence
  path. Every published pre-043 verb that opens the store was run against
  a fenced volume from the v0.2.0 image: `serve` and `ledger verify` panic
  on the marker, `restore` and `preflight` refuse on it, and the default
  entrypoint's `migrate` waits in attach (D-P8, D-P14).
  *Pre-043 supervisors against Rauthy's directory:* the supervisor fence.
  Every published pre-043 `supervise` (v0.1.0, v0.2.0) reads
  `<data>/rauthy/rauthy.env` with `read_to_string` before it spawns
  Rauthy and exits when the read fails; a directory at that path fails it.
  A pre-043 `first-boot` whose keys already exist skips a path that exists
  (executed on v0.2.0, D-P14); one that generates keys (`force`, an empty
  key directory, which an upgraded volume never has) tries to write the
  file over the directory and fails, so its entrypoint stops before
  `supervise` (source, not executed). T1 (e) installs
  it after the guard and before anything moves: write the rendered
  environment to `<data>/rauthy-env/rauthy.env` (temporary, fsync,
  rename), build `FENCE` in a temporary directory beside it, move the old
  file into `<data>/upgrade-cache/evidence/<id>/rauthy.env`, and no-replace
  rename the directory into place; a crash between the last two leaves the
  path absent, which a pre-043 `supervise` also refuses, and the resume
  completes it. The v0.2.0 image's `supervise` run directly on such a
  volume exits `3` before spawning Rauthy and leaves Rauthy's directory
  unchanged (D-P14); v0.1.0's is established from source only.
  *This version against itself:* `cell.lock`, `transition.lock` and the
  state file (B-4a).
  *What rahi does not exclude:* a pre-043 Rauthy started outside any rahi
  binary (a bare `ghcr.io/sebadob/rauthy:0.36.2` container or binary
  pointed at the volume), and a new Rauthy given T4's consent while such a
  process is live (D-P3). From T4 on, Rauthy's directory is in 0.15 format
  and a hiqlite 0.14 process over it can destroy its raft metadata (D-P2).
  The README states the precondition for this case only: no Rauthy outside
  the cell is started on the volume. The producer requests H-1, H-5 and
  R-4 (`docs/design/04-patched-adoption-producer-requests.md`) ask for
  mechanisms the producer can demonstrate against the old binary.
- **B-5a (abandoning and rolling back).** Before `flooring`,
  `upgrade-cache --abort` restores the pre-043 layout's **operational
  data**: every planned entry is returned to its original path, verified by
  identity, so its content is untouched; debris a refused old start created
  is moved into evidence first; the supervisor fence is replaced by the
  original rendered file from evidence; the guard is removed only when it
  still has the identity T1 recorded and this `id` as content; and an
  empty app-store directory is removed. The old image then starts as
  before. It is not a byte-identical volume. What remains, and is named in
  the verb's output and the README: `upgrade-cache.json` (state `aborted`,
  kept as history), `<data>/upgrade-cache/` with its evidence,
  `cell.lock`, `transition.lock`, `<data>/rauthy-env/` (read by nothing
  pre-043), and changed directory timestamps. From `flooring` on, the
  app store has been opened by 0.15, and the supported rollback is B-4's
  verified pre-upgrade archive restored into a fresh volume with the old
  image (030 restore, single-shot). The README states that an old binary
  started over a 0.15 store without the fence can destroy the SQLite raft
  metadata (D-P2), after which the volume is not trusted and the archive is
  the recovery; that removing the fence to satisfy a pre-043 `restore`'s
  "clear an unclean shutdown" message is unsupported; and that an archive
  written by this version restored by a pre-043 image is unsupported (030's
  restore compares migrations, not these chassis tables; not established
  by a probe).

### Revocation across cache loss

- **B-6 (durable revocation, bounded by construction).** Revocations taken
  by this version are written to SQL (a `jti` table and a subject table,
  each row carrying `revoked_at`) in the same `txn` as the revocation's
  ledger decision, and read on every bearer check. An in-process lookaside
  may accelerate reads; a miss falls through to SQL and never admits.
  **Admission.** Besides 025's and 038's refusals, the bearer check refuses
  a token: with no `iat`; with `exp < iat`, computed as a checked
  subtraction on the integer claims (no wrapping, no saturation); with
  `exp - iat > L`; and with `iat > now + LEEWAY_SECONDS` (a token from the
  future, the same leeway 025 grants `nbf`). Of these, today's validator
  enforces none: it checks `exp + LEEWAY_SECONDS > now` (saturating), `nbf`
  when present, and reads `iat` only for the subject deny-list, which
  treats a missing `iat` as issued before a revocation (D-P16). So every
  admitted token satisfies `iat <= now + LEEWAY_SECONDS` and validates at
  most until `iat + L + LEEWAY_SECONDS`.
  **Pruning.** A row is pruned only after `revoked_at + V(L_max)`, with
  `V(L_max)` the prune horizon, never by the current L. Why that bound
  holds under every L this version accepts, lowered or raised: a revoked
  token has `iat <= revoked_at + LEEWAY_SECONDS` (a self-revocation
  presents a token the check just admitted; an operator names a `jti` of a
  token that existed, and a subject row is by definition about tokens
  issued before `revoked_at`), and `exp - iat <= L <= L_max`, so it cannot
  validate after `revoked_at + L_max + 2 * LEEWAY_SECONDS`. Revision 2
  pruned at `V(L*)`, the largest L declared so far, which fails when L
  rises after a prune (7.4 item 4). A later build that raises `L_max`
  raises the horizon at its first boot and, in the same `txn`, raises the
  floor (B-6b) to that boot's instant, since rows pruned under the old
  horizon cannot be recovered. A cache loss of any cause (this upgrade, a
  lost cache directory, a restart) removes nothing a check relies on.
  **The clock.** The argument assumes the one host clock did not step
  backwards between a token's issue and its revocation; a subject
  revocation fails safe under a backward step after it (tokens issued
  afterwards carry a smaller `iat` and are refused).
- **B-6a (revocations that exist only in the old cache).** They cannot be
  carried: the 0.14 cache is unreadable by 0.15 by design, no backup holds
  it, and 0.2.0 has no export. B-6b replaces them conservatively.
- **B-6b (the floor).** The store keeps one durable floor, `before`, in
  whole seconds, which only rises. Every bearer token whose `iat` is at or
  before `before` is refused `401` with 025 B-6's challenge (inclusive:
  `iat == before` is refused, `iat == before + 1` is admitted); a token with
  no `iat` is refused (B-6). T3 raises it to `floor(instant)`. It never
  lifts: it needs no claim about any token's lifetime, historical or
  current, because a token at or before `before` is refused whatever its
  `exp`, and one that has expired would be refused anyway. It lives in SQL,
  so a restart, a cache loss and a lifetime change keep it. **Why
  `instant` covers every lost revocation:** every revocation in the old
  cache was recorded by a pre-043 `serve` before T1 found every pre-043
  node gone and put the guard in place, so its `revoked_at < instant`; it
  named tokens Rauthy had already issued, and in the N=1 cell Rauthy and
  rahi read one host clock, so their `iat <= revoked_at < instant`. A token issued by a pre-043
  cell has `iat <= instant` unless the clock stepped backwards, since the
  old cell stopped before T1. The assumption is that the wall clock did not
  step backwards between the last pre-043 issue or revocation and
  `instant`. **If it did**, by Δ seconds: a token issued or revoked by the
  old cell can carry an `iat` up to Δ after `before`, so a revocation that
  lived only in the old cache stops protecting a token whose `iat` falls in
  that gap, and that token is admitted until its own `exp +
  LEEWAY_SECONDS`: at most Δ + L + 60 seconds of clock time after the
  upgrade. The README states the assumption and the consequence. P-11 is
  the conservative, clock-independent alternative, offered for the owner's
  choice and not part of this text unless accepted.
  Consequence, stated in the README and the release notes: every bearer
  access token issued before the upgrade is refused from the transition on;
  a native client must refresh; a refresh presented before the refresh
  token's own `nbf` (Rauthy sets it to `issued_at + L - 60`) makes Rauthy end
  that user's sessions and tokens, so the user logs in again; browser
  sessions keep their Rauthy session and pay 022's renewal round trip.
  Revocations Rauthy enforces itself (038 D-8's session end) are in
  Rauthy's database and survive the move-aside.
- **B-6c (the restore boundary).** A restore returns both databases to the
  archive's instant, so revocations taken after that instant are absent
  from the restored SQL and from Rauthy's restored database. Always: a
  restore of an archive written by a pre-043 version raises the floor to the
  restore instant, because that archive's revocations lived in a cache it
  does not carry (B-6a's case, not P-7's). Proposed (P-7): every restore
  raises the floor to the restore instant, so every access token issued
  before the restore is refused, which covers every access-token revocation
  taken after the archive instant. In both cases `restore` records the
  pending floor in its marker, and the first open after the restore raises
  it in one `txn` before `/readyz` can succeed. What no floor covers, stated
  as the boundary: a Rauthy-side revocation (a session or refresh token
  ended after the archive instant) comes back with Rauthy's restore, and
  that refresh token can mint new access tokens, issued after the floor and
  admitted, until it expires or is revoked again.
  **What a restore forgets, and what compensates.** The floor, the prune
  horizon and the revocation rows return to the archive's values. Lost with
  the archive: revocation rows written after its instant, and any floor
  raise after it (only a restore raises the floor after T3). The pending
  floor compensates for both when it is raised (always for a pre-043
  archive; for every archive if P-7 is accepted), because the restore
  instant is later than every lost raise and every lost revocation; the
  first boot then raises the horizon to the running build's value. Without
  P-7 a token revoked after the archive instant, or covered only by a lost
  raise, is admitted until its own expiry, at most L + 60 seconds after
  its `iat` (B-6). The restore instant is read after the archive is
  verified and before the first byte of the volume changes; B-6b's clock
  assumption applies to it.

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
- **B-10 (observed outcome, inferred cause, exit status).** At boot the
  process rewrites `<data>/stop.json` as `{boot, started_at}`; on SIGTERM it
  adds `received_at`; on exit it adds the **outcome** and each phase's
  duration. Every write is write-to-temporary, fsync, rename, fsync of
  `<data>`, so a reader sees one whole record or the previous one. The
  process reports only what it observed:
  - **confirmed**: every phase finished inside its bound, no stream or
    connection was cut, every queued denial was ledgered,
    `Client::shutdown` returned `Ok`, and (under `supervise`) Rauthy exited
    `0` inside its grace. Exit `0`.
  - **unconfirmed**, with every reason that applies: `store_timeout`,
    `store_error`, `stream_drain_overrun`, `connection_drain_overrun`,
    `denials_abandoned{n}`, `serve_grace_overrun`, `rauthy_nonzero{code}`,
    `rauthy_killed` (the supervisor sent SIGKILL; witnessed by the process
    that sent it), `storage_terminal`. Exit `3`.

  The next boot classifies the previous one from the record it left:
  an outcome is taken as written; `started_at` with no `received_at` is
  **stopped without a recorded signal**; `received_at` with no outcome is
  **incomplete after SIGTERM**; a record for an older boot, or none, is
  **no record for the previous boot**. None of the last three names a
  cause: a SIGKILL, a crash, a power loss and an interrupted write all
  produce them. A cause is labelled **forced kill, witnessed** only when a
  witness record names that boot: the supervisor writes one for the Rauthy
  child it kills, and a test harness writes one for the process it kills.
  The next boot logs the classification and exports it as
  `rahi_previous_stop{outcome, cause}` with `cause` one of `recorded`,
  `witnessed_kill` or `unknown`.

## 4. Functional requirements

- **FR-001.** `tests/dependency_identity.rs` reads `cargo metadata --locked`
  for both Linux targets and fails on any hiqlite package other than B-1's
  two, any git source, or a checksum different from D-P1.
- **FR-002.** The image build fails when the two `RAUTHY_IMAGE` values
  differ, when `rauthy --version` differs, or when the binary hash differs.
- **FR-003.** The transition is a library function with an injectable fault
  point after every intent and every action of B-4, including after each
  single rename of T2, after the guard's temporary write, its fsync, the
  `link`, the directory fsync and the `guarded` record, and after each of
  the supervisor fence's four steps, run against real stores in temporary
  directories.
- **FR-004.** The bearer check reads revocation state and the floor through
  the store handle, so a test can delete the cache directories and observe
  refusals hold.
- **FR-005.** `NodeFailed` is produced in tests by a real storage fault
  (the data directory made unwritable under a running node, or a torn WAL
  record), never by a mocked client.
- **FR-006.** V has one implementation (`denylist_ttl`), used by preflight,
  pruning and every test; pruning reads it only as `V(L_max)`; no floor
  reads V.
- **FR-007.** The composition check of B-9 is a unit test over the
  constants and the shipped manifests.
- **FR-008.** B-4a's gate is one function in `rahi-ops`, called by every
  entry point in its table before anything else touches the volume, and it
  takes its locks before it reads the state file or inspects the layout; a
  test enumerates the CLI's verbs (030 D-9's `VERBS`) plus `first-boot`
  and fails on one that reads the state file, opens the store, or spawns
  Rauthy without it, or that reads before locking. A test asserts
  `first-boot` opens neither store.
- **FR-009.** Every exit path of `serve` and `supervise` carries B-10's
  outcome into its exit status: `Booted::shutdown` returns the store's
  shutdown result, the drains report overruns, and the supervisor's
  SIGTERM path returns non-zero on a serve overrun, a serve error, a Rauthy
  kill or a Rauthy non-zero exit (today all four return `0`, D-P9).
- **FR-010.** The old-version legs of AC-4 and AC-5 run the real v0.2.0
  image by digest, `ghcr.io/statecrafting/rahi-hello-cell:0.2.0@sha256:494a566d1ea97aa348a0ccbe0adda4a87522f0b67a87518a46f980d68b66f06b`,
  and the real v0.2.0 `rahi` binary from it, in the live workflow; the
  workspace takes no dependency on hiqlite 0.14 to test against it.
- **FR-011.** Every rename this spec adds is a no-replace rename through one
  helper; on a filesystem that rejects the flag the helper returns an error
  and no caller falls back to a plain rename. A test proves a no-replace
  rename onto an existing file or non-empty directory fails and changes
  neither.
- **FR-012.** A pre-043 node's interleavings with T1 are reproduced
  deterministically in the library tests by a helper that performs exactly
  the pre-043 operations in each order that matters: holding
  `logs/lock.hql` across the marker check; truncating the marker path with
  `File::create`; unlinking it; leaving `-wal` and `-shm` beside the
  database after releasing both WAL locks (D-P11, D-P12). The live
  workflow repeats the real-binary interleavings of D-P12 and D-P13.

## 5. Acceptance criteria

Every criterion runs against the real hiqlite and, where named, the real
patched Rauthy image and the real v0.2.0 image; none substitutes a mock for
storage or identity. A required leg that did not execute fails its job.

- **AC-1 (graph).** FR-001 passes; `cargo deny check` passes with no
  hiqlite allow-git entry and no new duplicate; `cargo test --workspace
  --locked` passes, with 016's extension test asserting the refusal as an
  error.
- **AC-2 (image).** FR-002 passes on `linux/amd64` and `linux/arm64`.
- **AC-3 (transition).** On a volume and an archive produced by the v0.2.0
  image: the new image without the verb refuses before spawning Rauthy and
  changes nothing; the verb without a verifying archive refuses and changes
  nothing; with it the state reaches `floored`, the next `supervise` reaches
  `done`, both caches are aside (the app store's by the verb, Rauthy's by
  Rauthy), the legacy path is the fence, and a second boot needs nothing;
  app rows, a logged-in principal's `sub`, `ledger verify --full` and the
  key set are unchanged.
- **AC-3a (fresh volume).** On an empty volume the new image's first boot
  creates both fences before the app store, reaches ready without the verb,
  and a v0.2.0 image started afterwards on that volume, with its default
  entrypoint and with `rahi supervise` and `rahi serve` run directly,
  serves nothing, spawns no Rauthy, and changes no path under the app
  store or under Rauthy's storage.
- **AC-4 (interruption, resumption, repetition).** For every fault point of
  FR-003, and for each persistent state `begin`, `guarded`, `verifying`, `verified`,
  `relocating` (after each single rename), `relocated`, `flooring` (with
  the node killed while open), `floored`, `rauthy-done` and `done`:
  (a) a rerun of the verb, or the next new-image boot for T4 and T5,
  reaches AC-3's end state; (b) a rerun after each **completed** state
  changes nothing; (c) at no point does `serve` answer a request before
  `floored`, and `supervise` spawns no Rauthy before `floored`; (d) the real
  v0.2.0 image started with its default entrypoint on the volume, and the
  real v0.2.0 `rahi serve`, each fail to serve, spawn no Rauthy from
  `guarded` on, and leave the app store's tree (paths and content hashes)
  and the fence unchanged; at `begin` without a guard they may serve on the
  untouched legacy store, after which a rerun of the verb completes with an
  `instant` read after that run; the same holds for the real v0.2.0
  `rahi supervise` run directly, which also spawns no Rauthy from `guarded`
  on; (e) `--abort` from every state it accepts returns every planned entry
  to its path with its identity, leaves exactly B-5a's named side effects,
  and yields a tree the v0.2.0 image serves with its rows; it is refused
  from `flooring` on; (f) at T2, each of these refuses before its intent
  is recorded, or at recovery, and changes nothing: a non-empty app store,
  a symbolic link among the planned entries, an entry on another device, a
  destination that holds an identity the plan does not name, and a planned
  identity found at neither path; a duplicate source with content beside a
  moved destination goes to evidence with its bytes intact. (g) With the
  pinned Rauthy build, a crash injected between Rauthy's two cache renames
  is run and its outcome recorded; the criterion passes only on a build
  that meets B-4b.
- **AC-5 (exclusion and concurrency).** With a live v0.2.0 cell the verb
  refuses at T1 and changes nothing but the guard's temporary, naming a
  live node; with a v0.2.0 cell stopped uncleanly (its marker left), it
  refuses naming an unclean stop and leaves the marker in place; FR-012's
  interleavings each refuse at T1 (c) or (d) and leave the marker as B-4
  says; a real v0.2.0 node stopped cleanly while T1 runs, at offsets
  covering its WAL-lock release, marker removal and SQLite close (D-P12),
  never yields a `guarded` record while its database is open, and no path
  under the legacy store changes after a `guarded` record; a real v0.2.0
  node started at offsets around T1 (D-P13) never serves after a `guarded`
  record. Two new-image processes of any two entry points of B-4a started
  together, including `first-boot` with `upgrade-cache`, `restore` with
  `first-boot`, and an attaching verb with `upgrade-cache` and with
  `restore`: exactly one proceeds and the other refuses at a lock or
  attaches as B-4a allows; no entry point acts on a state file it read
  before its locks. A marker in the app store with foreign content refuses
  T3. A normal start over a fence with debris starts, reports the debris
  and changes none of it; over a legacy path with a database file it
  refuses.
- **AC-6 (revocation, not weakened).** A token revoked by `jti` and one by
  subject: (a) revoked before the upgrade, in the 0.14 cache, are refused
  after it, and still refused at `instant + V`, `instant + 2V` and after a
  restart; (b) revoked after the upgrade, are refused across a restart,
  across deletion of both cache directories, and until `revoked_at +
  V(L_max)`, including after a deploy that lowers L and after one that
  raises it (7.4 item 4's timeline: L = 600, a token with `exp - iat =
  3600` revoked at `r`, L raised to 3600 at `r + 1000`: refused until its
  expiry); (c) the boundaries hold: `iat == before` refused, `iat == before
  + 1` accepted, no `iat` refused, `exp < iat` refused, a token with `exp -
  iat == L` accepted and `L + 1` refused, `iat == now + 60` accepted and
  `now + 61` refused, and claims near `u64::MAX` refused without overflow;
  (d) a restore of
  a v0.2.0 archive by the new image refuses a token issued before the
  restore; (e) if P-7 is accepted, a token revoked after a backup point is
  refused after restoring that backup, and the floor after any restore is
  at least the archive's; (f) if P-11 is accepted, a token signed under a
  key published before the transition is refused whatever its `iat`.
  B-6c's Rauthy-side boundary and B-6b's backward-step consequence are
  documented, not tested as closed.
- **AC-7 (stop, graceful).** FR-007 passes. Then a bounded series under the
  declared workload (streams open up to 026's per-identity limit, a
  200-denial backlog in flight, Rauthy running) records per run each
  phase's duration, the outcome, time to exit, and time to both owner
  locks' release. **Every run of this series must be confirmed**; a series
  with any unconfirmed run, or one whose measured maximum times 1.5 exceeds
  the configured grace, fails this criterion.
- **AC-7a (stop, unconfirmed and unrecorded).** Separately, each exits `3`
  with its reason recorded: the store's shutdown made to overrun
  (`store_timeout`); a phase made to overrun (`stream_drain_overrun` or
  `connection_drain_overrun`); Rauthy made to ignore SIGTERM
  (`rauthy_killed`, with the supervisor's witness). The harness SIGKILLs a
  process after SIGTERM and the next boot classifies **incomplete after
  SIGTERM** with cause `witnessed_kill`; the same state without the
  harness's witness classifies with cause `unknown`. Every next boot
  succeeds with no manual step. These runs never count toward AC-7.
- **AC-8 (terminal).** FR-005's fault makes `serve` fail `/readyz`, record
  `storage_terminal`, and exit `3` within B-9's bound; denials queued at the
  fault are ledgered or counted abandoned; a boot after the fault is
  cleared is ready. A Rauthy held unready does not end the process.
- **AC-9 (live).** The live workflow (037) passes with
  `RAHI_REQUIRE_RAUTHY=1` against the image built from this change on both
  architectures it builds, runs AC-3, AC-3a, AC-4 (d) and (g) and AC-5's
  v0.2.0 legs, and reports zero skipped required legs. A v0.1.0 leg runs
  when a v0.1.0 image is still published; if it is not, the leg is
  reported unexecuted with the reason, and v0.1.0's exclusion stays
  source-established.

## 6. Out of scope

N=3 in any layout (spec 044, and a later adoption of an N=3-qualified
release); any change to hiqlite or Rauthy source; choosing the repaired
versions B-4b waits for (a later owner decision); enabling hiqlite's
`auto-heal`; S3 upload outcome reporting; replacing 030 D-2's file-level
restore with hiqlite's repaired restore; a Rauthy terminal-storage signal
and a producer-side downgrade fence, which are producer requests and not
prerequisites; an in-place rollback after `flooring`.

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
  separation. B-4, B-4a and B-5 are the mechanism.
- **D-5 (2026-09-23, owner direction; Rauthy readiness).** Transient
  unready responses stay distinct from proven terminal failure, and this
  release adds no persistent-unready restart heuristic.
- **D-6 (2026-09-23, owner direction; headroom).** The 50 percent margin is
  qualification headroom over the measured workload, alongside a validated
  composition of the configured phase budgets (B-9).

These record direction. Approval of this text, the implementation contract
that carries them out, is a separate owner act; the packet is
`docs/design/06-owner-decision-packet-2026-09-23.md`.

Still open before approval: P-7, P-8, P-9, P-10, P-11, P-12.

### 7.1 Proposals (2026-09-23)

- **P-6 (withdrawn 2026-09-23, after independent review).** An earlier
  draft had T0 lock Rauthy's WAL lock files, which means opening, and on a
  cleanly stopped volume creating, files under Rauthy's directory; that
  contradicts the plain words of spec 000's frozen `store-separation`
  anchor ("separate storage that app code never opens") and 011 B-7, and no
  ordinary approval can grant an exception to a frozen anchor. B-5 excludes
  without touching Rauthy's storage and states the residual case.
- **P-7 (the restore floor).** B-6c's floor is raised by every restore, so
  access-token revocations taken after the archive instant are not lost.
  Cost: every access token issued before the restore is refused from then
  on, with B-6b's refresh consequence.
- **P-8 (the relocated store and its permanent fence, revision 2).** The
  app store moves from `<data>/hiqlite` to `<data>/app-store`, and
  `<data>/hiqlite` becomes a fence that is never removed, on every volume
  this version touches, including a fresh one. Cost: `Config::hiqlite_dir`
  (010) changes its answer, which the consumer contract records; a pre-043
  image started on any volume this version has touched waits in its
  `migrate` step instead of serving. Revision 3 keeps it and adds B-4 T1's
  whole-file guard and quiescence check and T2's identity plan (7.4).
  Rejected alternatives in 7.3.
- **P-9 (the lifetime ceiling, the permanent floor and the prune horizon,
  revision 3).** The bearer check refuses a token without `iat`, with `exp
  < iat`, with `exp - iat` above L, or with `iat` more than the leeway in
  the future; the floor never lifts; revocation rows are pruned at
  `revoked_at + V(L_max)`, 86,520 seconds, not at `V(L*)`. Cost: a Rauthy
  misconfigured above the manifest's lifetime is refused at the bearer
  instead of only failing preflight; a token without `iat` is always
  refused (Rauthy 0.36.2 always sets it, D-P10); revocation rows live about
  24 hours instead of 12 minutes, one row per revocation. Rejected
  alternatives in 7.3 and 7.4.
- **P-10 (the supervisor fence, revision 3).** B-5's second fence: the
  rendered Rauthy environment moves from `<data>/rauthy/rauthy.env` to
  `<data>/rauthy-env/rauthy.env`, and the old path becomes a directory
  holding `FENCE`, so every published pre-043 `supervise` exits before it
  spawns Rauthy (D-P14 for v0.2.0; source for v0.1.0). It touches one path
  under `<data>/rauthy`, a file rahi itself renders there (031 B-1), and
  none of Rauthy's storage; the owner decides whether that reading of
  spec 000's `store-separation` ("separate storage that app code never
  opens") holds. Alternative: an unparseable `<data>/restore.marker`, which
  v0.2.0's `supervise` also reads before spawning (D-P14), lies wholly
  outside `<data>/rauthy`, but does not stop v0.1.0's `supervise`, which
  never reads it. Without either, a directly started pre-043 `supervise`
  spawns Rauthy on the upgraded volume (D-P14) and B-5's precondition
  covers it.
- **P-11 (a clock-independent floor, optional).** At T4, once Rauthy is
  ready, `supervise` records the key ids Rauthy's JWKS publishes, asks
  Rauthy to rotate its signing keys (`POST /auth/v1/oidc/rotate_jwk`,
  `rotate_jwk` in `src/api/src/oidc.rs` at `17132b94`, which requires an
  API key or admin session with `Secrets: Update`), and records
  `rauthy-done` only after the rotation; the bearer then refuses every
  token signed under a recorded key id, permanently. That refuses every
  pre-upgrade token whatever its `iat`, so B-6b's backward-step exposure
  closes. Cost: a Rauthy admin call on the transition's path whose failure
  holds the cell at `floored`; the cell's admin key's rights for that call
  are not yet established; another list the bearer reads. Not
  recommended unless the owner requires clock independence (07 packet).
- **P-12 (the release gate).** B-4b: implementation proceeds on the
  current pins, and completion and qualification wait for a published
  hiqlite-patched carrying 035 B-5's move order and a Rauthy image built on
  it, adopted by a later owner decision. The alternative, keeping the
  current image and narrowing AC-4 so that any interruption at T4 is
  recovered only from the verified archive into a fresh volume, needs a
  way to tell an interruption inside Rauthy's move from any other failure
  to reach `rauthy-done`, which rahi cannot observe without opening
  Rauthy's storage; every unready first Rauthy start at `floored` would
  then call for an archive restore.

### 7.2 Evidence recorded for the decisions

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
  (arm64) and prints `rauthy 0.36.2-patched.2`. The GitHub release carries
  eight assets and its provenance file agrees with every value here (D-P15;
  revision 2's "no assets" was wrong). The graph on
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
  and the volume needed manual repair. Both versions refuse to start while
  `state_machine/lock` exists; 0.15 does so by panicking, and only after its
  consent move has run. The real upstream `ghcr.io/sebadob/rauthy:0.36.2`
  image (the 0.2.0 cell's Rauthy), probed from a second container on the
  same volume: while live it holds `flock` on `logs/lock.hql` and
  `logs_cache/lock.hql` and has no `hiqlite-owner.lock`; after `docker stop`
  (exit 0) both lock files are gone and `state_machine/lock` is removed.
- **D-P4 (stop probe, 2026-09-23).** Five runs of a real single-node
  `serve` on the patched graph, SIGTERM during a concurrent 200-denial
  burst, no stream, no Rauthy: exit `0` every run, 0.37 to 0.73 s from
  SIGTERM to exit, the owner lock free at the first check (0.46 to 1.31 s).
  `Client::shutdown`'s own result was not observed separately. This is not
  AC-7's workload and bounds nothing.
- **D-P5 (the in-place guard, 2026-09-23; corrected by D-P7).** With
  `state_machine/lock` written into a store a 0.15 transition had opened, a
  0.14 start panicked on it. Revision 1 recorded that no file changed; D-P7
  shows that holds in some runs only. `SHUTDOWN_WAIT` (15 s, B-9's H) is
  `hiqlite-patched-0.15.0-patched.1/src/client/mgmt.rs:592`, applied by
  `Client::shutdown` at `:278`, which returns an error when it elapses.
- **D-P6 (self-contention, 2026-09-23; revision 2).** Probe binary on
  `hiqlite-patched =0.15.0-patched.1` (crates.io, rahi's feature set),
  macOS arm64, `docs/design/evidence/043-probes/g1.sh`. With the probe's
  own descriptor holding `flock` on `hiqlite-owner.lock`, the in-process
  `start_node_with_cache` returned `StorageInUse` in 0.26 ms; with its own
  descriptor on `logs/lock.hql`, hiqlite-wal panicked at
  `log_store.rs:49` and start returned `WAL: Generic: task 14 panicked` in
  15 ms; with both taken and dropped before start, it started and shut down
  `Ok`. Source: `storage_lock.rs` (`fs4::try_lock`, and the test
  `a_second_node_in_the_same_process_is_refused`) and hiqlite-wal
  `lockfile.rs`. Revision 1's T0, which held those three locks until exit
  and opened the store at T3, could not have run.
- **D-P7 (old version at every intermediate state, 2026-09-23; revision
  2).** Real hiqlite 0.14 at `8f3b9bd` (v0.2.0's) against disposable
  volumes prepared by real 0.14 and 0.15 runs, a full path and content hash
  of the tree before and after each attempt
  (`docs/design/evidence/043-probes/g2.sh`). Revision 1's in-place layout:
  after a crash at `moved` and after a clean `floored`, 0.14 **started and
  served** (a fresh 0.14 cache, no floor, no guard); after a crash during
  T3, 0.14 refused on the left marker; at `armed`, 0.14 refused but created
  `logs/lock.hql` inside the live store. Revision 2's layout: with the guard
  in the intact store, with the store partly relocated, and with the fence
  alone, 0.14 refused every time, the guard was never removed, and the only
  changes were empty `logs/` WAL files and empty directories under the
  fence path (its WAL task races the state machine's panic); the relocated
  store was never touched. 0.15 opened the relocated store at its new path
  without consent, rows intact; after a SIGKILL while open, with the marker
  moved to evidence, it reopened with rows intact. Limitation: macOS
  arm64, one run per state; AC-4 repeats this on Linux with the real image.
- **D-P8 (the v0.2.0 image on a fenced volume, 2026-09-23; revision 2).**
  `ghcr.io/statecrafting/rahi-hello-cell:0.2.0` (index
  `sha256:494a566d...b66f06b`, arm64) with a disposable volume holding only
  the fence and a Rauthy sentinel: `first-boot` wrote its absent keys and
  Rauthy's rendered files, `migrate --adopt-manifest` blocked in attach, no
  Rauthy process existed, and nothing under `hiqlite/` changed; the
  container was stopped after three minutes. Source: v0.2.0
  `entrypoint.sh` (`set -eu`, `migrate` before `supervise`) and
  `Booted::open_or_attach` (attach when the marker exists). Limitation: a
  fresh volume, not an upgraded one; the image's default entrypoint only.
- **D-P9 (exit status on `main`, source, 2026-09-23; revision 2).** At
  `b815b18`: `Booted::shutdown` discards the store's result
  (`crates/rahi-cli/src/serve.rs:278-280`), so a store `Timeout` exits `0`;
  the stream and connection drains log an overrun and return `Ok`
  (`serve.rs` `serve_until`); the supervisor's SIGTERM arm maps a serve
  overrun to `0` (`_ => 0`) and discards Rauthy's termination status
  (`crates/rahi-ops/src/supervise.rs`, the `shutdown` arm of
  `supervise_with`), as does the arm taken before Rauthy is healthy.
- **D-P10 (Rauthy always sets `iat`, source, 2026-09-23; revision 2).**
  `bartekus/rauthy@17132b94` `src/jwt/src/claims.rs:15`:
  `JwtCommonClaims::iat` is an `i64`, not optional, and is always
  serialized.

- **D-P11 (the pre-043 marker lifecycle, source, 2026-09-23; revision
  3).** hiqlite 0.14 at `8f3b9bd`: `LogStore::start` takes `logs/lock.hql`
  and holds it in its writer (`hiqlite-wal/src/log_store.rs:43-51`, probe
  then `create` with `truncate(true)` then `lock`), and only afterwards
  does `StateMachineSqlite::new` run `check_set_lock_file`
  (`hiqlite/src/store/mod.rs:50-56`), which tests the marker with
  `File::open` and then creates it with `File::create`, truncating an
  existing file (`state_machine.rs:258-290`); a clean stop's SQL writer
  unlinks the marker by path with `remove_file` (`writer.rs:688`). So a
  pre-043 start that passed its check before a guard appeared truncates the
  guard in place, and a pre-043 node that created its own marker removes
  whatever file is at that path when it stops; in both cases it holds
  `logs/lock.hql` from before its check until its WAL writer stops.
- **D-P12 (a pre-043 node stopping under T1, 2026-09-23; revision 3).**
  The rebuilt probes of `evidence/043-probes/` with dynamically allocated
  ports and `race.py`, macOS arm64, real 0.14. A timeline of one clean
  stop, sampled continuously: `logs_cache/lock.hql` released and removed
  at 0.51 ms, `logs/lock.hql` at 1.27 ms, the marker removed at 1.57 ms,
  SQLite's `-shm` removed at 1.9 ms and `-wal` at 2.18 ms, the process gone
  at 3.06 ms. Two runs of T1 at ten offsets each across that stop: T1 met
  an existing marker 6 times; once, at 1.0 ms, both WAL locks were free and
  the new marker was in place while the old node still had SQLite open
  (`-wal` and `-shm` present), which revision 2's T1 would have accepted
  and revision 3's (c) refuses; 13 times it proceeded after the stop, and
  in every one of them no path under the legacy store changed afterwards
  and the new marker was untouched.
- **D-P13 (a pre-043 node starting under T1, 2026-09-23; revision 3).**
  Same setup, 15 launches with T1 0 to 30 ms after the 0.14 start, eight
  of them between 11.0 and 14.5 ms: T1 before the old node took
  `logs/lock.hql` proceeded and the old node then panicked on the guard
  (1); T1 while it held the lock and had not yet created its marker refused
  on the lock, and the old node then panicked on the guard (3); T1 after
  its marker refused on `EEXIST` (11). In no launch
  did T1 proceed and the old node serve. The microsecond window between the
  old node's check and its create (D-P11's truncation) was not hit; it is
  source-established, and FR-012 reproduces it deterministically.
- **D-P14 (published pre-043 verbs on a fenced volume, 2026-09-23;
  revision 3).** The v0.2.0 image by digest (arm64), a disposable volume
  with the guard, a populated `app-store/` and `first-boot`'s files, each
  verb run with `--entrypoint rahi`, path and hash snapshots before and
  after: `serve` panicked on the marker (exit 101) and left under the
  fence path `logs/` with a non-empty `0000000000000001.wal`, an empty
  `lock.hql` and a non-empty `meta.hql`, plus empty `state_machine/backups`
  and `state_machine/db`; `ledger verify` panicked on the marker and
  changed nothing; `restore` refused (exit 1, "the app node holds
  /data/hiqlite/state_machine/lock; stop it (or clear an unclean shutdown)
  before restoring") before reading the archive (v0.2.0 `restore.rs`
  `refuse_running`); `preflight` failed on the marker; the app store was
  unchanged every time. `rahi supervise` run directly spawned upstream
  Rauthy 0.36.2 within one second, which created `logs`, `logs_cache`,
  `state_machine` and `state_machine_cache` under `rauthy/`. With the
  supervisor fence (a directory at `rauthy/rauthy.env`): `supervise` exited
  3 ("Is a directory") before spawning, `first-boot` exited 0 writing
  nothing, and Rauthy's directory entries were unchanged. With an
  unparseable `restore.marker` instead: `supervise` exited 1 before
  spawning, twice. v0.1.0 (source at the tag): `supervise` reads the
  rendered environment with `read_to_string` before spawning and never
  reads `restore.marker`; `first-boot` skips an existing path; its
  entrypoint runs `migrate` before `supervise`. Limitation: arm64, one run
  per verb except where stated, a fresh rather than an upgraded volume.
- **D-P15 (Rauthy release assets, 2026-09-23T20:27:33Z; revision 3).**
  Authenticated and anonymous reads of release `v0.36.2-patched.2` list
  eight assets, uploaded 2026-09-23T09:14:48Z to 09:14:51Z: `image-index.txt`,
  `LICENSE`, `rauthy_amd64`, `rauthy_arm64`, `RELEASE-HANDOFF.md`,
  `RELEASE-LEDGER.md`, `RELEASE-PROVENANCE.md` and `SHA256SUMS`. The
  anonymously downloaded `SHA256SUMS`, `RELEASE-PROVENANCE.md` and
  `image-index.txt` hash to the digests the release reports, and state the
  index, both platform digests, both binary hashes, upstream base
  `dd61ac3c` and source `17132b94` exactly as D-P1 does.
- **D-P16 (today's bearer validator, source, 2026-09-23; revision 3).**
  `crates/rahi-idp/src/bearer.rs` `validate` at `b815b18`: `exp +
  LEEWAY_SECONDS <= now` refuses (saturating), an `nbf` beyond `now +
  LEEWAY_SECONDS` refuses, `iat` is optional and read only by
  `revoke::is_subject_denied`, which treats a missing `iat` as issued
  before the revocation; there is no check of `exp` against `iat`, of
  `exp - iat` against a lifetime, or of a future `iat`.
  `rahi-kernel/src/manifest.rs` validates `10 <= L <=
  MAX_ACCESS_TOKEN_LIFETIME_SECS` (86,400).

### 7.3 Revision 2 corrections (2026-09-23)

What revision 1 (`5707f60`) got wrong, and what replaced it.

1. **T0 to T3 ownership.** Revision 1 held `hiqlite-owner.lock` and both
   legacy WAL locks until exit and opened the store at T3 in the same
   process; D-P6 shows that start fails. Releasing them before T3 would
   have left the in-place store with no exclusion of pre-043 binaries,
   which revision 1 assumed `cell.lock` provided; it does not, since no
   pre-043 binary knows `cell.lock`. And the lock revision 1 held on the old
   `logs_cache/lock.hql` inode moved with the directory, protecting nothing
   at the original name. Revision 2 holds no hiqlite lock through its own
   descriptors (B-4, B-4a) and excludes pre-043 binaries by the fence, which
   does not depend on any process being alive.
2. **Interruption before the guard.** Revision 1 armed the guard at T4, so
   every interruption from T2 to T3 left a store a pre-043 binary opens and
   serves (D-P7). Revision 2 writes the guard at T1 before anything
   changes. The rejected alternative, an in-place guard written at T1,
   would have to be removed before any in-process 0.15 open (0.15 panics on
   it, like 0.14), leaving a window with neither the guard nor hiqlite's own
   marker at every such open; relocation removes the need to ever remove
   it. A second rejected alternative, swapping directories atomically,
   needs `renameat2(RENAME_EXCHANGE)` or `renamex_np(RENAME_SWAP)`; the
   per-entry relocation needs only `rename`.
3. **Floor lifetime.** Revision 1 lifted the floor at `before + V` with V
   from the first post-upgrade read of Rauthy's lifetime. That read does not
   bound every pre-upgrade token: tokens were issued under whatever
   lifetime Rauthy applied before the upgrade, which a later read cannot
   observe (a decrease at the upgrade makes V too short); the read covers
   declared clients only, at one moment; and it can be delayed or fail.
   Revision 2's floor never lifts (P-9), so no historical lifetime is
   needed, and the bearer's lifetime ceiling bounds every admitted token.
4. **Stop outcomes.** Revision 1 read a record with `received_at` and no
   outcome as a forced exit. It proves only that the stop did not finish
   recording; B-10 now separates the observed outcome from the cause and
   labels a kill only when a witness recorded it. And revision 1 did not
   notice that `main` returns `0` on a store timeout, a phase overrun and a
   killed Rauthy (D-P9, FR-009).
5. **The old image and Rauthy.** Revision 1 said a v0.2.0 serve refuses and
   its supervisor then ends its Rauthy. v0.2.0's `supervise` spawns Rauthy
   and waits for its health before `serve` reads anything (031 B-3), so on
   a volume whose Rauthy directory is already 0.15, old Rauthy would open
   it first. D-P8 shows the default entrypoint never reaches `supervise`
   on a fenced volume; B-5 states the remaining case as an operator
   precondition and a producer request.

### 7.4 Revision 3 corrections (2026-09-23)

What revision 2 (`95622f1`) got wrong or left open, and what replaced it.

1. **A stopping pre-043 node.** Revision 2's T1 accepted a guard with both
   WAL locks free. A pre-043 node releases both before it removes its
   marker and closes SQLite (D-P12), so T1 could proceed while the old
   process was still writing its database, and T2 would then move that
   database. T1 now also requires no SQLite `-wal` or `-shm` beside it,
   and re-verifies the guard's identity and content after the probe
   (D-P11: an old start truncates the guard in place; an old clean stop
   unlinks it by path). A refusal leaves the guard rather than removing it:
   revision 2's "remove the guard when its content is this `id`" could
   unlink a marker an old node had just truncated and adopted.
2. **A partial guard.** Revision 2 created the guard with `O_EXCL` and then
   wrote its content, so a crash left an empty marker indistinguishable from
   a pre-043 node's. The guard is now published whole by `link(2)`; an
   empty or foreign marker is never this transition's, and is refused and
   left in place.
3. **Relocation provenance.** Revision 2's recovery read an existing
   destination as a completed move and a surviving source as debris. The
   plan now records identities before the first rename and recovery decides
   by identity; every rename is no-replace, since a plain `rename` silently
   replaces a file or an empty directory; symbolic links and device
   boundaries refuse before the intent. Unexpected state refuses and
   overwrites nothing.
4. **Pruning under a raised lifetime.** Revision 2 pruned at `revoked_at +
   V(L*)`. Timeline: L = 600, so L* = 600; Rauthy is misconfigured and
   issues a token with `iat = 0`, `exp = 3600`, which the ceiling refuses;
   it is revoked at `r = 100`, and its row is pruned at `820`; at `1000` a
   deploy raises L to 3600; the token now passes the ceiling and is admitted
   until `3660`. Pruning now uses `L_max`, which no deploy of this version
   can exceed.
5. **Admission checks.** `exp < iat`, a future `iat`, and overflow in `exp
   - iat` were unspecified; today's validator enforces none (D-P16). They
   are now refusals.
6. **Locks before reads.** Revision 2's gate read the state and layout
   before taking `cell.lock`, and exempted `first-boot`. The gate now locks
   first; `transition.lock` keeps attaching verbs, which hold no
   `cell.lock`, out of a transition or restore for their whole attached
   life; `first-boot` takes both locks because it creates the fences and the
   app store's directory.
7. **Debris.** Revision 2 said old starts leave only empty files. The
   v0.2.0 `serve` left a non-empty WAL and `meta.hql` under the fence path
   (D-P14). Normal starts now accept debris without touching it and refuse
   only a database at the legacy path or a missing or foreign marker.
8. **Pre-043 supervisors.** Revision 2 left a directly started pre-043
   `supervise` to the operator. It spawns Rauthy before anything reads the
   fence (D-P14). P-10's supervisor fence stops every published one before
   the spawn.
9. **Abort.** "Returns the volume to the pre-043 layout exactly" is now the
   restoration of operational data by identity, with the side effects
   named (B-5a).
10. **F-130 is on the normal crash path.** Revision 2 called the
    interrupted Rauthy move the producer's and AC-4 still required its
    recovery. B-4b now gates completion and qualification on a repaired
    producer build (P-12).
11. **R-3.** The release assets exist and agree (D-P15).

## Verification

```verify:cli
cargo test -p rahi-store --locked --test dependency_identity
cargo test -p rahi-store --locked --test blob
cargo test -p rahi-ops --locked --test upgrade
cargo test -p rahi-ops --locked --test cell_lock
cargo test -p rahi-idp --locked --test revocation_durable
cargo test -p rahi-cli --locked --test terminal
cargo test -p rahi-cli --locked --test stop_budget
cargo test -p rahi-cli --locked --test stop_outcome
```
