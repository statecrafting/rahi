# Spec 044 qualification evidence (AC-3, AC-4)

Operator runs on 2026-10-31 and 2026-11-01 (UTC), recorded by spec 044 D-16.

## Release and cluster

- rahi `ghcr.io/statecrafting/rahi:0.7.1@sha256:cbbceea4e3f27fcc9422af48b1a09725d0f7deabf65d89ffd777e4321b77c3ac`
  (tag commit `0f338f9`).
- Rauthy `ghcr.io/bartekus/rauthy-patched:0.36.2-patched.3@sha256:d75cac0f708f3e238c458b622fea2f0b7dda9b67e9435eeafa37698d88a2a3c8`,
  the image v0.7.1's recipe pins.
- Cluster: the Statecraft k3s cluster (k3s v1.36.5, five nodes in one
  zone, `hcloud-volumes` storage, ingress-nginx v1.15.1), in a dedicated
  namespace, deleted with its volumes afterwards.
- Manifests: `deploy/n3-split` with an overlay that set only the
  namespace, the image digests, the host and the storage class. Run 1
  used the snippet-free edge of spec 040 D-8; run 2 used D-14's
  `Parallel` StatefulSets and no Ingress.

## Run 1 (`run1-ordered-ready/`): AC-3, AC-4, and the defect D-14 fixes

- **AC-3.** `tcpdump` on the rahi leader (filtered to `rauthy-internal`)
  and on all three Rauthy pods while `rahi backup` ran on the leader
  (token, `POST /backup`) and while rahi-1 restarted (discovery, JWKS,
  admin API). Every stream on Rauthy's port opens with a TLS handshake,
  every server hello negotiates TLS 1.3, rahi's client hellos carry the
  `rauthy-internal` SNI, and no capture holds an ASCII HTTP marker.
  Mechanism: Rauthy's native TLS, verified by rahi against the mounted CA
  (P-1). Files: `capture-points.txt`, `tls-summary.txt`, `pcap/`.
- **AC-4.** Rauthy scaled to 0: every rahi pod answered `/readyz` 503
  naming `identity`, `/healthz` 200, restart count 0 throughout, and
  recovered without a restart. A PDB-respecting drain of each of the five
  nodes (see Deviations): 9 evictions, none refused, all six pods ready
  after each node. Stop time from deletion to container exit: rahi 12 s
  (n=4), Rauthy 20 s (n=7), every exit 0, against a 60 s grace. Files:
  `ac4/`.
- **Edge (040 D-8).** `/binding`, `/metrics`, `/metrics?x=1` and
  `/%62inding` answer 403 through the load balancer, with or without
  `X-Forwarded-For: 127.0.0.1`; `/healthz` answers 200; plain HTTP
  redirects to HTTPS. File: `edge.txt`.
- **Defect.** Under `OrderedReady`, neither StatefulSet recovered from a
  full stop: pod 0 waited for a Raft quorum its peers were never created
  to give it. File: `defect/rahi-full-stop.txt`. Fixed by D-14.

## Run 2 (`run2-parallel/`): D-14 verified

| Step | Result | All ready after | Restarts |
|---|---|---|---|
| First bootstrap, empty volumes | pass | 71 s | none |
| Rauthy full stop | pass, rahi `/readyz` 503 `identity` meanwhile | 32 s | none |
| rahi full stop | pass | 70 s (about 25 s volume re-attach) | rahi-1 once |
| Both stopped at once | pass | 58 s | none |
| `rahi backup` on the leader | pass, 831,981 bytes; followers refuse (exit 2) | n/a | n/a |

Rauthy ended with one passkey-only backup admin. rahi-1's one restart in
the rahi full stop was 043 B-7's terminal store exit (code 3, `raft
stopped`, 11 s into serve); the next boot was ready 15 s later. Its cause
was not diagnosed at the default log level. It did not recur in the other
steps.

## Deviations

- The drain is a namespace-scoped emulation: cordon one node, evict only
  this namespace's pods through the eviction API (so PDBs bind), wait for
  readiness, uncordon. Other tenants' pods on the shared cluster were not
  evicted.
- The cluster has one zone, so zone spread was not exercised.
- The node-1-only provisioning of D-14 is in this change's code, not in
  v0.7.1: run 2 provisioned from whichever pod composed first, without
  overlap. The gate is covered by a unit test and ships with the next
  release.
- Run 1's first apply carried a wrong pod CIDR in Rauthy's trusted
  proxies, corrected before any measurement.
