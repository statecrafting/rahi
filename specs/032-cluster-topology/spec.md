---
id: "032-cluster-topology"
title: "Cluster topology: N=1 primary, N=3 StatefulSet on Kubernetes, one key set, S3 backups"
status: approved
kind: "tooling"
domain: "ops"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: in-progress
risk: high
wave: 3
depends_on:
  - "031-single-container-packaging"
establishes:
  - "deploy/k8s/kustomization.yaml"
  - "deploy/k8s/statefulset.yaml"
  - "deploy/k8s/service.yaml"
  - "deploy/k8s/ingress.yaml"
  - "deploy/k8s/backup-cronjob.yaml"
  - "deploy/k8s/servicemonitor.yaml"
  - "deploy/k8s/secret.example.yaml"
  - "deploy/README.md"
  - "scripts/k8s-validate.sh"
summary: >
  What multiplies and what does not. At N=1 nothing changes. At N=3 every
  replica is identical: a StatefulSet pod running the supervisor, with its
  own volume, two Raft clusters per replica (rauthy's and the app's), peers
  discovered by stable pod DNS, one key set injected as a Secret and
  custodied once, the public origin on an Ingress that terminates TLS and
  keeps /metrics off it, a CronJob that runs backup --to s3:// against the
  leader, and a ServiceMonitor example. Rate limiting is per node and says
  so. A shared volume between Raft members is refused by the manifests
  themselves. Carries enrahitu://030.
---

# 032: Cluster topology

## 1. Purpose

Constitution VI and VIII at scale. The manifests are the operator's
runbook made executable; the validation script is what CI runs so a
manifest that drifts from the chassis's port and volume contract fails the
gate. The first target is a Hetzner Kubernetes cluster with Hetzner object
storage as the S3 endpoint.

## 2. Territory

The `deploy/k8s/` tree, its README, and the validation script. Nothing in
`crates/` changes; the chassis already reads peers and keys from the
environment (011 B-1, 030).

## 3. Behavior

- **B-1 (StatefulSet).** `replicas: 1` by default; a documented patch sets
  `3`. One container per pod (the supervisor), `volumeClaimTemplates` for
  `/data`, `podManagementPolicy: Parallel`, a headless Service for stable
  DNS, and `RAHI_HIQ_NODES` rendered from the pod ordinal so each replica
  knows its peers. rauthy's `HQL_NODES` is rendered the same way with its
  own ports.
- **B-2 (keys).** All key material is one Secret mounted read-only at
  `/data/keys` when present; first boot detects mounted keys and generates
  nothing. `secret.example.yaml` documents every key and how `rahi
  first-boot --export` produces the initial set once, for the operator to
  custody.
- **B-3 (ingress).** TLS terminates at the Ingress; `RAHI_PUBLIC_URL` is
  `https`; `trusted_proxy_hops` is `1`. `/metrics` is excluded from the
  Ingress path list and scraped in-cluster by the ServiceMonitor.
- **B-4 (probes).** `livenessProbe` on `/healthz`, `readinessProbe` on
  `/readyz`, `startupProbe` on `/readyz` with a budget of ninety seconds
  for the two elections.
- **B-5 (backups).** A CronJob runs `rahi backup --to s3://<bucket>/<app>/`
  with the S3 endpoint and credentials from a Secret; the job targets the
  leader (it exits 2 on a follower and the job retries the next pod).
  Retention is a documented bucket lifecycle rule, not a chassis concern.
- **B-6 (what does not span).** The README states: the cache group is per
  node; rate limiting is per node so N replicas admit N times the ceiling;
  migrations are a Job, run once, before the rollout.
- **B-7 (validation).** `scripts/k8s-validate.sh` renders the kustomization
  and asserts: no `ReadWriteMany` volume, one container per pod, the
  volume mounted at `/data`, `/metrics` absent from the Ingress, both probe
  paths present, and `kubeconform` passes when installed.

## 4. Functional requirements

- **FR-001.** `scripts/k8s-validate.sh` exits 0 on the shipped manifests
  and 1 on a fixture that adds a `ReadWriteMany` claim.
- **FR-002.** The rendered `RAHI_HIQ_NODES` for ordinal 1 of 3 names all
  three peers with the app's Raft port and none with rauthy's.

## 5. Acceptance criteria

- **AC-1.** `scripts/k8s-validate.sh` exits 0.
- **AC-2.** The rollout check is written into the README as a runnable
  operator procedure: bring up the three-replica StatefulSet, confirm
  `/readyz` on every pod, trigger the backup CronJob once, and confirm one
  archive in the bucket. Recording that procedure satisfies this
  criterion. Running it needs the reference cluster and its S3 endpoint,
  so it is an operator check and never a `verify:cli` command.

## 6. Out of scope

Helm packaging; multi-cluster; the object store's own lifecycle rules.

## 7. Resolved decisions

- **D-1 (2026-09-05, human decision).** AC-2 is satisfied by recording the
  rollout and backup procedure in the README, not by executing it. A build
  session has no cluster and no bucket, and holding the spec at
  `implementation: in-progress` for a criterion no session can run would
  stall 034, which depends on this spec. The operator runs the recorded
  procedure out of band; a failure there is a defect report against this
  spec, not a reason to withhold completion.

## Verification

```verify:cli
scripts/k8s-validate.sh
```
