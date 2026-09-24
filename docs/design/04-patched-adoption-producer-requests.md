# Patched adoption: producer requests and the hiqlite reconciliation

Version 3, 2026-09-23. Owned by `specs/043-patched-dependency-adoption/spec.md`
(draft, revision 4). Version 1 answered revision 2; version 2 re-read every
request against revision 3 and hiqlite `8e4ec4b`; version 3 records each
request's state against hiqlite pull request #37 (`26e2fa0`) and Rauthy's
local `51e7328`, and changes no request's class (section 0). Maintained by the rahi owner. Stable references: the
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
- **N1-release-blocking**: 043 is implemented without it, but cannot be
  flipped `complete` or qualified for release, because an acceptance
  criterion requires the behavior it provides (043 B-4b).
- **Claim-blocking**: 043 ships without it, but a named claim stays out of
  rahi's documents until it arrives.
- **Non-blocking**: an improvement a later change adopts.
- **N3-only**: belongs to Track S7 or spec 044, never to 043.

No request below is N1-blocking. One, H-8, is N1-release-blocking.

## 0. State at version 3 (2026-09-23)

State only; each class below is as it was. Sources: hiqlite pull request
#37 (open), head `26e2fa0a15b8b94dcc0ef2b733f6cec70922f9df`, code
`048fcecd5bdab7d4d12b2207b69f46bf31aa9c99`,
`standards/spec/n3-rahi-reconciliation-handoff.md` (043 D-P20); Rauthy
branch `work/0.36.2-patched.3` at `51e732802ba48133fb430303f9cc20f5e62bf133`,
local and unpublished, `RELEASE-PRODUCER-RESPONSES.md` and
`RELEASE-STATE-INVENTORY.md` (043 D-P21). Evidence classes are the
producers' own: nothing below is released or qualified.

| request | class (unchanged) | state |
|---|---|---|
| H-1 | claim-blocking | implemented in the unreleased candidate `048fcec`, candidate-tested |
| H-2 | non-blocking | implemented in the candidate |
| H-3 | non-blocking | implemented in the candidate |
| H-4 | non-blocking | recorded (F-129; H-7 answer) |
| H-5 | claim-blocking | option 2 stated in 035 B-4, to reach the consumer handoff at release; option 1 a separate proposal, not built |
| H-6 | non-blocking | **done**: the branch is pushed and `8e4ec4b` is reachable from pull request #37 |
| H-7 | non-blocking | **answered**, source only, with limits; its findings are adopted by 043 revision 4 (7.5 items 1 and 4); its F11 is answered by stating the exclusions, not by detection |
| H-8 | **N1-release-blocking** | implemented in the unreleased candidate; no `hiqlite-patched` release carries it and no Rauthy image is built on it, so B-4b's gate is unchanged |
| R-1 | non-blocking | implemented and tested locally on Rauthy `51e7328`, unreleased |
| R-2 | claim-blocking | answered from source (the inventory); a normal release restart loses none of the cache-only items. 043 revision 4 carries the list (B-6b); the stale-restore obligation it exposes stays open (B-6c) |
| R-3 | closed | closed at version 2; the producer re-verified it |
| R-4 | claim-blocking | route 2 (the unsupported-downgrade statement) taken in the unpublished handoff; route 1 not attempted |
| R-5 | non-blocking | answered by the maintainer on the unpublished branch; its handling is the maintainer's, and rahi records no further detail |
| C-1 | non-blocking | unchanged; revision 4 adds nothing consumers see beyond revision 3 |

## 1. Requests to the hiqlite-patched producer

Owner: the `bartekus/hiqlite` maintainer. Consumer: rahi spec 043, and
through Rauthy's image every Rauthy consumer. Evidence: 043 D-P2, D-P3,
D-P6, D-P7 and `evidence/043-probes/`.

### H-1. Continuous exclusion before and through the move (the live-old-version hazard)

