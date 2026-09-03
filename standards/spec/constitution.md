# rahi constitution

Durable principles that govern this corpus. This document is **tier 2**: it is
subordinate to the bootstrap spec (`specs/000-rahi-bootstrap/spec.md`), whose
`unamendable` anchors it may not contradict, and it governs all ordinary specs
(`001`+).

**Normative hierarchy (highest wins):**

1. `specs/000-rahi-bootstrap/spec.md`: the bootstrap spec. Non-overridable.
2. `standards/spec/constitution.md`: this document.
3. `standards/spec/contract.md`: a normative summary of the bootstrap spec.
4. `specs/002-chassis-thesis/spec.md`: the architectural thesis. It owns no
   code; it fixes the responsibilities, the crate topology, and the build
   order every other spec operates inside.
5. Ordinary specs (`010`+): feature-level claims within this envelope.

When two specs conflict, resolve in this order, then by the typed authority
graph.

The first five principles are spec-spine's own and apply to the corpus. The
principles from VI onward are rahi's and apply to the system the corpus
describes. Both halves bind every spec and every build session equally.

---

## I. Markdown-only authored truth

Authored truth about the corpus lives only in markdown with YAML frontmatter.
No hand-authored JSON, YAML, or TOML data file governs the corpus. *(Bootstrap
anchor: `markdown-truth-boundary`.)* An application's manifest (spec 015) is
application configuration, not corpus truth; it is governed by the spec that
owns it like any other source file.

## II. Compiler-owned JSON machine truth

All machine-consumable truth about the corpus is emitted by `spec-spine` into
`.derived/` and is read only through `spec-spine` subcommands. Hand-editing a
derived artifact is a workflow violation; ad-hoc parsing of one is equally
forbidden. *(Bootstrap anchor: `json-truth-boundary`.)*

## III. Spec-first development

A change to behavior begins with a change to a spec. The spec defines the
territory (the units it owns) and the relationships (the typed edges) before
the code is written. The coupling gate enforces this at PR time. The escape
valve is a named, scoped waiver in the PR body, never a silent edit to an
owner spec. The ownership ratchet is on: every source file inside a crate
must be specifically claimed by a spec, so a build session that adds a file
adds it to the `establishes` list of the spec it is implementing, in the
same change.

## IV. Determinism and validation

Every artifact-producing function in the corpus toolchain is a pure function
of `(config, file contents)`. Validation is mechanical. *(Bootstrap anchor:
`determinism-requirement`.)*

## V. Legacy as evidence

Code that predates a governing spec is evidence, not a violation. rahi is
greenfield, so only the bootstrap spec carries `origin.retroactive: true`;
every ordinary spec is a forward claim. Decisions carried from enrahitu are
cited by `enrahitu://` provenance URIs and re-stated in the rahi spec that
owns them; the citation is context, never authority.

---

## VI. One deployment unit

A governed cell is one container, one volume, one public origin, one
command. rauthy is co-deployed inside that unit and reached only through the
app's origin. N=1 is the primary mode and pays nothing for the existence of
N=3. A spec that adds a sidecar, a second exposed port, a managed database,
or a cloud prerequisite must justify it in its Purpose section. *(Bootstrap
anchor: `one-deployment-unit`.)*

## VII. The IdP is the principal authority

rauthy owns authentication, principal identity, session validity, rotation,
revocation, MFA, and account lifecycle. The app keeps the shell (same-origin
httpOnly cookies, CSRF, rate limits, the audit trail) and gives up the
authority. rauthy's `sub` is the only principal identifier; no local account
row is ever written; roles and `email_verified` are re-read on every renewal.
A second opinion about a question the IdP already answers is a place for the
two answers to differ. *(Bootstrap anchor: `idp-authority`.)*

## VIII. Two stores, never shared

rauthy's hiqlite and the app's hiqlite are separate Raft clusters with
separate data directories, ports, keys, and backup schedules. App code never
opens, lists, or backs up rauthy's storage; rauthy's store is captured through
rauthy. A shared volume between Raft nodes is corruption, not a
simplification. *(Bootstrap anchor: `store-separation`.)*

## IX. One transaction is the atomic unit

A resource write and its outbox row commit in one `txn`. SQL and notify land
in different Raft groups and are never atomic together: notify is a hint and
the revision column is truth, so every consumer polls a watermark and is
idempotent. The cache group is derived: nothing durable lives there.
Migrations are a deploy step, never a boot step. *(Bootstrap anchor:
`txn-atomicity`.)*

## X. Deny by default

The manifest declares a capability ceiling. The build verifies that observed
usage is a subset of the declaration and refuses to build otherwise. The
kernel enforces the ceiling at runtime and ledgers every denial. Absence is
never permission. *(Bootstrap anchor: `deny-by-default`.)*

## XI. The chain is linear, verified at boot, and fails closed

The decision ledger is a hash-linked chain whose append is a compare-and-swap
on a unique parent index inside one transaction. The full chain is verified
at every boot; an integrity failure on the init path stops the process. A
cell serving requests under a broken audit proof is worse than a cell that is
down. Sealed segments are immutable and verifiable without archived history
resident. *(Bootstrap anchor: `chain-linear`.)*

## XII. Backup is one artifact with its keys

Both stores are encrypted at rest with keys custodied once per deployment.
A backup that separates the stores from their keys is worthless, so the
backup verb produces one artifact and the restore verb refuses partial
input. Restore is a cluster reset, single-shot by construction, never a
setting that re-applies on every restart.

## XIII. Observability is not optional

Every cell serves `/healthz` (liveness, touches no dependency), `/readyz`
(readiness, checks the store and the ledger), and `/metrics` (Prometheus
text, always on, kept off the public ingress by deployment). An OTel tracer
runs in-process with a bounded ring buffer whether or not an exporter is
configured. Kernel decisions are correlated to spans by id.

## XIV. Libraries extend downward; apps compose

Crates depend downward only: types, store, ledger and kernel, idp and edge,
ops and cli. The chassis never depends on an app. An app is a binary that
composes the crates, declares a manifest, and pins a chassis version; it
extends the chassis and never forks it. There is no template, no stamp, and
no upgrade mechanism beyond a version bump. *(Bootstrap anchor:
`layer-direction`.)*

---

## Amendment

This constitution may be amended by an ordinary spec that `amends` it and is
approved, **provided** the amendment does not contradict a `specs/000`
`unamendable` anchor. The bootstrap spec's freeze surface is the hard
boundary; everything else in this document is revisable through the normal
governed flow.
