---
id: "000-rahi-bootstrap"
title: "Bootstrap spec system for rahi (specify first, build by spec)"
status: approved
kind: "constitutional-bootstrap"
domain: "governance"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: n-a
risk: critical
wave: 1
origin:
  retroactive: true   # authority held since before the graph existed
unamendable:
  - "markdown-truth-boundary"
  - "json-truth-boundary"
  - "determinism-requirement"
  - "directory-name-equals-id"
  - "typed-authority-graph"
  - "refusal-rule"
  - "one-deployment-unit"
  - "idp-authority"
  - "store-separation"
  - "txn-atomicity"
  - "deny-by-default"
  - "chain-linear"
  - "layer-direction"
summary: >
  Foundational contract for the rahi corpus. Authored truth lives only in
  markdown with YAML frontmatter; machine-consumable truth about the corpus is
  compiler-emitted JSON read only through spec-spine; every artifact is a
  deterministic function of (config, file contents); and a typed authority
  graph governs who owns what. rahi is specified in full before a line of it
  is built: the corpus is the design, spec numbers are the build order, and an
  orchestrator drives one spec per fresh session through build, ship,
  shepherd, and verify. This spec also freezes the seven chassis invariants
  (constitution VI through XI and XIV) that no later spec may amend.
---

# 000: Bootstrap spec system for rahi

This is the spec that defines what a spec *is* for rahi. It sits at the top
of the constitutional hierarchy (`standards/spec/constitution.md` is
subordinate to it). It was authored by hand on 2026-09-03, before any code
existed, together with the whole corpus it governs; the code is built to
satisfy the corpus, one spec per driven session, and the coupling gate holds
from the first governed commit. The corpus is a rebuild of the enrahitu
chassis in Rust; `docs/design/00-lineage.md` records what was carried and
what was cut.

## 1. The authoring / derived boundary

There are exactly two kinds of truth in this repository.

- **Authored truth** lives only in markdown (`specs/NNN-slug/spec.md`,
  `standards/`), with YAML frontmatter. Humans, and agents holding explicit
  authority, write authored truth. *(anchor: `markdown-truth-boundary`)*
- **Machine-consumable truth** about the corpus is emitted only by
  `spec-spine`, as JSON, into `.derived/`. No hand-authored JSON is
  authoritative; compiled JSON is read only through `spec-spine` subcommands,
  never by `jq`, `grep`, or a hand-rolled reader. *(anchor:
  `json-truth-boundary`)*

The derived shard trees are committed. `build-meta.json` (the only wall-clock
artifact) is gitignored.

## 2. Identity: directory name equals id

A spec's directory under `specs/` is named exactly `NNN-slug`; its `id`
equals that name; `NNN` is unique across the corpus. In this corpus `NNN` is
also the build order: a spec's `depends_on` names only lower-numbered specs,
so the orchestrator's "lowest-numbered ready spec" rule reproduces the
thesis's build order (spec 002 §5) without a second scheduling table.
*(anchor: `directory-name-equals-id`)*

## 3. The typed authority graph

Specs declare typed edges (`establishes`, `extends`, `refines`,
`supersedes`, `amends`, `co_authority`, `constrains`, `references`) and the
units they own (`file`, `section`, `symbol`, `directory`, `crate`, `module`).
Authority is derived by walking the graph. `references` is non-owning.
*(anchor: `typed-authority-graph`)*

Corpus rules on top of the grammar:

- A crate-founding spec `establishes` the crate's `Cargo.toml`, `src/lib.rs`,
  and each of its own source files explicitly, plus its `tests/` and
  `testdata/` subtrees.
- A later spec that adds modules to an existing crate `establishes` its own
  files and `extends` the founder's `src/lib.rs` (re-exports) and, when it
  adds a dependency, the founder's `Cargo.toml` and the workspace manifest's
  `workspace.dependencies` section (spec 010).
- `[coupling] require_ownership` is on. Every source file inside a crate MUST
  be specifically claimed. A build session that adds a file adds it to the
  `establishes` list of the spec it is implementing, in the same change.
- The manifest floor (`[package.metadata.spec-spine].spec`) names the
  crate-founding spec and exists for drift, not for coverage.

## 4. Determinism

