# Owner decision packet, 2026-09-23: spec 043 revision 2, and the 044 anchor question

Version 1. Owned by `specs/043-patched-dependency-adoption/spec.md` (draft).
Prepared for the rahi owner. **Nothing in this packet is accepted.** Each
decision below is a recommendation until the owner records it; the text
under "proposed" is what would be written if it is accepted, and nothing is
written before that.

## 0. What is being decided, and what is already decided

**The revision under decision.** Spec 043 revision 2 on branch
`corpus/043-patched-dependency-adoption` (draft PR #75): commit
`95622f16b2660b161a1b360e8fd81e4882d5a2e6`, `specs/043-patched-dependency-adoption/spec.md` git blob
`22c9a8ec5c0debe5ec881a8ab4121dc77f79f750`. Revision 1 (`5707f60`) is superseded and is not offered for
approval; its section 7.3 lists why.

**Already decided (owner direction, 2026-09-23, 043 D-1 to D-6).** Adopt the
two published artifacts under a narrow exception; N=1 first with no N=3
claim; durable SQL revocation keeping the full validity window; explicit,
resumable consent with a verified pre-upgrade backup, preserving store
separation; transient Rauthy unreadiness distinct from terminal failure;
the 50 percent headroom beside a validated composition. These are
**directions**. They do not approve the implementation contract that
carries them out, which is decision 1.

**Independent review.** Revision 2 was reviewed adversarially after it was
written; section 5 lists what the review found and what changed. H-7 asks
the hiqlite producer to review the fence route too; the owner may wait for
it, and nothing else in 043 depends on it.

**Four decisions.** 1 and 2 are 043's. 3 keeps 043 independent. 4 is 044's
and is only an authorization to prepare an instrument. The choices 044
still has on TLS, the public path and qualification are listed in section 6
and are not part of this packet.

## 1. Decision 1: approve 043 revision 2 as the implementation contract

**Proposed.** Approve revision 2, including: the transition floor of B-6b
and its consequence; P-8 (the app store moves to `<data>/app-store`, and
`<data>/hiqlite` becomes a permanent fence); and P-9 (the bearer refuses a
token with no `iat` or with `exp - iat` above the manifest's lifetime, and
the floor never lifts).

**Options.**

| option | what follows |
|---|---|
| **1a. Approve revision 2 (recommended)** | The build starts at step 1 of `05-patched-adoption-delivery-plan.md`. 011 D-13 and the 037 cross-reference are recorded with the text in 1.2. |
| 1b. Approve without P-8 | Revision 3 would return to an in-place guard. 043 section 7.3 item 2 shows why that leaves a window at every in-process open: the guard must be removed before 0.15 opens the store, and nothing stops a pre-043 binary until hiqlite writes its own marker. Not recommended. |
| 1c. Approve without the floor | The revocations that exist only in the 0.14 cache (B-6a) stop protecting anything at the upgrade, contrary to direction D-3. Not viable. |
| 1d. Defer | N=1 stays on the root git patch (011 D-12) that no registry consumer inherits. |

**Consequences of 1a.**

- Users: every bearer access token issued before the upgrade is refused
  from the transition on. Native clients refresh. A refresh presented before
  the refresh token's own `nbf` makes Rauthy end that user's sessions, so
  the user logs in again. Browser sessions renew through 022. Rauthy's
  move-aside also lifts IP bans (manual ones included) and resets
  failed-login counters (R-2 asks for the full list).
- Operators: a v0.2.0 volume needs `rahi upgrade-cache --backup <archive>`
  once, with the old container stopped and removed first. rahi does not
  enforce that precondition, and the README says so. A pre-043 image
  started later on the volume waits in `migrate` and serves nothing.
  Rollback is the archive, restored into a fresh volume. `--abort` works
  before `flooring`.
- Consumers: `Config::hiqlite_dir()` changes its answer (C-1). The release
  is 0.3.0.
- Governance: 043 carries the 011 exception (D-1). 010, 011, 016, 021, 025,
  030, 031, 032, 035, 037, 038 and 039 are extended as 043's frontmatter
  lists. No complete spec's text changes except the two entries in 1.2.

**Owner:** the rahi owner. **Affected work:** 043 and the specs it extends;
the 0.3.0 release; consumers under C-1.

### 1.1 Proposed text for 043

Frontmatter `status: draft` becomes `status: approved`. Appended to section 7:

> - **D-7 (<date>, owner decision; approval of revision 2).** The owner
>   approves this specification as committed at `95622f16b2660b161a1b360e8fd81e4882d5a2e6` (blob `22c9a8ec5c0debe5ec881a8ab4121dc77f79f750`) as
>   the implementation contract for D-1 to D-6, including B-6b's transition
>   floor and its stated consequence, P-8 and P-9. P-8 and P-9 are accepted
>   as written and become binding with this entry.

### 1.2 Proposed text for 011 and 037 (recorded at build step 1)

Appended to 011 section 7:

> - **D-13 (<date>, owner decision, recorded from 043 D-1 and D-7; the
>   patched-dependency exception).** B-6's "the released crate from the
>   registry, never a fork or a patch" and D-10 stand as written. They are
>   suspended for exactly two artifacts: the crates.io packages
>   `hiqlite-patched =0.15.0-patched.1` and `hiqlite-wal-patched
>   =0.15.0-patched.1`, with the checksums of 043 D-P1, consumed under the
>   name `hiqlite` by package rename; and the Rauthy image
>   `ghcr.io/bartekus/rauthy-patched:0.36.2-patched.2@sha256:ea114a8bb743d578dea6d7800916ee43550939c749a2cf586f9abdc0d0c52478`.
>   D-12's `[patch.crates-io]` and the hiqlite allow-git entry end with
>   043. rahi authors no change to either artifact and maintains no fork. A
>   later version of either is its own governed adoption. Unlike D-12, the
>   exception reaches registry consumers: the published `rahi-store`
>   declares the renamed package, so a consumer resolves the same crates.

Appended to 037 section 7, as the next free decision number:

> - **D-10 (<date>; cross-reference).** Section 6's "a rauthy fork: rahi
>   consumes released rauthy" stands. The N=1 cell consumes one downstream
>   build of released Rauthy 0.36.2, pinned by digest, under 011 D-13 and
>   043 D-1. rahi authors no change to it and maintains no fork.

## 2. Decision 2: P-7, the restore floor

**Proposed.** Every restore raises the floor to the restore instant (043
B-6c). A restore of a pre-043 archive does so whatever this decision says,
because that archive's revocations were in a cache it does not carry.

| option | what follows |
|---|---|
| **2a. Accept P-7, keeping the Rauthy stale-refresh limitation visible (recommended)** | After any restore, every access token issued before the restore is refused. That covers every access-token revocation taken after the archive instant. B-6b's refresh and re-login consequence then applies to every restore. |
| 2b. Reject | An access token revoked after the archive instant is valid again after the restore, until its own expiry. P-9's ceiling bounds that expiry at `iat + L`, so the window is at most L plus the leeway. |

**What P-7 does not cover (stays stated).** A Rauthy-side revocation after the
archive instant, meaning a session or refresh token that was ended, comes back
with Rauthy's restore. That refresh token can then mint new access tokens.
They are issued after the floor, so they are admitted until the refresh token
expires or is revoked again. The spec, the README and the release notes say
so. The hiqlite reconciliation's F7 raises the same question, and R-2 and the
producer's restore work are where it would close.

**Proposed text for 043 section 7:**

> - **D-8 (<date>, owner decision; P-7).** P-7 is accepted: every restore
>   raises the floor to the restore instant. B-6c's statement of what no
>   floor covers (a Rauthy refresh token revoked after the archive instant
>   is live again after the restore) stays in this spec, the README and the
>   release notes.

**Owner:** the rahi owner. **Affected:** 043 B-6c and AC-6 (e); 030's restore
(an extension already listed).

## 3. Decision 3: N1 stays independent of N=3

**Proposed.** No 043 criterion waits on spec 044, on an N=3-qualified
hiqlite release (Track S7), or on any pending hiqlite decision (D-8a to
D-8e, D-12, D-15, D-16). When 044 is next revised, it adds `depends_on: 043`.

| option | what follows |
|---|---|
| **3a. Independent (recommended)** | N=1 ships as 0.3.0 once 043 completes. The N=3 work proceeds on its own blockers (decision 4, Track S7). |
| 3b. Coupled | 043 waits for 044, which is blocked on a bootstrap-tier question, and for an N=3 release that does not exist. N=1 stays on the git patch indefinitely. |

**Proposed text for 043 section 7:**

> - **D-9 (<date>, owner decision; independence of N1).** This spec's
>   delivery and acceptance depend on no N=3 work: not on spec 044, not on
>   an N=3-qualified hiqlite release, and not on any pending hiqlite
>   decision. D-2's labelling of `deploy/n3` as unqualified stands.

**Owner:** the rahi owner. **Affected:** 043, 044 (a later `depends_on`
edge), the hiqlite handoff's D-16 (which rahi's decision matches).

