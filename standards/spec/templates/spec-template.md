---
id: "NNN-slug"                 # MUST equal the directory name; NNN = unique 3-digit ordinal = build order
title: "Short imperative title"
status: draft                  # draft | approved | superseded | retired; only approved specs schedule
kind: kernel                   # constitutional-bootstrap | thesis | governance | kernel | feature | tooling
domain: store                  # governance | types | store | ledger | kernel | identity | edge | ops
created: "YYYY-MM-DD"
authors: ["Bartek Kus"]
implementation: pending        # pending | in-progress | complete | n-a | deferred
risk: medium                   # low | medium | high | critical (critical = a chassis invariant or key material)
wave: 1                        # build-order wave, 1..3 (spec 002 §5)
depends_on:
  - "NNN-lower-numbered"       # every dependency is lower-numbered; the graph is a DAG
summary: >
  One short paragraph: what territory this spec claims and why it exists.
# --- typed edges (declare territory + relationships) ---
# establishes:
#   - "crates/rahi-x/Cargo.toml"                      # a crate-founding spec claims its manifest
#   - "crates/rahi-x/src/lib.rs"
#   - "crates/rahi-x/src/thing.rs"                    # every source file, explicitly
#   - "crates/rahi-x/tests/thing.rs"
#   - "crates/rahi-x/testdata/thing/"                 # fixtures as a subtree
# extends:
#   - { spec: "NNN-founder", unit: "crates/rahi-x/src/lib.rs", nature: additive }           # re-exports
#   - { spec: "NNN-founder", unit: "crates/rahi-x/Cargo.toml", nature: additive }            # new deps
#   - { spec: "010-workspace-and-core-types", unit: { kind: section, file: "Cargo.toml", anchor: "workspace.dependencies" }, nature: additive }
# constrains:
#   - { flavor: invariant-freeze, unit: "crates/rahi-store/src/txn.rs", note: "one txn is the atomic unit" }
# references:
#   - { unit: { kind: file, path: "docs/design/00-lineage.md" }, role: context }
---

# NNN: Title

## 1. Purpose

What problem this spec solves, and which thesis section (spec 002) or
constitutional principle it serves. One or two paragraphs. Cite the enrahitu
decision it carries, if any, by `enrahitu://NNN`.

## 2. Territory

The units this spec claims, in prose (mirrors the frontmatter). Name the
crate, the modules, and the seams it exposes to later specs.

## 3. Behavior

- **B-1 (name).** What the governed code MUST do. Use MUST/SHOULD/MAY.
- **B-2 (name).** ...

## 4. Functional requirements

- **FR-001.** Testable requirement, including the seams (traits, injected
  clocks and stores) that keep the core testable without a cluster.
- **FR-002.** ...

## 5. Acceptance criteria

- **AC-1.** `cargo test -p rahi-x --locked` passes.
- **AC-2.** A concrete observable outcome against a fixture.

## 6. Out of scope

What this spec deliberately does not cover, and which later spec covers it.

## 7. Resolved decisions

None yet. The build session records D-n entries here for choices this spec
is silent on (date, provenance, the decision, the alternative rejected).

## Verification

```verify:cli
cargo test -p rahi-x --locked
```
