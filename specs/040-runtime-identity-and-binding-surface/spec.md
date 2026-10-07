---
id: "040-runtime-identity-and-binding-surface"
title: "Runtime identity: a replica states which bits it runs, which ceiling it enforces, and which instance it is, with every value's basis named, on bounded surfaces"
status: approved
kind: feature
domain: edge
created: "2026-09-11"
authors: ["Bartek Kus"]
implementation: in-progress
risk: medium
wave: 3
depends_on:
  - "015-kernel-manifest-and-adjudication"
  - "020-edge-server"
  - "023-observability"
  - "030-operational-verbs"
  - "031-single-container-packaging"
  - "032-cluster-topology"
establishes:
  - "crates/rahi-ops/src/binding.rs"
  - "crates/rahi-edge/src/binding.rs"
  - "crates/rahi-cli/tests/binding.rs"
extends:
  - { spec: "020-edge-server", unit: "crates/rahi-edge/src/lib.rs", nature: additive }
  - { spec: "020-edge-server", unit: "crates/rahi-edge/src/router.rs", nature: additive }
  - { spec: "023-observability", unit: "crates/rahi-edge/src/obs/tracer.rs", nature: additive }
  - { spec: "023-observability", unit: "crates/rahi-edge/src/obs/metrics.rs", nature: additive }
  - { spec: "023-observability", unit: "crates/rahi-edge/src/obs/mod.rs", nature: additive }
  - { spec: "023-observability", unit: "crates/rahi-edge/tests/obs.rs", nature: additive }
  - { spec: "023-observability", unit: "crates/rahi-edge/src/obs/layer.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-ops/src/lib.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/src/lib.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/src/cell.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/src/verbs.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/src/serve.rs", nature: additive }
  - { spec: "031-single-container-packaging", unit: "docker/Dockerfile", nature: additive }
  - { spec: "031-single-container-packaging", unit: ".github/workflows/image.yml", nature: additive }
  - { spec: "039-release-and-out-of-tree-packaging", unit: "docker/runtime.Dockerfile", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/Cargo.toml", nature: additive }
  - { spec: "032-cluster-topology", unit: "deploy/k8s/statefulset.yaml", nature: additive }
  - { spec: "032-cluster-topology", unit: "deploy/k8s/ingress.yaml", nature: additive }
  - { spec: "032-cluster-topology", unit: "scripts/k8s-validate.sh", nature: additive }
  - { spec: "032-cluster-topology", unit: "deploy/README.md", nature: additive }
references:
  - { unit: { kind: file, path: "docs/design/01-consumer-contract.md" }, role: context }
  - { unit: { kind: file, path: "docs/design/02-operational-prerequisites.md" }, role: context }
summary: >
  A consumer that correlates an immutable artifact, an authority snapshot,
  a deployment, and a running replica needs the replica to say what it is.
  Today it says almost nothing: rahi version prints the crate version, the
  manifest hash is visible only inside a ledger export or a backup, the
  OTel resource carries service.name alone, the image has no labels, and
  the deploy manifests pin no digest. This spec makes a replica report its
  measured executable digest, its booted manifest hash, its declared image
  and application revision, and a per-boot minted instance id, each with
  its basis (measured, declared, minted, or absent with a reason from a
  closed set), on one unguarded document at /binding kept off the ingress
  like /metrics, on the OTel resource, and in one build-info metric whose
  labels are versions only. Section 3.1 fixes the document normatively:
  one schema name, one path for every value, one wrapper shape, and an
  additive evolution rule. The document is boot-bound and immutable for
  the life of the process, so it is never a freshness proof and never an
  authorization. It states what the telemetry does not observe or count.
  It writes nothing to the chain; deployment epochs are spec 041.
  This contract, ratified on 2026-10-05 (D-3), is reconciled to released
  Rahi 0.4.0, including
  named migration sets and the exact patched component identities and limits
  carried by spec 043.
---

# 040: Runtime identity and the binding surface

> **Status (2026-10-07).** Implementation stays `in-progress`. #109 and
> #111 carry the binding document, route, resource, metric, deployment
> checks, `Cell::app_revision` and `rahi version --binding`; `make verify
> SPEC=040` passes on `main` at `7123d62`. What remains is AC-6 (FR-010):
> the image workflow change in PR #110, which needs the owner's workflow
> exception, and the first push build after it merges, which is the only
> run its label check executes on.

## 1. Purpose

The family evidence chain proposed on 2026-09-11 (handoff 02-rahi, the
September 11 amendment; realignment register B08 to B12) asks that a
control plane correlate four things without a hash cycle: an immutable
artifact, the accepted authority snapshot it was built under, a deployment
and its epoch, and a runtime instance. rahi's share is narrow. It supplies
reliable identity and metadata from a running replica and keeps the
manifest as the ceiling; it does not collect telemetry, evaluate runtime
assertions, score conformance, or store a proof graph. Those are the
consumer's (Statecraft's).

The original evidence below was verified at `444bcf8` on 2026-09-11. The
contract was reconciled at `e5c25c3` on 2026-09-26 against released Rahi
0.4.0 and the complete implementations of 035 through 039 and 042:

- `rahi version` still prints `rahi <CARGO_PKG_VERSION>` and nothing else
  (`crates/rahi-cli/src/lib.rs`, `Verb::Version`). No crate has a
  `build.rs`, no revision is embedded, no route answers a version, and no
  metric carries build information.
- The manifest hash is computed at boot (015 B-2) and an operator can see
  it only as the genesis record's id and payload in `ledger export`, or as
  `manifest_hash` in a backup's `manifest.json`. `/readyz` answers
  `{status, store, ledger}`; preflight reports the ledger verified without
  the hash; no boot line prints it. `[contract] version` is validated and
  read by nothing else.
- The OTel resource carries `service.name` only
  (`crates/rahi-edge/src/obs/tracer.rs`). Spans export through the SDK's
  batch processor with its default sampler, which keeps every span; a full
  batch queue drops spans and the chassis counts none of them.
- A replica's hiqlite node id is its pod ordinal plus one
  (`RAHI_POD_INDEX`, `crates/rahi-ops/src/rauthy_env.rs`). Nothing reads a
  pod name, and nothing names a process incarnation.
- Rahi 0.4.0 uses the registry package `hiqlite-patched`
  `0.15.0-patched.3` and packages
  `ghcr.io/bartekus/rauthy-patched:0.36.2-patched.3` at digest
  `sha256:d75cac0f708f3e238c458b622fea2f0b7dda9b67e9435eeafa37698d88a2a3c8`.
  The app store is rooted at `<data_dir>/app-store`; the Rauthy store is a
  separate component and is not inspected as an app-store identity.
- `docker/Dockerfile` still has no OCI identity `LABEL`; `image.yml` records
  no provenance and no SBOM; `deploy/k8s` names the image with no tag and no
  digest.
- Every metric label is a closed vocabulary (023 B-1, D-3).

