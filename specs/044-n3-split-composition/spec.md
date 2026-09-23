---
id: "044-n3-split-composition"
title: "N=3 split composition: two StatefulSets, rahi and rauthy, one hiqlite node per pod, rahi reaching rauthy only through an encrypted internal Service and losing readiness, never liveness, without it"
status: draft
kind: feature
domain: ops
created: "2026-09-23"
authors: ["Bartek Kus"]
implementation: pending
risk: critical
wave: 3
depends_on:
  - "020-edge-server"
  - "021-idp-proxy-and-discovery"
  - "030-operational-verbs"
  - "031-single-container-packaging"
  - "032-cluster-topology"
  - "036-manifest-and-schema-evolution"
  - "037-identity-recovery-and-live-proof"
establishes:
  - "deploy/n3-split/"
  - "crates/rahi-cli/tests/split.rs"
extends:
  - { spec: "020-edge-server", unit: "crates/rahi-edge/src/probes.rs", nature: amending }
  - { spec: "021-idp-proxy-and-discovery", unit: "crates/rahi-idp/src/config.rs", nature: amending }
  - { spec: "021-idp-proxy-and-discovery", unit: "crates/rahi-idp/src/discovery.rs", nature: amending }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/src/serve.rs", nature: amending }
  - { spec: "030-operational-verbs", unit: "crates/rahi-ops/src/backup.rs", nature: amending }
  - { spec: "030-operational-verbs", unit: "crates/rahi-ops/src/rauthy_api.rs", nature: amending }
  - { spec: "031-single-container-packaging", unit: "crates/rahi-ops/src/rauthy_env.rs", nature: additive }
  - { spec: "032-cluster-topology", unit: "scripts/k8s-validate.sh", nature: amending }
  - { spec: "032-cluster-topology", unit: "deploy/README.md", nature: amending }
references:
  - { unit: { kind: file, path: "docs/design/01-consumer-contract.md" }, role: context }
  - { unit: { kind: file, path: "docs/design/02-operational-prerequisites.md" }, role: context }
summary: >
  The owner decided on 2026-09-23 (hiqlite proposal D-14, recorded at
  bartekus/hiqlite@da4c910) that an N=3 cell is one namespace holding two
  StatefulSets, rahi and rauthy, three replicas each, one hiqlite node per
  pod, and that the single-container supervised composition of spec 031
  stays the N=1 profile. This spec is rahi's share of that decision. It
  amends 031 and 032 for N=3 only: rahi starts without rauthy, reaches it
  only through the internal ClusterIP Service
  rauthy-internal.<namespace>.svc.cluster.local over Rauthy's TLS or a
  mesh's mTLS, fails readiness and never liveness while rauthy is
  unavailable, and keeps peer discovery independent of readiness. It
  carries the proposal's routing, NetworkPolicy, per-StatefulSet
  disruption and storage requirements, and backup obligations. It changes
  no dependency and claims no N=3 support: that follows only from recorded
  qualification on a named release.
---

# 044: N=3 split composition

## 1. Purpose

Spec 031 made the cell one container: `rahi supervise` starts Rauthy,
reaches it on loopback, and dies with it. Spec 032 multiplied that
container into a three-replica StatefulSet (`deploy/n3`), which puts four
raft groups in every pod, two sequential hiqlite shutdowns inside one
grace, and a Rauthy fault behind every loss of a Rahi voter. That overlay
has never run on three pods. The owner has decided (D-14, fixed input to
this spec) that N=3 is two StatefulSets instead. The single-container
design gave four properties for free (loopback routing, die-together, one
volume cut for backup, one shutdown); this spec states what replaces each
at N=3 so no guarantee is lost silently, and leaves N=1 exactly as 031 and
043 define it.

## 2. Territory

