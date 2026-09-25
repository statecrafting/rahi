# Owner decision packet, 2026-09-23: spec 043 revision 3

**Superseded by `09-owner-decision-packet-rev4-2026-09-23.md`.** Revision 3
(`abd66fd`, blob `d2fd7bf`) is not offered for approval and the approval
text below is void: revision 4 withdraws its pruning and narrows its
supervisor-fence claim (043 section 7.5). 06 section 1.2's text for 011 and
037 is still cited by 09.

Version 1. Owned by `specs/043-patched-dependency-adoption/spec.md` (draft).
Prepared for the rahi owner. It supersedes
`06-owner-decision-packet-2026-09-23.md`, whose approval reference
(revision 2, `95622f1`) is withdrawn: revision 2 is not offered for
approval. **Nothing in this packet is accepted.** Each decision is a
recommendation until the owner records it.

## 0. What is under decision

**The revision.** Spec 043 revision 3 on branch
`corpus/043-patched-dependency-adoption` (draft PR #75): commit
`abd66fdd27c54f07870abc60c5b2b244f9ccdba2`, `specs/043-patched-dependency-adoption/spec.md` git blob
`d2fd7bfeea3fd6c25a016b1bc03d17b82587279f`. Approval names that blob; a later commit that changes the spec
file invalidates it.

**Already decided (owner direction, 043 D-1 to D-6).** Adopt the two
published artifacts under a narrow exception; N=1 first with no N=3 claim;
durable SQL revocation keeping the full validity window; explicit,
resumable consent with a verified pre-upgrade backup, preserving store
separation; transient Rauthy unreadiness distinct from terminal failure;
the 50 percent headroom beside a validated composition. None of these is
reopened here.

**What changed since revision 2** (043 section 7.4): a stopping pre-043
node could still be writing its database when revision 2's T1 accepted
(probed); the guard could be left partial or be truncated by a starting
pre-043 node (source); recovery trusted paths instead of identities; the
gate read before it locked; pruning by the largest declared lifetime fails
when a deploy raises it (timeline in 7.4 item 4); a directly started
pre-043 `supervise` spawns Rauthy on the upgraded volume (probed); hiqlite
F-130 is on the transition's normal crash path; R-3 was wrong (the assets
exist and agree).

**Independent review.** Revision 3 (`c2c7c72`) was reviewed read-only
against hiqlite 0.14 source, the v0.1.0 and v0.2.0 tags and current
source. No critical finding; it confirmed D-P11's citations, the D-P12 and
D-P13 counts, the pruning arithmetic, and that B-4b is a documented gate
rather than a contradiction with AC-4 and AC-9. Two precision warnings (the
read pool behind T1 (c); the pre-043 `first-boot` path that generates keys)
and two suggestions were applied in `abd66fd`, the revision offered here.

**Structure.** Decisions 2 to 8 are separable. Decision 1 approves the
contract text; the text already treats P-7 and P-11 as conditional, and
writes P-8, P-9, P-10 and P-12 as binding, so rejecting any of those four
means revision 4, which is prepared with the named fallback and resubmitted
by hash. Decision 9 concerns a different change.

## 1. Approve 043 revision 3 as the implementation contract

**Exact text.** See section 10, line 1.

| option | consequence |
|---|---|
| **Approve (recommended)**, with decisions 3, 4, 5 and 8 as recommended | The build starts at step 1 of `05-patched-adoption-delivery-plan.md` (version 2). 011 D-13 and 037 D-10 are recorded with the text in 06 section 1.2, which is unchanged. |
| Approve with a rejected P-8, P-9, P-10 or P-12 | Revision 4 with that proposal's fallback (sections 3, 4, 5, 8), resubmitted by hash before any build step. |
| Defer | N=1 stays on 011 D-12's root git patch, which no registry consumer inherits. |

**Practical consequences of approval.** Users: every bearer access token
issued before the upgrade is refused from the transition on; native
clients refresh, and one that refreshes before its refresh token's `nbf`
logs in again; browser sessions renew. Rauthy's move lifts IP bans and
resets failed-login counters (R-2 asks for the full list). Operators: a
v0.2.0 volume needs `rahi upgrade-cache --backup <archive>` once, with the
old container stopped and removed first; afterwards a pre-043 image waits in
`migrate`, and a pre-043 `supervise` run directly exits before spawning
Rauthy (P-10); rollback is the archive into a fresh volume, `--abort`
before `flooring`. Consumers: `Config::hiqlite_dir()` changes (C-1); 0.3.0.
Governance: the extends edges of 043's frontmatter, including the new one
on 031's `rauthy_env.rs`.

**Owner:** the rahi owner. **Blocks:** every 043 build step.

## 2. P-7: every restore raises the floor

