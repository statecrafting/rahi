# Owner instrument: re-founding the `one-deployment-unit` anchor

Prepared 2026-09-23 for the rahi owner. **Not approved. Nothing here is
written into `specs/000-rahi-bootstrap/spec.md` or
`standards/spec/constitution.md`.** This document is the complete text of a
proposed bootstrap-tier act, so the owner can approve, amend or reject its
exact wording. Its approval is a separate act from its preparation, and
approving it does not approve spec 044.

## 1. Why an instrument, and why only the owner can issue it

Spec 000 lists `one-deployment-unit` in its `unamendable` set and states it
in section 6: "A governed cell is one container, one volume, one public
origin; rauthy is inside it and reached only through the app's origin."
Constitution VI restates it.

Spec 044 (draft, PR #76, head `ecd28cc`) implements the hiqlite owner
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
owner's recorded act is.

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
`docs/design/06-owner-decision-packet-2026-09-23.md` (on the 043 branch). That
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
>   an anchor. Every spec is re-verified under the plan the instrument
>   names before it is next relied on.

### 2.3 Constitution VI (for spec 044, not this instrument)

Once the anchor reads as in 2.1, Constitution VI no longer states it
correctly, and an ordinary spec may amend VI because the amendment would no
longer contradict the anchor. The proposed text, for 044 to carry through
the ordinary flow (`amends` on the constitution) and not written by this
instrument:

> A governed cell is one public origin and one command, in one of the
> layouts `specs/000` section 6 names. rauthy is reached by users only
> through the app's origin. N=1 is the primary mode and pays nothing for
> the existence of N=3. A spec that adds a sidecar, a second exposed port, a
> managed database, or a cloud prerequisite must justify it in its Purpose
> section. *(Bootstrap anchor: `one-deployment-unit`.)*

## 3. Impact

| area | effect |
|---|---|
| N=1 (031, 034, 043) | None. The N=1 sentence says what the current text says. 043's cell satisfies both texts. |
| 032 co-located N=3 | Stays governed (first N=3 layout). Its manifests, which refuse a shared volume and require one container per pod, satisfy the new text. 043 D-2 still labels `deploy/n3` unqualified; that is a qualification statement, unchanged. |
| 044 split N=3 | Becomes approvable through the ordinary flow, subject to its own open choices (TLS for the internal Service, the public path, bootstrap ordering, coherent export, N=3 qualification). The new text fixes two of them as requirements: users reach rauthy only through the app's origin, and the internal Service is encrypted and admits only the app's pods. |
| `store-separation` | Untouched, and restated in the new text. |
| `idp-authority`, `layer-direction` and the other anchors | Untouched. |
| Constitution VI | Stale until 044 amends it (2.3). Until then the anchor governs. |
| Statecraft | Its rule that Rauthy is never a separate workload (044 P-4) is Statecraft's to amend. This instrument neither amends it nor depends on it. |
| Precedent | None, by the entry's own words. |

## 4. Bounded re-verification plan

Spec 000's conventions say an amendment invalidates every transitive
dependent until re-verification. Every ordinary spec depends on 000, so
every `implementation: complete` spec is re-verified once, as follows, and
nothing larger is run to prepare or approve the instrument.

- **Set.** The 26 specs at `implementation: complete` on `main` at `b815b18`:
  001, 010 to 016, 020 to 026, 030 to 039, 042. The set is re-read from
  `spec-spine registry` on the amendment's commit; a spec completed later is
  added.
- **Method.** On the commit that carries the amendment, in CI: `make gate`
  with the pinned spec-spine, then `make verify SPEC=<id>` for each spec in
  the set, each spec's declared `verify:cli` block and nothing else. The
  live workflow (037) runs as it does for any change; the plan adds no live
  leg and no N=3 run.
- **Bound.** One pass. A failure is reported with its output against the
  spec that failed and is not fixed inside the instrument's change; the
  anchor text does not change any code path, so a failure is evidence of
  something else and is handled as its own change.
- **Record.** The run ids and per-spec exit codes are appended to this
  document, and the `## Amendments received` entry cites them.

## 5. What is not decided here

044's own choices (section 6 of the 043 branch's owner packet): the
encryption mechanism for the internal Service, Rauthy's public path, the
first bootstrap ordering, coherent export, and N=3 qualification. N=3 is not
marked supported by this instrument, and 044 is not implemented before its
own approval and prerequisites.

## 6. Approval text

```
Owner act, <date>, at the bootstrap tier:

I approve docs/design/08-owner-instrument-one-deployment-unit.md as committed
at <commit> (blob <blob>). Replace the one-deployment-unit bullet of
specs/000-rahi-bootstrap/spec.md section 6 with the text of its section 2.1,
add the Amendments received entry of its section 2.2 with today's date, and
run the re-verification of its section 4. This is my act and not a
precedent. It does not approve spec 044.
```