A new overlay `deploy/n3-split/` (two StatefulSets, their headless and
client-facing Services, the `rauthy-internal` Service, NetworkPolicies,
PodDisruptionBudgets, the migrate Job reference); the validation script's
checks for it (032, amending); `serve`'s startup and readiness when
identity is remote (030, 020, 021, amending); the backup verb's Rauthy
address (030, amending); rendering Rauthy's environment for a standalone
StatefulSet (031, additive); the operator README (032, amending).

## 3. Behavior

### Proposed amendments to complete specs (N=3 only)

These restate, for N=3 only, what 031 and 032 require. At ratification each
becomes a dated decision in the amended spec; 031's and 032's existing text
stays as written and governs N=1 unchanged.

- **A-031 (supervision).** 031 B-3 (supervise, die-together, loopback
  health poll, client bootstrap before serve) does not apply at N=3. At N=3
  there is no supervisor: the rahi pod runs `rahi serve` with identity in
  remote mode (B-5 below), and the rauthy pod runs the patched Rauthy image
  directly. 031 B-1's one-volume layout is per StatefulSet: `/data/hiqlite`
  and `/data/keys` in rahi's claim, Rauthy's data in rauthy's claim.
- **A-032 (topology).** 032 B-1's "one container per pod (the supervisor)"
  and "rauthy's `HQL_NODES` rendered the same way with its own ports" are
  replaced at N=3 by two StatefulSets, each rendering its own peers from its
  own ordinal and headless Service. 032 B-4's startup probe on `/readyz`
  becomes a probe that does not depend on Rauthy (B-5). 032 B-6's statement
  that "the cache group is per node" is corrected in both profiles: the
  cache is a replicated raft group and holds the leases behind rahi's
  fencing tokens (proposal R-f). `deploy/n3` stays as the co-located
  overlay and is labelled unqualified (043 P-5).

### Routing and trust (replaces loopback)

- **B-1 (one address).** At N=3 rahi addresses Rauthy only through
  `https://rauthy-internal.<namespace>.svc.cluster.local:<port>`, a ClusterIP
  Service with no ingress, `LoadBalancer` or `NodePort`. Never a pod address,
  never the public ingress. The back channel (discovery, JWKS, token,
  admin API) and the backup verb's `POST /backup` with the admin token use
  it and no other address.
- **B-2 (encryption).** That path is encrypted by Rauthy's native TLS, with
  rahi verifying the certificate against a mounted CA, or by a service
  mesh's mTLS. A mesh qualifies only with a shutdown order that keeps its
  proxy up until each application's hiqlite shutdown has finished. The
  chosen mechanism is P-1. Plaintext on this path is a validation failure.
- **B-3 (NetworkPolicy).** Ingress to Rauthy's HTTP port is admitted only
  from pods carrying rahi's workload labels, plus the public path if Rauthy
  serves users directly (P-2). Rauthy's hiqlite ports (8100, 8200) admit
  only Rauthy's pods; rahi's (8300, 8400) only rahi's pods. No hiqlite port
  is reachable through ingress. NetworkPolicy is access control, not
  encryption; hiqlite's own raft and API encryption inside each StatefulSet
  is proposal D-5 and stays open.

### Readiness and liveness (replaces die-together)

- **B-4 (liveness).** Rahi's liveness never depends on Rauthy. An
  unreachable or unready Rauthy never restarts a rahi pod and never makes
  rahi exit. Rahi's own terminal store failure is a separate matter:
  draft 043 B-7 proposes an exit for it, and this spec neither depends on
  nor changes that proposal.
- **B-5 (startup).** `serve` in remote identity mode starts, opens its
  store, and serves its probes without Rauthy. Discovery is retried in the
  background; the startup probe checks liveness and the store, never
  Rauthy. (Today `serve` fetches discovery before serving and fails the
  boot without it, `serve.rs` near line 379; this spec changes that for
  remote mode only.)