**Exact text.** Section 10, line 2.

| option | consequence |
|---|---|
| **Accept, keeping the Rauthy stale-refresh limitation stated (recommended)** | After any restore, every access token issued before it is refused; that covers every access-token revocation taken after the archive instant and every floor raise the archive predates. B-6b's refresh consequence applies to every restore. |
| Reject | A token revoked after the archive instant is admitted again until its own expiry, at most L + 60 seconds after its `iat` (P-9's ceiling bounds it). A pre-043 archive still raises the floor. |

What neither option covers: a Rauthy refresh token revoked after the archive
instant comes back with Rauthy's restore and can mint new access tokens.
**Owner:** the rahi owner. **Blocks:** AC-6 (e) only.

## 3. P-8: the relocated app store and its permanent fence

**Exact text.** Section 10, line 3.

| option | consequence |
|---|---|
| **Accept (recommended)** | The app store moves to `<data>/app-store`; `<data>/hiqlite` becomes a fence that is never removed after `flooring`, written whole and after the old node is proven gone (T1), on every volume this version touches. Every published pre-043 verb that opens the store panics, refuses or waits on it (D-P7, D-P8, D-P14). Debris old starts leave beside it is accepted and reported, never removed at runtime. |
| Reject | Revision 4 returns to an in-place guard. 043 section 7.3 item 2 shows the window that leaves at every in-process open: the guard must be removed before 0.15 opens the store. Not recommended. |

**Owner:** the rahi owner. **Blocks:** build steps 4 and 6.

## 4. P-9: token admission and pruning

**Exact text.** Section 10, line 4.

| option | consequence |
|---|---|
| **Accept (recommended)** | The bearer refuses no `iat`, `exp < iat`, `exp - iat > L`, and `iat` beyond the leeway in the future; the floor never lifts; revocation rows are pruned at `revoked_at + 86,520 s`, so about a day of rows instead of twelve minutes. A Rauthy misconfigured above the manifest's lifetime is refused at the bearer. |
| Accept admission, prune by the largest declared L (revision 2) | Fails under a raised lifetime (043 7.4 item 4). Not viable. |
| Reject the ceiling | Revision 4 needs another bound on admitted tokens for both the pruning proof and the floor argument; none is known that needs no historical lifetime. Not recommended. |

The stated limitation stays: a backward wall-clock step of Δ seconds lets a
revocation that lived only in the old cache stop protecting a token for at
most Δ + L + 60 seconds after the upgrade (B-6b). Decision 6 is the option
that removes it. **Owner:** the rahi owner. **Blocks:** build step 5.

## 5. P-10: the supervisor fence

**Exact text.** Section 10, line 5.

| option | consequence |
|---|---|
| **Accept (recommended)** | The rendered Rauthy environment moves to `<data>/rauthy-env/rauthy.env`; `<data>/rauthy/rauthy.env` becomes a directory holding `FENCE`. Every published pre-043 `supervise` exits before spawning Rauthy (v0.2.0 executed, v0.1.0 by source), so the operator precondition shrinks to a Rauthy started outside rahi. This touches one path under `<data>/rauthy`, the file rahi renders there, and none of Rauthy's storage. |
| Accept the alternative | An unparseable `<data>/restore.marker`: wholly outside `<data>/rauthy`, executed on v0.2.0, but v0.1.0's `supervise` never reads it. |
| Reject | Revision 4 returns B-5 to revision 2's precondition for a directly started pre-043 `supervise`, which spawns Rauthy within a second (D-P14) and, after T4, can destroy Rauthy's raft metadata. |

**The anchor question the owner is asked to answer with this decision.**
Spec 000's `store-separation` reads "separate storage that app code never
opens" and Constitution VIII "never opens, lists, or backs up rauthy's
storage". rahi has written `rauthy/rauthy.env` and `rauthy/config.toml`
since 031. The recommendation reads "storage" as Rauthy's hiqlite data, not
rahi's rendered configuration beside it. If the owner reads it otherwise,
the alternative is the compliant choice. **Owner:** the rahi owner.
**Blocks:** build steps 4 and 6.

## 6. P-11: a clock-independent floor (optional)

**Exact text.** Section 10, line 6.

| option | consequence |
|---|---|
| **Decline for now (recommended)** | B-6b's backward-step limitation stays stated in the spec, README and release notes. At N=1 one host clock serves both processes, the transition runs after the old cell stops, and a backward step larger than the gap between the last old revocation and the transition is a clock fault an operator can see. |
| Accept | At T4 the supervisor records Rauthy's published key ids, rotates Rauthy's signing keys (`POST /auth/v1/oidc/rotate_jwk`), and the bearer refuses every recorded key id permanently, whatever the token's `iat`. Costs a Rauthy admin call on the transition path whose failure holds the cell at `floored`; the cell's admin key's rights for that call are unverified and are established first. |

**Owner:** the rahi owner. **Blocks:** nothing unless accepted (then AC-6 (f)).

## 7. N1 stays independent of N=3

**Exact text.** Section 10, line 7. Unchanged from 06 decision 3.

| option | consequence |
|---|---|
| **Independent (recommended)** | No 043 criterion waits on spec 044, an N=3-qualified hiqlite release, or a pending hiqlite N=3 decision. 044 later adds `depends_on: 043`. Decision 8 couples 043 to one N=1 safety repair; that is not an N=3 item. |
| Coupled | 043 waits on 044's bootstrap-tier question and on an N=3 release that does not exist. |

**Owner:** the rahi owner. **Blocks:** nothing now; it records scope.

## 8. Current image or repaired image for the release (P-12)

**Exact text.** Section 10, line 8.

| option | consequence |
|---|---|
| **1. Implement now; complete and release only on a repaired producer build (recommended)** | 043 is built and merged at `in-progress` on the current pins. It is flipped `complete`, and 0.3.0 qualified, only after a `hiqlite-patched` release carrying hiqlite 035 B-5's move order is published, a Rauthy image rebuilt on it is published with provenance, and the owner approves a repin amendment naming those exact artifacts. The agent prepares that amendment when the artifacts exist and chooses no version itself. |
| 2. Release on the current image with a narrowed promise | AC-4 would accept that an interruption inside Rauthy's move is recovered only from the verified archive into a fresh volume. rahi cannot tell that interruption from any other failure to reach `rauthy-done` without opening Rauthy's storage, so every unready first Rauthy start at `floored` would call for an archive restore. Needs revision 4 and new acceptance tests. |

**Owner:** the rahi owner; the producer artifacts are the hiqlite and Rauthy
maintainers' (H-8). **Blocks:** 043 `complete` and the 0.3.0 release, not
implementation.

## 9. The 044 owner instrument: prepared, not approved

This session prepared it, under the direction to keep the N=3 front ready:
`docs/design/08-owner-instrument-one-deployment-unit.md` on branch
`corpus/000-refounding-one-deployment-unit`, draft PR #77, commit
`82f2189c371d28a60863ae60b298dd724ec07761`, blob
`4e686d8fe64c52e166dd63359115d311f1f13e89`. It holds the authority
explanation (the existing constitution does not permit the change), the
exact replacement text for spec 000 section 6, the `## Amendments received`
entry, the Constitution VI text 044 would carry, and a bounded
re-verification plan (one pass of `make verify` over the 26 complete
specs). Its text differs from 06 section 4.3: it keeps spec 032's
co-located N=3 layout governed, which 06's draft would have removed.

**Decision requested now:** none. Approving the instrument is a separate
owner act with its own approval text (section 6 of the instrument). It
blocks only spec 044. 044's remaining choices (TLS for the internal Service,
Rauthy's public path, bootstrap ordering, coherent export, N=3
qualification) are listed in 06 section 6 and stay with 044.

