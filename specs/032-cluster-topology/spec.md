---
id: "032-cluster-topology"
title: "Cluster topology: N=1 primary, N=3 StatefulSet on Kubernetes, one key set, S3 backups"
status: approved
kind: "tooling"
domain: "ops"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: complete
risk: high
wave: 3
depends_on:
  - "031-single-container-packaging"
establishes:
  - "deploy/k8s/kustomization.yaml"
  - "deploy/k8s/configmap.yaml"
  - "deploy/k8s/statefulset.yaml"
  - "deploy/k8s/service.yaml"
  - "deploy/k8s/ingress.yaml"
  - "deploy/k8s/migrate-job.yaml"
  - "deploy/k8s/backup-cronjob.yaml"
  - "deploy/k8s/servicemonitor.yaml"
  - "deploy/k8s/secret.example.yaml"
  - "deploy/n3/"
  - "deploy/README.md"
  - "scripts/k8s-validate.sh"
  - "crates/rahi-store/tests/attach.rs"
extends:
  - { spec: "011-store-hiqlite", unit: "crates/rahi-store/src/store.rs", nature: additive }
  - { spec: "011-store-hiqlite", unit: "crates/rahi-store/src/backup.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-ops/Cargo.toml", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-ops/src/lib.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-ops/src/preflight.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/src/lib.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/src/serve.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/src/verbs.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/tests/cli.rs", nature: additive }
  - { spec: "031-single-container-packaging", unit: "crates/rahi-ops/src/first_boot.rs", nature: additive }
  - { spec: "031-single-container-packaging", unit: "crates/rahi-ops/src/rauthy_env.rs", nature: additive }
  - { spec: "031-single-container-packaging", unit: "crates/rahi-ops/src/supervise.rs", nature: additive }
  - { spec: "031-single-container-packaging", unit: "crates/rahi-ops/tests/first_boot.rs", nature: additive }
  - { spec: "031-single-container-packaging", unit: "docker/rauthy.env.template", nature: additive }
  - { spec: "001-agentic-harness", unit: "Makefile", nature: additive }
  - { spec: "001-agentic-harness", unit: ".github/workflows/govern.yml", nature: additive }
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

- **D-2 (2026-09-10, build session; reads the Territory, B-1, B-2).** The
  Territory says nothing in `crates/` changes because the chassis already
  reads peers and keys from the environment. It did not: spec 011's
  `StoreConfig::from_config` fixes a single voter and says in its own
  comment that "callers that run N=3 set `nodes` afterwards (spec 032)",
  no verb read a node id or a peer list, `first-boot --export` (B-2) did
  not exist, and a read-only Secret mount fails spec 030's `0700` and
  `0600` mode checks (the kubelet owns the files and grants the pod's
  fsGroup a read bit). Every B-n of this spec therefore needs chassis code,
  and this session added it as additive `extends` edges on the owning
  units rather than holding the spec: `RAHI_HIQ_NODES`, `RAHI_HIQ_NODE_ID`
  (or `RAHI_POD_INDEX` plus one), `RAHI_RAUTHY_HQL_NODES`, and
  `RAHI_RAUTHY_HQL_LISTEN_ADDR` are read into both clusters' configuration
  (`rahi_ops::store_config`, `rauthy_env::HqlPorts`); rauthy's rendered
  environment carries `HQL_NODE_ID`, `HQL_NODES`, and the listen addresses
  from the same values; `rahi first-boot --export` mints a set into a
  private temporary directory and prints it as a `Secret`; and
  `KeySet::check` accepts a directory nobody can write to (no group or
  world write bit, and a create probe fails) whose files carry no write
  bit and no world bit, while a writable directory is still held to
  `0700` and `0600`. Spec 030's text stays true for the volume first boot
  writes; the read-only mount is a directory first boot never writes, and
  `first_boot::layout` skips the chmod on it. Rejected: an initContainer
  that copies the Secret into the volume with `0600`, because B-2 says the
  mount is read-only and the point of one Secret is that a replica's
  volume never holds a copy the operator did not put there.
- **D-3 (2026-09-10, build session; reads B-5, B-6).** Spec 030's
  `backup` and `migrate` open the node themselves, which inside a pod
  whose `serve` holds the node would start a second hiqlite on a locked
  directory (hiqlite without `auto-heal` panics on the lock; with it, it
  wipes the state machine). B-5 asks the CronJob to run the verb against
  the leader and B-6 asks for migrations as a Job before the rollout, and
  neither is possible without a client mode. `Store::attach` (spec 011's
  unit, additive) reaches this replica's own running node as a hiqlite
  client, answers `is_leader` from the cluster's Raft metrics, and lists
  local backups from the filesystem, since hiqlite only lists them from
  inside the node; `Store::connect` reaches the cluster as a pure client
  through the peers' API addresses, for a process with no volume, and
  answers `is_leader` with `true` because hiqlite routes every write to
  the leader. `Booted::open_or_attach` chooses: `RAHI_STORE_CLIENT=true`
  connects, an existing app lock file attaches, otherwise the node opens
  as before; `backup` and `migrate` use it, `serve` never does. The
  CronJob therefore `exec`s `rahi backup --to s3://` into the pods in
  ordinal order (a follower exits 2, the leader's run ends the job), with
  the S3 credentials loaded into the pod from the `rahi-s3` Secret. The
  migration Job runs the new image with `RAHI_STORE_CLIENT=true` and the
  keys Secret and no volume. At N=3 the entrypoint's own `migrate` step
  (spec 031 B-3) is wrapped in the overlay so a follower's exit 2 does not
  stop the pod on a fresh cluster's first start; the base at N=1 uses the
  image's entrypoint unchanged. Rejected: a Job pod that mounts a
  replica's volume (a `ReadWriteOnce` claim is attached to its pod, and
  the verb would still face the lock); lifting spec 030 B-4's follower
  refusal so any pod could migrate (that is 030's requirement text).
  Spec 030 D-3's hold stands: against a real rauthy the backup verb's
  admin token is refused on the backup routes, and the README names that
  failure as the known hold rather than a manifest defect.
- **D-4 (2026-09-10, build session; reads B-1, B-7).** The overlay lives
  at `deploy/n3/`, a sibling of `deploy/k8s/`, because kustomize refuses
  an overlay nested inside its base; the peer lists are a `ConfigMap`
  resource with block scalars (one peer per line, as hiqlite and the
  chassis both read them) rather than generator literals, because the
  renderer folds long plain scalars across lines and the validator must
  read the peers line by line. `scripts/k8s-validate.sh` runs the FR-001
  fixture at the end of its own run, writing a temporary kustomization
  beside the base (kustomize only follows relative roots inside the
  tree) and expecting its own refusal. The script joins `make ci` and the
  govern workflow as a guarded step, both `extends` edges on spec 001.

## 8. Status

- **2026-09-10.** B-1 to B-7, FR-001, FR-002, AC-1, and AC-2 hold.
  `scripts/k8s-validate.sh` exits 0 on `deploy/k8s` and `deploy/n3` and
  refuses the FR-001 fixture; the rendered `RAHI_HIQ_NODES` for three
  replicas names three peers on 8400 and 8300 and none on 8100 or 8200
  (FR-002, asserted by the script and by
  `a_replica_renders_its_ordinal_and_its_peers_into_rauthys_environment`
  for rauthy's side); the rollout check is recorded in `deploy/README.md`
  (AC-2, per D-1). The chassis side of D-2 and D-3 is covered by
  `crates/rahi-store/tests/attach.rs` (attach, connect, and that neither
  opens a data directory),
  `backup_inside_a_running_replica_attaches_to_its_node_instead_of_opening_a_second`
  and `first_boot_export_renders_a_secret_with_every_key_and_touches_no_volume`
  in `crates/rahi-cli/tests/cli.rs`, and
  `a_read_only_key_mount_is_accepted_and_a_writable_wide_one_is_not` in
  `crates/rahi-ops/tests/first_boot.rs`. `cargo test --workspace --locked`,
  clippy, fmt, and deny pass. Not exercised here: a real three-pod cluster
  and a bucket (the operator check of AC-2), and the rauthy part of the
  in-cluster backup, which spec 030 D-3 holds.

## Verification

```verify:cli
scripts/k8s-validate.sh
```