**Problem.** With `HQL_CACHE_LEGACY_MOVE_ASIDE=true`, a 0.15 start against a
data directory a live 0.14 node is using renamed the live node's
`logs_cache` and `state_machine_cache` into `pre-upgrade-<secs>/` and only
then failed on the 0.14 WAL lock (043 D-P3; hiqlite F-126, reproduced as
035's P-2, where the live node then wrote into the moved WAL). This is an
N=1 hazard of the consent path and is not part of N=3 qualification.

**Requested.** Exclusion that is **continuous**, not a momentary probe: the
owner lock and each existing legacy WAL lock are acquired and **held** from
before the first rename or storage write until the component that uses the
directory stops, handed to the log stores rather than released and taken
again, and re-established on the new `logs_cache` before the lock that
followed the renamed inode is released. This is hiqlite 035 B-1 and B-2 at
`8e4ec4b`; rahi asks for that contract as written, including B-2's
unlink-while-held at clean stop (F-133), and for its X-1, X-4 and X-5
acceptance on Linux.

**Acceptance.** hiqlite 035's X-1 (live 0.14, then the candidate with
consent: refused at B-1 step 2, the 0.14 node writes, stops `Ok` and
restarts with every row) and X-4 (ten racing launches per run, exactly one
proceeds), release builds, both Linux architectures.

**Class.** Claim-blocking. rahi's own app store never uses the consent path
(043 B-4), and every published pre-043 rahi supervisor is stopped before it
spawns Rauthy (043 B-5, P-10), so the remaining case is a Rauthy started
outside rahi on the volume. Until a release rahi pins carries this, 043
claims no mechanical exclusion of such a process.

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

### H-5. A downgrade boundary the old binary actually meets

**Problem.** Nothing in a 0.15 directory makes a 0.14 binary refuse it, and
a 0.14 binary over it can destroy its raft metadata (043 D-P2; hiqlite F-129,
3 of 3 panics and 2 of 3 destructive in 035's P-6). A check added to 0.15
code is never executed by a 0.14 binary, so it cannot be the answer.

**Requested.** One of two, stated in the consumer handoff:

1. A **producer-owned layout fence**: a 0.15 directory arranged so that
   the unmodified 0.14 binary refuses before it writes anything, for
   example the arrangement 043 uses for rahi's app store (the store moved
   to a path 0.14 never opens, and a permanent `state_machine/lock` with a
   content tag at the path it does open), demonstrated against the real
   0.14 binary at `8f3b9bd` with 035's method (entries, identities, sizes
   and bytes before and after). 043 D-P7 and D-P14 show that 0.14's racing
   WAL task still writes files beside such a marker; the demonstration
   says where.
2. An **explicit unsupported-downgrade boundary**: the handoff says a 0.14
   binary on a directory any 0.15 build has written is unsupported, that
   the pre-upgrade archive restored into a fresh volume is the only way
   back, and that a manual move-aside is not shown safe. hiqlite 035 B-4 at
   `8e4ec4b` already requires this statement; rahi asks that it reach the
   published consumer handoff.

**Acceptance.** For 1, the demonstration on both Linux architectures; for 2,
the statement in the consumer handoff on a published ref.

**Class.** Claim-blocking, for the claim that an old Rauthy started outside
rahi cannot damage a post-upgrade volume. Non-blocking for 043's delivery.

### H-6. Publish the N=3 reconciliation

