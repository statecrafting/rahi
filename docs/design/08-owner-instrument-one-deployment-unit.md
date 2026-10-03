# Owner instrument: re-founding the `one-deployment-unit` anchor

Prepared 2026-09-23; refreshed 2026-10-02 for the rahi owner. **Not approved.
Nothing here is written into `specs/000-rahi-bootstrap/spec.md` or
`standards/spec/constitution.md`.** This document is the complete text of a
proposed bootstrap-tier act, so the owner can approve, amend or reject its
exact wording. Its approval is a separate act from its preparation, and
approving it does not approve spec 044.

## Decision for the owner

The choice is whether the governed cell boundary at N=3 may include separate
rahi and Rauthy workloads. This is a bootstrap decision because the current
anchor is unamendable through the ordinary spec flow.

| choice | consequence |
|---|---|
| **Permit both N=3 layouts (recommended)** | Preserve N=1 and the existing co-located N=3 layout; permit split N=3 only with one public origin, separate stores, and an encrypted, restricted internal identity path. The exact proposal is section 2. |
| Keep the current anchor | Split N=3 stays outside rahi's governed cell and spec 044 stays blocked. No bootstrap amendment or re-verification is needed. |
| Permit only split N=3 | Retire the co-located layout as a governed cell. This requires a different instrument with an explicit disposition of spec 032 and `deploy/n3`; section 2 does not authorize it. |

The recommendation preserves the existing layout's governance status while
allowing the failure isolation D-14 seeks. It creates obligations that the
single container previously supplied: authenticated encrypted transport,
readiness independent of liveness, bootstrap ordering, and backup coherence.
Spec 044 must resolve and verify those obligations before support is claimed.
Retaining a layout in the anchor is permission, not evidence that it works.

Preparation, selection of a preferred option, a green check, or merging this
proposal is not the owner act. Section 6 identifies the exact approval text.
After that act, a separate application PR carries the normative edits and
their provenance. Spec 044 remains draft and pending in both PRs.

## 1. Why an instrument, and why only the owner can issue it

Spec 000 lists `one-deployment-unit` in its `unamendable` set and states it
in section 6: "A governed cell is one container, one volume, one public
origin; rauthy is inside it and reached only through the app's origin."
Constitution VI restates it.

Spec 044 (draft, PR #76, merged at `1be339c2dd2098b2f7bad38064efeacf0ec89706`)
proposes implementing the hiqlite owner
decision D-14: at N=3, rahi's pods and Rauthy's pods are separate
StatefulSets, and rahi reaches Rauthy over an internal Service. That breaks
"one container", "one volume" and "rauthy is inside it" as written. No
mechanism inside 044 honors both texts, so under 001 D-10 an ordinary spec
may not resolve it.

The existing constitution does **not** permit the change:

- The constitution's amendment clause lets an ordinary spec amend the
  constitution only where the amendment "does not contradict a `specs/000`
  `unamendable` anchor". This one does.
- Spec 000's own conventions (section 7, "Amendments") describe an
  `## Amendments received` entry for a change to a shipped spec's
  contract. They say nothing that makes an anchor amendable, and the
  `unamendable` list exists to say the opposite.
- No spec defines an instrument for changing spec 000's freeze surface.

The only authority above spec 000 is its author and owner. The act is
therefore the owner's, at the bootstrap tier, recorded in spec 000 itself
with its provenance, and explicitly not a precedent that any ordinary spec,
session or agent may change an anchor. spec-spine 0.20.0 classifies a change
to a spec that declares `unamendable` as a `constitutional` delta and
reports it without refusing it, so the tool is not the authority either; the
owner's recorded act is. The tool's classification is evidence about the
delta, not a delegation of bootstrap authority.

## 2. Exact replacement text

### 2.1 Spec 000, section 6, first bullet

Current:

> - A governed cell is one container, one volume, one public origin; rauthy
>   is inside it and reached only through the app's origin. *(anchor:
>   `one-deployment-unit`)*

Replacement:

> - A governed cell has one public origin, rauthy is reached by users only
>   through the app's origin, and the app's store and rauthy's store are
>   never shared. At N=1 the cell is one container and one volume with
>   rauthy inside it. At N=3 the cell is one of exactly two layouts: three
>   replicas of the N=1 unit, one container and one volume per pod; or, in
>   one namespace, the app's pods and rauthy's pods as separate workloads,
>   each pod with its own volume, the app reaching rauthy only through one
>   internal Service that is encrypted and admits only the app's pods. No
>   sidecar arrangement, shared volume, or other topology is a governed
>   cell. *(anchor: `one-deployment-unit`)*

