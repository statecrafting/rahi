# Operational prerequisites, checked

Version 0, 2026-09-12, for review. This note answers the revision-3 planning
handoff for rahi ("close operational prerequisites before runtime
expansion"). The handoff is planning input: it approves no draft, and nothing
here amends a spec. It records what ran, what it showed, the answers to the
consumers' questions, and proposals a human accepts or rejects. The
contract itself stays `01-consumer-contract.md`; where this note supersedes
a statement there, that section says so. The rahi maintainer's decisions on
these proposals, taken later the same day, are indexed in section 8.1 and
recorded in the specs they govern; the evidence sections are unchanged.

Tags as in note 01: **Verified** (ran or read, and the section says where),
**Recommendation** (this note's proposal), **Unresolved** (a named owner
decides).

## 0. Baseline

**Verified** on 2026-09-12.

| Item | State |
|---|---|
| `origin/main` | `c13cc70`: `444bcf8` plus five Dependabot bumps to `.github/workflows/image.yml`. Every crate, app, Docker, and deploy source equals `444bcf8`. |
| Corpus on `main` | 22 specs `approved`: 20 `implementation: complete`, 2 `n-a` (000, 002). |
| Drafts | 035 to 041, `status: draft`, `implementation: pending`, on unpushed local branches: `corpus/consumer-contract` (035 to 039), `corpus/runtime-binding` (035 B-6, 040, 041), and `corpus/operational-prerequisites` (this note and the dated proposals in 035, 037, 038, 040, 041). None is on `main`, none is approved, none is implemented. |
| Governance | `spec-spine 0.18.0 check`: registry fresh, index fresh, 138 unwitnessed claims, 138 allowed. |
| Published artifacts | none: no tag, release, crate, or image (note 01 section 2.1, unchanged). |

Every run below used one environment, and none of it is CI, Kubernetes, or a
public origin:

- A Linux container, `rust:1.96.0-bookworm`, arm64, Docker Desktop, 6 CPUs,
  loopback only, plain `http://localhost`.
- **rauthy**: the binary copied from `/app/rauthy` of
  `ghcr.io/sebadob/rauthy:0.36.2@sha256:f7d3c501402165e023edbd958b032b41c9cfdac5ea7f8ca7d62217327145577e`,
  pulled by that digest; sha256
  `a9a5020839148fb0f10e59b4c7f2a7f1d813306ff5a0eea8e32f9d09f12e7d63`; it
  answers `/auth/v1/version` with `"current":"0.36.2"`. The same bytes are
  in the hello-cell image below.
- **hello-cell**: the release binary from the image `docker/Dockerfile`
  builds at `c13cc70` with `RAHI_PACKAGE=hello-cell`; sha256
  `54e8134b125b21ec79a0fa197194e4d39b06e892e0518fa66296d453cfd1d498`.
- **probe-cell**: an out-of-tree consumer with path dependencies on the
  `c13cc70` crates (appendix A). Its manifest grants `db.read` on one table
  and nothing else, so every write its routes attempt is a ledgered denial,
  and the routes are public, so no identity provider is needed to measure
  the kernel.

## 1. What ran and what was skipped

| Run | Result | Skipped, and why |
|---|---|---|
| `cargo test -p hello-cell --locked --test e2e` with `RAHI_TEST_RAUTHY` = pinned rauthy | pass, 26 s: login, notes, the ledgered denial, backup (rauthy half stubbed, as the test is written), restore, reboot | the independent `attest-ledger` verification (not installed in the container) |
| `cargo test -p rahi-harness --locked`, same variable | 8 passed, including `with_a_rauthy_at_hand_a_user_logs_in_and_reads_a_protected_route` | none |
| `cargo test --workspace --locked --no-fail-fast` with the variable (section 1.1) | 429 passed, 1 failed (root in the container; passes unprivileged), 1 ignored | `attest-ledger` (2), `discovery.rs`'s dead rauthy test, 025's live bearer test, the S3 backup test |
| probe: two kernels on one head, in process | reproduces 035 B-6 (section 3) | none |
| probe: three replica processes, one cluster | reproduces 035 B-6 at N=3 (section 3) | none |
| probe: 200 denials, then SIGTERM | reproduces 035 B-1 (section 2) | none |
| probe: identity recovery against pinned rauthy | section 4 | none |
| probe: bearer write against the CSRF layer | section 5 | a real bearer route with a real token (no public client exists; 038) |

Not run at all: spec 025's live bearer test (`RAHI_TEST_RAUTHY_URL` and a
client with `default_aud` and RS256, which nothing provisions); a device-code
login; a Kubernetes rollout (032 AC-2 is recorded, never run); rauthy or
backup at N=3; the image's page (a known `404`, note 01 section 2.5).

### 1.1 The workspace suite against the pinned release

The first run stopped at `rahi-ops --test first_boot` with one failure,
`a_read_only_key_mount_is_accepted_and_a_writable_wide_one_is_not`, because
the container runs as root and a `0555` directory is writable to root. The
same test binary run as an unprivileged user passes. The rerun with
`--no-fail-fast` is recorded here:

`cargo test --workspace --locked --no-fail-fast` with `RAHI_TEST_RAUTHY`
naming the pinned binary, as root in the container: **429 passed, 1 failed,
1 ignored** (a doc example), 496 seconds. The one failure is the
root-caused test above. Tests that still printed a skip with the variable
set:

- the independent `attest-ledger` verification, twice (the CLI is not
  installed in the container; the in-process check beside one of them ran);
- the rauthy test in `crates/rahi-idp/tests/discovery.rs`, which prints
  `skipped: <binary> needs the container spec 031 builds` and boots nothing
  whatever the variable names (draft 037 B-4 names it);
- spec 025's live bearer test (`RAHI_TEST_RAUTHY_URL` unset: it needs a
  running rauthy and a client whose tokens carry the cell's audience, which
  nothing provisions);
- the S3 backup test in `crates/rahi-store/tests/backup.rs` (`RAHI_TEST_S3`
  unset).

So every rauthy-dependent test that can run from `RAHI_TEST_RAUTHY` alone ran
and passed against 0.36.2. This is a hand run, not CI.

## 2. Denials lost at a graceful stop (draft 035 B-1 to B-4)

**Verified**. The probe cell boots through `rahi-harness` (`first-boot`,
`migrate`, `serve` with `RAHI_RAUTHY_MODE=none`). The test sends 200
concurrent `GET /api/deny`, collects the decision ids from the `403` bodies,
calls `Instance::stop` (SIGTERM, then a grace period), runs `ledger export` on
the stopped volume, and counts how many answered ids are in the export.

| Build | Wait before SIGTERM | Answered | In the chain after stop | `serve` exit | Logged or counted |
|---|---|---|---|---|---|
| debug | none | 200 `403`, 200 distinct ids | 4 | 0 | nothing |
| debug | none | 200, 200 | 13 | 0 | nothing |
| debug | none | 200, 200 | 10 | 0 | nothing |
| debug | none | 200, 200 | 10 | 0 | nothing |
| debug | none | 200, 200 | 14 | 0 | nothing |
| release | none | 200, 200 | 50 | 0 | nothing |
| release | none | 200, 200 | 58 | 0 | nothing |
| debug | 15 s (control) | 200, 200 | 200 | 0 | n/a |
| debug | 15 s (control) | 200, 200 | 200 | 0 | n/a |

Drain time, measured by polling the chain from the last answer until all 200
were resident: 0.592 s and 0.604 s (debug), 0.270 s and 0.263 s (release).

What it establishes: the loss is the stop, not the queue (the control keeps
every record); a graceful stop that exits `0` loses most of a burst that was
answered a moment earlier; nothing reports it. The prior session's 33 and 40
of 50 were the same defect at a smaller burst. A side observation: a
1000-request burst from one address was answered 300 `403` and 700 `429`,
because the edge's rate limit applies before the kernel.

## 3. Decision ids across replicas (draft 035 B-6)

**Verified in process.** Two kernels booted over one store and one chain
before either appended; each refused one request; both flushed:

```
replica a answered kernel:e60bab25ba6375f5:000000000000
replica b answered kernel:e60bab25ba6375f5:000000000000
kernel_ledger_failures: decision kernel:e60bab25ba6375f5:000000000000 was not written to the chain:
  conflict: ... is already in the chain under sha256:a2f3d079...: an id names one decision for the life of the chain
resident denial records 1
```

The control, a second kernel booted after the first appended, minted from
another nonce and both records landed.

**Verified with three independent processes.** `lab/scripts/three.sh`
(appendix A) starts three probe-cell processes as one three-node hiqlite
cluster on loopback, the way `deploy/README.md` describes N=3: first boot on
node 1, its keys copied to nodes 2 and 3, then on every node `migrate`
followed by `serve` with `RAHI_HIQ_NODE_ID` and `RAHI_HIQ_NODES` set. Then each
replica receives ten concurrent denials.

