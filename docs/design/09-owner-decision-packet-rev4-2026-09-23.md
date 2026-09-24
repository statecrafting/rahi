# Owner decision packet, 2026-09-23: spec 043 revision 4

Version 1. Owned by `specs/043-patched-dependency-adoption/spec.md` (draft).
Prepared for the rahi owner. It supersedes
`07-owner-decision-packet-rev3-2026-09-23.md` and, through it,
`06-owner-decision-packet-2026-09-23.md`. **Nothing in this packet is
accepted.** Each recommendation is a recommendation until the owner records
a decision in the words of section 5.

## 0. What is under decision

**The revision.** Spec 043 revision 4 on branch
`corpus/043-patched-dependency-adoption` (draft PR #75): commit
`344782571d179e3f138afb0bebe04bf3c9dd197d`, `specs/043-patched-dependency-adoption/spec.md` git blob
`6cf02a3363db1cd14eb68198a4a823a35666cbd0`. Approval names that blob; a later commit that changes the spec
file invalidates it.

**Not offered.** Revision 3 (`abd66fd`, blob `d2fd7bf`) is not offered for
approval: its pruning is withdrawn and its supervisor-fence claim is
narrowed (043 section 7.5), so approving it would approve requirements this
packet already changes. Revision 2 stays withdrawn, and 06's and 07's
approval texts are void.

**Already decided, not asked again.** Owner direction 043 D-1 to D-6:
adopt the two published artifacts under a narrow exception; N=1 first with
no N=3 claim, and so N1 independent of spec 044 and of any N=3
qualification; durable SQL revocation keeping the full validity window;
explicit, resumable consent with a verified pre-upgrade backup, preserving
store separation; transient Rauthy unreadiness distinct from terminal
failure; the 50 percent headroom beside a validated composition.

**Independent review.** Revision 4 (`9fa03c5`) was reviewed read-only
against hiqlite's H-7 answer at `26e2fa0`, `hiqlite-patched
0.15.0-patched.1` source, rahi's v0.1.0 and v0.2.0 tags and Rauthy's
inventory at `51e7328`, and the three models were rerun (all exit `0`, every
control failing as intended). One finding was an internal contradiction:
T3 still raised a prune horizon that option (i) says is never stored; it is
now conditioned on option (ii). Three precision findings (which of T1 (e)'s
renames decides a pre-043 read; the Rauthy loss list's scope; AC-4 (f)'s
wording) and one caveat were applied. All in `3447825`, the revision
offered here.

## 1. Design corrections in revision 4 (part of the contract, no choice)

Each is in the text the owner approves with decision 1; none is a policy
choice, and none reopens D-1 to D-6. Details in 043 section 7.5.

1. **Old invocations that act before any lock or marker.** hiqlite's H-7
   answer shows `HQL_DANGER_RAFT_STATE_RESET` and `HQL_BACKUP_RESTORE` make
   a starting hiqlite 0.14 destructive before T1 can see it. B-4 now states
   two preconditions: every old process stopped and the old container
   removed with its restart sources disabled (kept from revision 3, now
   including directly started supervisors), and neither variable in any old
   process's environment. This version also refuses both in its own
   environment; that proves nothing about another process, and the text
   says so.
2. **The supervisor fence's exact guarantee.** A pre-043 supervisor that
   read `rauthy.env` before the fence can still spawn the command it built
   (source; D-P18 model). The fence guarantees only that a read beginning
   after its rename fails before the spawn. No public hiqlite exclusion
   interface is asked for.
3. **Pruning under a backward clock step.** Revision 3's pruning admits a
   revoked token again after a backward step that follows the prune (D-P17:
   floor 0, `iat` 100, `exp` 700, L 600, revoked at 200, pruned at 86,721,
   clock set to 300, admitted until 760). It is withdrawn; decision 4
   chooses its replacement. The admission rules, the hard 86,400-second
   maximum, checked arithmetic and the inclusive permanent floor stay.
4. **Evidence names and the aside directory** (precision). Revision 3
   already refused to overwrite; a repeated occurrence would have stalled
   T2 (D-P19). Names are now unique per occurrence, and the app store's old
   caches move to `<data>/upgrade-cache/aside/<id>/`, out of hiqlite's
   reserved `pre-upgrade-*` names. H-7 shows no corruption from revision
   3's layout; this is hygiene, not a repair.
5. **Rauthy's cache and restore** (from the producer's source inventory).
   The upgrade's full list of cache-only losses, the ban export and
   re-import step, and what a stale restore revives, including a refresh
   credential that mints tokens rahi's floor admits. That obligation stays
   open.
6. **Producer state** recorded; no request changes class; H-8 still gates
   completion (decision 7).

## 2. Decision 1: approve revision 4 as the implementation contract

| option | consequence |
|---|---|
| **Approve (recommended)**, with decisions 3, 4 (option (i)), 5 and 7 as recommended | A separately authorized build may then record the D entries of section 5, record 011 D-13 and 037 D-10 with the text of 06 section 1.2 (unchanged), flip 043 to `in-progress`, and execute `05-patched-adoption-delivery-plan.md` version 3. |
| Approve with P-8, P-10 or P-12 rejected | Revision 5 with that proposal's fallback, resubmitted by hash before any build step. |
| Defer | N=1 stays on 011 D-12's root git patch, which no registry consumer inherits. |

**Practical consequences.** Users: every bearer access token issued before
the upgrade is refused from the transition on; native clients refresh, and
one that refreshes before its refresh token's `nbf` logs in again; browser
sessions renew; logins in progress restart; every IP ban lifts unless the
operator re-applies it; failed-login counters reset. Operators: the old
container is stopped and removed with its restart sources disabled, no old
process is left anywhere on the volume, neither hiqlite variable is in any
old environment; then `rahi upgrade-cache --backup <archive>` once;
rollback is the archive into a fresh volume, `--abort` before `flooring`.
Consumers: `Config::hiqlite_dir()` changes (C-1); 0.3.0.

**Blocks:** every 043 build step.

## 3. Decisions 2 to 6: the proposals

### Decision 2. P-7: every restore raises the floor

| option | consequence |
|---|---|
| **Accept (recommended)** | After any restore every access token issued before it is refused, covering every access-token revocation after the archive instant. |
| Reject | A token revoked after the archive instant is admitted again until its own expiry, at most L + 60 s after its `iat`. A pre-043 archive still raises the floor. |

Neither option covers a revived Rauthy refresh credential (B-6c, open).
**Blocks:** AC-6 (e) only.

### Decision 3. P-8: the relocated app store and its permanent fence

| option | consequence |
|---|---|
| **Accept (recommended)** | The app store moves to `<data>/app-store`; `<data>/hiqlite` becomes a permanent fence written whole after the old node is proven gone; old caches go to the aside directory. |
| Reject | Revision 5 returns to an in-place guard, with the window 043 7.3 item 2 describes. Not recommended. |

**Blocks:** build steps 4 and 6.

### Decision 4. P-9: retention or a pruning watermark

Both keep the admission rules (no `iat`, claims outside `u64`, `exp <
iat`, the hard 86,400, `exp - iat > L`, future `iat`, every overflow) and
the inclusive permanent floor.

| option | consequence |
|---|---|
| **(i) Retain (recommended)** | No revocation row is ever deleted. Correct under any clock movement after a revocation. Cost: unbounded growth, one row per `jti` revocation and at most one per subject, not measured; reported by `rahi_revocation_rows{kind}`; later pruning is its own governed change. |
| (ii) Watermark | Rows pruned at `revoked_at + 86,520 s`, with a durable watermark raised to the pruning instant in the same `txn`; every token refused while the clock is below it. Bounded storage. Cost: a clock that ran ahead and was corrected locks every bearer out until it catches up. |
| Reject both | No viable text: revision 3's pruning is unsound (D-P17). |

Neither option narrows B-6b's gap for old-cache revocations under a
backward step at the transition; decision 6 is the choice that does.
**Blocks:** build step 5.

### Decision 5. P-10: the supervisor fence, and the anchor reading

| option | consequence |
|---|---|
| **Accept (recommended)**, reading spec 000's `store-separation` anchor as governing Rauthy's storage and not rahi's own rendered configuration file beside it | `<data>/rauthy/rauthy.env` becomes a directory holding `FENCE`; the rendered environment moves to `<data>/rauthy-env/`. Every published pre-043 `supervise` whose read begins after the fence exits before spawning (v0.2.0 executed, v0.1.0 source); one already past its read is B-4's precondition. |
| Accept the alternative | An unparseable `<data>/restore.marker`, wholly outside `<data>/rauthy`; stops v0.2.0's `supervise`, not v0.1.0's. |
| Reject | Revision 5 without a supervisor fence; a directly started pre-043 `supervise` is left wholly to the precondition. |

**Blocks:** build steps 4 and 6.

### Decision 6. P-11: a clock-independent transition floor

| option | consequence |
|---|---|
| **Decline for now (recommended)** | B-6b's backward-step gap at the transition stays stated: at most Δ + L + 60 s for a revocation that lived only in the old cache. |
| Accept | Rauthy's signing keys are rotated at T4 and every pre-transition key id refused permanently; an admin call on the transition path whose failure holds the cell at `floored`; the admin key's rights for it are established first. Rotation does not invalidate tokens at other relying parties. |

**Blocks:** nothing unless accepted (then AC-6 (f)).

## 4. Decision 7. P-12: the producer-release gate

| option | consequence |
|---|---|
| **1. Implement after approval; complete and qualify 0.3.0 only on repaired, published artifacts (recommended)** | 043 is built and merged at `in-progress` on the current pins. It is flipped `complete`, and 0.3.0 qualified, only after a `hiqlite-patched` release carrying hiqlite 035 B-5's move order is published, a Rauthy image rebuilt on it is published with its own provenance, and the owner approves an exact repin amendment naming both. The unreleased hiqlite candidate (`048fcec`, pull request #37) and Rauthy's unpublished branch do not satisfy this. |
| 2. Release on the current image with a narrowed promise | Needs revision 5 and new acceptance: every unready first Rauthy start at `floored` would call for an archive restore, since rahi cannot tell an interrupted cache move from any other failure without opening Rauthy's storage. |

**Blocks:** 043 `complete` and 0.3.0, not implementation.

## 5. Approval text

The owner may accept any subset, line by line. Recommendations are not
approvals, and no line below covers a release, a publication, a consumer
repin, ratifying spec 044 or its instrument, or a deployment.

```
Owner decision, <date>, on docs/design/09-owner-decision-packet-rev4-2026-09-23.md:

1. I approve specs/043-patched-dependency-adoption/spec.md as committed at
   344782571d179e3f138afb0bebe04bf3c9dd197d (git blob 6cf02a3363db1cd14eb68198a4a823a35666cbd0), revision 4,
   as the implementation contract, including its design corrections, B-4's
   preconditions, B-6b's transition floor and its consequence, and B-6c's
   stated open boundary. Record it as 043 D-7 and set 043 to approved. A
   separately authorized build records 011 D-13 and 037 D-10 with the text
   of 06 section 1.2 at build step 1.
2. I accept P-7. Record it as 043 D-8.
3. I accept P-8 as written in revision 4. Record it as 043 D-9.
4. I accept P-9 option (i): retain every revocation row, with no pruning,
   accepting unbounded growth. Record it as 043 D-10.
   [or: I accept P-9 option (ii): prune at revoked_at + V(L_max) under the
   persisted watermark with fail-closed admission below it. Record it as
   043 D-10.]
5. I accept P-10, and read spec 000's store-separation anchor as governing
   Rauthy's storage and not rahi's rendered configuration beside it.
   Record it as 043 D-11.
6. I decline P-11 for now. Record it as 043 D-12.
7. I choose option 1 of P-12: implement 043 after this approval, and flip it
   complete and qualify 0.3.0 only after a repaired hiqlite-patched release
   and a Rauthy image rebuilt on it are published and I approve an exact
   repin naming them. Record it as 043 D-13.
```

## 6. What stays outside this packet

- **Spec 044 and its instrument** (`08-owner-instrument-one-deployment-unit.md`,
  draft PR #77 at `82f2189`): prepared, not approved, unchanged. Its
  failed cargo gate is `adopt_appends_once_and_says_so_when_there_is_nothing_to_adopt`
  in `crates/rahi-ops/tests/evolution.rs`: hiqlite 0.14's raft listener
  panicked at `start.rs:237` with `AddrInUse` on a port the test helper
  `free_addr()` (`crates/rahi-ops/tests/common/mod.rs:59`) bound, released
  and returned before hiqlite bound it, so the chain's first request timed
  out (`Upstream("Connect: request timed out")`). The pull request changes
  one document and derived shards and no code; the defect is the helper's
  port race, owned by 030 (establishes) and extended by 036, 037 and 039,
  and belongs in its own change, not in PR #77 and not in 043. A rerun is
  not a fix. The instrument keeps both co-located and split N3 governed;
  spec 000 is not altered without the owner's separate act.
- **Not executed for revision 4:** any runtime probe (the models of D-P17
  to D-P19 run no rahi, hiqlite or Rauthy binary); D-P18 on the published
  binary; D-P19 on Linux; the hiqlite H-7 interleavings (source only on the
  producer's side); the Rauthy inventory (source only); every acceptance
  criterion of 043 (nothing is implemented).