The anchor keeps its name and stays in `unamendable`.

This text differs from the draft in section 4.3 of
`docs/design/06-owner-decision-packet-2026-09-23.md`. That
draft allowed only the split layout at N=3, which would have taken spec
032's complete, co-located N=3 layout (`deploy/n3`, one container per pod
with rauthy inside, one volume per pod) out of the governed cell without
saying so. This text keeps it.

### 2.2 Spec 000, a new section after section 9

> ## Amendments received
>
> - **<date>, owner act at the bootstrap tier (re-founding of
>   `one-deployment-unit`).** Provenance: hiqlite owner decision D-14
>   (2026-09-23, the N=3 cell as two StatefulSets); rahi spec 044 (draft);
>   `docs/design/08-owner-instrument-one-deployment-unit.md`. Section 6's
>   `one-deployment-unit` bullet is replaced by the text now in section 6.
>   The anchor keeps its name and stays in `unamendable`. This entry is the
>   owner's act and the only instrument by which the anchor changed; it is
>   not a precedent that an ordinary spec, a session or an agent may change
>   an anchor. Constitution VI is aligned in the same change. The owner's
>   approval message identifies the instrument's commit and blob. Every
>   affected spec is re-verified under section 4 before it is next relied
>   on; the evidence record identifies the normative commit and per-spec
>   outcomes. Pending outcomes remain explicit blockers.

### 2.3 Constitution VI (in the same application PR)

The application PR replaces Constitution VI's paragraph alongside the
bootstrap bullet, under the same recorded owner act. This avoids an
interval where the constitution still forbids the layout the anchor now
permits. The exact proposed replacement is:

> A governed cell is one public origin and one command, in one of the
> layouts `specs/000` section 6 names. rauthy is reached by users only
> through the app's origin. N=1 is the primary mode and pays nothing for
> the existence of N=3. A spec that adds a sidecar, a second exposed port, a
> managed database, or a cloud prerequisite must justify it in its Purpose
> section. *(Bootstrap anchor: `one-deployment-unit`.)*

The heading and the constitution's ordinary amendment clause stay as written.
This act changes the named anchor and aligns VI; it grants no general power
to amend other anchors.

## 3. Impact

| area | effect |
|---|---|
| N=1 (031, 034, 043) | None. The N=1 sentence says what the current text says. 043's cell satisfies both texts. |
| 032 co-located N=3 | Stays governed (first N=3 layout). Its manifests, which refuse a shared volume and require one container per pod, satisfy the new text. 043 D-2 still labels `deploy/n3` unqualified; that is a qualification statement, unchanged. |
| 044 split N=3 | Becomes approvable through the ordinary flow, subject to its own open choices (TLS for the internal Service, the public path, bootstrap ordering, coherent export, N=3 qualification). The new text fixes two of them as requirements: users reach rauthy only through the app's origin, and the internal Service is encrypted and admits only the app's pods. |
| `store-separation` | Untouched, and restated in the new text. |
| `idp-authority`, `layer-direction` and the other anchors | Untouched. |
| Constitution VI | Aligned with the new anchor in the same application PR (2.3). |
| Statecraft | Its rule that Rauthy is never a separate workload (044 P-4) is Statecraft's to amend. This instrument neither amends it nor depends on it. |
| Precedent | None, by the entry's own words. |

## 4. Bounded re-verification plan

Spec 000's conventions say an amendment invalidates every transitive
dependent until re-verification. Every ordinary spec depends transitively
on 000. This proposal changes no normative requirement yet, so it does not
trigger that invalidation. Applying the approved instrument does.

- **Set.** The current typed registry reports 29 completed specs: 001,
  010 to 016, 020 to 026, 030 to 039, 042, 045, 046, and 048. This was read
  on 2026-10-02 at `6f91769d3acc478b6af2f14fe4510f4f3989b2ef` (the upgrade
  branch based on merged main `1be339c`). Re-read the registry on the
  normative commit and include any further completed specs. Specs that are
  pending or in progress satisfy the revised contract and their own
  acceptance before completion; this instrument grants them no acceptance.