## 4. Decision 4: prepare a separate owner re-founding instrument for 044

### 4.1 The conflict

Spec 000 lists `one-deployment-unit` in `unamendable` and states it in
section 6 as "A governed cell is one container, one volume, one public
origin; rauthy is inside it and reached only through the app's origin."
Constitution VI restates it. The constitution's amendment clause lets an
ordinary spec amend the constitution only where it "does not contradict a
`specs/000` `unamendable` anchor". The corpus defines no instrument for
changing spec 000's freeze surface.

Spec 044 implements the hiqlite owner decision D-14: at N=3, two
StatefulSets, with Rauthy in its own pods and volumes, reached by rahi over
an internal Service. That contradicts "one container", "one volume" and
"rauthy is inside it" as written. No mechanism in 044 honors both, so under
001 D-10's test (resolve only if a mechanism honors both specs) 044 cannot
be approved through the ordinary flow.

**A sidecar does not comply.** The hiqlite proposal's fallback 12.5 (Rauthy
as a native sidecar in rahi's pod, shared volume, loopback) is a second
container, so "one container" is still broken. It also contradicts D-14.

### 4.2 Options

| option | honors the anchor? | consequence |
|---|---|---|
| **A. Owner re-founding (recommended)** | Yes. The freeze is changed deliberately by the owner, and the anchor stays frozen under its own name. | Every spec that transitively depends on 000 is re-verified (000 section 7, Amendments). |
| B. Keep D-14 outside rahi | Yes | rahi's N=3 stays 032's co-located overlay. A split deployment is Statecraft's, outside rahi's governed cell. |
| C. Sidecar (fallback 12.5) | No | Still needs A for "one container". Contradicts D-14. |
| D. Approve 044 as an ordinary spec | No | Forbidden by the amendment clause. Not available. |

