# Patched adoption: producer requests and the hiqlite reconciliation

Version 1, 2026-09-23. Owned by `specs/043-patched-dependency-adoption/spec.md`
(draft, revision 2). Maintained by the rahi owner. Stable references: the
request ids below (`H-n`, `R-n`, `C-n`) are cited by 043 and by
`06-owner-decision-packet-2026-09-23.md`, and are never renumbered; a
withdrawn request keeps its id and says so.

This document replaces three scratchpad notes of the 2026-09-23 session
(`hiqlite-producer-request.md`, `rauthy-producer-request.md`,
`rauthy-producer-followup.md`), which were never in a repository. rahi edits
neither producer's repository. Nothing in rahi's N=1 delivery waits on any
request marked non-blocking; each says what changes if it arrives.

**Blocking classes.**

- **N1-blocking**: 043 cannot be approved or cannot complete without it.
- **Claim-blocking**: 043 ships without it, but a named claim stays out of
  rahi's documents until it arrives.
- **Non-blocking**: an improvement a later change adopts.
- **N3-only**: belongs to Track S7 or spec 044, never to 043.

No request below is N1-blocking.

## 1. Requests to the hiqlite-patched producer

Owner: the `bartekus/hiqlite` maintainer. Consumer: rahi spec 043, and
through Rauthy's image every Rauthy consumer. Evidence: 043 D-P2, D-P3,
D-P6, D-P7 and `evidence/043-probes/`.

### H-1. Exclude before moving (the live-old-version cache-move hazard)

**Problem.** With `HQL_CACHE_LEGACY_MOVE_ASIDE=true`, a 0.15 start against a
data directory a live 0.14 node is using renamed the live node's
`logs_cache` and `state_machine_cache` into `pre-upgrade-<secs>/` and only
then failed on the 0.14 WAL lock (`hiqlite-wal-patched log_store.rs:49`). The
live node then panicked at shutdown and the directory needed manual repair
(043 D-P3). This is an N=1 hazard of the consent path, and it is **not** part
of N=3 qualification: it concerns two processes on one directory, which the
N=3 work does not change.

**Requested.** Before `ensure_cache_log_format` moves anything, and after
taking `hiqlite-owner.lock`, test the legacy WAL locks (`logs/lock.hql`,
`logs_cache/lock.hql`) with a non-blocking `flock` on existing files only;
if either is held, return `StorageInUse` and change nothing.

**Acceptance.** A live 0.14 node, then a 0.15 start with consent on the same
directory: refused, every path and content hash unchanged except
`hiqlite-owner.lock`, and the 0.14 node stops cleanly.

**Class.** Claim-blocking. Until it ships in a release rahi pins, 043 B-5
states the precondition (old container stopped and removed before the new
image starts) as the operator's, and rahi claims no mechanical exclusion of
an old Rauthy. When it ships, the Rauthy image rebuilt on it closes the case
for Rauthy's directory too, and a later rahi change may say so.

### H-2. Check the unclean-stop marker before the move, and refuse without panicking

**Problem.** A 0.15 start with consent over a directory holding
`state_machine/lock` performed the move, then panicked on the marker (D-P3).

**Requested.** Check the marker before any move; report it as
`Error::Startup` rather than a panic.

**Acceptance.** A directory with a 0.14 marker and a 0.14 cache, a 0.15
start with consent: `Err(Startup)`, no move, no panic.

**Class.** Non-blocking. 043 never starts 0.15 with consent over the app
store, and Rauthy meets it only after an unclean Rauthy stop.

### H-3. Say what a refusal created

**Problem.** The legacy-cache refusal says "Nothing was changed." but creates
`hiqlite-owner.lock` (D-P2).

**Requested.** Either create nothing before refusing or name what was
created. **Acceptance.** Message and behavior agree in a before-and-after
tree hash. **Class.** Non-blocking.

### H-4. Record the unsafe downgrade and the racy marker refusal

**Problem.** A 0.14 start over a 0.15-written cache panicked in both of two
runs and in one zeroed `logs/meta.hql`, after which neither version could
start (D-P2). Separately, a 0.14 start that refuses on `state_machine/lock`
may first create or remove WAL files, because its WAL task races the state
machine's panic (D-P7).

**Requested.** Findings-register entries beside the handoff's downgrade
warning. **Acceptance.** The entries exist on a published branch.
**Class.** Non-blocking.

### H-5. A downgrade fence in the 0.15 format

**Problem.** Nothing in a 0.15 directory makes a 0.14 binary refuse it, and
a 0.14 binary over it can destroy its raft metadata (D-P2). rahi fences its
own app store by relocating it (043 P-8), but cannot fence Rauthy's
directory without opening it (spec 000 `store-separation`), so an old Rauthy
started on a post-upgrade volume is protected only by the operator
precondition.

