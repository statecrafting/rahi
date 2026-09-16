# Deploying a cell on Kubernetes

The manifests under `deploy/k8s/` are the operator's runbook made
executable (spec 032). They deploy one cell: a StatefulSet whose every pod
runs the supervisor of spec 031 (rauthy and the app in one container), owns
one volume, and carries both Raft clusters. `deploy/n3/` is the documented
patch that sets three replicas. `scripts/k8s-validate.sh` renders both and
holds them to the chassis's port and volume contract; CI runs it.

The first target is a Hetzner Kubernetes cluster with Hetzner object
storage as the S3 endpoint. Kubernetes 1.28 or later: the pod ordinal
reaches the container through the `apps.kubernetes.io/pod-index` label.

No image is published yet. The manifests name
`ghcr.io/statecrafting/rahi:0.1.0`, which is what `image.yml` pushes when
the `v0.1.0` tag is pushed (spec 039 B-3): the version only, both
architectures, and never `latest`. Until that tag exists, build the image
from `docker/Dockerfile`, push it where your cluster can pull it, and
substitute its name and tag (`kustomize edit set image`).

A cell in another repository does not copy this recipe. It builds on
`ghcr.io/statecrafting/rahi-runtime:<version>`, which carries the pinned
rauthy, the non-root user, `/data`, and the entrypoint, and adds its own
binary at `/usr/local/bin/rahi` and its page at
`/usr/local/share/rahi/static` (spec 039 B-4). `scripts/k8s-validate.sh`
refuses a render that names `:latest` or a cell image outside
`ghcr.io/statecrafting/`.

## What is in the tree

| File | What it declares |
|---|---|
| `k8s/configmap.yaml` | the cell's environment: `RAHI_PUBLIC_URL`, the trusted hop, the listeners, the peer lists |
| `k8s/statefulset.yaml` | one container per pod, `volumeClaimTemplates` for `/data`, the keys mounted read-only at `/data/keys`, the probes |
| `k8s/service.yaml` | the headless Service that names the pods (`rahi-<n>.rahi-hl`) and the ClusterIP Service the Ingress reaches |
| `k8s/ingress.yaml` | TLS termination, `/metrics` closed at the edge |
| `k8s/migrate-job.yaml` | the Job that runs a new image's migrations before a rollout |
| `k8s/backup-cronjob.yaml` | the nightly `rahi backup --to s3://` against the leader, with the RBAC it needs |
| `k8s/servicemonitor.yaml` | the in-cluster scrape of `/metrics` |
| `k8s/secret.example.yaml` | the shape of `rahi-keys` and `rahi-s3`, which the operator custodies and applies by hand |
| `n3/` | the three-replica patch: peers, replicas, anti-affinity, and the entrypoint wrapper |

## Choose N before the first rollout

At N=1 nothing about the chassis changes: one pod, one volume, both
clusters single-voter, the image's own entrypoint. At N=3 every replica is
identical and each knows its two peers by stable pod DNS. The peer lists
are Raft membership, and hiqlite reads them at the node's first start, so
choose the topology before the first rollout: `deploy/k8s` for one
replica, `deploy/n3` for three. Growing a running N=1 cell to three is not
a procedure this repository has recorded.

## Keys: one Secret, custodied once

All key material is one Secret mounted read-only at `/data/keys` on every
replica. First boot detects the mounted keys and generates nothing, which
is how three pods share one ledger key, one session key, one backup key,
and one rauthy admin. Mint the set once, anywhere the image runs:

```sh
docker run --rm -e RAHI_PUBLIC_URL=https://cell.example.com \
  ghcr.io/statecrafting/rahi:0.1.0 first-boot --export > rahi-keys.yaml
```

The document is a complete `Secret` named `rahi-keys` (the shape is in
`k8s/secret.example.yaml`). It holds every key of the deployment,
including rauthy's bootstrap admin password: custody it as you would a
root credential, and keep it where a restore can find it. Apply it before
the StatefulSet.

The S3 credentials are a second Secret, `rahi-s3`, whose keys are the
`RAHI_BACKUP_S3_*` variables of `rahi backup`. The StatefulSet loads it
when present, because the backup verb runs inside the pod.

## Rollout

```sh
kubectl create namespace rahi
kubectl -n rahi apply -f rahi-keys.yaml
kubectl -n rahi apply -f rahi-s3.yaml            # your copy of the example
kubectl apply -k deploy/k8s                      # or deploy/n3
kubectl -n rahi rollout status statefulset/rahi
```