- **Method.** Run `make gate` with the repository's exact pinned engine on
  the normative commit. Read each completed spec's `spec-spine verify <id>
  --plan`, provision its declared prerequisites, and run `make verify
  SPEC=<id>` once for each. Existing required CI checks still apply, but
  they do not substitute for these per-spec verdicts. Do not assume a CI
  workflow runs this loop: record local evidence or arrange an explicitly
  reviewed runner. Operator-only acceptance is reviewed separately; where
  the revised contract affects it, obtain that evidence before reliance.
- **Bound.** One pass per completed spec on the frozen normative commit.
  Record a failure or missing prerequisite against that spec and keep it
  blocked from reliance. Diagnose the evidence before attributing a failure
  to an unrelated cause. Remediation is a separate change with its own
  checks, not a relaxation of the anchor or acceptance criteria.
- **Record.** Append the normative commit, engine version, frozen spec set,
  per-spec exit codes, prerequisite blockers, and log locations or CI run
  URLs to this document. The `## Amendments received` entry cites that
  evidence record. A merged amendment is not a passing verification record.

No N=3 support is established by this plan. Spec 044's AC-3 and AC-4 still
require operator evidence on a named release and a real three-node layout.

## 5. What is not decided here

044's own choices (section 6 of the original owner packet): the
encryption mechanism for the internal Service, Rauthy's public path, the
first bootstrap ordering, coherent export, and N=3 qualification. N=3 is not
marked supported by this instrument, and 044 is not implemented before its
own approval and prerequisites.

The replacement anchor excludes sidecar arrangements. Native Rauthy TLS
fits that boundary. A mesh requiring an additional container per pod does
not; spec 044's mesh alternative must satisfy the anchor as approved rather
than treating this instrument as a sidecar exception.

## 6. Approval text

```
Owner act, <date>, at the bootstrap tier:

I approve docs/design/08-owner-instrument-one-deployment-unit.md as committed
at <commit> (blob <blob>). Replace the one-deployment-unit bullet of
specs/000-rahi-bootstrap/spec.md section 6 with the text of its section 2.1,
add the Amendments received entry of its section 2.2 with today's date, and
align Constitution VI with section 2.3 in the same application PR. Run and
record section 4's re-verification before affected specs are relied on again.
This is my act and not a precedent. It does not approve spec 044, select its
remaining implementation choices, or claim N=3 qualification.
```

Approval must identify the final proposal's full commit and blob. The
application PR records the owner's actual message and date rather than
claiming approval from this template. Changes to sections 2 or 4 after that
approval require the owner to approve the revised text.

## 7. Application and bounded re-verification record (2026-10-02)

### Owner approval and normative commit

The preparation status at the beginning of this document is historical. On
2026-10-02 the owner explicitly approved PR #99's instrument at commit
`9bbe773f52b004cea882226760f92a76148aee08`, document blob
`5e010c1effabbb78cdc2cbba270c6a7c0e597893`. Spec 000's Amendments received
entry records the owner's actual approval message. This appendix preserves
the approved document as its exact byte prefix, including sections 2 and 4.
It records execution of that approval and changes none of its terms.

The frozen normative verification commit is
`0ef9dca4401f2a488c06d5b80751440f3b31c141`. It replaces the named anchor
under section 2.1, records the dated bootstrap act under section 2.2, and
aligns Constitution VI under section 2.3 together. The anchor remains in
`unamendable`. Spec 044 remains draft and pending. Its bootstrap prohibition
is disposed of subject to this re-verification; its other blockers remain.

### Frozen set, engine and method

The completed set was read through the pinned CLI on the frozen commit:
001, 010 to 016, 020 to 026, 030 to 039, 042, 045, 046 and 048, 29 specs.
Each `spec-spine verify <id> --plan` and acceptance section was read before
execution. `make gate` passed on the frozen commit. Each spec then received
exactly one local `make verify SPEC=<id>` execution, sequentially, without
changing the tracked tree or normative commit during that pass.

The engine was `spec-spine 0.28.0` at
`/Users/bart/.cargo/bin/spec-spine`, binary SHA-256
`9e93eca21424b22154965e537a85deee006e5ad5f18234679dd99ba7571ca875`.
The local host was macOS 26.5.1 arm64. Cargo used the configured shared
target at `~/.statecraft/worktrees/rahi/target`; `CARGO_TARGET_DIR` was unset.
The already built shared rahi binary was supplied as `RAHI_TEST_BINARY`.