### 4.3 Proposed verbatim bootstrap text (for review; not written into spec 000)

Section 6's first bullet would read:

> - A governed cell is one public origin, and the app's store and rauthy's
>   store are never shared. At N=1 the cell is one container and one
>   volume, rauthy is inside it, and it is reached only through the app's
>   origin. At N=3 the cell is one namespace in which the app's pods and
>   rauthy's pods each keep their own volume per pod; users reach rauthy
>   only through the app's origin, and the app reaches rauthy only through
>   one internal Service that is encrypted and admits only the app's pods.
>   No other topology, and no sidecar arrangement, is a governed cell.
>   *(anchor: `one-deployment-unit`)*

A new section after section 9 would read:

> ## Amendments received
>
> - **<date>, owner act at the bootstrap tier (re-founding of
>   `one-deployment-unit`).** Provenance: hiqlite proposal D-14, decided by
>   the owner on 2026-09-23; rahi spec 044 (draft); decision 4 of
>   `docs/design/06-owner-decision-packet-2026-09-23.md`. Section 6's
>   `one-deployment-unit` text is replaced as above. The anchor keeps its
>   name and stays in `unamendable`. This entry is the owner's act and the
>   only instrument by which the anchor changed. It is not a precedent that
>   an ordinary spec, or an agent, may change an anchor. Every spec
>   depending on this one is re-verified before it is next relied on.

Constitution VI would then be amended by 044 in the ordinary flow to match.
Its "one command" and "N=1 pays nothing for the existence of N=3" are kept.

### 4.4 Impact assessment

- **Mechanics.** spec-spine 0.20.0 classifies any change to a spec that
  declares `unamendable` as a `constitutional` delta (`delta.rs`,
  `DeltaClass::Constitutional`). It reports the change and does not refuse
  it, so the authority is the owner's act alone. That is why the entry
  names the act.
- **Re-verification.** Every ordinary spec depends on 000 transitively. The
  N=1 text is unchanged in substance, so no code should change. The cost is
  one `make verify` per complete spec, recorded in the change that carries
  the amendment.
- **Other anchors.** `store-separation` is untouched: the stores stay
  separate at both sizes, and the new text restates that. `idp-authority`
  is untouched. `layer-direction` is untouched.