```
replica 1 readyz: {"ledger":"verified","status":"ready","store":"up"}
replica 2 readyz: {"ledger":"verified","status":"ready","store":"up"}
replica 3 readyz: {"ledger":"verified","status":"ready","store":"up"}
migrate on replica 1 exited 0
migrate on replica 2 exited 2
migrate on replica 3 exited 2
replica 1 answered: 10 403    ids kernel:e60bab25ba6375f5:000000000000 ... :000000000009
replica 2 answered: 10 403    ids kernel:e60bab25ba6375f5:000000000000 ... :000000000009
replica 3 answered: 10 403    ids kernel:e60bab25ba6375f5:000000000000 ... :000000000009
answered ids total: 30; distinct: 10; answered by more than one replica: 10
replica 1 reads the chain: "count":11, denial records 10
replica 1 metrics: kernel_decisions_total{outcome="deny"} 10 kernel_ledger_failures_total 0
replica 2 metrics: kernel_decisions_total{outcome="deny"} 10 kernel_ledger_failures_total 10
replica 3 metrics: kernel_decisions_total{outcome="deny"} 10 kernel_ledger_failures_total 10
denials answered: 30; distinct ids resident: 10; answers whose record is another replica's denial: 20
replica 1 serve exited 0
replica 2 serve exited 0
replica 3 serve exited 0
```

What it establishes:

- The collision is not a boot race. Replicas that boot on one head share
  the nonce, and their counters advance independently, so every counter
  value repeats on every replica for as long as they run. Two thirds of the
  denials of a three-replica cell under even load are lost, and each of
  those callers holds an id that names a different decision.
- The loss is counted on the replicas that lost, as ledger failures, not as
  collisions, and nothing says which ids.
- The genesis head, and so the first nonce, is the same for every fresh
  chain of this manifest and ledger key: the in-process tests and the
  three-process cluster all minted from `e60bab25ba6375f5`.
- As a side result, the N=3 start procedure worked on loopback: the leader
  migrated, the followers refused with exit `2` and served, all three
  verified the chain at boot, and all three exited `0` on SIGTERM. This is the
  only three-replica run on record; it had no rauthy, no backup, no rolling
  update, no network between hosts, and no Kubernetes.

Draft 035 now carries these numbers, a three-process functional requirement
(FR-005), and proposals for its three open decisions plus the one residual
case its id shape leaves (035 section 7, P-1 to P-5).

## 4. Identity recovery against the pinned release (draft 037)

**Verified**, by `lab/tests/recovery.rs` (appendix A), one run of 63 seconds,
with the hello-cell release binary and the pinned rauthy. The supervisor
clears rauthy's environment, so the test's `RAHI_TEST_RAUTHY` is a four-line
wrapper that execs the pinned rauthy and sources extra variables from a
control file when that file exists (appendix A). Output, trimmed only of
timestamps:

```
1. cell A up; alice sub p1H3lRAFsVP34DOnY53DkF61; note create 201 Created
1. rauthy reports version {"current":"0.36.2",...}
2. `backup` against live rauthy: exit 1; stderr "error: unauthorized: rauthy refused the admin token
   at http://127.0.0.1:36951/auth/v1/backup (401 Unauthorized)"
2. archives written: 0
3. admin session, default MFA rule: POST /auth/v1/backup -> 406 Not Acceptable
   {"error":"MfaRequired","message":"Rauthy admin access only allowed with MFA active"}
4. ADMIN_FORCE_MFA=false: POST /auth/v1/backup -> 204 No Content
4. GET /auth/v1/backup -> 200 OK; newest local backup_node_1_1789247129.sqlite
4. GET /auth/v1/backup/local/backup_node_1_1789247129.sqlite -> 200 OK, 675840 bytes
5. `backup` with the real snapshot relayed: exit 0; rahi-backup-20260912T210533Z.tar.age (765876 bytes, 9 parts)
5. `restore` into a fresh volume: exit 0; rauthy's snapshot is at .../restore/rauthy/relayed.sqlite
6. restored cell, no hand-off: alice in rauthy? Ok(None)
6. alice logs in on the restored cell without re-creating her: Err(... 401 ... "Invalid user credentials")
6. GET /api/notes as that session -> 401 Unauthorized {"error":"unauthorized","message":"this request carries no session"}
6. note: ledger verify on a running node exits 3 ("LockFile .../hiqlite/logs is locked and in use by another process")
7. after the hand-off: alice sub Some("p1H3lRAFsVP34DOnY53DkF61") (A was p1H3lRAFsVP34DOnY53DkF61)
7. alice logs in with her original password: Ok("ok")
7. GET /api/notes -> 200 OK [{"id":"n-18d4aec70ad92a17-aaa6","body":"survives-restore","revision":1,...}]
8. second start, no variable: login Ok("ok"); notes 200 OK [same note]
8. ledger heads: A sha256:f2098a96...; restored sha256:f2098a96...; after hand-off sha256:f2098a96...
```

Steps 5 to 8 are a construction, and the construction is the point: step 5
relays the real snapshot of step 4 to the unchanged `backup` verb through a
stub on rauthy's port, so the archive, the restore, and the boot are the
chassis's own; step 7's hand-off is the mechanism draft 037 B-3 proposes,
applied by hand through the wrapper (`HQL_BACKUP_RESTORE=file:<placed
snapshot>` for one start, then removed).

Findings:

1. **The backup verb cannot back up a real cell.** Unchanged since 030 D-3,
   now shown against the pinned release inside the chassis's own flow.
2. **The session route is blocked by MFA by default, and the only way past
   it is instance-wide.** rauthy 0.36.2 has one switch,
   `ADMIN_FORCE_MFA` (`mfa.admin_force_mfa`, read in
   `Principal::validate_admin_session`). Draft 037's second option, "a rauthy
   user whose MFA requirement is off", does not exist; turning the switch off
   exempts every rauthy admin of the cell. With it off, rauthy's backup API
   works as the verb expects.
3. **Upstream has not moved.** rauthy's `main`, read on 2026-09-12, calls
   `validate_admin_session()` in all four backup handlers.
4. **A restore today brings back the rows and the chain and loses every
   user.** Nothing hands the snapshot to rauthy, rauthy starts empty, and
   every `sub` in the app's rows is unreachable (constitution VII).
5. **The proposed hand-off works on the pinned release at N=1.** One start
   with `HQL_BACKUP_RESTORE` restores rauthy's database, the same `sub`, the
   original password, and the app's data behind the session, with the ledger
   head unchanged. At N=3 hiqlite restores on node 1 and makes the other
   nodes delete their data and rejoin; that path was not exercised.
6. **The chain cannot be read from a running cell** (`ledger verify` exits
   `3` on the lock), as 041 B-14 records.
7. **rauthy's login endpoint refuses an empty `User-Agent`** and reports
   "Invalid user credentials" to the caller; only its log says why. Found
   because this test's first client sent none.

Draft 037 now carries these results and three proposals (037 section 7).

## 5. Bearer writes and the CSRF layer (draft 038 B-1)

**Verified** against the release probe cell (`RAHI_RAUTHY_MODE=none`):

```
1. POST /api/private, Authorization: Bearer <token>, no CSRF pair
   -> 403 {"error":"csrf","message":"the request carries no matching CSRF cookie and X-CSRF-Token pair"}
2. same, plus an equal csrf cookie and X-CSRF-Token header chosen by the client
   -> 401 {"error":"unauthorized","message":"this request carries no session"}
3. POST /api/deny (public), no pair            -> 403 csrf
4. POST /api/deny (public), equal arbitrary pair -> 403 denied, decision kernel:e60bab25ba6375f5:000000000000
5. GET /api/deny (safe method), no pair        -> 403 denied, decision kernel:e60bab25ba6375f5:000000000001
```

The CSRF layer answers every unsafe method outside `/auth/*` before any
authentication, so a bearer client's write is refused whatever its token.
The pair is compared, not issued: any equal pair the client invents passes.
With identity mounted, step 2 reaches the resource server instead of the
session check; that variant needs a bearer route and a client whose tokens
pass, which nothing provisions today.

## 6. Answers to the consumers' questions

### 6.1 Statecraft R-1 and statecraft-cli D72: renewal and audience

Two different things are in play, and today only the first exists.

**Today, as built (Verified):**

- The cell is a resource server. It validates an access token rauthy issued:
  RS256, issuer `<origin>/auth/v1/`, `aud` exactly the cell's origin,
  `exp` and `nbf` with 60 seconds of leeway, scopes exact (025, 021 D-10). It
  issues nothing, stores nothing, and renews nothing for a bearer client.
- 025 B-5's "fifteen minutes" is the chassis's revocation lag constant
  (`DEFAULT_REVOCATION_LAG`, 900 s), not a lifetime the chassis enforces. The
  lifetime is rauthy's client setting, 1800 s by default in 0.36.2. A token
  is accepted until `exp + 60`.
- Revocation before `exp` does not happen: `ResourceServer::deny` has no
  caller. Were it called, the 900 s entry would lapse before a default
  1800 s token.
- The browser session renews differently: the cell holds the refresh token
  inside its sealed session envelope and trades it with rauthy every 900 s,
  re-reading roles (022 B-5). That path serves cookies, not bearer clients.
- No client a CLI or a runner could use exists: first boot makes one
  confidential client for the browser flow; a new rauthy client signs EdDSA
  and carries no audience; rauthy's device grant takes no `resource`
  parameter, so `aud` comes only from an admin-set `default_aud`.

**Renewal as proposed (Recommendation, drafted in 038):**