Every artifact-producing step of the corpus toolchain is a pure function of
`(config, file contents)`. *(anchor: `determinism-requirement`)*

## 5. The refusal rule

If the coupling gate fails because code and its owning spec disagree, no
agent resolves it by editing the spec to match the code it just wrote. The
contradiction is surfaced to a human, or to an agent with explicit authority
recorded in the spec's Territory section. *(anchor: `refusal-rule`)*

## 6. The frozen chassis invariants

The following invariants of the system rahi describes are frozen here, at
tier 1, so that no ordinary spec and no amendment to the constitution can
weaken them. Each is stated in full in the constitution; the anchor is the
freeze.

- A governed cell is one container, one volume, one public origin; rauthy
  is inside it and reached only through the app's origin. *(anchor:
  `one-deployment-unit`)*
- rauthy's `sub` is the only principal identifier; no local account row;
  session validity, rotation, and revocation are the IdP's. *(anchor:
  `idp-authority`)*
- rauthy's hiqlite and the app's hiqlite are separate Raft clusters with
  separate storage that app code never opens. *(anchor: `store-separation`)*
- A write and its outbox row commit in one `txn`; notify is a hint and the
  revision column is truth; nothing durable lives in the cache group.
  *(anchor: `txn-atomicity`)*
- The manifest is a verified ceiling; the kernel denies by default and
  ledgers every denial. *(anchor: `deny-by-default`)*
- The decision chain is linear by CAS append, verified at boot, and fails
  closed. *(anchor: `chain-linear`)*
- Crates depend downward only; the chassis never depends on an app; apps
  compose and never fork. *(anchor: `layer-direction`)*

## 7. Corpus conventions

- **Frontmatter.** Every ordinary spec carries `kind`, `domain` (its
  responsibility), `implementation`, `risk`, `authors`, `wave`, and a
  non-empty `depends_on`. `domain` and `kind` are closed enums
  (`spec-spine.toml`).
- **Body.** Sections in order: Purpose, Territory, Behavior (B-n with
  MUST/SHOULD/MAY), Functional requirements (FR-nnn), Acceptance criteria
  (AC-n), Out of scope, Resolved decisions (D-n), and an unnumbered
  `## Verification` section holding `verify:cli` fenced blocks (one shell
  command per line, run after merge by the verify stage). A spec for code
  with no observable command records that explicitly in Verification rather
  than omitting the section.
- **Decisions.** Where a spec is silent, the build session records a dated
  D-n entry under Resolved decisions (and drops a copy in the orchestrator's
  decision drop-box when driven). Decisions are appended, never rewritten; a
  later decision supersedes by naming the earlier one.
- **Amendments.** A change to a shipped spec's contract is an `## Amendments
  received` entry with a date and provenance, and it invalidates every
  transitive dependent until re-verification.
- **Provenance.** A decision carried from enrahitu cites its source as
  `enrahitu://NNN` in the Purpose section. The citation is context; the rahi
  spec is the authority.
- **Style.** No em dash character anywhere in authored text; conventional
  commit messages referencing the spec id; no AI attribution and no session
  links in anything that lands in git or on GitHub.

## 8. Lifecycle as scheduling

- `status: approved` + `implementation: pending` is a work order.
- `status: draft` is never schedulable and stays visible as a blocker.
  Approval is the operator's act; a machine-authored spec is born draft.
- `implementation: n-a` (this spec, the thesis) and `complete` (the
  harness) count as shipped, pinned at the sha256 of the spec's normalized
  `spec.md`.
- `depends_on` MUST be acyclic. A cycle refuses scheduling for the whole
  corpus; the gate does not catch it, so the `/spec` skill and the
  `spec-dag` check in `make spine` do.

## 9. Bootstrap order

1. This spec, the constitution, the thesis (002), and the harness (001) are
   authored by hand, together with every ordinary spec of the corpus.
2. `spec-spine compile`, `index`, `lint --fail-on-warn`, and `couple` are
   green with zero packages discovered and every owning unit reported as
   `W-001` (declared, not yet built). That is the honest starting state.
3. Spec 010 creates the Cargo workspace and the first crate. From then on
   each driven session implements exactly one spec's territory, and the
   corpus governs the code it produced.