- **Precedent.** The owner re-founding tier 1 once is recorded as the
  owner's act only, and the entry says it is not a precedent for anything
  below the owner.
- **Statecraft.** Statecraft's rule that Rauthy is never a separate workload
  (044 P-4) is Statecraft's to amend. The rahi amendment neither does that
  nor depends on it.
- **043.** Unaffected. 043's N=1 cell satisfies both the current and the
  proposed text.

**Decision requested now.** Only whether to prepare the instrument (option
A) as a separate change for the owner's review. The text in 4.3 is a
starting point. Nothing is written into spec 000 until the owner approves
the exact text of that separate change.

**Owner:** the rahi owner, at the bootstrap tier. **Affected:** spec 000,
the constitution, spec 044 (PR #76), every spec's re-verification.

## 5. Independent review of revision 2

An independent reviewer read revision 2 at `35e0e89` against hiqlite's and
rahi's source. It confirmed D-P6 and D-P9 line by line, confirmed that no
step opens anything under Rauthy's directory, confirmed B-6b's `iat <=
revoked_at` argument, and found no contradiction with the acceptance
criteria of the complete specs 025 and 038. It asked for changes, all made
at `95622f1`:

| finding | severity | resolution |
|---|---|---|
| Nothing fenced a fresh volume, and the image's `migrate` would refuse the first boot, because only T1 wrote a fence | critical | `first-boot` creates the fence before its layout creates the app store; the gate does the same for any entry point meeting an absent legacy path; AC-3a tests a fresh volume against the v0.2.0 image |
| `first_boot.rs` changes behavior but was not claimed | critical | added to `extends` (031, amending) and to plan step 4 |
| FR-008's enumeration covered the verb table only | warning | FR-008 states `first-boot`'s exemption and its only writes, with a test that it opens neither store |
| AC-4 omitted the `verifying` state | warning | added |
| T1's probe did not say what an absent lock file means | warning | an absent file reads as not held |
| T3's foreign-marker branch looked proven | suggestion | marked defensive; only AC-5's synthetic case reaches it |

The reviewer's gate verdict (stale) came from the shared global spec-spine
0.22.0, not the 0.20.0 pin; `make gate` under the pin is green on both
commits. Separately, the hiqlite working tree's uncommitted F-130 led to
T2's rename order and request H-8.

## 6. 044's remaining choices (not part of this packet)

They are listed so they are not lost. Each is decided with 044 after
decision 4, never inside 043.

| choice | 044 reference | recommendation, as 044 drafts it |
|---|---|---|
| Encryption of rahi's path to Rauthy | P-1, B-2 | Rauthy's native TLS with a CA mounted into rahi. A mesh only with a qualified shutdown order. |
| Rauthy's public path | P-2, B-3 | Users reach Rauthy only through rahi's origin, as at N=1. The proposed anchor text in 4.3 requires this. |
| First bootstrap ordering | P-3 | `OrderedReady` until hiqlite F-118 is repaired, then `Parallel`. |
| Coherent export (B-11) | "still open" list | Approve 044 with B-11 held until a hiqlite release carries the offline export. |
| N=3 qualification | 044 summary, hiqlite Track S7 | No N=3 support claim until a hiqlite release has passed the proposal's stages 4 to 6 and rahi's own N=3 acceptance has run on three pods. |

## 7. Approval text

The owner may accept any subset. Each line names the exact revision.

```
Owner decision, <date>, on docs/design/06-owner-decision-packet-2026-09-23.md:

1. I approve specs/043-patched-dependency-adoption/spec.md as committed at
   95622f16b2660b161a1b360e8fd81e4882d5a2e6 (git blob 22c9a8ec5c0debe5ec881a8ab4121dc77f79f750), revision 2, as the implementation contract,
   including the transition floor of B-6b and its consequence, P-8 and P-9.
   Record it as 043 D-7, set 043 to approved, and at build step 1 record
   011 D-13 and 037 D-10 with the text of the packet's section 1.2.
2. I accept P-7. The Rauthy stale-refresh limitation of B-6c stays stated in
   the spec, the README and the release notes. Record it as 043 D-8.
3. Track N1 is independent of spec 044 and of any N=3 qualification.
   Record it as 043 D-9.
4. Prepare, as a separate change for my review, an owner re-founding
   instrument for spec 000's one-deployment-unit anchor, starting from the
   packet's section 4.3. Nothing is written into spec 000 until I approve
   its exact text.
```
