---
id: "040-runtime-identity-and-binding-surface"
title: "Runtime identity: a replica states which bits it runs, which ceiling it enforces, and which instance it is, with every value's basis named, on bounded surfaces"
status: draft
kind: feature
domain: edge
created: "2026-09-11"
authors: ["Bartek Kus"]
implementation: pending
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
  - { spec: "030-operational-verbs", unit: "crates/rahi-ops/src/lib.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/src/lib.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/src/serve.rs", nature: additive }
  - { spec: "031-single-container-packaging", unit: "docker/Dockerfile", nature: additive }
  - { spec: "031-single-container-packaging", unit: ".github/workflows/image.yml", nature: additive }
  - { spec: "032-cluster-topology", unit: "deploy/k8s/statefulset.yaml", nature: additive }
  - { spec: "032-cluster-topology", unit: "deploy/k8s/ingress.yaml", nature: additive }
  - { spec: "032-cluster-topology", unit: "scripts/k8s-validate.sh", nature: additive }
  - { spec: "032-cluster-topology", unit: "deploy/README.md", nature: additive }
references:
  - { unit: { kind: file, path: "docs/design/01-consumer-contract.md" }, role: context }
summary: >
  A consumer that correlates an immutable artifact, an authority snapshot,
  a deployment, and a running replica needs the replica to say what it is.
  Today it says almost nothing: rahi version prints the crate version, the
  manifest hash is visible only inside a ledger export or a backup, the
  OTel resource carries service.name alone, the image has no labels, and
  the deploy manifests pin no digest. This spec makes a replica report its
  measured executable digest, its booted manifest hash, its declared image
  and build revision, and a per-boot instance id, each with its basis
  (measured, declared, or absent), on one unguarded document at /binding
  kept off the ingress like /metrics, on the OTel resource, and in one
  build-info metric whose labels are versions only. It states what the
  telemetry does not observe or count. It writes nothing to the chain;
  deployment epochs are spec 041.
---

# 040: Runtime identity and the binding surface

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

Verified at `444bcf8` on 2026-09-11 by reading the code:

- `rahi version` prints `rahi <CARGO_PKG_VERSION>` and nothing else
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
- `docker/Dockerfile` has no `LABEL`; `image.yml` records no provenance
  and no SBOM; `deploy/k8s` names the image with no tag and no digest.
- Every metric label is a closed vocabulary (023 B-1, D-3).

Constitution XIII makes observability part of the contract. This spec
extends it from "what happened" to "what is running", with the same rule
023 applies to labels: nothing unbounded.

## 2. Territory

- `crates/rahi-ops/src/binding.rs` (new): self-measurement and the binding
  document, assembled once at boot.
- `crates/rahi-edge/src/binding.rs` (new): the `/binding` route, which
  serves the bytes the composer hands it and names no ops type, so the
  edge keeps its dependency direction (thesis §4).
- `crates/rahi-cli/tests/binding.rs` (new): the end-to-end proof.
- Additive changes to the edge's module list and router (020), the tracer,
  metrics, observation options, and their test (023), the ops module list,
  the version verb, and the serve composition (030), the image recipe and
  workflow (031), and the StatefulSet, Ingress, validation script, and
  deploy notes (032).

## 3. Behavior

- **B-1 (every value has a basis).** Each identity value the chassis
  reports MUST carry one of three bases: `measured` (this process computed
  it from bytes it read), `declared` (its build or its deployer supplied
  it and the process did not check it), or `absent` (not available, with
  the reason). A value is never omitted for being unavailable, and a
  declared value is never reported as measured.
- **B-2 (build identity).** At boot, before it listens, the composer
  hashes its own executable once: sha256 over the bytes of
  `std::env::current_exe()`, reported as `build.binary.sha256`, measured.
  A read failure makes the value `absent` with the error and does not stop
  the boot. `build.revision` is declared: `option_env!("RAHI_BUILD_REVISION")`
  captured when the chassis is compiled, so the build that produced the
  binary names the revision it built, or `absent`. `build.rahi_version`
  is `CARGO_PKG_VERSION`, declared. `build.platform` is the target the
  binary was compiled for (`<os>/<arch>` from `std::env::consts`),
  declared; a multi-architecture image holds one binary per platform, and
  each has its own digest. `rahi version` prints these four, one per line,
  each with its basis.
- **B-3 (the ceiling).** The document reports the booted manifest hash
  (measured, 015 B-2), `app.name`, `app.org`, and `contract.version`
  (declared by the manifest), and the store's `schema_version` read at
  boot (measured).
- **B-4 (the instance).** `instance.node` is the hiqlite node id (declared
  by the deployment through `RAHI_HIQ_NODE_ID` or the pod ordinal).
  `instance.id` is `<node>-<16 hex of system entropy>`, minted once per
  process by the composer, so two boots of one replica never share an id;
  the kernel's ids stay free of randomness (015 D-8), since the composer,
  like the edge's trace id (023 D-4), may use entropy. `instance.pod` is
  `RAHI_POD_NAME` when the deployment sets it from the downward API, else
  absent. `instance.started` is the host's wall time at boot, declared.
- **B-5 (the artifact).** `artifact.image` is `RAHI_ARTIFACT_IMAGE`, an OCI
  reference pinned by digest (`<repository>@sha256:<64 hex>`), declared.
  A malformed value is `Error::Config` at startup naming the variable;
  unset is absent. A process cannot measure the image it runs in, and the
  document says the value is the deployer's claim.