Set `RAHI_PUBLIC_URL` and the Ingress host to your origin first (a
kustomize patch on the ConfigMap and the Ingress, or edit the files in a
copy). TLS terminates at the Ingress; the cell sees one trusted hop
(`RAHI_TRUSTED_PROXY_HOPS=1`) and derives its issuer from the `https`
public URL.

The pods start in parallel, because a Raft cluster needs its peers up to
elect. The startup probe allows ninety seconds on `/readyz` for the two
elections (rauthy's cluster and the app's); readiness stays on `/readyz`
and liveness on `/healthz`.

## Migrations are a Job, run once, before the rollout

At N=1 a container start is the deployment and the image's entrypoint runs
`rahi migrate` before `supervise`. At N=3 the entrypoint's migrate step is
wrapped so a follower's refusal (exit 2) does not stop the pod: on a fresh
cluster the leader applies the migrations and the followers carry on.

For every rollout after the first, run the migrations before rolling the
image, with the image that carries them:

```sh
kustomize edit set image ghcr.io/statecrafting/rahi=ghcr.io/statecrafting/rahi:<new version>
kubectl -n rahi delete job rahi-migrate --ignore-not-found
kubectl apply -k deploy/n3 --selector app.kubernetes.io/name=rahi-migrate
kubectl -n rahi wait --for=condition=complete job/rahi-migrate --timeout=300s
kubectl apply -k deploy/n3
kubectl -n rahi rollout status statefulset/rahi
```

The Job runs `rahi migrate` with `RAHI_STORE_CLIENT=true`: a pure client
of the peers in `RAHI_HIQ_NODES`, no volume, no node of its own. hiqlite
routes the migration to the leader and Raft replicates it to every
replica, so a new pod finds the store current when its `serve` checks.

## Backups go to S3 from the leader

`k8s/backup-cronjob.yaml` runs nightly. The job's container carries
`kubectl` and walks the pods in ordinal order, running
`rahi backup --to s3://<bucket>/<app>/` inside each. The verb attaches to
the pod's running node (it never starts a second one against the locked
volume), builds one archive from that replica's snapshot, its rauthy, and
its keys, and uploads it. On a follower it exits 2 and the job moves to
the next pod; the leader's run exits 0 and ends the job.

Change the target in the CronJob's `RAHI_BACKUP_TARGET`. Retention is the
bucket's lifecycle rule, not the chassis's. A restore is spec 030's
`rahi restore <archive>` on a fresh volume before the first start, and it
is single-shot: the archive's keys are the cell's keys, so `rahi-keys`
must be the same set the archive carries.

One known hold, inherited from spec 030 D-3: against a real rauthy the
backup verb's admin token is refused on the backup routes (they want an
admin session). Until that is resolved the CronJob's run fails at the
rauthy part with rauthy's error; the failure is the known hold, not a
manifest defect.

A second gap sits on the restore side. `rahi restore` places rauthy's
snapshot under `/data/restore/rauthy/` and names it in the marker, and
nothing hands it to rauthy on the next start: the supervisor starts rauthy
from its rendered environment only. A restore therefore recovers the
app's store, its decision chain, and the key set; rauthy comes up on its
own directory as it finds it, which on a fresh volume is empty. Every
principal id in the app's rows is rauthy's `sub`, so a cell restored
without rauthy's state holds rows no user can reach until rauthy's
database is restored by hand. `docs/design/01-consumer-contract.md` tracks
both gaps.

## Token lifetimes and revocation

Spec 025 B-5 asks the deployment documentation to state the revocation
bound. As built:

- A browser session holds a cached assertion for fifteen minutes and then
  renews through rauthy, re-reading the user's roles. A user disabled in
  rauthy loses the session at the next renewal, so within fifteen minutes.
- A bearer access token is validated locally against rauthy's key set,
  never introspected, and accepted until its `exp` plus sixty seconds of
  leeway. rahi sets no token lifetime, so the lifetime is rauthy's client
  default: 1800 seconds in rauthy 0.36.2. The `jti` deny-list the resource
  server consults exists with its writer, and nothing calls the writer yet
  (neither logout nor a verb), so a bearer token cannot be revoked before
  it expires.
- `preflight` does not report the bound yet.

## Denials at a stop

Spec 035 B-4 asks the deployment documentation to state what a decision id
in a `403` promises. As built:

- A denial answered with a decision id is in the chain unless one of four
  things happened. Three are counted by name on `/metrics`, and each writes
  one `ERROR rahi.decision` line to stderr naming the id and the cause:
  - the denial queue was full: `kernel_decisions_dropped_total`;
  - the append failed: `kernel_ledger_failures_total`;
  - the stop's drain bound expired first:
    `kernel_decisions_abandoned_total`, plus one `WARN rahi.decision` line
    naming how many were abandoned.