All 29 declared CLI plans exited 0. This is a per-spec command verdict,
subject to the prerequisite and operator limitations below. It does not
replace acceptance criteria outside those commands. In 001, the nested
`spec-spine verify 000-rahi-bootstrap` reports no declared plan and exits
0; it is not evidence of a separate bootstrap verification. No failed frozen plan
was retried or relaxed.

### Per-spec outcomes

In the table, the log is `<spec-id>-verify.log` and the saved plan is
`<spec-id>-frozen-plan.txt`. Every command ran at the frozen commit above.
The manifest records full log and plan hashes, start and end times, exit
codes, and emitted prerequisite skips.

| Spec id | CLI exit | Commands | Acceptance and prerequisite disposition |
|---|---:|---:|---|
| `001-agentic-harness` | 0 | 57 | 57 commands passed; AC-4 linear-history setting fails operator review. |
| `010-workspace-and-core-types` | 0 | 2 | Declared commands passed. |
| `011-store-hiqlite` | 0 | 1 | S3 integration skipped locally: RAHI_TEST_S3 absent. |
| `012-store-coordination` | 0 | 2 | Declared commands passed. |
| `013-ledger-decision-chain` | 0 | 2 | Independent attest-ledger unavailable; in-process verification ran. |
| `014-ledger-sealing-and-archive` | 0 | 1 | Declared commands passed. |
| `015-kernel-manifest-and-adjudication` | 0 | 1 | Declared commands passed. |
| `016-store-binary-values-and-extensions` | 0 | 1 | Declared commands passed. |
| `020-edge-server` | 0 | 2 | Declared commands passed. |
| `021-idp-proxy-and-discovery` | 0 | 2 | Declared commands passed. |
| `022-session-and-principal` | 0 | 2 | Declared commands passed. |
| `023-observability` | 0 | 1 | Declared commands passed. |
| `024-hardening` | 0 | 1 | Declared commands passed. |
| `025-api-tokens-and-resource-server` | 0 | 1 | Live Rauthy skipped locally; frozen live workflow supplies that proof. |
| `026-streaming-responses` | 0 | 2 | Declared commands passed. |
| `030-operational-verbs` | 0 | 2 | Live Rauthy and admin export skipped locally; frozen live workflow supplies the live suite. |
| `031-single-container-packaging` | 0 | 2 | Fresh local Docker build and smoke plus both hosted architecture smokes passed. |
| `032-cluster-topology` | 0 | 1 | Manifests passed; no real three-node qualification run. |
| `033-dev-substrate-and-harness` | 0 | 1 | Live local harness cases skipped; frozen live workflow and Compose validation supplement them. |
| `034-hello-cell` | 0 | 2 | Live local cases skipped; frozen live workflow passed. Independent verifier unavailable. |
| `035-denials-survive-shutdown` | 0 | 3 | Declared commands passed. |
| `036-manifest-and-schema-evolution` | 0 | 4 | Ledger, store, operations and evolution commands passed; live local skips supplemented by frozen live suite. |
| `037-identity-recovery-and-live-proof` | 0 | 3 | Recovery commands passed; frozen live workflow supplies fresh live proof. |
| `038-native-clients-and-bearer-revocation` | 0 | 4 | Bearer and revocation commands passed; frozen live workflow supplies fresh live proof. |
| `039-release-and-out-of-tree-packaging` | 0 | 4 | Packaging commands passed; release-checkpoint operator evidence was not refreshed. |
| `042-ledger-lifetime-identity` | 0 | 5 | Identity, append, seal, verification and CLI proof commands passed. |
| `045-store-receipts-and-work-claims` | 0 | 6 | Declared commands passed. |
| `046-named-migration-sets` | 0 | 4 | Declared commands passed. |
| `048-store-clean-shutdown` | 0 | 3 | Declared commands passed. |

### Prerequisites and supplementary operator evidence