Spec 043 shipped its owner-approved 0.4.0 scope but remains honestly
`implementation: in-progress` on the limits B-14 records. Those limits are
not promoted into success claims by this identity surface. Spec 042 is
complete and fixes ledger lifetime identity, but this spec neither reads nor
writes ledger lifetime records. The old decision to wait for pilot
prerequisites is discharged: 035 through 039 and 042 are complete, so this
contract was presented for owner ratification and ratified on 2026-10-05
(D-3).

Constitution XIII makes observability part of the contract. This spec
extends it from "what happened" to "what is running", with the same rule
023 applies to labels: nothing unbounded.

**What this document is, and is not.** It is one process's statement about
its own boot: the bytes it measured, the values it was handed, the
incarnation it minted. It is assembled once, before the listener opens,
and never changes while the process lives. That makes it a stable join key
and a poor clock. It cannot report the newest deployment, it cannot be
polled for freshness, and nothing in it authorizes anything (B-6, B-8).
A consumer that needs current deployment state reads the chain, which is
spec 041's territory and is not specified here.

## 2. Territory

- `crates/rahi-ops/src/binding.rs` (new): self-measurement and the binding
  document, assembled once at boot; the serializer that section 3.1 fixes.
- `crates/rahi-edge/src/binding.rs` (new): the `/binding` route, which
  serves the bytes the composer hands it and names no ops type, so the
  edge keeps its dependency direction (thesis section 4).
- `crates/rahi-cli/tests/binding.rs` (new): the end-to-end proof, and the
  conformance test for section 3.1's three examples.
- Additive changes to the edge's module list and router (020), the tracer,
  metrics, observation options, and their test (023), the ops module list,
  the version verb, and the serve composition (030), the image recipe and
  workflow (031), and the StatefulSet, Ingress, validation script, and
  deploy notes (032).

No fixture file is established here. Section 3.1's examples are normative
as written in this spec; the machine-readable `testdata/binding/` set is
claimed by 041 FR-003, which composes it with the producers 040 has none
of.

## 3. Behavior

- **B-1 (every value has a basis).** Each identity value the chassis
  reports MUST carry exactly one basis:
  - `measured`: this process observed it directly from bytes or a system
    source, and the document names the source (B-2, B-3, B-4);
  - `declared`: its build, its deployer, or its composer supplied it and
    the process did not check it;
  - `minted`: this process generated it at boot from system entropy. It
    names this incarnation and describes nothing outside the process, so
    it is neither measured from the world nor handed over by anyone
    (B-4);
  - `absent`: not available, with a `reason` from the closed set in 3.1.

  A value is never omitted for being unavailable, a declared value is
  never reported as measured, and a derived value is never reported at a
  basis stronger than its weakest input. A comparison over a measured
  value and a declared value does not make the declared value measured:
  each compared value keeps its own basis, and the comparison carries its
  own (B-7).
- **B-2 (build identity, and exactly what is measured).** At boot, after
  the ledger opens and before the listener binds, the composer hashes its
  own executable once; `rahi version --binding` (B-13), which opens no
  ledger, hashes it the same way once before it prints:
  - **what.** sha256 over the bytes the process reads by opening the path
    `std::env::current_exe()` returns, read to end of file, reported as
    `build.binary.sha256`, basis `measured`.
  - **what it is not.** It is a file read by pathname. It is not a
    measurement of the mapped text of the running process, not a
    measurement of anything the process loaded afterwards, and not an
    attestation. The path may be replaced, deleted, or already different
    from the bytes the kernel exec'd, and the Rust standard library
    documents `current_exe` as platform-dependent and not a security
    primitive. A divergence between the bytes read and the bytes running
    is undetectable by this method. It catches a wrong image or a stale
    node, not an adversary inside the process (section 6).
  - **on failure.** `absent` with reason `unreadable`, and the boot
    continues. The document carries no path and no OS error string; the
    full error goes to the boot log (3.1, absence carries no free text).
  - `build.binary.platform` is the OCI platform string for the target the
    binary was compiled for, `<os>/<arch>`, declared. The chassis maps
    Rust's `std::env::consts::{OS, ARCH}` onto the OCI vocabulary through
    one closed table, so the value compares directly against an image
    index's platform entries: `aarch64` is reported as `arm64` and
    `x86_64` as `amd64`. A target with no entry in that table is `absent`
    with reason `unmapped`.
  - `build.rahi_version` is the chassis crate's `CARGO_PKG_VERSION`,
    declared. It names the chassis, never the application.
  - `build.revision` is the **application's** source revision, declared,
    supplied by the composer through a defaulted composition parameter
    (B-12). It is `absent` with reason `not_declared` when the composer
    supplies none. It is not read from `option_env!` in a chassis crate:
    a chassis crate's compile-time environment names whichever build
    populated the cargo cache, which for an out-of-tree application is a
    different repository at a different commit, and a value that names
    the wrong repository is worse than an absent one.
- **B-3 (the ceiling and store schema).** The document reports
  `manifest.hash`, the booted
  manifest hash (measured, 015 B-2, over the manifest bytes this process
  read), and `manifest.app.name`, `manifest.app.org`, and
  `manifest.contract_version` (declared by that manifest).
  `store.layout` is the declared relative layout name `app-store`, matching
  `Config::hiqlite_dir()` in Rahi 0.4.0; it exposes no host path.
  `store.schema_version` is the `app` set's version read from
  `schema_version` at boot (measured). `store.migration_sets` is one measured
  object whose keys are the linked named sets and whose integer values are
  their last recorded versions from `schema_set_version`. It includes the
  chassis sets and linked library sets present in that cell, is ordered by set
  name for stable serialization, and is empty when none are recorded. A set
  name is application metadata, not a metric label.
- **B-4 (the instance).** `instance.node` is the hiqlite node id (declared
  by the deployment through `RAHI_HIQ_NODE_ID` or the pod ordinal).
  `instance.id` is `<node>-<32 lowercase hex>`: 128 bits drawn once per
  process from the operating system's entropy source, basis `minted`.
  Where `instance.node` is `absent` (B-13's version output outside a
  cell), the prefix is `0`. The prefix is part of an opaque id: a consumer
  reads the node from `instance.node`, never from the id. The
  kernel's ids stay free of randomness (015 D-8); the composer, like the
  edge's trace id (023 D-4), may use entropy.
  - **The guarantee, stated as a probability.** Two boots of one replica
    are overwhelmingly unlikely to share an id: over 2^32 boots of a
    single node the chance that any two collide is below 2^-65. It is not
    impossible, and no consumer should treat `instance.id` as a key whose
    uniqueness is guaranteed by construction. A correlation that must be
    exact keys on `instance.id` together with the chain identity and, once
    041 exists, the epoch reference (D-1), and tolerates a repeat rather
    than assuming one cannot happen.
  - **Entropy failure.** If the entropy source fails, `serve` refuses to
    start with `Error::Config` naming it. A process that cannot honestly
    name its incarnation would silently poison every downstream
    correlation, and on every platform the chassis supports an entropy
    failure already means the cryptographic stack is unusable.
  `instance.pod` is `RAHI_POD_NAME` when the deployment sets it from the
  downward API, else `absent` with reason `not_declared`.
  `instance.started` is the host's wall time read at boot, measured from
  the system clock. It is an ordering hint from an untrusted clock, not a
  monotonic timestamp or proof of when the process began.
