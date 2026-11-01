---
id: "050-split-cell-coherent-export-and-archive-identity"
title: "Split cell coherent export and archive identity: an offline export across both clusters, and one archive manifest naming both images and both applied log ids"
status: draft
kind: feature
domain: ops
created: "2026-10-31"
authors: ["Bartek Kus"]
implementation: pending
risk: high
wave: 3
depends_on:
  - "030-operational-verbs"
  - "044-n3-split-composition"
summary: >
  Takes over spec 044's B-11 (coherent offline export across the rahi and
  Rauthy clusters of a split N=3 cell) and B-12 (the cell archive names both
  images, both applied log ids and both digests in one manifest), which the
  owner split out on 2026-10-31 so 044 could complete on its operator
  evidence. Both need hiqlite fork changes that no release carries yet: the
  offline export entry point, and an applied log id recorded with each
  backup. This spec schedules nothing until a hiqlite release carrying both
  is adopted.
establishes:
  - "crates/rahi-cli/tests/split_archive.rs"
references:
  - { unit: { kind: file, path: "docs/design/02-operational-prerequisites.md" }, role: context }
  - { unit: { kind: file, path: "crates/rahi-ops/src/archive.rs" }, role: context }
  - { unit: { kind: file, path: "crates/rahi-ops/src/backup.rs" }, role: context }
  - { unit: { kind: directory, path: "deploy/n3-split" }, role: context }
---

# 050: Split cell coherent export and archive identity

## 1. Purpose

Spec 044 built the split N=3 cell: a rahi StatefulSet and a Rauthy
StatefulSet, each its own hiqlite Raft cluster. Two of its obligations
could not be met by anything rahi controls. B-11 needs hiqlite's offline
export entry point, defined by B-2 of the external hiqlite proposal 034 in
`bartekus/hiqlite`, which no published release carries (044 D-1 approved
it held). B-12 needs Rauthy's applied log id, which 044 D-12 measured as
unreadable from rahi: Rauthy's backup writes `StateMachineData::default()`
into the copy's metadata, no Rauthy route returns its raft metrics, and
Rauthy's hiqlite API port admits only Rauthy's pods.

The owner decided on 2026-10-31 to move both here, so that 044 completes on
the evidence it can produce and these two wait visibly on their upstream
prerequisite instead of holding 044 `in-progress`. The text of B-1 and B-2
below is 044's B-11 and B-12, carried without change of requirement.

## 2. Territory

- `crates/rahi-ops/src/archive.rs` (030's): the manifest gains the second
  cluster's image, applied log id and digest.
- `crates/rahi-ops/src/backup.rs` (030's): a split cell's backup reads
  both clusters' applied log ids and refuses to write a manifest that
  names only one.
- `deploy/n3-split/` (044's): the documented coherent export procedure.
- `crates/rahi-cli/tests/split_archive.rs` (new): the end-to-end proof.

## 3. Behavior

- **B-1 (coherent export; was 044 B-11).** An export that must be coherent
  across both clusters (a migration from a split cell, an offline DR
  archive) MUST scale both StatefulSets to 0, wait until every pod of both
  has terminated and released its `hiqlite-owner.lock`, and only then
  export both volumes offline, through the offline export entry point of
  hiqlite proposal 034 B-2.
- **B-2 (archive identity; was 044 B-12).** The cell archive MUST name both
  images, both applied log ids and both digests in one manifest (hiqlite
  proposal D-7, R-g).
- **B-3 (no partial identity).** A backup that cannot read either
  cluster's applied log id MUST refuse to write the archive, rather than
  write a manifest naming one cluster beside nothing for the other (044
  D-12: such a manifest reads as coherent when it is not).

## 4. Functional requirements

- **FR-001.** The archive manifest carries, per cluster (`rahi`,
  `rauthy`), the image reference, the applied log id and the digest of
  that cluster's backup part. An N=1 archive keeps its current shape.
- **FR-002.** The applied log id of Rauthy's cluster is read from the
  backup artifact hiqlite produces (the fork change this spec waits on),
  not from a route on Rauthy's hiqlite API port, which B-3 of 044 keeps
  closed to rahi.
- **FR-003.** `deploy/n3-split` documents B-1's procedure, naming the
  hiqlite release that provides the export entry point.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p rahi-cli --locked --test split_archive` passes:
  a split cell's backup against a real patched Rauthy writes a manifest
  naming both images, both applied log ids and both digests, and a backup
  whose Rauthy part carries no applied log id is refused (B-3).
- **AC-2 (operator check, never `verify:cli`).** On a real three-node
  cluster and a named release, B-1's export runs end to end and the two
  exported volumes restore into a cell whose chains verify.

## 6. Out of scope

The hiqlite fork changes themselves (the offline export entry point and
the applied log id recorded with each backup), which belong to
`bartekus/hiqlite` and its release; the N=1-to-N=3 migration protocol
(hiqlite proposal section 7); any dependency version change, which is its
own adoption under 043's rules.

## 7. Resolved decisions

- **D-1 (2026-10-31, owner decision; the split from 044).** The owner chose
  to move 044's B-11 and B-12 into a follow-up spec so 044 can complete on
  AC-3 and AC-4. This spec is born `draft` and stays unschedulable until a
  hiqlite release carrying both prerequisites exists; approval is a human
  flip.
- **D-2 (2026-10-31; a draft claims no unit).** The three `extends` edges
  this spec was born with (030's `archive.rs` and `backup.rs`, 044's
  `deploy/n3-split/`) made it an owner of those units while `draft`, and
  the ratification gate refuses any change to a unit a draft spec owns: it
  blocked 040's edge closure and 044's completion, neither of which is this
  spec's work. They are `references` edges until approval; the approving
  change turns them back into the `extends` edges section 2 describes.

## Verification

```verify:cli
cargo test -p rahi-cli --locked --test split_archive
```