- A native client (the CLI) renews with rauthy's `refresh_token` grant at
  the issuer's token endpoint; the cell validates each new access token like
  any other. Proposed: access lifetime 600 s, set by the manifest and applied
  to every declared client; a refresh lifetime set from the manifest (86,400 s
  proposed) instead of rauthy's 72-hour device default; revocation by subject
  ends the whole refresh chain's earlier tokens (038 section 7, P-1, P-2).
- A runner authenticates as a client-credentials service principal, one
  rauthy client per enrolled runner created by the control plane through
  rauthy's admin API with the cell's audience and RS256. It renews by asking
  for a new token before `exp`; it holds no refresh token. Job continuity
  across tokens is the control plane's lease fence, not a longer token.
  Revocation is disabling the client plus a subject deny-list entry (038 P-3).
  This answers Statecraft 015 section 4.3's three options: re-request, not a
  refresh grant and not a longer lifetime. **Unresolved** (owner Statecraft):
  whether a customer-controlled runner may hold a client secret, or must log
  in as a user through the device grant instead.

### 6.2 Statecraft R-2: unknown manifest sections

**Verified**: every manifest struct is `#[serde(deny_unknown_fields)]`
(`crates/rahi-kernel/src/manifest.rs`, the root `Manifest` included), and
`Manifest::parse` documents "Unknown keys are refused at every level". A
kernel refuses a manifest with a section it does not know; there is no
forward-compatible section. A semantic change to a known section moves the
manifest hash and, today, freezes the volume (note 01 section 7).

**Recommendation**: keep the refusal. Statecraft's endpoint table belongs in
a Statecraft-owned overlay, which is also Statecraft 017 section 10's own
recommendation; the chassis does not grow a consumer view. Draft 036 covers
changing the ceiling a running cell enforces.

### 6.3 Statecraft R-3: restore across a version change, and a second durable store

**Verified**, today: `restore` compares no manifest hash or schema version
with the binary; `serve` refuses a store behind its migrations and accepts
one ahead; a restored archive whose manifest differs from the binary's fails
at boot with `integrity`; and a restored cell has no users (section 4). Draft
036 B-9 proposes the compatibility check; draft 037 the identity hand-off.

On a second durable store: the thesis refuses "a second relational store
beside hiqlite's SQLite group" (002 section 6), and the backup archive holds
exactly the app snapshot, rauthy's snapshot, and the keys. A file Statecraft
keeps under `/data` is not in that archive, is not replicated at N=3, and is
outside every recovery statement this repository makes.

**Recommendation**: store the evidence chain's records in an app table as
their original bytes (binary values are first class, 016 B-1), keyed by a
typed digest, written in the same `txn` as whatever references them. Then
backup, restore, and replication cover them without a chassis change, and
Statecraft's own verifier reads the bytes it wrote. If the chain must stay a
file, Statecraft owns its backup and its replication, and the cutover
checklist says so.

### 6.4 Statecraft R-4: tenancy, billing, business authorization

**Verified**: they stay out. Thesis sections 2 and 6 and note 01 section 1
leave tenancy, business authorization and approvals, billing, fleet
operation, external integrations, customer UI, schedulers, and application
SDKs to the consumer; none of drafts 035 to 041 adds one.

### 6.5 aicortex: leases, crates, notifications, consumption

- **Leases (Verified).** `rahi-store`'s lease is hiqlite's lock: ten seconds,
  `LEASE_TTL_SECONDS`, documented as not configurable; `StoreHandle::lease`,
  `Lease::release`, and `fenced_txn` exist; there is no renew. aicortex 035's
  requested duration and `PATCH` renewal are not a chassis lease.
  **Recommendation**: model a work claim as an app row (`key`, `holder`,
  `fence`, `expires_at`) created and renewed by `fenced_txn` under a
  short-held lease; the requested duration lives in the row and renewal
  updates `expires_at`. No chassis change is proposed.
- **Crates (Verified).** A cell needs `rahi-cli`, which exports `Cell` and
  `run`; aicortex 010 B-2 and hqgit 003 B-2 both omit it.
- **Notifications (Verified).** The outbox stages key-only envelopes;
  `Outbox::drain` publishes and deletes; notify is a hint and the revision
  column is truth (constitution IX). Nothing in the chassis calls `drain`; a
  cell that stages runs the loop.
- **Consumption (Verified).** Nothing is published; a git dependency pinned
  to a full sha is the only route, which aicortex 010 D-1 forbids. Draft 039.

### 6.6 hqgit: one hiqlite or several (a consumer design review)

hqgit's open decision P-6 (its `docs/design/03-hosted-topology-and-transport.md`)
asks whether per-repository Raft groups from its spec 091 share the chassis
store's runtime or run as an independent hiqlite deployment beside it.

**Verified**: a cell has exactly two hiqlite clusters, the app's and
rauthy's (constitution VIII), and hiqlite 0.14 runs a fixed pair of Raft
groups per node, SQL and cache (`RaftType::Sqlite`, `RaftType::Cache`). The
chassis has no API for adding a Raft group, and no spec proposes one. Nothing
stops a process from calling `Store::open` twice with two configurations, but
that is a third cluster with its own ports, membership, keys, and backup,
which no chassis verb backs up, restores, or supervises.

**Recommendation**: for any hosted hqgit pilot, keep one app store and model
repositories as rows scoped by a repository key inside it, with 091's
`_cluster` table dropped in favour of the chassis's node identity. Per-repository
consistency is then a property of transactions scoped by key, not of
separate Raft groups. Revisit only with a measured need, and then as a
chassis spec for multiple groups, not as a second deployment inside a cell.
Do not add a second cluster to satisfy 091's text.

Transport, **Verified** on the release probe cell: the edge answers
HTTP/1.1 and HTTP/2 cleartext with prior knowledge (`curl
--http2-prior-knowledge` receives `HTTP/2 200` from `/healthz`); it does not
upgrade HTTP/1.1 to h2c. Both Connect's unary calls and gRPC are `POST`s, so
today the CSRF layer refuses them (section 5) unless they carry an equal
cookie and header pair, until 038 B-1 exempts bearer routes. Whether a tonic
service mounted in a cell's router survives the rest of the middleware
(trailers, streaming bodies, the rate limit) is unverified. A REAPI listener
on a second port would need the justification constitution VI demands.
**Unresolved** (owner hqgit): P-6 and its transport decision.

## 7. Contract proposals

### 7.1 The runtime binding record and fixture (drafts 040 and 041)

Statecraft's drafts state the concept (016 section 10.1: a detached binding
that carries an epoch and may be `unknown`) and name no type, schema, or
fixture; none of note 01's section 9 requests 6 to 10 is answered there. So
this is a **counterproposal**, not an agreement. Shapes, with every value's
basis:

An epoch reference. Equality is `chain` plus `epoch`; `number` orders and never
identifies (041 B-7):

```json
{
  "type": "rahi.epoch-ref/v0",
  "chain": "sha256:<genesis record hash>",
  "epoch": "sha256:<epoch record hash>",
  "number": 2
}
```

A replica's `/binding` document (040 B-6, with 041's members reserved per 040
P-1):

```json
{
  "schema": "rahi.binding/v0",
  "instance": {
    "id":      { "value": "2-9f1c04e2a7b3d815", "basis": "measured" },
    "node":    { "value": 2, "basis": "declared" },
    "pod":     { "value": "rahi-1", "basis": "declared" },
    "started": { "value": 1789247129, "basis": "declared" }
  },
  "build": {
    "binary_sha256": { "value": "sha256:54e8134b...", "basis": "measured" },
    "platform":      { "value": "linux/arm64", "basis": "declared" },
    "rahi_version":  { "value": "0.1.0", "basis": "declared" },
    "revision":      { "basis": "absent", "reason": "RAHI_BUILD_REVISION not set at build" }
  },
  "manifest": {
    "hash":             { "value": "sha256:...", "basis": "measured" },
    "app":              { "value": "hello-cell", "basis": "declared" },
    "org":              { "value": "statecrafting", "basis": "declared" },
    "contract_version": { "value": "1.0.0", "basis": "declared" }
  },
  "artifact": {
    "image": { "value": "ghcr.io/statecrafting/rahi@sha256:...", "basis": "declared" }
  },
  "store": { "schema_version": { "value": 1, "basis": "measured" } },
  "epoch": {
    "ref":   { "value": { "type": "rahi.epoch-ref/v0", "chain": "sha256:...", "epoch": "sha256:...", "number": 2 }, "basis": "measured" },
    "match": { "value": "bound", "basis": "measured" }
  },
  "observation": {
    "traces":    { "export": "off", "export_loss": "uncounted", "ring_capacity": 1000 },
    "metrics":   { "unobserved": ["/metrics", "/binding", "unmatched routes"] },
    "decisions": { "allows": "not recorded", "queue_capacity": 1024,
                   "loss_counters": ["kernel_decisions_dropped_total", "kernel_decisions_abandoned_total", "kernel_ledger_failures_total"] }
  }
}
```

A decision payload after 041 carries `manifest`, `epoch` (the record hash),
`epoch_number`, and `instance` (041 B-9 as revised on this branch).

Rules proposed with the shapes:

- **Identity keys.** A consumer keys a runtime observation by `instance.id`
  plus the epoch reference, never by epoch number, pod name, or node alone.