**Requested.** A persistent marker in every 0.15 data directory that every
0.14 build refuses before touching storage, for example a permanent
`state_machine/lock` with a content tag while 0.15 keeps its own unclean-stop
marker under a new name.

**Acceptance.** 0.14 against a 0.15 directory refuses, and every path and
content hash under the directory's raft and state-machine subdirectories is
unchanged (the probe method of `evidence/043-probes/g2.sh`).

**Class.** Claim-blocking, for the claim that an old Rauthy cannot damage a
post-upgrade volume; non-blocking for 043's delivery.

### H-6. Publish the N=3 reconciliation

**Problem.** The hiqlite reconciliation with rahi's decision packet,
`ec8fc6d` ("docs(033): reconcile the N=3 proposal with Rahi's
patched-adoption packet"), is on the local branch `proposal/033-n3-topology`
and no remote ref contained it on 2026-09-23. Section 2 below cites it.

**Requested.** Push the branch or merge it. **Acceptance.** `ec8fc6d` is
reachable from a ref of `bartekus/hiqlite`. **Class.** Non-blocking; section
2 records what was read.

### H-7. Review revision 2's fence route

**Problem.** The hiqlite working tree after `ec8fc6d` (section 2.1) concludes
that no public-API rearrangement keeps lock-based exclusion continuous from
rahi's locks into a 0.15 start, and offers rahi two routes: wait for its
draft `035` B-6 handle, or rely on an operator precondition. 043 revision 2
takes a third route that draft did not examine: pre-043 binaries are
excluded by a persistent marker at a path they are the only readers of, and
the live store is relocated to a path they never open (043 B-4, P-8, D-P7).

**Requested.** An adversarial reading against 0.14 (`8f3b9bd`) and
`0.15.0-patched.1` source: does any 0.14 path, without `auto-heal`, remove
or bypass `state_machine/lock` before failing; is a 0.14 data directory
relocated entry by entry to a new `data_dir` path sound for 0.15 (membership
names addresses, not paths); what else 0.14's racing WAL task can write.

**Acceptance.** A written answer on a published branch, or a finding.
**Class.** Non-blocking; the owner may ask for it before approving.

### H-8. An interrupted consent move (F-130)

**Problem.** The uncommitted register entry F-130 (source-read) says a crash
between the consent move's two renames can leave a 0.14 cache snapshot that
the next start restores without refusal. rahi never uses consent on the app
store and orders its own two renames to avoid the case (043 T2), but
Rauthy's T4 move is hiqlite's consent path.

**Requested.** Move `state_machine_cache` first, or make the legacy check
look at snapshots too. **Acceptance.** A crash injected between the renames,
then a start: refused or completed, never a restored 0.14 snapshot.
**Class.** Claim-blocking for "an interrupted Rauthy move recovers cleanly";
043 states it as the producer's (B-4, T4).

## 2. Reconciliation with the hiqlite handoff at `ec8fc6d`

Read: `standards/spec/n3-rahi-reconciliation-handoff.md`,
`standards/spec/n3-topology-proposal.md` (sections 4, 7, 12 to 15),
`specs/033-n3-topology-qualification/spec.md`,
`specs/034-fresh-cell-restore/spec.md` and
`standards/spec/findings-register.md` (F-124, F-125), all at `ec8fc6d`.
Nothing there authorizes rahi work; its section 1 records D-14 as decided
and every other hiqlite decision as pending.

| hiqlite handoff at `ec8fc6d` | rahi's position after 043 revision 2 |
|---|---|
| §2 packet D2: the `iat` floor should use the **reported** maximum bearer lifetime, not the packet's 600 s constant; 038 records a Rauthy default of 1800 s | **Superseded.** The floor no longer depends on any lifetime: it never lifts (043 B-6b, P-9). Every admitted token is bounded by the manifest's lifetime L, enforced at the bearer (B-6), so Rauthy's default does not enter; preflight still reports Rauthy's configured value against L (038 B-6). |
| §2 packet D2: the Rauthy-side move-aside lifts every IP ban, including manual ones, and resets failed-login counters (proposal §14.1) | **Adopted as a stated consequence.** The 043 README and release notes list it; R-2 asks Rauthy's maintainer to confirm the full list. |
| §2 packet D2: Rauthy's revocation state is in SQLite and survives | Agrees with 043 B-6b's last sentence. |
| §2 packet D4: "5 s drain" and "about 15" are bounds, not measurements; `Err(Timeout)` is unconfirmed completion | **Agrees and is adopted.** 043 B-9 budgets H = 15 s from `SHUTDOWN_WAIT` rather than the handoff's ten-second allowance, and B-10 records `store_timeout` as unconfirmed with exit `3`. AC-7 measures; nothing is claimed from the bound. |
| §2 packet D4: record confirmed completion, unconfirmed completion, forced exit (proposal §4) | **Refined.** B-10 keeps confirmed and unconfirmed as observed outcomes and replaces "forced exit" with three unrecorded classifications whose cause is `unknown` unless a witness recorded a kill. |
| §2 Track N1 and Track S7 separate | Agrees (043 D-2, and decision 3 of the owner packet). |
| §3 items 1 to 7 and 9 (split composition, tombstone, quiescence, background writers, barrier, export procedure, restore validator, manifest fields) | **N3-only.** Spec 044 and Track S7. 043 absorbs none of it. |
| §3 item 8: bearer floor at activation for migration, restore and upgrade (D-8b) | Upgrade: 043 B-6b. Restore: 043 B-6c (always for a pre-043 archive; for every restore if P-7 is accepted). Migration into a new cell: N3-only. |
| §3 item 10: correct `deploy/README.md`, "the cache group is replicated, and holds the deny-lists and the leases" | **Adopted in 043's README change.** Today's line reads "The cache group is per node. Nothing durable lives in it." The correction: the cache group is a replicated raft group; after 043 the deny-lists are in SQL; it still holds the distributed locks behind 012's fencing tokens, which are lost with it. |
| §3 item 11: replace 030 D-2's file-level restore | Out of 043's scope (section 6). |
| §4 F5: other relying parties are not covered by rahi's floor | Stated in the 043 README: the floor protects this cell's bearer check only. |
| §4 F7: a stale restore rolls back Rauthy session and refresh-token deletions | The same limitation 043 B-6c states and P-7 keeps visible. |
| §4 F9: `DPoPNonce::is_valid` returns `slf.is_ok()` (read from source, untested) | Forwarded to Rauthy's maintainer as R-5; not a rahi finding and not a 043 blocker. |
| §5: a planned clean-stop marker (034 B-7) | When it ships, a later rahi change may use it to confirm the store's stop in B-10; 043 does not wait. |
| F-124, F-125 (committed log id not persisted; applied id persisted only at snapshots and clean exit) | N3-only (export currency). Noted because B-10's `store_timeout` is exactly the case where the applied id may not have been persisted. |

### 2.1 The hiqlite working tree after `ec8fc6d` (uncommitted, 2026-09-23)

Observed, not citable as a revision: a concurrent hiqlite session had, on top
of `ec8fc6d` and uncommitted, a draft `specs/035-n1-upgrade-exclusion`, a
revised handoff and new findings F-126 to F-133. As observed, it accepts H-1
to H-4 (widened: the live 0.14 node also wrote into the moved WAL, F-126),
reproduces revision 1's T0 to T3 self-contention independently (rahi's
D-P6), notes that hiqlite has no start mode without listeners (043 T3 now
says so), and asks rahi (its F10) to wait for a public exclusion handle or
rely on an operator precondition. rahi's answer is revision 2's fence (H-7),
which needs neither for the app store; the precondition remains for Rauthy's
directory only (043 B-5). It also records F-130 (H-8) and that a clean WAL
stop releases its lock before unlinking the file (F-133), which T1's probe
tolerates: a released lock reads as not held. When that work is committed,
this section is updated to cite its revision.