## 10. Approval text

The owner may accept any subset, line by line.

```
Owner decision, <date>, on docs/design/07-owner-decision-packet-rev3-2026-09-23.md:

1. I approve specs/043-patched-dependency-adoption/spec.md as committed at
   abd66fdd27c54f07870abc60c5b2b244f9ccdba2 (git blob d2fd7bfeea3fd6c25a016b1bc03d17b82587279f), revision 3, as the implementation
   contract, including B-6b's transition floor and its consequence. Record
   it as 043 D-7, set 043 to approved, and at build step 1 record 011 D-13
   and 037 D-10 with the text of 06 section 1.2.
2. I accept P-7. The Rauthy stale-refresh limitation of B-6c stays stated in
   the spec, the README and the release notes. Record it as 043 D-8.
3. I accept P-8 as written in revision 3. Record it as 043 D-9.
4. I accept P-9 as written in revision 3, including pruning at
   revoked_at + V(L_max) and B-6b's stated backward-clock limitation.
   Record it as 043 D-10.
5. I accept P-10, the supervisor fence at <data>/rauthy/rauthy.env, and
   read spec 000's store-separation anchor as not covering rahi's rendered
   configuration beside Rauthy's storage. Record it as 043 D-11.
6. I decline P-11 for now. Record it as 043 D-12.
7. Track N1 is independent of spec 044 and of any N=3 qualification.
   Record it as 043 D-13.
8. I choose option 1 of section 8: implement 043 now, and flip it complete
   and qualify 0.3.0 only after a repaired hiqlite-patched and a Rauthy image
   rebuilt on it are published and I approve a repin amendment naming them.
   Record it as 043 D-14.
```