- **No identity in labels.** No digest, reference, instance id, pod, or
  deployment id is a metric label (040 B-9, 041 B-10); the only additions are
  `rahi_build_info{rahi_version, contract_version}`, a label-free epoch-number
  gauge for dashboards, and a mismatch gauge whose label is a closed set.
- **Observed is not permitted.** `/binding` and the epoch record say what
  runs and what was deployed. They authorize nothing; the deploy step never
  refuses for a reference it cannot judge; a replica never refuses to boot on
  a mismatch; a permit to deploy is Statecraft's, compared by its broker with
  these observations at effect time (041 B-10a).
- **Fixture.** 041 FR-003's `testdata/binding/` set, as original bytes: an
  authority snapshot digest, an in-toto Statement, a deployment record, the
  epoch record, one `/binding` document, one denial. Statecraft names its
  deployment record's type URI and schema version; spec-spine names the
  authority snapshot's; the CLI names the in-toto and SLSA versions it
  verifies. **Unresolved** (owners Statecraft, spec-spine, statecraft-cli).

### 7.2 The first hosted pilot: what of 035 to 039 it needs

**Recommendation**, for agreement with Statecraft (017's cutover checklist).
The pilot is a new Statecraft cell at N=1, not the migration of the live
plane.

| Draft | Needed for the pilot | Not needed for the pilot |
|---|---|---|
| 035 | all of it: B-1 to B-4 (a stop must not lose denials), B-6 (needed before any N=3) | synchronous denials |
| 036 | B-1 to B-4 (the pilot's ceiling will change at least once), B-7 checksums, B-9 restore check | B-8's additive rule, if refusing a store ahead of the binary is acceptable for the pilot; the N=3 rollout window |
| 037 | B-1 with a chosen mechanism, B-2, B-3 at N=1, B-4, B-6 (a restored login in the test), B-5 as an advisory job | B-3 at N=3 |
| 038 | B-1 (any bearer write), B-3 and B-4 for whichever client the CLI and runners use, B-5 before a runner outside the operator's control, B-7's device login proof | preflight's lifetime report (B-6) can follow |
| 039 | a tag Statecraft pins, the images a tag builds, the template moved so `rahi-cli` packages, static assets in the image | crates.io publication |
| 040, 041 | nothing; design alongside | all of it |

Acceptance for the pilot's rahi side, proposed: the exact pinned consumer
build and image digest; an authenticated bearer write that passes without a
CSRF pair; a manifest change adopted on an existing volume with the chain
intact; a restart and a rollback per 036's worked example; a backup and a
restore into a fresh volume followed by the same user's login with the same
`sub` against the pinned rauthy; every skipped provider-dependent step named
in the output.

### 7.3 Supported topology, stated

**Verified** as exercised: N=1, one container, one volume, rauthy
co-deployed, over `http://localhost` (the hello-cell image and the tests of
section 1). **Not** exercised: N=3 on Kubernetes (never run), N=3 with rauthy
(never run), backup or restore at N=3 (never run). Three chassis processes
on loopback formed a cluster and served (section 3); that shows the start
procedure works and that decision ids collide, not that N=3 is supported.
**Recommendation**: state N=1 as the supported topology until 035 lands and
a three-replica exercise with rauthy, a rolling update, and a backup passes;
no three-replica claim before that.

## 8. Remaining decisions

| Decision | Owner | Proposal | Evidence |
|---|---|---|---|
| 035 drain bound | rahi maintainer | 5 s | section 2 |
| 035 id shape | rahi maintainer | `kernel:<nonce>:<node>:<counter>` | section 3 |
| 035 re-minted id after a lost denial | rahi maintainer | accept, state in B-4, log nonce and node at boot | 035 P-4 |
| 037 backup mechanism (030 D-3) | rahi maintainer | ask upstream for an API key group; spike a software passkey; not `ADMIN_FORCE_MFA=false` with human admins | section 4 |
| 037 hand-off at N=3 | rahi maintainer | node 1 applies, others rejoin; exercise with three processes first | section 4, finding 5 |
| 038 lifetimes and refresh | rahi maintainer | 600 s access, 86,400 s refresh from the manifest | section 6.1 |
| runner authentication | Statecraft | client-credentials principal per runner | section 6.1 |
| evidence chain storage | Statecraft | original bytes in an app table | section 6.3 |
| binding and epoch shapes, type URIs | Statecraft, spec-spine, CLI | section 7.1 | none implemented |
| pilot slice | Statecraft with rahi | section 7.2 | |
| hqgit P-6 | hqgit | one store, rows scoped by repository | section 6.6 |
| `AGENTS.md` says "97 of 97" unwitnessed; the check says 138 of 138 | rahi maintainer | change the line and record a dated decision in 001, on a `corpus/001-*` branch, because 001 owns `AGENTS.md` and the coupling gate (C-001) refuses the edit alone; this session did not make it | `spec-spine check` |
| publishing these branches | the user | the repository is public and the notes describe security-relevant gaps; nothing is pushed | |
| release line | rahi maintainer | draft 039 | note 01 section 2 |

### 8.1 Decided on 2026-09-12

The rahi maintainer's rows above were decided on 2026-09-12, as decisions
RH-01 to RH-08 of the revision-3 register. Each is recorded as a dated
decision in the spec it governs, and that entry is the authority; this table
only points there. No draft is approved by these decisions; approval is a
separate human flip, which the maintainer made for 035 the same day. 036 to
041 stay `status: draft`.

| Decision | Resolution | Recorded in |
|---|---|---|
| 035 drain bound, id shape, re-minted id | a five-second bounded drain (not a guarantee against process death or storage failure); `kernel:<nonce>:<node>:<counter>`; the re-mint residual accepted with the boot identity logged and every countable loss reported by cause; tests show no unexplained loss at a graceful stop, unique ids across three processes, and observable exhaustion | 035 D-1 to D-4 (RH-01) |
| 037 backup mechanism | a software-passkey spike first, with rauthy's admin MFA left on, then the mechanism it proves; no waiting on an upstream API-key route and never `ADMIN_FORCE_MFA=false`; if automation fails, only a rehearsed operator-assisted procedure, and no unattended recovery claim | 037 D-1 (RH-02) |
| 037 hand-off at N=3 | deferred; the candidate (one node restores, the others rejoin, then test) is recorded and not adopted | 037 D-2 (RH-03) |
| 038 lifetimes and refresh | 600 s access and 86,400 s refresh in the manifest, clients read the actual expiry; real bearer writes and device refresh; cookie-session CSRF kept and the equal cookie and header workaround not authorized; service-principal runners deferred under Statecraft's G-09 | 038 D-1 to D-3 (RH-04) |
| release line | registry publication of all nine chassis crates, `rahi-cli` included, with matching release tags and pinned images; packaging implemented and tested locally, publication at the release checkpoint | 039 D-1 (RH-05) |
| manifest version, capability kinds, hiqlite provenance | the manifest version wired and validated and unknown sections refused; a new capability kind is a reviewed minor schema change with compatibility tests, a breaking change a new major; upstream hiqlite kept and the July fork proposal reversed; 012 B-1, B-4, and B-6 amended to the implementation; no renewable chassis lease | 039 D-2, 015 D-9, 011 D-10 and D-11, 012 D-10 (RH-06) |
| binding and epoch shapes, rahi's side | the chain hash plus the epoch record hash plus instance identity retained; an observation grants no deployment permission; no identity value in a metric label; implementation deferred past the pilot's prerequisites | 040 D-1, 041 D-1 to D-4 (RH-07) |
| the `AGENTS.md` claim count | corrected with no hardcoded count, on `corpus/001-claim-count` off `main`, not on this branch | 001 D-12 (RH-08) |

Not decided by these: the rows owned by Statecraft, spec-spine, the CLI, and
hqgit; publishing the branches; the approval flips; and the items each
draft's section 7 still lists as open.

## 9. The next smallest increment

**Recommendation**: approve draft 035 with P-1, P-3, and P-4, and build it as
the next spec. It is one kernel change, one serve change, and two counters;
it closes the two losses this note measured; its acceptance reuses sections
2 and 3 as FR-001 and FR-005. In parallel, the maintainer picks 037's
mechanism, because every recovery claim of the pilot waits on it.

Update, 2026-09-12, after section 8.1: both have happened. P-1, P-3, and P-4
are adopted as 035 D-1 to D-3, and 037's mechanism is chosen (D-1). The
maintainer then approved 035 the same day, so what stands between 035 and
its build is the corpus reaching `main`. 037's FR-005 spike is the parallel
increment, since its outcome decides which branch of 037 D-1 is built.

## Appendix A: the probes

Out-of-tree, not part of this repository's build, claimed by no spec, and
kept here so the numbers above can be reproduced. Paths are relative to a
working directory holding a `c13cc70` checkout at `rahi-main/` beside
`lab/`, and the container of section 0 mounts that directory at `/work`.

### How each run was invoked

```sh
# in the container: rust:1.96.0-bookworm, /work = this directory, CARGO_TARGET_DIR=/target
cd /work/lab && cargo build --tests --bins && cargo build --release --bin probe-cell
/target/debug/deps/replicas-<hash> --nocapture --test-threads 1
DENIALS=10 /work/lab/scripts/three.sh /target/debug/probe-cell
LAB_SHUTDOWN_RUNS=3 /target/debug/deps/shutdown-<hash> --nocapture
LAB_SHUTDOWN_SETTLE_SECS=15 LAB_SHUTDOWN_RUNS=2 /target/debug/deps/shutdown-<hash> --nocapture
LAB_SHUTDOWN_MEASURE=1 LAB_SHUTDOWN_RUNS=2 /target/release/deps/shutdown-<hash> --nocapture
RAHI_TEST_RAUTHY=/work/bin/rauthy-wrap LAB_HELLO_CELL=/work/bin/hello-cell-release \
  LAB_RAUTHY_EXTRA=/work/state/rauthy-extra.env /target/debug/deps/recovery-<hash> --nocapture
cd /work/rahi-main && RAHI_TEST_RAUTHY=/work/bin/rauthy cargo test --workspace --locked --no-fail-fast
```

`/work/bin/rauthy` is `/app/rauthy` copied out of the pinned image and
`/work/bin/hello-cell-release` is `/usr/local/bin/rahi` copied out of the
hello-cell image (section 0).

### `lab/Cargo.toml`

```toml
[package]
name = "rahi-lab"
version = "0.0.0"
edition = "2024"
rust-version = "1.96"
publish = false

# An out-of-tree consumer used only to gather evidence against the chassis
# at origin/main. Nothing here lands in the rahi repository.

[[bin]]
name = "probe-cell"
path = "src/main.rs"

[dependencies]
rahi-cli = { path = "../rahi-main/crates/rahi-cli" }
rahi-edge = { path = "../rahi-main/crates/rahi-edge" }
rahi-idp = { path = "../rahi-main/crates/rahi-idp" }
rahi-kernel = { path = "../rahi-main/crates/rahi-kernel" }
rahi-ledger = { path = "../rahi-main/crates/rahi-ledger" }
rahi-store = { path = "../rahi-main/crates/rahi-store" }
rahi-types = { path = "../rahi-main/crates/rahi-types" }
axum = "0.8"
serde_json = "1"

[dev-dependencies]
rahi-harness = { path = "../rahi-main/crates/rahi-harness" }
rahi-ops = { path = "../rahi-main/crates/rahi-ops" }
reqwest = { version = "0.13", default-features = false, features = ["json", "form"] }
tokio = { version = "1", features = ["full"] }
tempfile = "3"
serde = { version = "1", features = ["derive"] }
futures = "0.3"
```

### `lab/manifest.toml`

```toml
# The probe cell's ceiling: `items` may be read and nothing else. Every
# write the probe routes attempt is therefore a ledgered denial.

[app]
name = "probe-cell"
org = "lab"

[resources]
tables = ["items"]

[[capabilities]]
id = "items-read"
kind = "db.read"
resource = "items"

[services.items]
capabilities = ["items-read"]

[ledger]
schema_version = "1.0.0"
max_record_bytes = 65536

[observability]
metrics_path = "/metrics"
otel = false

[auth]
operator_role = "probe_operator"

[contract]
version = "1.0.0"
```

### `lab/src/main.rs`

```rust
//! probe-cell: an out-of-tree cell whose public routes are denied by the
//! kernel, so denial durability and decision identity can be measured
//! without an identity provider.
//!
//! - `GET /api/deny` and `POST /api/deny`: a governed `db.write` the
//!   manifest never granted; answers 403 with the decision id.
//! - `GET /api/chain`: every resident record id and the head, read from
//!   the replica's own ledger handle.
//! - `POST /api/private`: authenticated; used to show which layer answers
//!   a bearer write first.

use std::sync::LazyLock;

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use rahi_cli::Cell;
use rahi_edge::AppState;
use rahi_edge::exposure::{Route, RouteClass};
use rahi_idp::Authenticated;
use rahi_kernel::{CapabilityKind, Governed};
use rahi_ledger::Ledger;
use rahi_store::{Migration, StoreHandle};
use rahi_types::Sub;

struct ProbeCell;

static MIGRATIONS: LazyLock<Vec<Migration>> = LazyLock::new(|| {
    vec![Migration::new(
        1,
        "items",
        "CREATE TABLE IF NOT EXISTS items (id INTEGER PRIMARY KEY, body TEXT NOT NULL)",
    )]
});

impl Cell for ProbeCell {
    fn manifest() -> &'static str {
        include_str!("../manifest.toml")
    }

    fn migrations() -> &'static [Migration] {
        &MIGRATIONS
    }

    fn routes(state: AppState) -> Router {
        let write = Governed::new(
            state.kernel(),
            "items",
            CapabilityKind::DbWrite,
            "items",
            state.store().clone(),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let deny = Router::new()
            .route("/api/deny", get(deny).post(deny))
            .with_state(write);
        let chain = Router::new()
            .route("/api/chain", get(chain))
            .with_state(state.ledger().clone());
        let private = Router::new().route("/api/private", post(private));
        deny.merge(chain).merge(private)
    }

    fn exposed() -> Vec<Route> {
        vec![
            Route::new("/api/deny", RouteClass::Public),
            Route::new("/api/chain", RouteClass::Public),
        ]
    }
}

async fn deny(State(write): State<Governed<StoreHandle>>) -> Response {
    match write
        .execute(
            &Sub::new("lab-probe"),
            "INSERT INTO items (body) VALUES ('x')",
            vec![],
        )
        .await
    {
        Ok(r) => format!("unexpectedly allowed: {}", r.rows_affected).into_response(),
        Err(e) => rahi_edge::error::response(&e),
    }
}

async fn chain(State(ledger): State<Ledger>) -> Response {
    let records = match ledger.records().await {
        Ok(r) => r,
        Err(e) => return rahi_edge::error::response(&e),
    };
    let head = ledger.head().await.map(|h| h.as_str().to_owned()).ok();
    let ids: Vec<&str> = records.iter().map(|r| r.record.id.as_str()).collect();
    Json(serde_json::json!({ "count": ids.len(), "head": head, "ids": ids })).into_response()
}

async fn private(Authenticated(p): Authenticated) -> String {
    format!("authenticated as {}", p.sub.as_str())
}

fn main() {
    rahi_cli::run(ProbeCell);
}
```

### `lab/tests/replicas.rs`

```rust
//! Decision identity across kernels, as the chassis mints it at origin/main.
//!
//! Negative evidence for draft 035 B-6: two kernels that boot on the same
//! chain head mint the same decision id, and the chain keeps one of the two
//! denials. The sequential case is the control: a kernel that boots after
//! another has appended mints from a different nonce.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::{SocketAddr, TcpListener};
use std::path::Path;
use std::time::Duration;

use rahi_kernel::{Kernel, KernelOptions, Manifest};
use rahi_ledger::{Ledger, LedgerSigner};
use rahi_store::{EncKey, EncKeys, Store, StoreConfig, StoreSecrets};
use rahi_types::Sub;

const MANIFEST: &str = include_str!("../manifest.toml");

fn free_addr() -> SocketAddr {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap()
}

fn store_config(dir: &Path) -> StoreConfig {
    StoreConfig {
        node_id: 1,
        nodes: Vec::new(),
        data_dir: dir.to_path_buf(),
        raft_addr: free_addr(),
        api_addr: free_addr(),
        secrets: StoreSecrets {
            secret_raft: "raft-secret-for-tests-0000".to_owned(),
            secret_api: "api-secret-for-tests-00000".to_owned(),
            enc_keys: EncKeys {
                active: "test".to_owned(),
                keys: vec![EncKey { id: "test".to_owned(), key: vec![7u8; 32] }],
            },
        },
        backup_keep_days: 1,
        s3: None,
    }
}

async fn kernel(store: &rahi_store::StoreHandle, manifest: &Manifest) -> Kernel {
    let ledger = Ledger::open(store.clone(), LedgerSigner::from_seed([9u8; 32]), manifest.hash().unwrap())
        .await
        .unwrap();
    Kernel::boot_with(manifest.clone(), store.clone(), ledger, KernelOptions::default())
        .await
        .unwrap()
}

fn deny(k: &Kernel, who: &str) -> String {
    k.refuse("lab.probe", &Sub::new(who), "lab: denied", serde_json::Map::new())
        .as_str()
        .to_owned()
}

async fn resident_ids(store: &rahi_store::StoreHandle, manifest: &Manifest) -> Vec<String> {
    let ledger = Ledger::open(store.clone(), LedgerSigner::from_seed([9u8; 32]), manifest.hash().unwrap())
        .await
        .unwrap();
    ledger.records().await.unwrap().into_iter().map(|r| r.record.id).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_kernels_on_one_head_mint_one_id_and_the_chain_keeps_one_denial() {
    let dir = tempfile::tempdir().unwrap();
    let node = Store::open(&store_config(&dir.path().join("hiqlite"))).await.unwrap();
    let store = node.handle();
    let manifest = Manifest::parse(MANIFEST).unwrap();

    // Two replicas that booted before either appended: same head, same nonce.
    let a = kernel(&store, &manifest).await;
    let b = kernel(&store, &manifest).await;
    let id_a = deny(&a, "replica-a-caller");
    let id_b = deny(&b, "replica-b-caller");
    a.flush(Duration::from_secs(10)).await.unwrap();
    b.flush(Duration::from_secs(10)).await.unwrap();

    let ids = resident_ids(&store, &manifest).await;
    let denials: Vec<&String> = ids.iter().filter(|i| i.starts_with("kernel:")).collect();
    println!("LAB replicas: replica a answered {id_a}");
    println!("LAB replicas: replica b answered {id_b}");
    println!("LAB replicas: resident denial records {}: {denials:?}", denials.len());

    assert_eq!(id_a, id_b, "at origin/main both callers hold one id");
    assert_eq!(denials.len(), 1, "at origin/main the chain keeps one of the two denials");
    node.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_kernel_that_boots_after_an_append_mints_from_another_nonce() {
    let dir = tempfile::tempdir().unwrap();
    let node = Store::open(&store_config(&dir.path().join("hiqlite"))).await.unwrap();
    let store = node.handle();
    let manifest = Manifest::parse(MANIFEST).unwrap();

    let a = kernel(&store, &manifest).await;
    let id_a = deny(&a, "first");
    a.flush(Duration::from_secs(10)).await.unwrap();
    let b = kernel(&store, &manifest).await;
    let id_b = deny(&b, "second");
    b.flush(Duration::from_secs(10)).await.unwrap();

    let ids = resident_ids(&store, &manifest).await;
    let denials = ids.iter().filter(|i| i.starts_with("kernel:")).count();
    println!("LAB sequential: {id_a} then {id_b}; resident denials {denials}");
    assert_ne!(id_a, id_b);
    assert_eq!(denials, 2);
    node.shutdown().await.unwrap();
}
```

### `lab/tests/shutdown.rs`

```rust
//! Denials answered before a graceful stop, and how many reach the chain.
//!
//! Negative evidence for draft 035 B-1..B-4 at origin/main: `serve` never
//! drains the kernel's denial queue on SIGTERM. The test measures and
//! prints; it asserts only what it needs to be a valid measurement.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use rahi_harness::{BootSpec, Harness};

const REQUESTS: usize = 200;

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_probe-cell"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn denials_answered_before_sigterm_and_how_many_reach_the_chain() {
    let runs: usize = std::env::var("LAB_SHUTDOWN_RUNS").ok().and_then(|v| v.parse().ok()).unwrap_or(3);
    for run in 1..=runs {
        let cell = Harness::boot(BootSpec::new(binary())).expect("probe-cell boots");
        let base = cell.base_url().to_owned();
        let http = reqwest::Client::new();

        let mut tasks = Vec::new();
        for _ in 0..REQUESTS {
            let http = http.clone();
            let url = format!("{base}/api/deny");
            tasks.push(tokio::spawn(async move {
                let r = http.get(url).send().await.unwrap();
                let status = r.status().as_u16();
                let body: serde_json::Value = r.json().await.unwrap_or_default();
                (status, body["decision"].as_str().map(str::to_owned))
            }));
        }
        let mut statuses: BTreeMap<u16, usize> = BTreeMap::new();
        let mut answered: Vec<String> = Vec::new();
        for t in tasks {
            let (status, id) = t.await.unwrap();
            *statuses.entry(status).or_default() += 1;
            if let Some(id) = id {
                answered.push(id);
            }
        }

        // The graceful stop an orchestrator sends, immediately after the
        // answers, or after LAB_SHUTDOWN_SETTLE_SECS for the control run.
        let settle: u64 = std::env::var("LAB_SHUTDOWN_SETTLE_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
        if std::env::var("LAB_SHUTDOWN_MEASURE").is_ok() {
            // Poll the chain (at 250 ms, inside the edge's rate limit) until
            // every answered denial is resident, from the last answer.
            let start = std::time::Instant::now();
            loop {
                let v: serde_json::Value = http.get(format!("{base}/api/chain")).send().await.unwrap().json().await.unwrap_or_default();
                let count = v["count"].as_u64().unwrap_or(0) as usize;
                if count > answered.len() || start.elapsed().as_secs() > 60 {
                    println!("LAB drain run {run}: chain count {count} {:?} after the last answer", start.elapsed());
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(settle)).await;
        let stopped = cell.stop().expect("the cell stops");

        let export = cell.data_dir().join("chain.jsonl");
        let out = cell.command(&["ledger", "export", export.to_str().unwrap()]).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let chain = std::fs::read_to_string(&export).unwrap();
        let resident = answered.iter().filter(|id| chain.contains(&format!("\"{id}\""))).count();
        let distinct: std::collections::BTreeSet<&String> = answered.iter().collect();
        let log = cell.stderr_tail(400);
        let counted = log.lines().filter(|l| l.contains("rahi.decision") || l.contains("ledger")).count();

        println!(
            "LAB shutdown run {run} (settle {settle}s): statuses {statuses:?}; answered ids {} ({} distinct); in chain after stop {resident}; lost {}; stop {stopped:?}; decision/ledger log lines {counted}",
            answered.len(),
            distinct.len(),
            answered.len() - resident
        );
    }
}
```

### `lab/scripts/three.sh`

```sh
#!/usr/bin/env bash
# Three independent probe-cell processes as one hiqlite cluster on loopback,
# started the way deploy/README.md describes N=3: shared keys, every
# replica runs `migrate` (a follower may exit 2) and then serves. Then each
# replica denies DENIALS requests concurrently and the chain is read back.
#
# Evidence only; not a chassis test. Usage: three.sh <probe-cell binary>
set -uo pipefail

BIN=${1:?probe-cell binary}
DENIALS=${DENIALS:-10}
ROOT=${ROOT:-/tmp/lab-n3}
rm -rf "$ROOT"; mkdir -p "$ROOT"

NODES="1 127.0.0.1:19201 127.0.0.1:19101;2 127.0.0.1:19202 127.0.0.1:19102;3 127.0.0.1:19203 127.0.0.1:19103"

node_env() {
  local i=$1
  export RAHI_PUBLIC_URL=http://localhost:19000
  export RAHI_DATA_DIR=$ROOT/node$i
  export RAHI_LISTEN_ADDR=127.0.0.1:1900$i
  export RAHI_HIQLITE_API_ADDR=127.0.0.1:1910$i
  export RAHI_HIQLITE_RAFT_ADDR=127.0.0.1:1920$i
  export RAHI_HIQ_NODE_ID=$i
  export RAHI_HIQ_NODES="$NODES"
  export RAHI_RAUTHY_MODE=none
}

run() { local i=$1; shift; ( node_env "$i"; exec "$BIN" "$@" ); }

mkdir -p "$ROOT/node1"
run 1 first-boot > "$ROOT/first-boot-1.log" 2>&1 || { echo "first-boot 1 failed"; cat "$ROOT/first-boot-1.log"; exit 1; }
for i in 2 3; do
  mkdir -p "$ROOT/node$i"
  cp -a "$ROOT/node1/keys" "$ROOT/node$i/keys"
  run "$i" first-boot > "$ROOT/first-boot-$i.log" 2>&1 || { echo "first-boot $i failed"; cat "$ROOT/first-boot-$i.log"; exit 1; }
done

# The N=3 entrypoint: migrate (a follower's exit 2 is tolerated), then serve.
declare -a SERVE
for i in 1 2 3; do
  ( node_env "$i"
    "$BIN" migrate > "$ROOT/migrate-$i.log" 2>&1
    echo "migrate on replica $i exited $?" >> "$ROOT/migrate-$i.log"
    exec "$BIN" serve > "$ROOT/serve-$i.log" 2>&1 ) &
  SERVE[$i]=$!
done

for i in 1 2 3; do
  for _ in $(seq 1 240); do
    curl -fsS "http://127.0.0.1:1900$i/readyz" >/dev/null 2>&1 && break
    sleep 0.5
  done
  echo "replica $i readyz: $(curl -s http://127.0.0.1:1900$i/readyz)"
done
for i in 1 2 3; do tail -n 1 "$ROOT/migrate-$i.log"; done

echo "--- $DENIALS concurrent denials at each of the three replicas"
declare -a CURLS
for i in 1 2 3; do
  for n in $(seq 1 "$DENIALS"); do
    curl -s "http://127.0.0.1:1900$i/api/deny" -o "$ROOT/deny-$i-$n.json" -w "%{http_code}\n" >> "$ROOT/codes-$i" &
    CURLS+=($!)
  done
done
wait "${CURLS[@]}"
sleep 5

for i in 1 2 3; do
  echo "replica $i answered: $(sort "$ROOT/codes-$i" | uniq -c | tr -s ' \n' ' ')"
  cat "$ROOT"/deny-$i-*.json | grep -o '"decision":"[^"]*"' | sed 's/"decision"://; s/"//g' | sort > "$ROOT/ids-$i"
  echo "replica $i ids: $(tr '\n' ' ' < "$ROOT/ids-$i")"
done
cat "$ROOT"/ids-? | sort > "$ROOT/ids-all"
echo "answered ids total: $(wc -l < "$ROOT/ids-all"); distinct: $(uniq "$ROOT/ids-all" | wc -l); answered by more than one replica: $(uniq -d "$ROOT/ids-all" | wc -l)"

for i in 1 2 3; do
  curl -s "http://127.0.0.1:1900$i/api/chain" > "$ROOT/chain-$i.json"
  echo "replica $i reads the chain: $(grep -o '"count":[0-9]*' "$ROOT/chain-$i.json"), denial records $(grep -o 'kernel:[^"]*' "$ROOT/chain-$i.json" | sort -u | wc -l)"
  echo "replica $i metrics: $(curl -s http://127.0.0.1:1900$i/metrics | grep -E '^kernel_' | tr '\n' ' ')"
  echo "replica $i log lines naming a conflict: $(grep -c 'already in the chain' "$ROOT/serve-$i.log")"
done
resident=$(grep -o 'kernel:[^"]*' "$ROOT/chain-1.json" | sort -u)
lost=0
while read -r id; do
  n=$(grep -c "^$id\$" "$ROOT/ids-all"); [ "$n" -gt 1 ] && lost=$((lost + n - 1))
done <<< "$resident"
echo "denials answered: $(wc -l < "$ROOT/ids-all"); distinct ids resident: $(echo "$resident" | grep -c kernel); answers whose record is another replica's denial: $lost"

echo "--- stop"
for i in 1 2 3; do kill -TERM "${SERVE[$i]}" 2>/dev/null; done
for i in 1 2 3; do wait "${SERVE[$i]}"; echo "replica $i serve exited $?"; done
```

### `bin/rauthy-wrap`

```sh
#!/bin/sh
# Test-only wrapper: exec the pinned rauthy, sourcing extra variables from
# a control file when it exists (the supervisor clears the environment).
EXTRA=/work/state/rauthy-extra.env
if [ -f "$EXTRA" ]; then
  set -a; . "$EXTRA"; set +a
  echo "rauthy-wrap: sourced $(cut -d= -f1 "$EXTRA" | tr '\n' ' ')" >&2
fi
exec /work/bin/rauthy "$@"
```

### `lab/tests/recovery.rs`

```rust
//! Identity recovery against the pinned rauthy release, as the chassis
//! behaves at origin/main. Evidence for draft 037; not a chassis test.
//!
//! Needs `RAHI_TEST_RAUTHY` (a wrapper that execs the pinned rauthy and
//! sources `LAB_RAUTHY_EXTRA` when that file exists) and `LAB_HELLO_CELL`
//! (the hello-cell binary). Prints `LAB recovery:` lines; each step records
//! what happened rather than asserting a hoped-for outcome, except where a
//! later step depends on an earlier one.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]

use std::path::PathBuf;
use std::time::Duration;

use rahi_harness::{BootSpec, Client, Harness, Instance, Rauthy, User};
use reqwest::StatusCode;
use reqwest::header::{COOKIE, LOCATION, SET_COOKIE};
use serde_json::{Value, json};

fn say(line: impl AsRef<str>) {
    println!("LAB recovery: {}", line.as_ref());
}

fn extra_path() -> PathBuf {
    PathBuf::from(std::env::var("LAB_RAUTHY_EXTRA").expect("LAB_RAUTHY_EXTRA"))
}

fn set_extra(lines: &[&str]) {
    if lines.is_empty() {
        let _ = std::fs::remove_file(extra_path());
    } else {
        std::fs::write(extra_path(), lines.join("\n") + "\n").unwrap();
    }
}

fn verb(cell: &Instance, args: &[&str], env: &[(&str, &str)]) -> (Option<i32>, String, String) {
    let mut c = cell.command(args);
    for (k, v) in env {
        c.env(k, v);
    }
    let out = c.output().unwrap();
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// rauthy's login, through the cell's origin, for `email`, returning the
/// rauthy session cookie and CSRF token (the harness keeps them private).
async fn rauthy_session(cell: &Instance, email: &str, password: &str) -> Result<(String, String), String> {
    let base = cell.base_url().to_owned();
    let start = cell.client().get("/session/login").await.map_err(|e| e.to_string())?;
    let location = start
        .headers()
        .get(LOCATION)
        .and_then(|v| v.to_str().ok())
        .ok_or("no authorize location")?
        .to_owned();
    let url = reqwest::Url::parse(&location).map_err(|e| e.to_string())?;
    let param = |n: &str| url.query_pairs().find(|(k, _)| k == n).map(|(_, v)| v.into_owned());
    let http = reqwest::Client::builder().user_agent("rahi-lab")
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();
    let proxy = format!("{base}/auth/v1");
    let session = http.post(format!("{proxy}/oidc/session")).send().await.map_err(|e| e.to_string())?;
    let mut cookie = session
        .headers()
        .get(SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .ok_or("session set no cookie")?
        .to_owned();
    let info: Value = session.json().await.map_err(|e| e.to_string())?;
    let csrf = info["csrf_token"].as_str().ok_or("no csrf token")?.to_owned();
    let challenge = http.post(format!("{proxy}/pow")).send().await.map_err(|e| e.to_string())?.text().await.unwrap();
    let pow = rahi_harness::rauthy::solve_pow(challenge.trim()).map_err(|e| e.to_string())?;
    let mut body = json!({
        "email": email, "password": password, "pow": pow,
        "client_id": param("client_id"), "redirect_uri": param("redirect_uri"),
        "scopes": param("scope").unwrap_or_else(|| "openid".into()).split_whitespace().collect::<Vec<_>>(),
    });
    for n in ["state", "nonce", "code_challenge", "code_challenge_method"] {
        if let Some(v) = param(n) {
            body[n] = Value::String(v);
        }
    }
    let answer = http
        .post(format!("{proxy}/oidc/authorize"))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = answer.status();
    if let Some(c) = answer.headers().get(SET_COOKIE).and_then(|v| v.to_str().ok()).and_then(|v| v.split(';').next()) {
        if c.contains("session") {
            cookie = c.to_owned();
        }
    }
    if !status.is_success() {
        return Err(format!("authorize answered {status}: {}", answer.text().await.unwrap_or_default()));
    }
    Ok((cookie, csrf))
}

async fn rauthy_admin_call(
    base: &str,
    session: &(String, String),
    method: reqwest::Method,
    path: &str,
) -> (StatusCode, Vec<u8>) {
    let http = reqwest::Client::builder().user_agent("rahi-lab").timeout(Duration::from_secs(60)).build().unwrap();
    let r = http
        .request(method, format!("{base}/auth/v1{path}"))
        .header(COOKIE, &session.0)
        .header("x-csrf-token", &session.1)
        .send()
        .await
        .unwrap();
    let status = r.status();
    (status, r.bytes().await.unwrap_or_default().to_vec())
}

fn admin_password(cell: &Instance) -> String {
    let out = cell.first_boot_output().unwrap();
    out.lines()
        .find_map(|l| l.strip_prefix("rauthy password:"))
        .map(|p| p.trim().to_owned())
        .expect("first-boot printed the password")
}

async fn list_notes(client: &Client) -> (StatusCode, String) {
    let r = client.get("/api/notes").await.unwrap();
    let s = r.status();
    (s, r.text().await.unwrap_or_default())
}

fn head_of(verify: &str) -> String {
    verify.split("head ").nth(1).map(|h| h.trim().to_owned()).unwrap_or_default()
}

/// A stub on rauthy's port that answers the backup routes with `bytes`:
/// the relay that lets the unchanged `backup` verb carry a real snapshot.
fn relay(port: u16, bytes: Vec<u8>) -> tokio::runtime::Runtime {
    use axum::routing::get;
    let bytes = std::sync::Arc::new(bytes);
    let posted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let seen = posted.clone();
    let app = axum::Router::new()
        .route("/auth/v1/health", get(|| async { "ok" }))
        .route(
            "/auth/v1/backup",
            get(move || {
                let listed = seen.load(std::sync::atomic::Ordering::SeqCst);
                async move {
                    let local: Vec<Value> = if listed {
                        vec![json!({"name": "relayed.sqlite", "last_modified": 1, "size": 1})]
                    } else {
                        vec![]
                    };
                    json!({"local": local, "s3": []}).to_string()
                }
            })
            .post(move || {
                posted.store(true, std::sync::atomic::Ordering::SeqCst);
                async { "" }
            }),
        )
        .route(
            "/auth/v1/backup/local/{name}",
            get(move || {
                let b = bytes.clone();
                async move { b.as_ref().clone() }
            }),
        );
    let listener = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    rt.spawn(async move {
        let l = tokio::net::TcpListener::from_std(listener).unwrap();
        axum::serve(l, app).await.unwrap();
    });
    rt
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn identity_recovery_against_the_pinned_rauthy() {
    let Ok(rauthy_bin) = std::env::var("RAHI_TEST_RAUTHY") else {
        say("skipped: RAHI_TEST_RAUTHY is not set");
        return;
    };
    let hello = PathBuf::from(std::env::var("LAB_HELLO_CELL").expect("LAB_HELLO_CELL"));
    set_extra(&[]);

    // 1. A cell with the pinned rauthy; a user, a note, the user's sub.
    let cell = Harness::boot(BootSpec::new(&hello).with_rauthy(&rauthy_bin)).expect("cell A boots");
    let base = cell.base_url().to_owned();
    let admin = Rauthy::new(&cell.rauthy_loopback(), cell.admin_token());
    let alice = User::new("alice@example.com", "Correct-Horse-Battery-Staple-2026");
    let client = cell.client();
    cell.login_as(&client, &alice).await.expect("alice logs in on A");
    let sub_a = admin.find_user(&alice.email).await.unwrap().expect("alice exists")["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let created = client.post_json("/api/notes", &json!({"body": "survives-restore"})).await.unwrap();
    say(format!("1. cell A up at {base}; alice sub {sub_a}; note create {}", created.status()));
    assert_eq!(created.status(), StatusCode::CREATED);
    let rauthy_version = admin_version(&cell).await;
    say(format!("1. rauthy reports version {rauthy_version}"));

    // 2. The backup verb against the live rauthy, unchanged (030 D-3).
    let archives = tempfile::tempdir().unwrap();
    let (code, out, err) = verb(&cell, &["backup", "--to", archives.path().to_str().unwrap()], &[]);
    say(format!("2. `backup` against live rauthy: exit {code:?}; stdout {:?}; stderr {:?}", out.trim(), err.trim()));
    let archives_written = std::fs::read_dir(archives.path()).unwrap().count();
    say(format!("2. archives written: {archives_written}"));

    // 3. The admin-session alternative with rauthy's default ADMIN_FORCE_MFA.
    let password = admin_password(&cell);
    match rauthy_session(&cell, "admin@localhost", &password).await {
        Ok(s) => {
            let (st, body) = rauthy_admin_call(&base, &s, reqwest::Method::POST, "/backup").await;
            say(format!("3. admin session, default MFA rule: POST /auth/v1/backup -> {st} {}", String::from_utf8_lossy(&body)));
        }
        Err(e) => {
            say(format!("3. admin session, default MFA rule: login refused: {e}"));
            for l in cell.stdout_tail(400).lines().chain(cell.stderr_tail(400).lines()) {
                if l.contains("authorize") || l.contains("MFA") || l.contains("mfa") || l.contains("rauthy-wrap") || l.contains("WARN") {
                    say(format!("3. log: {l}"));
                }
            }
        }
    }

    // 4. The same, with ADMIN_FORCE_MFA=false injected for rauthy only.
    let ports = cell.ports();
    let data_a = cell.data_dir();
    cell.stop().unwrap();
    set_extra(&["ADMIN_FORCE_MFA=false"]);
    let cell = Harness::boot(BootSpec::new(&hello).with_rauthy(&rauthy_bin).on_data_dir(&data_a).with_ports(ports))
        .expect("cell A reboots with ADMIN_FORCE_MFA=false");
    let snapshot: Vec<u8> = match rauthy_session(&cell, "admin@localhost", &password).await {
        Ok(s) => {
            let (st, body) = rauthy_admin_call(&base, &s, reqwest::Method::POST, "/backup").await;
            say(format!("4. ADMIN_FORCE_MFA=false: POST /auth/v1/backup -> {st} {}", String::from_utf8_lossy(&body)));
            let mut name = None;
            for _ in 0..60 {
                let (st, body) = rauthy_admin_call(&base, &s, reqwest::Method::GET, "/backup").await;
                let listing: Value = serde_json::from_slice(&body).unwrap_or_default();
                if let Some(n) = listing["local"].as_array().and_then(|a| a.last()).and_then(|e| e["name"].as_str()) {
                    say(format!("4. GET /auth/v1/backup -> {st}; newest local {n}"));
                    name = Some(n.to_owned());
                    break;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            let name = name.expect("rauthy listed a local backup");
            let (st, bytes) = rauthy_admin_call(&base, &s, reqwest::Method::GET, &format!("/backup/local/{name}")).await;
            say(format!("4. GET /auth/v1/backup/local/{name} -> {st}, {} bytes", bytes.len()));
            assert_eq!(st, StatusCode::OK);
            bytes
        }
        Err(e) => panic!("admin login with ADMIN_FORCE_MFA=false refused: {e}"),
    };

    // 5. A rahi archive whose rauthy part is that real snapshot, relayed by
    //    a stub on rauthy's port, while the cell is stopped; then restore.
    cell.stop().unwrap();
    set_extra(&[]);
    let verified = verb(&cell, &["ledger", "verify"], &[]);
    let head_a = head_of(&verified.1);
    let stub = relay(ports.rauthy, snapshot.clone());
    let (code, out, err) = verb(&cell, &["backup", "--to", archives.path().to_str().unwrap()], &[]);
    stub.shutdown_background();
    say(format!("5. `backup` with the real snapshot relayed: exit {code:?}; {} {}", out.trim(), err.trim()));
    assert_eq!(code, Some(0));
    let archive = std::fs::read_dir(archives.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|e| e == "age"))
        .expect("one archive");
    let fresh = tempfile::tempdir().unwrap();
    let key = data_a.join("keys").join("backup.key");
    let (code, out, err) = verb(
        &cell,
        &["restore", archive.to_str().unwrap(), "--key", key.to_str().unwrap()],
        &[("RAHI_DATA_DIR", fresh.path().to_str().unwrap())],
    );
    say(format!("5. `restore` into a fresh volume: exit {code:?}; {} {}", out.trim(), err.trim()));
    assert_eq!(code, Some(0));
    let placed: Vec<String> = std::fs::read_dir(fresh.path().join("restore").join("rauthy"))
        .map(|d| d.map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    say(format!("5. restore placed rauthy files: {placed:?}"));

    // 6. Boot the restored volume with identity, as the supervisor does today.
    let restored = Harness::boot(BootSpec::new(&hello).with_rauthy(&rauthy_bin).on_data_dir(fresh.path()).with_ports(ports))
        .expect("the restored volume boots with rauthy");
    let admin_b = Rauthy::new(&restored.rauthy_loopback(), restored.admin_token());
    let found = admin_b.find_user(&alice.email).await;
    say(format!("6. restored cell, no hand-off: alice in rauthy? {:?}", found.as_ref().map(|u| u.as_ref().map(|v| v["id"].clone()))));
    let client_b = restored.client();
    let login = rahi_harness::rauthy::login(&client_b, restored.base_url(), &alice).await;
    say(format!("6. alice logs in on the restored cell without re-creating her: {:?}", login.as_ref().map(|()| "ok")));
    let (st, body) = list_notes(&client_b).await;
    say(format!("6. GET /api/notes as that session -> {st} {body}"));
    let verified_b = verb(&restored, &["ledger", "verify"], &[]);
    say(format!("6. note: ledger verify on a running node exits {:?} ({})", verified_b.0, verified_b.2.lines().last().unwrap_or_default()));

    // 7. Hand rauthy its snapshot once (the mechanism draft 037 B-3 proposes),
    //    by hand: HQL_BACKUP_RESTORE for one start, then removed.
    let data_b = restored.data_dir();
    restored.stop().unwrap();
    let head_b = head_of(&verb(&restored, &["ledger", "verify"], &[]).1);
    let snapshot_file = fresh.path().join("restore").join("rauthy").join(placed.first().expect("a placed rauthy file"));
    set_extra(&[&format!("HQL_BACKUP_RESTORE=file:{}", snapshot_file.display())]);
    let handed = Harness::boot(BootSpec::new(&hello).with_rauthy(&rauthy_bin).on_data_dir(&data_b).with_ports(ports));
    set_extra(&[]);
    let handed = match handed {
        Ok(c) => c,
        Err(e) => {
            say(format!("7. boot with HQL_BACKUP_RESTORE failed: {e}"));
            panic!("hand-off boot failed");
        }
    };
    let admin_c = Rauthy::new(&handed.rauthy_loopback(), handed.admin_token());
    let found = admin_c.find_user(&alice.email).await.unwrap();
    let sub_c = found.as_ref().and_then(|u| u["id"].as_str()).map(str::to_owned);
    say(format!("7. after the hand-off: alice sub {sub_c:?} (A was {sub_a})"));
    let client_c = handed.client();
    let login = rahi_harness::rauthy::login(&client_c, handed.base_url(), &alice).await;
    say(format!("7. alice logs in with her original password: {:?}", login.as_ref().map(|()| "ok")));
    let (st, body) = list_notes(&client_c).await;
    say(format!("7. GET /api/notes -> {st} {body}"));

    // 8. A second start without the variable: the restore is not re-applied.
    let data_c = handed.data_dir();
    handed.stop().unwrap();
    let head_c = head_of(&verb(&handed, &["ledger", "verify"], &[]).1);
    let again = Harness::boot(BootSpec::new(&hello).with_rauthy(&rauthy_bin).on_data_dir(&data_c).with_ports(ports))
        .expect("second start");
    let client_d = again.client();
    let login = rahi_harness::rauthy::login(&client_d, again.base_url(), &alice).await;
    let (st, body) = list_notes(&client_d).await;
    say(format!("8. second start, no variable: login {:?}; notes {st} {body}", login.as_ref().map(|()| "ok")));
    again.stop().unwrap();
    say(format!("8. ledger heads: A {head_a}; restored {head_b}; after hand-off {head_c}"));
    assert_eq!(sub_c.as_deref(), Some(sub_a.as_str()), "the original sub is back");
}

async fn admin_version(cell: &Instance) -> String {
    let http = reqwest::Client::builder().user_agent("rahi-lab").build().unwrap();
    match http.get(format!("{}/auth/v1/version", cell.rauthy_loopback())).send().await {
        Ok(r) => r.text().await.unwrap_or_default(),
        Err(e) => format!("unknown ({e})"),
    }
}
```