What the earlier conversational handoff said and this replaces: the lifetime
figure used for the floor (above), the forced-exit classification (above),
and "the guard is installed after the floor", which D-P7 disproved (043
section 7.3).

## 3. Requests to the Rauthy patched-release producer

Owner: the maintainer of `bartekus/rauthy`, line `patched/0.36.2`. Consumer:
rahi spec 043 and the rahi images that pin
`ghcr.io/bartekus/rauthy-patched:0.36.2-patched.2`. Routing: issues are
disabled on `bartekus/rauthy`; the rahi owner forwards this section as a
discussion, a pull-request description, or directly. rahi edits nothing in
that repository and does not wait for any item.

### R-1. A machine-readable terminal-storage signal

**Problem.** `GET /auth/v1/ready` answers `503` both when the health watcher's
last sample found storage unreachable (transient) and after a terminal
storage failure on the embedded node (the node refuses every storage
operation until a restart). `GET /auth/v1/health` returns
`{db_healthy, cache_healthy}` with no terminal indication. Sources at
`17132b94`: `src/api/src/generic.rs` (`get_ready`, `get_health`),
`src/error/src/error_impls.rs`, `src/data/src/events/health_watch.rs`.

**Requested.** A field such as `"storage": "ok" | "degraded" | "terminal"` on
the health answer: `terminal` exactly when the embedded node is out of
service after a terminal storage failure, monotonic for the life of the
process, correct inside `HEALTH_CHECK_DELAY_SECS`, unauthenticated, and
computed without touching storage.