Local Cargo, Rust, the pinned engine, Docker and kubectl were available.
Standalone kustomize and kubeconform were absent; the validation script
used its kubectl kustomize fallback and its declared optional-schema-check
behavior. No S3 endpoint, named three-node cluster, local Rauthy binary or
Rauthy credentials, or independent attest-ledger binary were supplied.
Those absences are recorded, not silently converted into acceptance. Spec
011 FR-003 permits the named S3 skip; 013 FR-004 and 034 FR-003 permit
the named independent-verifier skip. These clauses explain the passing
command verdicts and do not claim the absent external proof was obtained.

The following reviewed workflows ran at the same frozen normative SHA:

- [Live proof run 37050143344](https://github.com/statecrafting/rahi/actions/runs/37050143344)
  passed. Its full workspace suite used pinned Rauthy with
  `RAHI_REQUIRE_RAUTHY=1`; real login, hello-cell and bearer proof ran.
  Its separate final-stop series also passed. This is fresh N=1 live
  evidence, including the cases that the local plans skip without Rauthy.
  The final-stop series does not mark spec 043 complete.
- [Image run 37050991488](https://github.com/statecrafting/rahi/actions/runs/37050991488)
  passed amd64 and arm64 builds and empty-cell and hello-cell smokes.
  Publication was skipped, as appropriate for a branch dispatch.
- A fresh local Docker build and `docker/smoke.sh` passed readiness,
  same-origin identity, client custody and clean termination. The image
  was `rahi:bootstrap-0ef9dca`. Compose configuration validation passed.
- Locked dependency trees confirmed the downward crate boundaries for
  store, ledger, kernel and edge; edge had no idp dependency.

These proofs supplement the local per-spec pass. Neither these workflows
nor green ordinary CI substitute for the recorded 29-spec pass.

### Remaining blockers to reliance

- **001 AC-4:** the audited main branch protection had
  `required_linear_history.enabled=false`. The required `ci-gate`, signed
  commits, administrator enforcement, and force-push/deletion restrictions
  matched the other checked requirements, but AC-4's linear-history
  requirement did not. Its local 57-command plan does not test that remote
  setting. This operator criterion remains failed until the governed
  branch policy is corrected and witnessed; this application does not
  change that shared policy or claim AC-4 satisfied.
- **039 release checkpoints:** this pass did not publish a release, pull
  newly published artifacts anonymously, or prove a fresh crates.io
  consumer resolution. AC-2, AC-3 and AC-4 retain their named-release
  operator evidence requirements. Historical release records are not a
  fresh publication verdict for this commit. No new publication or
  published-only consumer acceptance is claimed.
- **S3 and independent verification:** no fresh S3-backed proof or
  independent attest-ledger result was obtained. Where a plan explicitly
  permits their absence, its command verdict is still 0; any reliance that
  requires those external proofs remains blocked until they are supplied.
- **N=3:** manifest validation is not three-node qualification. Spec 032's
  recorded operator procedure remains available, but neither N=3 layout
  is newly qualified by this application. Spec 044's P-1 through P-4,
  coherent export, prerequisites involving 040, 041 and 042, and the
  032 AC-2 obligation remain owed. Its AC-3 and AC-4 still require a named
  release and real operator evidence. It remains draft and pending.

The anchor permits the two named N=3 layouts. Permission, merging this
amendment, historical completion labels, and ordinary CI are not N=3
support or clearance of missing acceptance evidence. Passing results above
may be relied on only within their recorded scope; each failed or missing
required criterion remains a blocker to the affected reliance. Any
remediation has its own scope and checks, without relaxing the anchor.

### Evidence locations and integrity

The durable local archive is
`/Users/bart/DevWork/rahi/data/evidence/bootstrap-owner-application/2026-10-02-0ef9dca/`.
It is ignored evidence, not a committed source directory. Per-spec logs and
plans use the filenames specified above. `verification-manifest.json`
records their hashes and outcomes; its SHA-256 is
`05b40978ad92be2aaa9809a70cbc92a4bae94fbac92b61d383753fbb170283d7`.

`frozen-registry.json`, `frozen-spec-set.json`, `engine.txt`,
`prerequisites.json`, `normative-gate.log`, `make-ci.log`,
`frozen-main-protection.json`, `dependency-acceptance.json`,
`frozen-live-workflow.log`, `frozen-image-workflow.log`,
`frozen-docker-build.log`, `frozen-docker-smoke.log` and
`frozen-compose-config.log` preserve the supporting evidence. The workflow
URLs above provide the corresponding durable hosted results.