- The fourth cannot be counted. A process killed without a stop signal
  (SIGKILL, an out-of-memory kill, power loss) loses whatever its queue
  held, and nothing reports it.
- The drain is bounded. It is not a guarantee against process death or
  storage failure. On SIGTERM, `serve` lets in-flight requests finish, then
  gives the queue `RAHI_DENIAL_DRAIN_TIMEOUT_SECS` (default 5) before it
  shuts the store; what the store has not taken by then is abandoned and
  counted. If the supervisor stops waiting for `serve` first (fifteen
  seconds, spec 031 D-4, which open streams and slow connections can use
  up), the records still owed are counted as abandoned as the process ends.
- An id names one denial across replicas. It is
  `kernel:<nonce>:<node>:<counter>`, where the node is the replica's
  `RAHI_HIQ_NODE_ID` (the pod ordinal plus one). One residual remains (spec
  035 D-3): a replica whose denials were all lost, and which restarts before
  any replica appends, boots on the same head as the same node and can mint
  again the ids its lost denials' callers hold. Every boot writes one
  `INFO rahi.decision` line naming its nonce and node, so a re-mint shows as
  two boot lines of one node on one nonce.

## What does not span replicas

- **The cache group is per node.** Nothing durable lives in it.
- **Rate limiting is per node.** N replicas admit N times the ceiling the
  manifest declares. Size the ceiling for one node and multiply.
- **Migrations are a Job**, run once, before the rollout, as above.
- **`/metrics` is per pod.** The ServiceMonitor scrapes every pod through
  the ClusterIP Service; aggregate in Prometheus, never at the Ingress,
  where the path is closed.
- **A volume is per pod.** `ReadWriteOnce`, from the claim template. Two
  Raft members on one volume is corruption by construction, and the
  validation script refuses any `ReadWriteMany` claim in the render.

## Validation

```sh
scripts/k8s-validate.sh
```

Renders `deploy/k8s` and `deploy/n3` and asserts: no `ReadWriteMany`
volume, one container per pod, the claim mounted at `/data`, the keys
mounted read-only at `/data/keys`, `/metrics` absent from the Ingress,
both probe paths present, the peer list sized to the replicas and carrying
the app's ports (8400, 8300) and none of rauthy's (8100, 8200), and
`kubeconform` when installed. It ends with the fixture of spec 032 FR-001:
a kustomization that adds a `ReadWriteMany` claim must be refused.

## The rollout check (AC-2)

This is the operator's check on the reference cluster. Recording it here
satisfies the criterion (spec 032 D-1); running it needs the cluster and
its bucket.

1. Apply `rahi-keys` and `rahi-s3`, then `kubectl apply -k deploy/n3`, and
   wait: `kubectl -n rahi rollout status statefulset/rahi` reports three
   ready replicas within the startup budget.
2. Confirm `/readyz` on every pod, through the pod itself and not the
   Service. The readiness probe is `/readyz`, so the pod's ready condition
   is that answer; to see the body, forward to one pod at a time:

   ```sh
   kubectl -n rahi get pods -l app.kubernetes.io/name=rahi \
     -o custom-columns=NAME:.metadata.name,READY:.status.containerStatuses[0].ready
   for i in 0 1 2; do
     kubectl -n rahi port-forward pod/rahi-$i 18443:8443 >/dev/null 2>&1 &
     sleep 1; curl -s http://127.0.0.1:18443/readyz; echo " rahi-$i"; kill %1; wait
   done
   ```

3. Trigger the backup once and watch it walk to the leader:

   ```sh
   kubectl -n rahi create job --from=cronjob/rahi-backup rahi-backup-check
   kubectl -n rahi wait --for=condition=complete job/rahi-backup-check --timeout=300s
   kubectl -n rahi logs job/rahi-backup-check
   ```

   The log names each pod tried; followers say `exit 2`, the leader's line
   ends with `is the leader; done`.

4. Confirm one archive in the bucket:

   ```sh
   aws --endpoint-url https://fsn1.your-objectstorage.com \
     s3 ls s3://rahi-backups/cell/
   ```

   One `rahi-backup-<utc>.tar.age` object, newer than the job.

5. Confirm the origin end to end: `curl -sI https://cell.example.com/healthz`
   is `200`, and `curl -sI https://cell.example.com/metrics` is `404`.

A failure at any step is a defect report against spec 032.