- **B-5 (the artifact and packaged components).** `artifact.image` is
  `RAHI_ARTIFACT_IMAGE`, an OCI
  reference pinned by digest (`<repository>@sha256:<64 lowercase hex>`),
  declared. A malformed value is `Error::Config` at startup naming the
  variable; unset is `absent` with reason `not_declared`. A process cannot
  measure the image it runs in, and the document says the value is the
  deployer's claim.
  - **Index or platform manifest.** The deployer declares which it pinned.
    A multi-platform index digest and the platform manifest digest under
    it are different identities, and a consumer joining an index digest
    against a per-platform provenance subject must resolve the index
    itself. The chassis records the string and resolves nothing.
  - **The image's own labels are a different claim.** `RAHI_ARTIFACT_IMAGE`
    is what the deployer pinned; `org.opencontainers.image.revision`
    (B-12) is what the image builder recorded. With 039's published base
    image and a runtime image composed on top of it, those can name
    different repositories at different commits. The document keeps them
    apart and never reconciles them.
  `components.hiqlite` reports the exact locked package map
  `{hiqlite-patched: 0.15.0-patched.3, hiqlite-wal-patched:
  0.15.0-patched.3, hiqlite-derive-patched: 0.15.0-patched.3}`, declared
  from build metadata. It identifies the app-store implementation compiled
  into Rahi 0.4.0, not Rauthy's separate store and not a live process
  measurement. `components.rauthy.image` reports
  the exact packaged source image reference
  `ghcr.io/bartekus/rauthy-patched:0.36.2-patched.3@sha256:d75cac0f708f3e238c458b622fea2f0b7dda9b67e9435eeafa37698d88a2a3c8`,
  declared by the image build. A composition that does not package Rauthy
  reports it `absent` with reason `not_applicable`; the field never claims
  that a child is running, healthy, or byte-identical to that image.
- **B-6 (the document).** `GET /binding` answers `application/json` with
  the object section 3.1 fixes: `{schema, instance, build, manifest,
  artifact, components, store, epoch, observation}`. The document is assembled once at
  boot and is byte-identical for the life of the process. The route is
  mounted on the unguarded branch beside the probes and `/metrics`
  (020 B-2, 023 D-8): no session, no CSRF check, no rate limit, not
  instrumented (023 B-5). It carries no secret, no principal, and no
  per-request value, and the deployment MUST keep it off the public
  ingress exactly as it keeps `/metrics` off (032 B-3).
- **B-7 (the epoch members are reserved, and boot-bound).** The document
  carries `epoch.ref` and `epoch.match`. In this spec both are `absent`
  with reason `not_implemented` and source `041-deployment-epochs`,
  because a binary without 041 cannot produce an epoch at all. 040 fixes
  their names, their wrapper shape, and their meaning; it does not fix the
  value vocabulary of `match`, which is 041's (the open register).
  - `epoch.ref`, when populated, is an epoch reference
    `{type, chain, epoch, number}`: `chain` and `epoch` are digests and
    together identify it, `number` orders it within one chain and never
    identifies it, because a restore can reuse a number (041 B-7).
  - **Absent is not epoch 0.** With 041 built and a chain that holds no
    epoch record, `epoch.ref` is **present**, naming the genesis as the
    chain identity with `number: 0`, and `match` reports the unbound
    state. `absent` with reason `not_implemented` means only that this
    binary has no epoch feature. A consumer MUST distinguish the two:
    the first says "deployed under no epoch yet", the second says "this
    chassis cannot tell you".
  - **Boot-bound.** Both are read once, at boot, with the rest of the
    document. `epoch.ref` names the epoch this process booted under, not
    the newest epoch in the chain, and `match` is a comparison performed
    once against that epoch. Neither is refreshed, and a later deployment
    does not change either.
  - **Each compared value keeps its own basis.** A comparison whose inputs
    are a measured binary digest and a declared image reference does not
    make the image measured (B-1). A comparison the process could not
    perform, because an input was `absent` or the platforms differ, is
    never reported as agreement: missing evidence yields an explicitly
    unknown result, never a verified match. 041 defines the per-field
    vocabulary that carries that.
- **B-8 (not a permission, and not a clock).** Two consequences of B-6 that
  a consumer MUST NOT design around:
  - **Not a permission.** The document is an observation with bases and
    authorizes nothing. A consumer's permission to deploy is separate and
    is judged by the consumer (041 B-10a, D-1).
  - **Not a clock.** Because the document is boot-bound, a replica whose
    process started before the newest deployment reports the older
    epoch, correctly. `/binding` therefore cannot be advertised as a
    freshness source, and polling it (or any single document) is not an
    atomic authorization of an effect: between the read and the effect the
    deployment state can move, and the document would not have shown it
    moving in any case. A validity predicate revalidated at effect time
    reads the chain, and its consistency, availability, and race
    semantics are 041's to specify. If consumers need a current-epoch
    observation surface, it is a separate surface with separate
    guarantees; this spec does not propose one.
- **B-9 (what is not observed, stated).** `observation` states coverage
  and loss rather than implying completeness: `traces` (export `on` or
  `off`, the sampler, both `not_applicable` outside a cell (B-13), `export_loss: "uncounted"`, the ring capacity),
  `metrics.unobserved` (`/metrics`, `/binding`, and requests that matched
  no route, 023 D-3), and `decisions` (`allows: "not recorded"` per 015
  D-5, the denial queue's capacity, and the names of the counters that
  count lost denials). A figure the chassis cannot produce is written as
  such, never left out. `observation` describes the chassis's own
  telemetry configuration rather than the world, so its members are plain
  values and carry no basis (3.1).