- **B-6 (readiness).** `/readyz` fails while rahi cannot complete an
  authenticated request to Rauthy's `/auth/v1/ready` through B-1's address,
  and names `identity` as the failed component; it recovers when Rauthy
  answers again. A transient failure and a terminal one look the same here
  and are treated the same: not ready, still live.
- **B-7 (discovery independent of readiness).** Each StatefulSet's headless
  Service sets `publishNotReadyAddresses: true`, so a pod not ready because
  Rauthy is away stays addressable to its raft peers, and a bootstrapping
  pod is addressable before it is ready. The client-facing Service is a
  separate object that honors readiness.

### Disruption and storage

- **B-8 (per StatefulSet).** Each StatefulSet has a PodDisruptionBudget
  `maxUnavailable: 1`, required pod anti-affinity across nodes within the
  application, zone spread when the cluster has zones, one `ReadWriteOnce`
  claim per replica on storage not shared between replicas and not a
  network filesystem, `node_id` equal to its own ordinal plus one, peers
  fixed before first start, and `cache_storage_disk = true` (proposal D-3).
  `terminationGracePeriodSeconds` is set per StatefulSet from a measured
  single-node stop at N=3 plus margin (043 B-9's method), never below the
  measured maximum; the hiqlite pre-shutdown delay (9.5 s at N>1) is inside
  that measurement.
- **B-9 (the migrate Job).** 036's manifest adoption and migration cutover
  run once per cell as a Job before the rahi StatefulSet rolls, as 032's
  migrate Job does today, restoring the `--adopt-manifest` flag the
  co-located overlay drops (proposal R-d).

### Backup (replaces the single-volume cut)

- **B-10 (hot archive).** The scheduled backup remains 030 B-5's verb run
  against the rahi leader, reaching Rauthy's backup through B-1. Under the
  split it is two images taken at two instants; the README states the
  skew between the clusters as part of the DR recovery point, not hidden.
- **B-11 (coherent export).** An export that must be coherent across both
  clusters (a migration from a split cell, an offline DR archive) scales
  both StatefulSets to 0, waits until every pod of both has terminated and
  released its `hiqlite-owner.lock`, and only then exports both volumes
  offline. The offline export entry point is hiqlite's (proposal `034` B-2)
  and does not exist in any published release; B-11 is therefore an
  obligation this spec records and cannot satisfy until that release is
  adopted.
- **B-12 (archive identity).** The cell archive names both images, both
  applied log ids and both digests in one manifest (proposal D-7, R-g).

## 4. Functional requirements

- **FR-001.** `scripts/k8s-validate.sh` renders `deploy/n3-split` and
  asserts: two StatefulSets; one container per pod in each; the
  `rauthy-internal` Service is ClusterIP with no ingress path, no
  `LoadBalancer`, no `NodePort`; NetworkPolicies as B-3; a PDB
  `maxUnavailable: 1` per StatefulSet; required anti-affinity; no
  `ReadWriteMany`; headless Services with `publishNotReadyAddresses: true`;
  rahi's liveness and startup probes not on a path that consults Rauthy;
  and refuses a fixture violating each, generated by the script the way
  032's `ReadWriteMany` fixture is.
- **FR-002.** Remote identity mode is testable without a cluster: a real
  rahi `serve` and a real patched Rauthy on separate addresses with TLS.

## 5. Acceptance criteria

- **AC-1.** FR-001 passes and refuses each negative fixture.
- **AC-2.** With the real patched Rauthy stopped: `serve` in remote mode
  starts, `/healthz` is `200`, `/readyz` is `503` naming `identity`; with
  Rauthy started, `/readyz` becomes `200` without a rahi restart; stopping
  Rauthy again returns `/readyz` to `503` and the process stays up.
- **AC-3.** A capture on the rahi-to-Rauthy path shows no plaintext HTTP.
- **AC-4 (operator check, never `verify:cli`).** On a real three-node
  cluster, per proposal stage 8: both StatefulSets up; a Rauthy outage
  makes rahi pods unready and restarts none; a PDB-respecting drain of each
  node; the measured per-pod stop within grace with margin. Recording the
  procedure does not satisfy this criterion; running it on a named release
  does.

## 6. Out of scope

Any dependency version change (043 adopts N=1 artifacts; an N=3-qualified
release is its own later adoption, and no version changes inside a
migration window, proposal D-10); the N=1-to-N=3 migration protocol
(proposal section 7; its own spec); hiqlite transport encryption inside a
StatefulSet (proposal D-5); Statecraft's cross-replica serialization.

**Obligations of other specs that this spec does not close:**

- **040** (draft): `/binding` is per pod and reports the epoch the pod
  booted under; its exposure beside `/metrics` must be verified on this
  layout's paths. Not evidenced by N=1 runs.
- **041** (draft): the epoch append at the deploy step, concurrent deploy
  arbitration and fencing, the interruption between migration, manifest
  transition and epoch append, and its own N=3 acceptance; under this
  layout the deploy step is B-9's Job. Deployment authority stays outside
  the chassis (041 B-10a).
- **042** (complete at N=1): ledger lifetime identity, sealing and
  archived-identity recovery across a three-voter chain; owed, not
  re-evidenced here.
- **032 AC-2's rollout check** was written for the co-located overlay and is
  not satisfied for this one by anything above.

N=3 support is not claimed by this spec, by its acceptance, or by any N=1
evidence.

## 7. Resolved decisions

None. D-14 is an owner decision recorded in the hiqlite repository and is
this spec's fixed input, not one of its decisions.

**Blocking governance conflict (found 2026-09-23).** Spec 000 freezes the
`one-deployment-unit` anchor as `unamendable`: "A governed cell is one
container, one volume, one public origin; rauthy is inside it and reached
only through the app's origin", restated as constitution principle VI. The
constitution's amendment clause allows ordinary specs to amend it only
where the amendment "does not contradict a `specs/000` `unamendable`
anchor". An N=3 cell of two StatefulSets with Rauthy outside rahi's
container and volume contradicts that anchor as written, and nothing in
this spec's mechanism can honor both. This spec therefore cannot be
approved through the ordinary flow. What would unblock it is an owner act
at the bootstrap tier that scopes the anchor to the N=1 profile (for
example: the anchor governs the N=1 cell; an N=3 cell is one namespace and
one public origin, with Rauthy reached only through the app's origin by
users and only through an internal, encrypted, policy-restricted Service
by rahi). Whether spec 000's freeze surface can be changed at all, and by
what instrument, is the owner's to decide; this draft proposes no text for
spec 000 and changes nothing there. The rest of this spec is drafted so it
is ready once that question is answered.

Still open before approval:

- the bootstrap-tier question above (blocking);

- P-1 to P-4 below;
- whether B-11's coherent export waits for a hiqlite release carrying the
  offline export, or this spec is approved with B-11 held (recommended).

### Proposals (2026-09-23)

- **P-1 (encryption mechanism).** Rauthy native TLS with a CA mounted into
  rahi (recommended: no extra process in the shutdown path); a mesh only
  with a qualified shutdown order.
- **P-2 (public path).** Rauthy serves users only through rahi's origin, as
  at N=1, so its HTTP port admits rahi's pods only (recommended; keeps 000's
  one-origin invariant).
- **P-3 (bootstrap ordering).** `OrderedReady` for first bootstrap until
  hiqlite F-118 (node-1 replacement) is repaired, `Parallel` afterwards
  (proposal section 10).
- **P-4 (Statecraft coordination).** Statecraft's rule that Rauthy is never
  a separate workload holds for N=1 and must be amended by a Statecraft spec
  adopting D-14; this spec does not do it and does not depend on it.

## Verification

```verify:cli
scripts/k8s-validate.sh
cargo test -p rahi-cli --locked --test split
```