- **B-6 (the document).** `GET /binding` answers `application/json` with
  `{schema, instance, build, manifest, artifact, store, observation}`,
  `schema` naming the document's type and version (7). The document is
  assembled at boot and identical for the life of the process. The route
  is mounted on the unguarded branch beside the probes and `/metrics`
  (020 B-2, 023 D-8): no session, no CSRF check, no rate limit, not
  instrumented (023 B-5). It carries no secret, no principal, and no
  per-request value, and the deployment MUST keep it off the public
  ingress exactly as it keeps `/metrics` off (032 B-3).
- **B-7 (what is not observed, stated).** `observation` states coverage
  and loss rather than implying completeness: `traces` (export on or off,
  the sampler, `export_loss: "uncounted"`, the ring capacity),
  `metrics.unobserved` (`/metrics`, `/binding`, and requests that matched
  no route, 023 D-3), and `decisions` (`allows: "not recorded"` per 015
  D-5, the denial queue's capacity, and the names of the counters that
  count lost denials). A figure the chassis cannot produce is written as
  such, never left out.
- **B-8 (the resource).** The OTel resource carries `service.name` (as
  today), `service.version` (the manifest's `contract.version`),
  `service.instance.id` (`instance.id`), and `rahi.version`,
  `rahi.manifest.hash`, `rahi.binary.sha256`, `rahi.node`, and
  `rahi.artifact.image` when declared. Resource attributes are per process
  and so bounded; none of these is copied onto a span or a metric label.
- **B-9 (metrics stay bounded).** One family is added:
  `rahi_build_info{rahi_version, contract_version}`, value `1`, both
  labels semantic versions fixed at build. A digest, an instance id, a
  pod name, or a deployment id MUST never be a metric label (023 B-1's
  rule, extended to identity). A scraper attributes a series to a pod
  through its own target labels.
- **B-10 (the image and the deployment say what they are).**
  `docker/Dockerfile` takes `RAHI_BUILD_REVISION` as a build argument,
  exports it to the cargo build, and labels the image
  `org.opencontainers.image.revision`, `org.opencontainers.image.source`,
  and `org.opencontainers.image.version`; `image.yml` passes the commit
  sha. The StatefulSet sets `RAHI_POD_NAME` from the downward API and
  documents `RAHI_ARTIFACT_IMAGE`. The Ingress answers `404` for
  `/binding` as it does for `/metrics`. `scripts/k8s-validate.sh` refuses
  a render that routes `/binding` publicly, and one whose
  `RAHI_ARTIFACT_IMAGE` differs from a container image already pinned by
  digest.

## 4. Functional requirements

- **FR-001.** `tests/binding.rs` boots a fixture cell with
  `RAHI_RAUTHY_MODE=none` and `RAHI_ARTIFACT_IMAGE` set, fetches
  `/binding`, and asserts: `build.binary.sha256` equals sha256 over the
  bytes of the fixture's executable (`CARGO_BIN_EXE_*`), basis
  `measured`; the manifest hash equals `Manifest::hash()` of the fixture
  manifest and the genesis parent in `ledger export`; `artifact.image` is
  the value set, basis `declared`; every B-7 member is present.
- **FR-002.** Two boots of the same volume report different `instance.id`
  values with the same `instance.node`; with `RAHI_ARTIFACT_IMAGE` unset
  the document names it `absent` with a reason; with it malformed, serve
  refuses to start with `Error::Config` naming it.
- **FR-003.** `/binding` answers without a session cookie and without a
  CSRF token, and a scrape of it leaves `http_requests_total` and the
  trace ring unchanged.
- **FR-004.** An edge test asserts the tracer's resource carries every B-8
  key and `/metrics` renders `rahi_build_info` with exactly its two
  labels.
- **FR-005.** `scripts/k8s-validate.sh` refuses a fixture render that
  routes `/binding` through the Ingress and one whose
  `RAHI_ARTIFACT_IMAGE` disagrees with a digest-pinned image, and passes
  the shipped render.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p rahi-cli --locked --test binding` and `cargo
  test -p rahi-edge --locked --test obs` pass, and
  `scripts/k8s-validate.sh` exits 0.
- **AC-2.** Spec 020's and 023's acceptance criteria still hold: the edge
  gains no dependency on `rahi-ops` or `rahi-idp`.
- **AC-3.** Nothing in this spec writes to the chain: the implementing
  change touches no file under `crates/rahi-ledger/` or
  `crates/rahi-kernel/`, and `docs/design/01-consumer-contract.md` names
  the document and its bases.

## 6. Out of scope

- Deployment epochs, and anything appended to the chain (spec 041).
- Collecting, storing, querying, or evaluating observations, runtime
  assertions, or drift; conformance scoring (the consumer's).
- Producing or signing build provenance (spec 039 leaves signing to a
  later supply-chain spec; 041 names the subject convention it expects).
- Hardware-rooted attestation. A digest a process measures of itself is an
  observation that a compromised process can falsify; it catches the
  wrong image or a stale node, not an adversary inside the process.
- Per-request identity beyond the existing trace id (023 D-4).

## 7. Resolved decisions

None yet. Before approval a human decides:

- the document's `schema` name and version (`rahi.binding/v0` proposed)
  and whether a consumer's envelope wraps the document or references it,
  agreed with Statecraft and the CLI before either side implements (the
  handoff's exit criterion: agree on schema references before a parallel
  format exists);
- whether `/binding` sits beside `/metrics` (proposed, the same exposure
  model) or behind the operator role or a bearer scope, which a collector
  would then need credentials for;
- whether the executable is hashed at every boot (proposed; one sha256
  pass proportional to the binary's size, before the listener opens) or on
  the first request;
- whether `rahi version`'s longer output is a consumer contract change
  under spec 039 B-1.

## Verification

```verify:cli
cargo test -p rahi-cli --locked --test binding
cargo test -p rahi-edge --locked --test obs
scripts/k8s-validate.sh
```