**Acceptance.** A strict leg on amd64 and arm64: fresh start reports `ok`; a
recoverable fault reports `degraded` and returns to `ok` without a restart;
the terminal fault reports `terminal` within one watcher interval and keeps
it for three more samples; a restart after clearing the fault reports `ok`;
a terminal fault inside the delay window is reported inside it; the leg
fails if any step did not execute.

**Class.** Non-blocking (043 B-8, D-5). A later rahi change may restart on
`terminal`.

### R-2. Enumerate cache-only state

**Requested.** List every piece of state that lives only in Rauthy's cache
raft and is therefore lost at the 0.14 to 0.15 transition, at every restore,
and at every migration: the handoff names authorization codes, device codes,
WebAuthn challenges, rate-limit counters and IP blacklist entries, and the
hiqlite reconciliation adds manual IP bans and failed-login counters. Say
which of these a plain restart already loses.

**Acceptance.** A list confirmed from source at a named revision.
**Class.** Claim-blocking for the completeness of the 043 README's list of
upgrade consequences; the README ships with the known list and says it is
the known list.

### R-3. Attach the release assets

**Problem.** Release `v0.36.2-patched.2` has zero assets (authenticated and
anonymous REST API, 2026-09-23), while `RELEASE-LEDGER.md` section 7 on
`patched/0.36.2` says the publish run created eight and `RELEASE-HANDOFF.md`
tells consumers to pin from the attached `RELEASE-PROVENANCE.md`.

**Requested.** Attach `RELEASE-PROVENANCE.md`, `SHA256SUMS`, the binaries and
the other listed assets, or correct the ledger and handoff.

**Acceptance.** An anonymous read of the release lists the assets and their
hashes agree with 043 D-P1: index
`sha256:ea114a8bb743d578dea6d7800916ee43550939c749a2cf586f9abdc0d0c52478`;
`linux/amd64` `sha256:6774d28c9f611777dc4ad2243a8f4cb1fa0df3d94caca1aeaf052cc10d9e5658`;
`linux/arm64` `sha256:6ae9225a9243a7e660f6c407d07f81258d06f456470dfb4b6c899a6db13146f8`;
`/app/rauthy` `742b18ba3717a92577a2ae0d517546a64ef6967c86e2847b50b10a22ab8dfc59`
(amd64), `5e498c31ef23ebc27a6d2dbdbf73f6d6f48129f541fced92d48a53b87d61e312`
(arm64).

**Class.** Claim-blocking for "pinned from the release's provenance file";
rahi pins from the registry and the ledger meanwhile and says so (D-P1).

### R-4. Refuse an older Rauthy on an upgraded directory

**Problem.** A Rauthy built on hiqlite 0.14 started on a directory a
0.15-based Rauthy has written can destroy its raft metadata (043 D-P2), and
rahi may not open Rauthy's directory to prevent it.

**Requested.** Ship H-5 in the next patched Rauthy, or an equivalent check in
Rauthy's own startup. **Acceptance.** As H-5, with the upstream 0.36.2 image
against a directory the patched image has written. **Class.**
Claim-blocking, as H-5.

### R-5. Forwarded: the DPoP nonce observation

The hiqlite reconciliation (`ec8fc6d`, handoff §4 F9) read from source at
Rauthy `ccf2250` that `DPoPNonce::is_valid` returns `slf.is_ok()` on a cache
lookup, so a nonce that was never issued evaluates as valid. Callers and
exploitability were not tested. rahi forwards it for the maintainer's
assessment. **Class.** Non-blocking for 043, which does not use DPoP.

## 4. Requests to rahi's consumers

### C-1. The app store moves

Owner: the rahi owner. Consumers: `statecrafting/hqgit`, `statecrafting/aicortex`
and any out-of-tree app on the runtime image. What changes at 0.3.0 if 043 is
approved as revision 2: `Config::hiqlite_dir()` answers `<data>/app-store`;
`<data>/hiqlite` is a permanent fence; a v0.2.0 volume needs
`rahi upgrade-cache` once; a pre-043 image on a post-043 volume waits in
`migrate`. **Acceptance.** The release notes and the consumer contract carry
it, and a consumer that hard-codes the path is named in the 0.3.0
qualification record. **Class.** Non-blocking for 043; delivered with 0.3.0.