- **B-10 (the resource).** The OTel resource carries `service.name` (as
  today), `service.version` (the manifest's `contract_version`),
  `service.instance.id` (`instance.id`), and `rahi.version`,
  `rahi.revision`, `rahi.manifest.hash`, `rahi.binary.sha256`,
  `rahi.binary.platform`, `rahi.node`, and `rahi.artifact.image`, each
  carried only when the corresponding document value is present. Resource
  attributes are per process and so bounded; none is copied onto a span or
  a metric label.
  - **The resource states no basis, so it is not the contract.** An OTel
    resource is a flat map of strings with nowhere to put a basis or an
    absence reason, so an absent value is simply not emitted. The document
    is where absence is stated and where a consumer reads bases; the
    resource is a convenience for a collector that already has one.
  - **No comparison and no freshness on the resource.** Neither
    `epoch.match` nor any other comparison result is exported as a
    resource attribute: a resource is fixed for the process's life and a
    consumer reading one under a present-tense name would take a
    boot-time comparison for current state. Should 041 export the
    boot-bound epoch reference at all, its key says so
    (`rahi.epoch.boot_ref`), never a present-tense name.
- **B-11 (metrics stay bounded).** One family is added:
  `rahi_build_info{rahi_version, contract_version}`, value `1`.
  `rahi_version` is `build.rahi_version` and `contract_version` is the
  manifest's `[contract] version` as read at boot (B-3), both fixed for
  the life of the process. A digest, an instance id, a pod name, a
  revision, or a deployment id MUST never be a metric label (023 B-1's
  rule, extended to identity). A scraper attributes a series to a pod
  through its own target labels.
  - **The cardinality claim, stated exactly.** One process emits exactly
    one series of this family. Across a fleet the family's cardinality is
    the number of distinct deployed `(rahi_version, contract_version)`
    pairs, which changes only when a deployment changes one of them; it
    is bounded by the deployment history, not by a constant. That is the
    property 023 B-1 asks for, and it is weaker than "one series".
- **B-12 (the image, the deployment, and the composition interface).**
  - `docker/Dockerfile` takes `RAHI_BUILD_REVISION` as a build argument
    and labels the image `org.opencontainers.image.revision`,
    `org.opencontainers.image.source`, and
    `org.opencontainers.image.version`; `image.yml` passes the commit sha.
    The label records what the image builder knew. It is not read back
    into `build.revision` (B-2, B-5).
  - The composer supplies the application's identity through one
    defaulted parameter on the composition entry point, so an in-tree
    application and an out-of-tree one use the same path and an
    application that supplies nothing gets `absent`, never a wrong value.
    The parameter is named `app_revision` and is an optional string;
    `None` produces `absent` with source `app_revision`.
  - The packaged build supplies the locked Hiqlite package identity and the
    exact Rauthy source image reference to the binding composer. A downstream
    composition may replace either only by rebuilding, and the resulting
    values remain `declared`. The runtime does not inspect Cargo metadata,
    image labels, a registry, or the Rauthy child to strengthen their basis.
  - The StatefulSet sets `RAHI_POD_NAME` from the downward API and
    documents `RAHI_ARTIFACT_IMAGE`. The Ingress answers `404` for
    `/binding` as it does for `/metrics`. `scripts/k8s-validate.sh`
    refuses a render that routes `/binding` publicly, and one whose
    `RAHI_ARTIFACT_IMAGE` differs from a container image already pinned by
    digest.
  - **Exposure is checked on every supported path, not only the Ingress.**
    032's Kubernetes render is one deployment path; a directly published
    container port and a Compose file are others, and an ingress check
    proves nothing about them. Each supported path carries its own check
    that `/binding` and `/metrics` are not reachable from outside the
    deployment boundary, and `deploy/README.md` states the rule once for
    a path the chassis does not ship.
- **B-13 (`rahi version` stays what it is).** `rahi version` continues to
  print exactly `rahi <CARGO_PKG_VERSION>` on one line to stdout and exit
  0. Extended build identity is the separate, explicit invocation `rahi
  version --binding`, which prints the same `rahi.binding/v0` document, with the members a process
  outside a booted cell cannot produce (`instance.node`, `manifest`,
  `store`, and `components.rauthy.image`, which B-5 has the image build
  declare, so a binary run outside its image has none) `absent` with
  reason `not_applicable`, and `epoch` `absent`
  with reason `not_implemented` as B-7 states for every 040 producer
  (3.1, example 3). Its invocation is
  part of this contract. No existing line of output changes
  shape, order, or exit code; a script that greps today's line keeps
  working.
  - **These are consumer-contract surfaces under 039 B-1.** The `/binding`
    route, the document schema, `RAHI_ARTIFACT_IMAGE`, `RAHI_POD_NAME`,
    the composition parameter of B-12, and the extended version output
    all appear for the first time in whichever release carries this spec,
    and 039's versioning rule governs them from that release on.
- **B-14 (the 0.4.0 component identity does not erase 043's limits).** The
  exact identities in B-5 describe the owner-approved Rahi 0.4.0 payload.
  They do not claim patched.4, N=3 qualification, or closure of spec 043.
  The binding document MUST NOT turn any of these recorded limits into a
  positive status field:
  - the backward-clock transition retains the recorded revocation gap;
  - restoring stale Rauthy state can revive refresh credentials that later
    mint access tokens after the floor;
  - a kill or shutdown timeout before app-store shutdown can leave the
    unclean marker, and the next boot refuses pending an operator decision;
  - a live crash between Rauthy cache renames remains unexercised;
  - the v0.2 stop/start race and the v0.1 upgrade leg remain unexecuted;
  - N=3 remains unqualified; and
  - revocations are durable SQL state, not ledger events.

  These are release and qualification limits, not per-process identity
  values. A consumer joins the binding to the 0.4.0 release record to learn
  them. Spec 043's closure lane remains separate from 040 implementation.

### 3.1 The document, normatively

This subsection is the contract. Where any other text in this corpus, in
`docs/design/`, or in draft 041 spells a path differently, this subsection
governs and the other text is the one to correct.

**The wrapper.** Every identity value is an object in exactly one of two
shapes, and never a bare scalar:

```json
{ "value": <any JSON except null>, "basis": "measured" | "declared" | "minted" }
{ "basis": "absent", "reason": "<token>", "source": "<optional>" }
```

`basis` is always present. `value` is present if and only if `basis` is
not `absent`, and is never `null`. `reason` is present if and only if
`basis` is `absent`.

**Absence carries no free text.** `reason` is one token from this closed
set:

| token | meaning |
|---|---|
| `not_implemented` | the feature that would supply the value is not built in this binary; `source` names the spec |
| `not_declared` | the build, the deployer, or the composer supplied no value; `source` names the variable or parameter |
| `unreadable` | the process tried to read the bytes and failed |
| `unmapped` | the process has a value the document's vocabulary cannot express (B-2's platform table) |
| `not_applicable` | this producer cannot have the value at all (B-13's version output has no booted manifest) |

`source`, when present, is a name the chassis itself defines: an
environment variable, a composition parameter, or a spec id. No OS error
string, no filesystem path, and no user-supplied text ever enters the
document; the detail of an `unreadable` goes to the boot log, which is not
an unguarded surface.

**Two members are not wrappers.** `schema` is a bare string naming the
document, because it is not an observation about the world. `observation`
is a plain object of coverage facts about this chassis's own telemetry
(B-9), for the same reason. Everything else is a wrapper.

**Digest and reference syntax.** A digest is the string `sha256:` followed
by 64 lowercase hex characters. An image reference is
`<repository>@sha256:<64 lowercase hex>`; `components.rauthy.image`, a
composer constant (B-5), may also carry a tag, as
`<repository>:<tag>@sha256:<64 lowercase hex>`, where the digest identifies
the image and the tag is informative. A platform is `<os>/<arch>` in
the OCI vocabulary (B-2). A wall time is an integer of whole seconds since
the Unix epoch.

**Every path, once.**

| path | shape | basis in this spec |
|---|---|---|
| `schema` | bare string, `"rahi.binding/v0"` | not a wrapper |
| `instance.id` | wrapper, string `<node>-<32 hex>` | `minted` |
| `instance.node` | wrapper, integer | `declared`, or `absent`/`not_applicable` (B-13) |
| `instance.pod` | wrapper, string | `declared`, or `absent`/`not_declared` |
| `instance.started` | wrapper, integer seconds | `measured` |
| `build.binary.sha256` | wrapper, digest | `measured`, or `absent`/`unreadable` |
| `build.binary.platform` | wrapper, OCI platform | `declared`, or `absent`/`unmapped` |
| `build.rahi_version` | wrapper, semver string | `declared` |
| `build.revision` | wrapper, string | `declared`, or `absent`/`not_declared` |
| `manifest.hash` | wrapper, digest | `measured`, or `absent`/`not_applicable` (B-13) |
| `manifest.app.name` | wrapper, string | `declared`, or `absent`/`not_applicable` (B-13) |
| `manifest.app.org` | wrapper, string | `declared`, or `absent`/`not_applicable` (B-13) |
| `manifest.contract_version` | wrapper, semver string | `declared`, or `absent`/`not_applicable` (B-13) |
| `artifact.image` | wrapper, image reference | `declared`, or `absent`/`not_declared` |
| `components.hiqlite` | wrapper, object of exact locked package names to versions | `declared` |
| `components.rauthy.image` | wrapper, image reference | `declared`, or `absent`/`not_applicable` |
| `store.layout` | wrapper, string `app-store` | `declared`, or `absent`/`not_applicable` (B-13) |
| `store.schema_version` | wrapper, integer | `measured`, or `absent`/`not_applicable` (B-13) |
| `store.migration_sets` | wrapper, object of set name to integer version | `measured`, or `absent`/`not_applicable` (B-13) |
| `epoch.ref` | wrapper, epoch reference object | `absent`/`not_implemented` here (B-7) |
| `epoch.match` | wrapper, vocabulary owned by 041 | `absent`/`not_implemented` here (B-7) |
| `observation` | plain object | not a wrapper |

`build.binary` groups the two facts about the executable so that a
consumer joining a provenance subject reads one object per platform; there
is no `build.binary_sha256` and no top-level `build.platform`.

**Versioning, so that 041 needs no bump.** `schema` is `<name>/v<major>`.

- Adding a member, or populating a member that was `absent`, is **not** a
  version change. A consumer MUST ignore members it does not know.
- Removing, renaming, or retyping a member, or changing the wrapper shape,
  bumps the major.
- The `basis` and `reason` sets may gain tokens without a bump. A consumer
  that meets an unknown `basis` MUST treat the value as unusable rather
  than as verified, and an unknown `reason` as absence it cannot explain.
  It MUST NOT fail to parse the document.

This rule is what makes B-7's reservation safe: 041 populates `epoch.ref`
and `epoch.match` and defines `match`'s vocabulary without moving anyone
off `v0`, and it would equally have been safe to omit both members and add
them later. The members are named here so that one shape is agreed before
two implementations exist, not to avoid a bump the rule already makes
unnecessary.

**Example 1: a replica in a cluster.** Everything this spec can supply.

```json
{
  "schema": "rahi.binding/v0",
  "instance": {
    "id":      { "value": "2-9f1c04e2a7b3d8151d6c0b47ea395f82", "basis": "minted" },
    "node":    { "value": 2, "basis": "declared" },
    "pod":     { "value": "rahi-1", "basis": "declared" },
    "started": { "value": 1789247129, "basis": "measured" }
  },
  "build": {
    "binary": {
      "sha256":   { "value": "sha256:54e8134b9c0a2f7d6e5b1a8c3f4d2e0b9a7c6d5e4f3a2b1c0d9e8f7a6b5c4d3e", "basis": "measured" },
      "platform": { "value": "linux/arm64", "basis": "declared" }
    },
    "rahi_version": { "value": "0.4.0", "basis": "declared" },
    "revision":     { "value": "5728f2d3184d4d12c166a2e80d0bebb59bb6ccf4", "basis": "declared" }
  },
  "manifest": {
    "hash": { "value": "sha256:1b9d6bcd4bf1d9a2e8c3f0a7d5e2b4c6a8f1e3d5c7b9a0f2e4d6c8b1a3f5e7d9", "basis": "measured" },
    "app":  {
      "name": { "value": "hello-cell", "basis": "declared" },
      "org":  { "value": "statecrafting", "basis": "declared" }
    },
    "contract_version": { "value": "1.0.0", "basis": "declared" }
  },
  "artifact": {
    "image": { "value": "ghcr.io/statecrafting/hello-cell@sha256:0a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f9", "basis": "declared" }
  },
  "components": {
    "hiqlite": { "value": { "hiqlite-patched": "0.15.0-patched.3", "hiqlite-wal-patched": "0.15.0-patched.3", "hiqlite-derive-patched": "0.15.0-patched.3" }, "basis": "declared" },
    "rauthy": { "image": { "value": "ghcr.io/bartekus/rauthy-patched:0.36.2-patched.3@sha256:d75cac0f708f3e238c458b622fea2f0b7dda9b67e9435eeafa37698d88a2a3c8", "basis": "declared" } }
  },
  "store": {
    "layout": { "value": "app-store", "basis": "declared" },
    "schema_version": { "value": 1, "basis": "measured" },
    "migration_sets": { "value": { "rahi.coordination": 1, "rahi.receipts": 1 }, "basis": "measured" }
  },
  "epoch": {
    "ref":   { "basis": "absent", "reason": "not_implemented", "source": "041-deployment-epochs" },
    "match": { "basis": "absent", "reason": "not_implemented", "source": "041-deployment-epochs" }
  },
  "observation": {
    "traces":    { "export": "off", "sampler": "always_on", "export_loss": "uncounted", "ring_capacity": 1000 },
    "metrics":   { "unobserved": ["/metrics", "/binding", "unmatched routes"] },
    "decisions": {
      "allows": "not recorded",
      "queue_capacity": 1024,
      "loss_counters": ["kernel_decisions_dropped_total", "kernel_decisions_abandoned_total", "kernel_ledger_failures_total"]
    }
  }
}
```

**Example 2: a partial observation.** The same cell where the executable
could not be read, no image was declared, the deployment sets no pod name,
and the composer supplied no revision. Nothing is omitted, nothing is
guessed, and no reason carries a path or an error string.

```json
{
  "schema": "rahi.binding/v0",
  "instance": {
    "id":      { "value": "1-3c7e91a04bd25f68ae3019cf7b4d2e85", "basis": "minted" },
    "node":    { "value": 1, "basis": "declared" },
    "pod":     { "basis": "absent", "reason": "not_declared", "source": "RAHI_POD_NAME" },
    "started": { "value": 1789247402, "basis": "measured" }
  },
  "build": {
    "binary": {
      "sha256":   { "basis": "absent", "reason": "unreadable" },
      "platform": { "value": "linux/amd64", "basis": "declared" }
    },
    "rahi_version": { "value": "0.4.0", "basis": "declared" },
    "revision":     { "basis": "absent", "reason": "not_declared", "source": "app_revision" }
  },
  "manifest": {
    "hash": { "value": "sha256:1b9d6bcd4bf1d9a2e8c3f0a7d5e2b4c6a8f1e3d5c7b9a0f2e4d6c8b1a3f5e7d9", "basis": "measured" },
    "app":  {
      "name": { "value": "hello-cell", "basis": "declared" },
      "org":  { "value": "statecrafting", "basis": "declared" }
    },
    "contract_version": { "value": "1.0.0", "basis": "declared" }
  },
  "artifact": {
    "image": { "basis": "absent", "reason": "not_declared", "source": "RAHI_ARTIFACT_IMAGE" }
  },
  "components": {
    "hiqlite": { "value": { "hiqlite-patched": "0.15.0-patched.3", "hiqlite-wal-patched": "0.15.0-patched.3", "hiqlite-derive-patched": "0.15.0-patched.3" }, "basis": "declared" },
    "rauthy": { "image": { "value": "ghcr.io/bartekus/rauthy-patched:0.36.2-patched.3@sha256:d75cac0f708f3e238c458b622fea2f0b7dda9b67e9435eeafa37698d88a2a3c8", "basis": "declared" } }
  },
  "store": {
    "layout": { "value": "app-store", "basis": "declared" },
    "schema_version": { "value": 1, "basis": "measured" },
    "migration_sets": { "value": { "rahi.coordination": 1, "rahi.receipts": 1 }, "basis": "measured" }
  },
  "epoch": {
    "ref":   { "basis": "absent", "reason": "not_implemented", "source": "041-deployment-epochs" },
    "match": { "basis": "absent", "reason": "not_implemented", "source": "041-deployment-epochs" }
  },
  "observation": {
    "traces":    { "export": "on", "sampler": "always_on", "export_loss": "uncounted", "ring_capacity": 1000 },
    "metrics":   { "unobserved": ["/metrics", "/binding", "unmatched routes"] },
    "decisions": {
      "allows": "not recorded",
      "queue_capacity": 1024,
      "loss_counters": ["kernel_decisions_dropped_total", "kernel_decisions_abandoned_total", "kernel_ledger_failures_total"]
    }
  }
}
```

A consumer reading example 2 learns that the binary is unidentified, and
MUST NOT record the replica as running the image the deployment believes
it pinned. An absent measurement is not a passed check.

**Example 3: the extended version output, outside a cell.** B-13's
invocation runs with no store, no manifest, and no epoch feature, so three
subtrees are `not_applicable` and the rest is what the binary knows about
itself.

```json
{
  "schema": "rahi.binding/v0",
  "instance": {
    "id":      { "value": "0-b41d7fe2960c85a3d1704ecb26f893a5", "basis": "minted" },
    "node":    { "basis": "absent", "reason": "not_applicable" },
    "pod":     { "basis": "absent", "reason": "not_declared", "source": "RAHI_POD_NAME" },
    "started": { "value": 1789250011, "basis": "measured" }
  },
  "build": {
    "binary": {
      "sha256":   { "value": "sha256:54e8134b9c0a2f7d6e5b1a8c3f4d2e0b9a7c6d5e4f3a2b1c0d9e8f7a6b5c4d3e", "basis": "measured" },
      "platform": { "value": "linux/arm64", "basis": "declared" }
    },
    "rahi_version": { "value": "0.4.0", "basis": "declared" },
    "revision":     { "value": "5728f2d3184d4d12c166a2e80d0bebb59bb6ccf4", "basis": "declared" }
  },
  "manifest": {
    "hash":             { "basis": "absent", "reason": "not_applicable" },
    "app":              {
      "name": { "basis": "absent", "reason": "not_applicable" },
      "org":  { "basis": "absent", "reason": "not_applicable" }
    },
    "contract_version": { "basis": "absent", "reason": "not_applicable" }
  },
  "artifact": {
    "image": { "basis": "absent", "reason": "not_declared", "source": "RAHI_ARTIFACT_IMAGE" }
  },
  "components": {
    "hiqlite": { "value": { "hiqlite-patched": "0.15.0-patched.3", "hiqlite-wal-patched": "0.15.0-patched.3", "hiqlite-derive-patched": "0.15.0-patched.3" }, "basis": "declared" },
    "rauthy": { "image": { "basis": "absent", "reason": "not_applicable" } }
  },
  "store": {
    "layout": { "basis": "absent", "reason": "not_applicable" },
    "schema_version": { "basis": "absent", "reason": "not_applicable" },
    "migration_sets": { "basis": "absent", "reason": "not_applicable" }
  },
  "epoch": {
    "ref":   { "basis": "absent", "reason": "not_implemented", "source": "041-deployment-epochs" },
    "match": { "basis": "absent", "reason": "not_implemented", "source": "041-deployment-epochs" }
  },
  "observation": {
    "traces":    { "export": "not_applicable", "sampler": "not_applicable", "export_loss": "uncounted", "ring_capacity": 0 },
    "metrics":   { "unobserved": ["/metrics", "/binding", "unmatched routes"] },
    "decisions": {
      "allows": "not recorded",
      "queue_capacity": 0,
      "loss_counters": ["kernel_decisions_dropped_total", "kernel_decisions_abandoned_total", "kernel_ledger_failures_total"]
    }
  }
}
```

## 4. Functional requirements

- **FR-001 (the shape is the contract).** `tests/binding.rs` boots a
  fixture cell with `RAHI_RAUTHY_MODE=none` and `RAHI_ARTIFACT_IMAGE` set,
  fetches `/binding`, and asserts against 3.1's table: every listed path is
  present; `schema` is `rahi.binding/v0`; every wrapper is in exactly one
  of the two shapes with no `null` value; every `basis` is in the closed
  set; every `absent` carries a `reason` from the closed set; no member
  outside `schema` and `observation` is a bare scalar.
- **FR-002 (the measured values are the measured bytes).**
  `build.binary.sha256` equals sha256 over the bytes of the fixture's
  executable (`CARGO_BIN_EXE_*`) with basis `measured`; `manifest.hash`
  equals `Manifest::hash()` of the fixture manifest and the genesis parent
  in `ledger export`, basis `measured`; `store.schema_version` equals the
  app set's recorded version and `store.migration_sets` equals the sorted
  named-set versions, both basis `measured`; `store.layout` is `app-store`,
  basis `declared`; `artifact.image` is the value set, basis `declared`;
  `epoch.ref` and `epoch.match` are `absent` with reason `not_implemented`.
- **FR-003 (the instance is minted, and says so).** `instance.id` matches
  `^[0-9]+-[0-9a-f]{32}$` with basis `minted`; two boots of the same volume
  report different `instance.id` values with the same `instance.node`. A
  unit test asserts the id draws 128 bits and that an entropy failure is
  `Error::Config` rather than a fallback value.
- **FR-004 (absence is stated, bounded, and leaks nothing).** With
  `RAHI_ARTIFACT_IMAGE` unset the document names it `absent` with reason
  `not_declared` and source `RAHI_ARTIFACT_IMAGE`; with it malformed,
  serve refuses to start with `Error::Config` naming it. With the
  executable unreadable at the path `current_exe()` returns, the boot
  completes, `build.binary.sha256` is `absent` with reason `unreadable`,
  the boot log carries the underlying error, and the served document
  contains no filesystem path and no error string.
- **FR-005 (the platform vocabulary is OCI).** A unit test asserts the
  mapping table: `aarch64` renders `arm64`, `x86_64` renders `amd64`, and
  a target with no entry renders `absent` with reason `unmapped`. No Rust
  architecture spelling reaches the document.
- **FR-006 (the surface stays unguarded and uninstrumented).** `/binding`
  answers without a session cookie and without a CSRF token, and a scrape
  of it leaves `http_requests_total` and the trace ring unchanged.
- **FR-007 (the resource and the metric).** An edge test asserts the
  tracer's resource carries every B-10 key whose document value is
  present, omits the keys whose value is `absent`, carries no epoch or
  comparison attribute, and that `/metrics` renders `rahi_build_info` with
  exactly its two labels.
- **FR-007a (component identities keep their limits).** A packaged-image
  test asserts the exact B-5 Hiqlite package and Rauthy image identities,
  both basis `declared`. A non-packaged composition reports Rauthy
  `absent`/`not_applicable`. Neither form contains a health, qualification,
  attestation, patched.4, or N=3-success claim.
- **FR-008 (the version verb is unchanged).** A CLI test asserts `rahi
  version` writes exactly `rahi <CARGO_PKG_VERSION>\n` to stdout and exits
  0, byte for byte as before this spec, and that `rahi version --binding`
  emits a `rahi.binding/v0` document in which `manifest`, `store`, and the
  `epoch` members are `absent`, the first two with reason
  `not_applicable`.
- **FR-009 (exposure is refused on every shipped path).**
  `scripts/k8s-validate.sh` refuses a fixture render that routes
  `/binding` through the Ingress and one whose `RAHI_ARTIFACT_IMAGE`
  disagrees with a digest-pinned image, and passes the shipped render. For
  every other deployment path the repository ships, the same check runs
  against that path's own render; for a path it does not ship,
  `deploy/README.md` states the rule.
- **FR-010 (image labels identify the build without attesting it).** The
  image built by `image.yml` carries
  `org.opencontainers.image.revision`, `.source`, and `.version`; their
  values equal the workflow inputs and release version. Tests call them
  declared builder metadata and never provenance or attestation.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p rahi-cli --locked --test binding` and `cargo
  test -p rahi-edge --locked --test obs` pass, and
  `scripts/k8s-validate.sh` exits 0. The binding test covers FR-001 through
  FR-006, FR-007a, FR-008, and boot stability: two reads in one boot are
  byte-identical and a restart mints a different instance id. The obs
  test covers FR-007.
- **AC-2.** Spec 020's and 023's acceptance criteria still hold: the edge
  gains no dependency on `rahi-ops` or `rahi-idp`.
- **AC-3.** Nothing in this spec writes to the chain: the implementing
  change touches no file under `crates/rahi-ledger/` or
  `crates/rahi-kernel/`, and `docs/design/01-consumer-contract.md` names
  the document, its bases, and its boot-bound limit.
- **AC-4.** Section 3.1's three examples are test input, not prose: the
  implementing change feeds each to the same validator FR-001 uses, and
  each validates. An example that drifts from the code fails the build.
- **AC-5.** `rahi version`'s default output is byte-identical to its
  output before this spec.
- **AC-6.** The image workflow's build inspection verifies FR-010's three
  OCI labels, and the Kubernetes validation refuses public `/binding`
  exposure and an artifact-image mismatch.

## 6. Out of scope

- Deployment epochs, and anything appended to the chain (spec 041). This
  spec reserves `epoch.ref` and `epoch.match` and fixes nothing about the
  values 041 puts in them beyond the reference shape (B-7).
- A current-epoch or freshness observation surface, and the consistency,
  availability, and effect-time race semantics such a surface would need.
  `/binding` is boot-bound and is not one (B-8).
- The machine-readable consumer fixture set. 041 FR-003 claims
  `crates/rahi-cli/testdata/binding/` and composes it with the producers
  this spec has none of.
- Collecting, storing, querying, or evaluating observations, runtime
  assertions, or drift; conformance scoring (the consumer's).
- Producing or signing build provenance (spec 039 leaves signing to a
  later supply-chain spec; 041 names the subject convention it expects).
- Hardware-rooted attestation. A digest a process measures of itself by
  reading a path is an observation that a compromised process can
  falsify; it catches the wrong image or a stale node, not an adversary
  inside the process (B-2).
- Per-request identity beyond the existing trace id (023 D-4).

## 7. Resolved decisions

- **D-1 (2026-09-12, owner decision RH-07; the contract kept, the build
  deferred).** Three rules this draft already states are retained as the
  contract. A runtime observation is keyed by instance identity together
  with 041's epoch reference (the chain hash and the epoch record hash),
  never by pod, node, or epoch number alone; `instance.id` (B-4) is this
  spec's share. An observation grants no deployment permission (B-8, which
  now says so, taking P-2). No identity value is a metric label (B-11).
  Implementation was deferred until the first hosted pilot's prerequisites
  were built. That historical condition is discharged by completed specs 035
  through 039. P-1 and P-3 were not decided by this entry. The spec stays
  `draft` until Bart personally ratifies it.

- **D-2 (2026-09-26, ratification-readiness reconciliation, not an owner
  decision).** The provisional September material is consolidated into the
  one normative schema in section 3.1. It is reconciled to named migration
  sets (046), ledger lifetime identity (042), released Rahi 0.4.0, and the
  exact owner-approved component scope and disclosed limits of 043. The
  schema uses closed absence reasons, reserves `epoch.ref` and `epoch.match`
  without implementing them, keeps `/binding` boot-bound, and makes the
  default version output byte-compatible. This entry grants no approval,
  implementation, release, or spec-041 authority.

- **D-3 (2026-10-05, owner decision; ratification).** The owner directed,
  verbatim: "Approve all; proceed with increased velocity development." This
  ratifies the contract as revised by D-2 and moves the spec to `approved`.
  Every row of the table below is resolved by its recommended answer:
  `rahi.binding/v0` with one wrapper per value and closed absence reasons;
  `epoch.ref` and `epoch.match` reserved as `absent`/`not_implemented`;
  `/binding` beside `/metrics`, unguarded and excluded from every public
  ingress; the executable hashed once at boot before listening; `minted`
  instance identity with startup failing on entropy failure; optional
  `app_revision` and `rahi version --binding` with default output
  unchanged; `app-store`, named set versions and exact declared hiqlite and
  Rauthy identities; and 039 B-1 applying from the first release that
  carries 040. Not changed: no B-n, FR or AC text.

- **D-4 (2026-10-05, approval review; consistency of the ratified text).**
  The approving change's review found three places where the normative
  text disagreed with example 3, which AC-4 also takes as test input. Each
  is resolved toward the example, which B-13 already required: section
  3.1's table now admits `absent`/`not_applicable` for `instance.node`,
  `manifest.*` and `store.*` (B-13's version output); B-4 states that the
  id's prefix is `0` when `instance.node` is absent and that consumers read
  the node from `instance.node`; B-2 states that `rahi version --binding`
  measures the executable once before printing, since it opens no ledger;
  B-13 names `epoch` as `not_implemented`, as B-7 and example 3 do, and
  `instance.node` as `not_applicable`; section 3.1's image-reference
  syntax admits the tagged form B-5 and example 1 use for
  `components.rauthy.image`; example 2, the same cell as example 1, now
  declares the same Rauthy image; B-9 names `not_applicable` for trace
  export and sampler outside a cell, as example 3 shows, and B-13 lists
  `components.rauthy.image` among the `not_applicable` members there;
  AC-1's binding test names FR-006 and its obs test FR-007, which no
  step named; example 2's migration sets equal example 1's, the same
  cell's. No other behavior changed.

- **D-5 (2026-10-05, build decisions; the first implementing change).**
  (a) **The hiqlite set and the lock.** B-5 names three packages as "the
  exact locked package map", but `Cargo.lock` holds two:
  `hiqlite-derive-patched` belongs to the `0.15.0-patched.3` release set
  and no feature this workspace enables compiles it. The document reports
  the three-member map B-5 and every example fix, declared, and
  `rahi_ops::binding`'s unit test holds the two locked members to the lock
  and admits only the derive crate as unlocked. Surfaced for the owner:
  whether the map should name only compiled packages is a change to B-5
  and the examples, which this build does not make. (b) **How the
  packaged build declares Rauthy.** Both image recipes set
  `RAHI_RAUTHY_IMAGE` from the `RAUTHY_IMAGE` argument they copy Rauthy
  from; the composer reports it declared when set (a malformed value is
  `Error::Config`, as for `RAHI_ARTIFACT_IMAGE`) and `absent` with reason
  `not_applicable` when unset, which is every composition that did not
  package Rauthy. Nothing reads the child, its labels or a registry.
  (c) **`app_revision`.** The defaulted parameter is
  `Cell::app_revision() -> Option<&'static str>`, `None` by default, so
  an in-tree and an out-of-tree cell supply it the same way. `cell.rs` and
  `lib.rs` are also claimed by draft spec 049, and the governance gate
  refuses a change to a path a draft owns, so this change ships the
  document, the route, the resource, the metric and the deployment checks
  with `build.revision` stated `absent`/`not_declared`, and a second change
  adds `Cell::app_revision` and `rahi version --binding` (B-13, FR-008,
  AC-5) once 049 is ratified. (d) **Platforms.** The closed table maps
  `linux` and `macos` (`darwin`) by `x86_64` (`amd64`) and `aarch64`
  (`arm64`); anything else is `unmapped`. (e) **Unobserved.** Keeping
  `/binding` out of the ring and the request counters (B-6, FR-006) is a
  change to 023's `is_instrumented`, under an `extends` edge; the exposure
  table lists `/binding` as a probe-class route. (f) **The resource.**
  `obs::init_with` and `tracer::install_with` carry the identity
  attributes, so `ObsOptions` gains no field and no consumer's struct
  literal breaks; the edge reads the attributes off the document's JSON
  (`rahi_edge::binding::resource_attributes`) and names no ops type.
  (g) **Every shipped path.** Besides the Kubernetes render, the
  repository ships `docker/compose.yml`, a developer path that publishes
  on `127.0.0.1` only; `scripts/k8s-validate.sh` refuses a published port
  beyond the loopback there. `deploy/README.md` states the rule for a path
  the repository does not ship. (h) **The image workflow.** FR-010's
  labels are written by `docker/Dockerfile` from build arguments; passing
  the commit and verifying the labels in `image.yml` (AC-6) is a workflow
  change, which needs the owner's exception, and lands as its own change.
  (i) **The unreadable executable.** FR-004's test runs a copy of the
  binary at mode `0111`, which a non-root user can execute and not read;
  it refuses to run as a user file modes do not bind.

- **D-6 (2026-10-05, build decision; the second implementing change).**
  `Cell::app_revision` and `rahi version --binding` land as D-5 (c) said.
  `rahi version` keeps its old parse: `--binding` is the one flag, and
  any other trailing argument is still ignored, so every invocation that
  printed `rahi <version>` before prints it unchanged (AC-5).

### Owner choices at ratification (resolved by D-3)

Every row was a bounded choice Bart resolved by ratifying this draft or asking
for a stated revision; D-3 took each row's recommended answer. No cross-project agreement is required for Rahi to
name its producer document. A consumer envelope and retention policy remain
consumer-owned and outside this spec.

| choice | alternative | recommended answer | consequence of the recommendation |
|---|---|---|---|
| schema and wrappers | choose another name or permit bare and nullable values | `rahi.binding/v0`, one wrapper per identity value, closed absence reasons | one parseable producer contract; a breaking shape change later requires a new major |
| reserved 041 members | omit epoch members until 041 | retain `epoch.ref` and `epoch.match` as `absent`/`not_implemented` | 041 can populate one agreed shape without implying that epochs exist now |
| route protection | require operator or bearer credentials | mount `/binding` beside `/metrics`, unguarded and excluded from every public ingress | collectors need no principal; deployment checks become the confidentiality boundary |
| executable measurement | hash on first request | hash once at boot before listening | every served byte is stable for that boot, at the cost of one bounded startup read |
| instance basis and failure | call the id declared, or serve without one on entropy failure | use `minted` and fail startup on entropy failure | consumers can distinguish generated identity from handed-over claims and never receive a knowingly unusable join key |
| application revision and version CLI | leave names open | optional `app_revision`; extended output is `rahi version --binding`; default `rahi version` is unchanged | implementation has no naming ambiguity and existing scripts remain byte-compatible |
| component and store identity | report only the old scalar app schema version | add `app-store`, named set versions, and exact declared Hiqlite and Rauthy identities | Rahi 0.4.0 is described without confusing package claims with runtime measurement or qualification |
| contract classification | treat these surfaces as internal | apply 039 B-1 from the first release that carries 040 | future incompatible changes require the release discipline promised to consumers |

The value vocabulary and implementation behavior of `epoch.match` remain a
spec-041 owner choice, not an unresolved 040 choice. The index-versus-platform
meaning of `RAHI_ARTIFACT_IMAGE` remains exactly the deployer's declared
string; consumers may resolve it, but Rahi does not. Spec 043's outstanding
qualification work remains on its own closure lane and does not block an
owner decision on this contract.

## Verification

```verify:cli
cargo test -p rahi-cli --locked --test binding
cargo test -p rahi-edge --locked --test obs
scripts/k8s-validate.sh
```