**Problem.** The hiqlite reconciliation with rahi's decision packet,
`ec8fc6d`, and the N=1 repair contract and second reconciliation on top of
it, `8e4ec4b` ("docs(035): the N=1 upgrade-exclusion repair contract, and a
second N=3 reconciliation with Rahi 043"), are on the local branch
`proposal/033-n3-topology`; at 2026-09-23T20:30Z the remote held `main`
(`52122ae`) and two `spec/*` branches, none containing either commit.
Sections 2 and 2.1 cite them.

**Requested.** Push the branch or merge it. **Acceptance.** `8e4ec4b` is
reachable from a ref of `bartekus/hiqlite`. **Class.** Non-blocking; sections
2 and 2.1 record what was read. **State (version 3).** Done: pull request
#37 carries `8e4ec4b`.

### H-7. Review revision 3's fence route

**Problem.** hiqlite's handoff at `8e4ec4b` answers 043 as reviewed at
`5707f60` (revision 1): it confirms that no public-API rearrangement keeps
lock-based exclusion continuous from rahi's locks into a 0.15 start, and
offers two routes (wait for 035 B-6's handle, or an operator precondition).
Revision 3 takes a third route for rahi's app store, and its T1 now depends
on specific 0.14 behavior rahi read from source and probed once.

**Requested.** A written, adversarial answer, against 0.14 at `8f3b9bd` and
`0.15.0-patched.1`, to five questions:

1. Can any 0.14 path without `auto-heal`, starting or stopping, remove,
   truncate or bypass `state_machine/lock` other than D-P11's two (a start
   between its `File::open` check and its `File::create`, which holds
   `logs/lock.hql` throughout; its own clean stop, which unlinks by path)?
2. Does a 0.14 node ever write under its data directory after its SQLite
   `-wal` file is removed at a clean stop (D-P12's timeline)? Is "`-wal`
   and `-shm` absent, both WAL locks free, marker absent" sufficient
   evidence that no 0.14 process has the directory open, with the default
   and with any configuration a 0.14 consumer could use?
3. Is a 0.14 data directory relocated entry by entry to a new `data_dir`
   path sound for 0.15 (membership names addresses, not paths)?
4. What can 0.14's racing WAL task write under a marker-refused start
   beyond what D-P7 and D-P14 saw (a non-empty WAL and `meta.hql`)?
5. Does anything in 0.15 treat an unknown directory entry under its
   `data_dir` (rahi's evidence and `pre-upgrade-*` directories) as its own?

**Acceptance.** A written answer on a published branch, or findings.
**Class.** Non-blocking; the owner may ask for it before approving.

### H-8. The interrupted consent move (F-130), on the normal crash path

**Problem.** hiqlite F-130 (`defect`, confidence `medium`, committed at
`8e4ec4b`, source-read): `move_legacy_cache_aside` renames `logs_cache`
before `state_machine_cache`, and the legacy check keys on `.wal` files in
`logs_cache` only, so a crash between the renames can leave a 0.14 cache
snapshot that the next start restores without refusal and without consent.
Rauthy's cache move at 043 T4 is this path, so the crash is on the cell
transition's normal crash path, and 043 AC-4 requires every interruption
to resume cleanly.

**Requested.** hiqlite 035 B-5 as written at `8e4ec4b` (`state_machine_cache`
first, so the legacy evidence moves last; no state leaves a 0.14 cache log
or snapshot where a 0.15 cache raft opens it), with its U-7 and X-5
acceptance; a published `hiqlite-patched` release carrying it; and a
patched Rauthy image rebuilt on that release, published with its own
provenance, whose own leg J includes a crash between the two renames.

**Acceptance.** Those three artifacts exist; rahi's owner then decides the
repin (043 B-4b, P-12). **State (version 3).** The first exists as an
unreleased candidate (`048fcec`, pull request #37); the release and the
Rauthy image do not exist. **Class.** **N1-release-blocking.** 043 is
implemented on the current pins; it is not flipped `complete` and 0.3.0 is
not qualified until this lands and is adopted. It is not an N=3 item.

## 2. Reconciliation with the hiqlite handoff at `ec8fc6d`

Read: `standards/spec/n3-rahi-reconciliation-handoff.md`,
`standards/spec/n3-topology-proposal.md` (sections 4, 7, 12 to 15),
`specs/033-n3-topology-qualification/spec.md`,
`specs/034-fresh-cell-restore/spec.md` and
`standards/spec/findings-register.md` (F-124, F-125), all at `ec8fc6d`.
Nothing there authorizes rahi work; its section 1 records D-14 as decided
and every other hiqlite decision as pending.

| hiqlite handoff at `ec8fc6d` | rahi's position after 043 revision 3 |
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

### 2.1 hiqlite `8e4ec4b` (committed locally, 2026-09-23)

What version 1 of this document described as an uncommitted working tree
is now the local commit `8e4ec4b` (not on a remote ref, H-6): spec
`035-n1-upgrade-exclusion` (draft, the repair contract: B-1 held exclusion
before any mutation, B-2 lock handoff and unlink-while-held, B-3 refusals as
errors naming what they left, B-4 the unsupported downgrade, B-5 the
interruption-safe move order, B-6 an optional public exclusion handle under
pending owner decision D-17), findings F-126 to F-131 and F-133, and
`standards/spec/n3-rahi-reconciliation-handoff.md`. That handoff answers
043 as of `5707f60` (revision 1) and asks rahi (its F10) to wait for B-6's
handle or state an operator precondition. rahi's answer is revision 3: the
app store needs neither (fence, whole-file guard, quiescence check,
identity-planned relocation), published rahi supervisors are fenced
(P-10), and the precondition remains only for a Rauthy started outside
rahi. H-7 asks hiqlite to review that route; H-8 is 035 B-5.

## 3. Requests to the Rauthy patched-release producer

Owner: the maintainer of `bartekus/rauthy`, line `patched/0.36.2`. Consumer:
rahi spec 043 and the rahi images that pin
`ghcr.io/bartekus/rauthy-patched:0.36.2-patched.2`. Routing: issues are
disabled on `bartekus/rauthy`; the rahi owner forwards this section as a
discussion, a pull-request description, or directly. rahi edits nothing in
that repository and does not wait for any item.

### R-1. A machine-readable terminal-storage signal

**State (version 3).** Implemented and tested locally (Rauthy `51e7328`,
`storage` on `/auth/v1/health`: `ok`, `degraded`, `terminal`, `unknown`),
unreleased. 043 adopts nothing from it (B-8).

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

### R-3. Release assets (closed 2026-09-23)

Closed. Version 1 said release `v0.36.2-patched.2` had no assets; that was
wrong. At 2026-09-23T20:27:33Z authenticated and anonymous reads listed eight
assets uploaded between 09:14:48Z and 09:14:51Z, and the anonymously
downloaded `SHA256SUMS`, `RELEASE-PROVENANCE.md` and `image-index.txt` agree
with 043 D-P1 on the index digest, both platform digests, both binary hashes,
the upstream base and the source commit (043 D-P15). Nothing is requested.

### R-4. Refuse an older Rauthy on an upgraded directory

**Problem.** A Rauthy built on hiqlite 0.14 started on a directory a
0.15-based Rauthy has written can destroy its raft metadata (043 D-P2), and
rahi may not open Rauthy's directory to prevent it.

**Requested.** One of H-5's two answers for Rauthy's own directory: a layout
fence the unmodified upstream `ghcr.io/sebadob/rauthy:0.36.2` image refuses
before it writes, demonstrated against that image on a directory the
patched image has written; or the unsupported-downgrade boundary stated in
`RELEASE-HANDOFF.md`. A startup check in the patched build is not an answer:
the old image never runs it. **Acceptance.** As H-5. **Class.**
Claim-blocking, as H-5. rahi already stops its own published supervisors
from spawning an old Rauthy (043 P-10); this covers a Rauthy started outside
rahi.

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
approved as revision 3: `Config::hiqlite_dir()` answers `<data>/app-store`;
`<data>/hiqlite` is a permanent fence; the rendered Rauthy environment moves
to `<data>/rauthy-env/rauthy.env` and `<data>/rauthy/rauthy.env` becomes a
directory (if P-10 is accepted); a v0.2.0 volume needs `rahi upgrade-cache`
once; a pre-043 image on a post-043 volume waits in `migrate`, and a
pre-043 `supervise` run directly exits before spawning Rauthy. **Acceptance.** The release notes and the consumer contract carry
it, and a consumer that hard-codes the path is named in the 0.3.0
qualification record. **Class.** Non-blocking for 043; delivered with 0.3.0.
