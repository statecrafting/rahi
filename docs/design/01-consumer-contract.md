# The consumer contract, as built

**Current release preparation, 2026-09-17:** 0.2.0 is an unpublished
candidate under approved spec 039, carrying completed spec 037 from
`ea6d0da125aaafe0927570410a8a1b0ee9d1286e`. Section 2.0 is the current
consumer guidance; the dated readings below remain historical evidence.
The final release revision is not known inside this preparation commit.
The prospective in-repository release identity is
[`v0.2.0` source](https://github.com/statecrafting/rahi/tree/v0.2.0): after
publication, the release revision is the tested merged `main` commit to
which the annotated `v0.2.0` tag resolves (039 B-2). This link does not
assert that the tag or release exists yet. The coordinator must record the
exact SHA and release date in forge metadata to supplement this identity.
A version declaration is not publication or downstream adoption.

Version 0, 2026-09-11, for review; revised the same day with the runtime
binding of section 11. This note is a consumer's view of the chassis: how
to depend on it today, what it guarantees, what it does not, and which of
the gaps are proposed for change. The design truth stays the corpus;
nothing here amends a spec. The behavior changes this note asks for are
the draft specs 035 to 041, which a human approves or rejects.

Revised 2026-09-12: `02-operational-prerequisites.md` re-ran the gaps below
against `c13cc70` (whose crate sources equal `444bcf8`) and the pinned
rauthy release, answered the consumers' questions, and proposed the open
decisions. Statements it supersedes are marked in place.

Revised 2026-09-16 for the `v0.1.0` release (spec 039 B-2): section 2 now
names the tag rather than a commit. Everything below section 2 was last
checked against `444bcf8`, whose crate sources the tag carries forward
through `39f778b`; where a statement has since changed, the release's
entry in `CHANGELOG.md` is the one that governs.

Revised 2026-09-17 with a status reconciliation in section 12, taken during
the spec-spine 0.20.0 upgrade. It adds a dated reading and supersedes no
statement above: sections 2.1 and 2.2 are re-measured there, not rewritten.

Every statement carries one of three tags.

- **Verified**: checked against commit `444bcf8` (`main`) by reading the
  code or running it, and the note names where.
- **Recommendation**: this note's proposal. Not yet adopted anywhere.
- **Unresolved**: a decision a named owner has to make.

## 1. What a consumer gets, and what stays theirs

**Verified** (`crates/rahi-cli/src/cell.rs`, spec 030 B-1 and D-5). A cell
is a Rust binary whose `main` is `rahi_cli::run(MyCell)` over one
implementation of `rahi_cli::Cell`:

| Method | What it declares |
|---|---|
| `manifest() -> &'static str` | the capability ceiling, TOML (spec 015 B-1), usually `include_str!` |
| `migrations() -> &'static [Migration]` | versioned DDL in ascending order (spec 011 B-5); apply `rahi_store::coordination_migration(n)` first if the cell uses the outbox or fenced leases (spec 034 D-3) |
| `routes(AppState) -> Router` | merged at the root, classified authenticated |
| `operator_routes(AppState) -> Router` | mounted under `/operator` behind `auth.operator_role` (spec 024 B-2); empty by default |
| `exposed() -> Vec<Route>` | routes inside the root merge that are public (spec 024 B-3) |
| `static_dir() -> Option<PathBuf>` | the directory the static slot serves (spec 020 B-7) |

The composer supplies `serve` and the verbs `preflight`, `migrate`,
`backup`, `restore`, `ledger verify`, `ledger export`, `supervise`, and
`first-boot`, with four exit codes: `0` ok, `1` failure, `2` stale, `3`
infrastructure.

What the chassis supplies: identity through a co-deployed rauthy on the
cell's own origin, replicated operational state in hiqlite, capability
facades adjudicated by the kernel, the decision chain, the edge with
probes, metrics, tracing, and server-sent events, the operational verbs,
one container image, and the Kubernetes topology.

What stays the consumer's (thesis §2 and §6, spec 034 §6): tenancy,
business authorization and approvals, billing, fleet operations, external
integrations such as GitHub installations, customer-facing and operator
dashboards, any scheduler or job system, and any application SDK. **Verified**
(spec 032, `deploy/`): three replicas are three identical copies of one
cell, each a member of the app's Raft group and rauthy's; they replicate
the cell's state and serve its traffic. They do not schedule work, and
nothing in the chassis distributes jobs to workers.

## 2. Consuming today

### 2.0 The 0.2.0 candidate and its compatibility boundary

All nine inherited chassis versions and internal requirements are 0.2.0.
Spec 039 B-1 permits a consumer-contract change only at a minor bump before
1.0. This release carries the key-set, backup authentication, restore, and
export API changes of 037; it is not a compatible patch to 0.1.0. The
complete release proposal is [CHANGELOG.md](../../CHANGELOG.md#020-release-candidate-not-published).

After all nine crates are published and `consumer-registry` passes for the
new tag, the intended registry stanza is:

```toml
[dependencies]
rahi-cli    = "=0.2.0"
rahi-edge   = "=0.2.0"
rahi-idp    = "=0.2.0"
rahi-kernel = "=0.2.0"
rahi-ledger = "=0.2.0"
rahi-ops    = "=0.2.0"
rahi-store  = "=0.2.0"
rahi-types  = "=0.2.0"

[dev-dependencies]
rahi-harness = "=0.2.0"
```

This is a prospective stanza, not a claim that crates.io resolves it now.
Use the same release for every chassis dependency. Before publication, a
consumer can pin the actual preparation commit over git or test locally
staged `.crate` artifacts. Neither route passes published-only acceptance.
Third-party dependencies stay registry-sourced; hiqlite remains locked to
0.14.0. Consumer lockfiles resolve independently and need their own checks.

The upgrade boundary is explicit:

- New first boots and exported key sets include `backup_passkey.json`, bound
  to the intended public origin. `first-boot --export` requires
  `RAHI_PUBLIC_URL`; Rust callers pass an `EnvReader` to `first_boot::export`.
  A container export uses `--entrypoint /usr/local/bin/rahi` to reach that
  verb. The backup account is passkey-only and satisfies rauthy's MFA.
- Old key sets are not silently upgraded. They can start with a supervisor
  warning, but `backup` refuses without a usable backup passkey. Re-running
  first boot on existing keys does not add it. There is no automatic key-set
  migration, cross-origin re-enrollment, or safe replacement-key procedure
  in this release. An old archive does not gain a missing credential.
- N=1 recovery is proven at the same origin and ports with matching keys
  and rauthy registration. Restoring at another origin is not covered.
  The restore marker records application of the exact snapshot after child
  health; missing pending files fail closed. Old markers without the applied
  field are pending, not proof that their contents are usable.
- Each store's backup has its own bounded freshness guarantee, with a
  120-second default deadline. A rapid second backup may wait about a minute.
  The two snapshots do not establish one cross-store evidence head. The
  archive envelope and chain format are unchanged, but the key payload and
  restore behavior have changed. No rollback or old-binary compatibility
  across these changes has been established.
- The `Cell` trait, schema version `1.0.0`, exit codes, and production bearer
  audience validation are unchanged. Manifest evolution, native-client
  changes, runtime identity, and deployment epochs remain drafts 036, 038,
  040, and 041, with no approval implied by this release.

Two independent limitations constrain adoption. Published hiqlite 0.14.0
has the stale lease release defect after TTL takeover; full-node restart
followed by a second lease is unverified. Ledger ID/content classification
looks only at resident records, so retry after sealing can append a duplicate
ID even when chain verification passes (013 D-5, 014 sealing). Atomic
lifetime idempotence and migration/backfill need a separately governed
design, including unavailable-history handling. Neither limitation is fixed
by 037 or 0.2.0, and aicortex must not adopt it as such. Section 5.1's journal
recommendation does not establish these missing guarantees.

Publication still requires the annotated tag on tested main, all nine
crates, the tag and registry consumer jobs, both architecture images,
anonymous pulls, and the published-image walkthrough. The earlier merged
037 checks cited in the changelog are inherited evidence, not new-release
acceptance. The existing Kubernetes target remains 0.1.0; neither this PR
nor artifact publication changes that target or proves a consumer rollout.

### 2.1 What was published at 0.1.0 (historical)

**Verified** on 2026-09-16 (`git ls-remote`, `gh run view`, crates.io API),
revising the 2026-09-11 reading that found nothing published:

| Artifact | State |
|---|---|
| source | public at `github.com/statecrafting/rahi`, Apache-2.0, clonable without credentials |
| crates on crates.io | the nine chassis crates at `0.1.0`, published by the `v0.1.0` tag and proven by `consumer-registry` (spec 039 AC-4) |
| git tags, GitHub releases | `v0.1.0`, annotated and signed |
| container image | `ghcr.io/statecrafting/rahi:0.1.0` and `ghcr.io/statecrafting/rahi-runtime:0.1.0` exist, both architectures, but the GHCR packages are **private**: an anonymous manifest fetch answers `403`, so AC-2 is not met until the owner makes them public (spec 039 D-12) |
| deploy manifests | name `ghcr.io/statecrafting/rahi` and a version, never `latest` (spec 039 B-3) |
| rauthy | the image pins `ghcr.io/sebadob/rauthy:0.36.2` by digest; a Cargo consumer gets no rauthy from rahi |

The workspace version is no longer only a declaration: `0.1.0` is a tag, a
set of crates on crates.io, and a pair of images that name one release.
Pin the version. The images are the one part still gated on an owner step,
so a consumer building a cell from the crates is unblocked and a consumer
pulling the runtime image is not.

### 2.2 The 0.1.0 dependency stanza (historical proof)

**Verified** on 2026-09-16 by `consumer-registry` in the `v0.1.0` run: a
consumer cell outside this workspace, with its own manifest and one
migration, resolved this stanza from crates.io, then ran `first-boot`,
`migrate`, and `serve`, answered `/readyz`, performed one governed write,
was denied one ungranted operation with a decision id, and passed
`ledger verify` with the denial as the chain's last record (`2 resident
record(s)`). The tag runs that proof twice through the same
`.github/consumer-cell/prove.sh`: `consumer` against the git stanza and
`consumer-registry` against the registry stanza (spec 039 AC-4, D-11).

```toml
[dependencies]
rahi-cli    = "=0.1.0"
rahi-edge   = "=0.1.0"
rahi-idp    = "=0.1.0"
rahi-kernel = "=0.1.0"
rahi-store  = "=0.1.0"
rahi-types  = "=0.1.0"
axum = "0.8"

[dev-dependencies]
rahi-harness = "=0.1.0"
```

The `=` is deliberate. The nine crates move as one version (spec 039 B-1),
and pre-1.0 a minor bump may change the consumer contract, so a caret range
would let `cargo update` cross a contract change on its own.

To track an unreleased change, the same stanza over git still works, and is
what the `consumer` job proves on every tag:

```toml
rahi-cli = { git = "https://github.com/statecrafting/rahi", tag = "v0.1.0" }
```

`rahi-cli` is the crate that exports `Cell` and `run`; a consumer that
lists the other chassis crates and not this one cannot compose a cell.
`rahi-idp` is needed only for `Authenticated`, `RequireScope`, and the
bearer layers.

Historical constraints **verified on 2026-09-11**, before publication;
the no-tag and packaging blockers below were superseded by 039:

- rustc 1.96 or newer and edition 2024 (`rust-version = "1.96"`; 1.95
  refuses).
- Only a full sha can be pinned, since no tag exists.
- The consumer's `Cargo.lock` re-resolves: at this rev it differs from
  rahi's lock on 20 packages (for example `reqwest 0.13.4` becomes
  `0.13.5`). rahi's `--locked` determinism does not carry over.
- The registry route is blocked twice: nothing is published, and
  `cargo package` fails on `rahi-ops` because
  `crates/rahi-ops/src/rauthy_env.rs:19` includes
  `docker/rauthy.env.template` from outside the crate. `rahi-cli` depends
  on `rahi-ops`, so the crate carrying `Cell` cannot be published until the
  template moves. Git checkouts contain the whole repository, which is why
  the git route works.

**Recommendation** (draft spec 039): tag releases, move the template into
`rahi-ops`, publish the images a tag builds, and decide whether crates go
to crates.io.

### 2.3 The smallest cell

**Verified**: this exact code compiles out of tree against the stanza of
2.2 with no warning, boots, and refuses an unauthenticated `POST
/api/items`. The CSRF layer answers first (`403`, `"error":"csrf"`), since
every unsafe method outside `/auth/*` needs a matching `csrf` cookie and
`X-CSRF-Token` header. The governed write and the ledgered denial of 2.2
ran in a variant with public routes, because no rauthy was booted.

```rust
use std::sync::LazyLock;
use axum::{Router, extract::State, routing::post};
use rahi_cli::Cell;
use rahi_edge::AppState;
use rahi_idp::Authenticated;
use rahi_kernel::{CapabilityKind, Governed};
use rahi_store::{Migration, StoreHandle, Value};

struct MyCell;

static MIGRATIONS: LazyLock<Vec<Migration>> = LazyLock::new(|| vec![
    Migration::new(1, "items", "CREATE TABLE IF NOT EXISTS items (id INTEGER PRIMARY KEY, sub TEXT NOT NULL, body TEXT NOT NULL)"),
]);

impl Cell for MyCell {
    fn manifest() -> &'static str { include_str!("../manifest.toml") }
    fn migrations() -> &'static [Migration] { &MIGRATIONS }
    fn routes(state: AppState) -> Router {
        let write = Governed::new(state.kernel(), "items", CapabilityKind::DbWrite, "items", state.store().clone())
            .unwrap_or_else(|e| panic!("{e}"));
        Router::new().route("/api/items", post(create)).with_state(write)
    }
}

async fn create(State(write): State<Governed<StoreHandle>>, Authenticated(p): Authenticated) -> String {
    match write.execute(&p.sub, "INSERT INTO items (sub, body) VALUES ($1, $2)",
                        vec![Value::from(p.sub.as_str()), Value::from("hello")]).await {
        Ok(r) => format!("{}", r.rows_affected),
        Err(e) => e.to_string(),
    }
}

fn main() { rahi_cli::run(MyCell); }
```

```toml
# manifest.toml
schema_version = "1.0.0"

[app]
name = "my-cell"
org = "example"

[resources]
tables = ["items"]

[[capabilities]]
id = "items-write"
kind = "db.write"
resource = "items"

[services.items]
capabilities = ["items-write"]

[ledger]
schema_version = "1.0.0"
max_record_bytes = 65536

[observability]
metrics_path = "/metrics"
otel = false

[auth]
operator_role = "my_operator"

[contract]
version = "1.0.0"
```

The run, at N=1:

```sh
export RAHI_PUBLIC_URL=https://cell.example.com RAHI_DATA_DIR=/data
my-cell first-boot    # mints the key set and renders rauthy's environment, once
my-cell migrate       # a deploy step; serve refuses a store behind the migrations
my-cell supervise     # rauthy (RAHI_RAUTHY_BIN) and serve as one unit
```

`RAHI_RAUTHY_MODE=none my-cell serve` serves without identity, which is
what a test without rauthy uses. `apps/hello-cell` is the complete example:
an outbox row and a revision stamp in the same `txn`, an operator surface,
a page, and the end-to-end test.

### 2.4 The build check is opt-in

**Verified** (`crates/rahi-kernel/src/verify.rs`, spec 034 D-6). The
ceiling is held at build only by a cell that calls `rahi_kernel::verify!`
from a test, as `apps/hello-cell/tests/verify.rs` does; nothing in the
composer or in `cargo build` calls it. It reads source text: literal
`Governed::new` triples and `#[governed(...)]` comment markers within five
lines of an opaque call. **Recommendation**: every consumer adds the same
test and runs it in CI.

### 2.5 Packaging an out-of-tree cell

**Current:** spec 039 supplies `rahi-runtime` with the pinned rauthy,
entrypoint, non-root user, and static directory. A consumer adds its binary
at `/usr/local/bin/rahi` and assets at `/usr/local/share/rahi/static`.
`RAHI_STATIC_DIR` selects that directory. Use 0.2.0 only after publication
and verification of its image digest and anonymous availability. The
following is the pre-039 observation, preserved as historical evidence.

**Verified** (`docker/Dockerfile`). The image recipe builds a package of
*this* workspace (`COPY . .`, `cargo build -p $RAHI_PACKAGE`), so a cell
in another repository writes its own Dockerfile today. It needs three
things, all in rahi's: rauthy's binary from the pinned image by digest at
`/usr/local/bin/rauthy` (`RAHI_RAUTHY_BIN`), the cell binary, and the
entrypoint's three lines (`first-boot`, `migrate`, `exec supervise`).

**Verified** (`docker/Dockerfile`, `apps/hello-cell/src/cell.rs:47`, and
by running the image). A `static_dir()` under `env!("CARGO_MANIFEST_DIR")`
names the build machine's source tree, and the runtime stage copies only
the two binaries. The hello-cell image, built from this Dockerfile with
`RAHI_PACKAGE=hello-cell`, came up with the pinned rauthy 0.36.2 and
answered `/readyz` in 14 seconds; discovery through the proxy named the
issuer with its trailing slash; and `/`, `/index.html`, and `/app.js`
answered `404`, because `/src/apps/hello-cell/web` does not exist in the
image. Spec 034's compose procedure (AC-2), which says "see the page", was
recorded and never run (034 D-1); it fails at that step.
**Recommendation**: until spec 039 settles it, copy the assets into the
image and return that runtime path from `static_dir()`.

## 3. What is proven, and against which identity provider

**Updated 2026-09-17 (spec 037).** The ordinary cargo gate runs without
rauthy. The separate `live.yml` workflow extracts the binary from the
rauthy 0.36.2 image pinned by digest in `docker/Dockerfile`, provisions the
live fixture, and runs the whole workspace with `RAHI_REQUIRE_RAUTHY=1`:
a missing live fixture fails instead of skipping. Spec 037's Status records
the passing run and revision. This workflow remains advisory by the
owner's decision. `docker/smoke.sh` separately covers readiness, discovery
through the proxy, and clean SIGTERM inside the built image.

| Property | Proven in CI (no rauthy) | Proven against a real rauthy |
|---|---|---|
| browser login through the cell's origin | stub OIDC (`crates/rahi-idp/tests/session.rs`) | `apps/hello-cell/tests/e2e.rs`, `crates/rahi-harness/tests/boot.rs`, when `RAHI_TEST_RAUTHY` is set |
| bearer resource server | stub key set (`crates/rahi-idp/tests/bearer.rs`) | `bearer.rs` live test: administered audience, scoped admission, insufficient-scope refusal, and real-token wrong/missing-audience controls |
| governed write with its outbox row | the pieces separately (`rahi-store/tests/outbox.rs`, `rahi-kernel/tests/adjudicate.rs`) | whole, as an authenticated user, in the e2e only |
| a ledgered denial | `rahi-kernel/tests/adjudicate.rs`, `rahi-edge/tests/stream.rs` | the e2e (`db.migrate` refused, the denial is the chain's last record) |
| streaming | `rahi-edge/tests/stream.rs`, ten tests | not exercised with identity |
| migration | `rahi-cli/tests/cli.rs`, `rahi-store/tests/migrate.rs` | the e2e |
| backup | stub rauthy backup routes in the verb tests (`rahi-ops/tests/backup.rs`, `rahi-cli/tests/cli.rs`) | against a real rauthy since 2026-09-17: `apps/hello-cell/tests/e2e.rs` and `rahi-ops/tests/rauthy_backup_admin.rs` (spec 037 B-1, B-6) |
| restore | archive validation and actual supervised restore handoff (`rahi-ops/tests/{restore,rauthy_restore}.rs`, `rahi-cli/tests/cli.rs`) | the e2e restores identity, logs in with the original password and `sub`, reads the original note, and verifies the original ledger head |
| restart with the chain verified | `rahi-ledger/tests/verify.rs`, `rahi-store/tests/cache.rs` (in process) | the e2e's reboot on the restored volume |

**Historical evidence through 2026-09-12** (spec 034 §8, and the
maintainer's local rauthy checkout):
the "driven green against a native rauthy 0.36.0" run used a debug build
of an unreleased rauthy branch (`feat/rfc9068-at-jwt`), not a release;
rahi does not depend on that branch's `at+jwt` header. The same run
passed again at `444bcf8` on 2026-09-11 (76 seconds, with the exported
chain verified by the independent `attest-ledger` CLI). The pinned
release, 0.36.2, has met the chassis in `docker/smoke.sh` and in the image
run of 2.5, never in a login. *Superseded 2026-09-12:* by hand, in a Linux
container, the pinned 0.36.2 binary drove hello-cell's end-to-end test and
the harness's login test green, and a restore with identity was
demonstrated, then still not in CI (note 02 sections 1 and 4). Spec 037
supersedes that CI limitation and removes the empty rauthy-gated discovery
test; the image smoke test carries that discovery proof (037 B-4).

**Implemented by spec 037:** the live CI job, bearer proof, restart
without restore, and recovery with identity. Streaming with real identity
remains outside this proof; its coverage above is unchanged.

## 4. What the kernel enforces, and what it does not

**Verified**:

- Every call through a `Governed` facade is adjudicated against the
  manifest's grants before it runs (spec 015 B-4, B-5). An ungranted
  `(service, kind, resource)` is refused with `Error::Denied` carrying a
  decision id, which the edge renders as `403` with the id in the body.
- `AppState::store()` returns the raw `StoreHandle` to every route
  (`crates/rahi-edge/src/state.rs:82`); a cell needs it to build its
  facades. A write through the raw handle is neither adjudicated nor seen
  by `verify!`. Deny by default holds for governed call sites, not for the
  process.
- The verifier is text over `src/`, not whole-program effect analysis, and
  it is opt-in (2.4). The kernel is not a sandbox: a cell's code runs with
  the process's privileges.
- HTTP authorization is the edge's: routes are authenticated by default,
  public only when named, operator routes behind one role, bearer routes
  behind scopes (specs 024, 025).

**Unresolved** (owner: the rahi maintainer): whether to close the raw
handle, by giving routes a facade factory instead of `StoreHandle` and
teaching `verify!` to flag raw use. It is a breaking change to `AppState`
and to every cell, so it waits for a release line (spec 039).

## 5. What the chain records, and when a record can be lost

**Current qualification:** the shutdown and replica-ID findings below
predate 035 and were corrected in 0.1.0: shutdown drains within a bound,
loss counters distinguish causes, and IDs include the replica node.
This does not provide lifetime ID uniqueness across sealed history.
The independent archived-retry limitation in section 2.0 remains in 0.2.0.
The following dated findings preserve the evidence that motivated 035.

**Verified** (`crates/rahi-kernel/src/lib.rs`, `adjudicate.rs`,
`crates/rahi-edge/src/obs/mod.rs`, spec 015 B-6, D-5, D-7):

| Event | Recorded? |
|---|---|
| an ordinary allow | never (015 D-5: the chain is not the request log) |
| a deny or a degrade | queued, then appended by one background task |
| genesis | once, at the first open of an empty chain |
| anything a cell does outside a facade | never |

A deny or degrade is answered before its record is durable. The record
is lost in four ways:

1. **The queue is full.** 1024 records by default; `try_send` fails and
   the record is dropped. Counted.
2. **The append fails** after its three CAS attempts, or on a store error.
   Counted.
3. **The process stops.** `serve` never calls `Kernel::flush` on shutdown,
   so whatever is still queued when the runtime ends is gone. **Not
   counted and not logged.** Reproduced: fifty concurrent denials, every
   one answered `403` with a decision id, then SIGTERM; in two runs 40 and
   33 of the 50 reached the chain, and nothing in the log or the metrics
   said so.
4. **Two replicas mint the same id.** A decision id is
   `kernel:<last 16 hex of the chain head at boot>:<counter>` (spec 015
   D-8), with no replica in it. Replicas that boot on the same head, which
   spec 032's `podManagementPolicy: Parallel` makes the usual case, share
   the id sequence; the second append of an id is refused
   `Error::Conflict` (`crates/rahi-ledger/src/append.rs`, `classify`), so
   that denial is lost, and two callers hold one id for two denials.
   Counted, as a ledger failure. **Verified** in process: two kernels
   booted over one store and one chain, as two replicas reading the head
   through the leader are, each denied one request; both answered
   `kernel:0910ee4a5d5baaf1:000000000000`, one record landed, and the
   observer reported one `conflict`. Not reproduced on a live three-node
   cluster. *Superseded 2026-09-12:* reproduced with three independent
   processes forming one three-node cluster on loopback (not Kubernetes):
   30 denials answered, 10 distinct ids, 10 records, 20 lost; and the
   shutdown loss re-measured at 142 to 196 of 200 (note 02 sections 2, 3).

Counted means one `kernel_ledger_failures_total` increment and one error
line at target `rahi.decision`. `/metrics` does not separate a dropped
record from a failed append; only the log line's text does.

A decision id is therefore unique within a single replica's life, not
across replicas, and not across a restore, which rewinds the head the ids
are minted from.

Constitution X says the kernel "ledgers every denial". Under a graceful
shutdown it does not, and at N=3 it does not for colliding ids.
**Recommendation** (draft spec 035): drain the queue on shutdown within a
bound, count what the bound abandons, expose a separate dropped counter,
and put the replica's node id in every decision id.

What is guaranteed durable, **Verified**: any `txn` is one Raft entry and
one SQLite transaction, replicated before it returns; an outbox row and
the write it describes commit together; a fenced write under a superseded
lease is refused whole; a chain append that returns is CAS-protected
against forks and verified at every later boot.

### 5.1 A critical action journal for a consumer

A consumer that must record an intent before an external side effect, and
its outcome after, can build that journal today from `txn`, the outbox,
fenced leases, and the watermark, as ordinary tables in its own
migrations. **Verified** that each piece exists and behaves as below
(specs 011, 012).

```
1. txn { INSERT action (id, idempotency_key UNIQUE, state='intended', target, request_digest, actor_sub, fence)
         Watermark::next(action)
         Outbox::stage(envelope for the worker) }
2. the worker holds a Lease on the action's key and performs the side effect
   with the idempotency key the external system honours
3. fenced_txn(lease) { UPDATE action SET state='done' | 'failed', outcome_digest = ... }
4. on restart, a controller reads Watermark::since and every 'intended' row,
   and reconciles each against the external system before retrying
```

What that gives: durability before the side effect, atomicity with the
business row, replication, fencing against a stale worker, and recovery by
reconciliation. What it does not give: tamper evidence. App rows are not
hash-linked, not signed, and not verified at boot; only
`kernel_decisions` is. `Ledger::append` is public and reachable through
`AppState::ledger()`, but it is its own `txn`, never atomic with a
business write, and its handle is documented as the kernel's.

**Recommendation**: the journal needs no new chassis primitive unless the
consumer needs its action records to be tamper-evident in the cell's own
chain. **Unresolved** (owner: Statecraft): whether it does. If it does,
the chassis change is an app record that commits in the same `txn` as the
business write: read the head, build and sign the record, submit both, and
retry the whole `txn` when the parent index refuses. That would be its own
spec; this note does not draft it until a consumer asks.

`Outbox::drain` has no caller in the chassis or in hello-cell: a cell that
stages envelopes also runs the drain loop (spec 012 §6).

## 6. Replication and recovery

**Verified** (specs 030 to 032, `crates/rahi-ops/src/{backup,restore,supervise}.rs`):

- Two state systems per replica: the app's hiqlite (`/data/hiqlite`, the
  chain inside it) and rauthy's (`/data/rauthy`), separate Raft clusters,
  one key set for both.
- `rahi backup` builds one archive of the app snapshot, rauthy's snapshot
  fetched over its HTTP API, and the keys, sealed to the backup key. A
  missing part is an error. *Resolved 2026-09-17 (spec 037 B-1):* rauthy
  accepts only an admin session with MFA satisfied on its backup routes, so
  the deployment carries a dedicated rauthy admin of its own whose only
  credential is a passkey custodied in the key set. `first-boot` mints the
  private key, `supervise` registers its public half and converts the
  account to passkey only (no password, so nothing behind the verb
  expires), and `rahi backup` completes rauthy's WebAuthn assertion from
  the custodied key with `ADMIN_FORCE_MFA` left on. Measured against the
  pinned `0.36.2`: `POST /auth/v1/backup` answers `204` and the snapshot
  downloads. A key set minted before this holds no `backup_passkey.json`,
  and both verbs say so by name.
- The app half of the verb no longer races, and "newer" is not file age.
  *Resolved 2026-09-17 (spec 037 B-2):* hiqlite ignores a backup request
  within sixty seconds of the one before **and acknowledges it as success**,
  so reporting the newest file on disk reports somebody else's snapshot.
  Both stores now serialise their own requests, wait the window out before
  triggering, accept only a snapshot whose name carries a second at or after
  their own trigger, and refuse under one deadline (120 seconds by default)
  rather than sealing a stale file. A backup taken soon after another
  therefore takes about a minute.
- The two halves are each fresh as of their own trigger and are **not** a
  coordinated instant. Nothing supports a claim of one evidence head across
  the app store and rauthy.
- `rahi restore` runs on a stopped volume, checks every part's hash,
  resets the app node, restores the keys, and places rauthy's snapshot
  under `/data/restore/rauthy/`. *Resolved 2026-09-17 (spec 037 B-3):* the
  next supervised start hands that file to rauthy's own hiqlite restore,
  waits for rauthy's health, and records the application in the marker;
  every later start passes nothing, and the app still never opens rauthy's
  directory. A restored cell comes back with its users.
- Every principal id in app rows is rauthy's `sub` (constitution VII), and a
  restored cell hands back the same one: `apps/hello-cell/tests/e2e.rs`
  asserts the original `sub`, the note readable behind that principal's
  session on the restored cell, and the same ledger head, against the pinned
  rauthy and with nothing stubbed. All of it is N=1; restore at N=3 is
  deferred (spec 037 D-2).
- Restore does not compare the archive's manifest hash or schema versions
  with the running binary; the next `serve` finds out (section 7).
- Sealed chain segments live in the ledger archive, not in the backup
  (spec 014 B-6).
- A restored volume must reopen on the ports it was written under
  (spec 034 D-5).

*Spec 037 implements the recovery recommendation. Its D-7 correction
resolves the live bearer fixture failure on pinned rauthy 0.36.2: the
dynamically registered client receives an administered `default_aud`,
which the fixture reads back before PKCE without `resource` parameters.
The production audience validator remains unchanged. Real signed tokens
with missing or wrong cell audiences are refused.*

## 7. Manifest evolution

**Verified** (`crates/rahi-ledger/src/chain.rs`, `verify.rs`,
`crates/rahi-kernel/src/lib.rs`, `manifest.rs`; spec 015 B-2, B-8), and
reproduced with the scratch consumer of 2.2:

- The manifest hash is sha256 over the canonical JSON of the whole parsed
  model and the gate's config hash. Reordering keys or editing comments
  does not move it; any semantic change does, including `otel`,
  `contract.version`, and `operator_role`.
- The chain's genesis record links to the manifest hash of the first boot.
  `Ledger::open` checks the first resident record against the booted
  manifest's hash on every open.
- Adding one grant and rebuilding: `serve` and `ledger verify` exit 1 with
  `integrity: ... record(s) unreachable from the genesis parent`, and
  `migrate` exits 0. Reverting the manifest makes the volume boot again.
  The error does not say the manifest changed; spec 015 B-8's clearer
  message is never reached, because the ledger refuses first.
- Spec 015 B-8 names "a deploy genesis record" as the missing step. No
  spec defines one and no code writes one.

So today **a deployed cell's manifest is frozen at its first boot.** There
is no supported way to add a capability to a running cell without losing
its chain.

Migrations, **Verified** (`crates/rahi-store/src/migrate.rs`,
`crates/rahi-ops/src/migrate.rs`), and reproduced where marked:

- Forward-only; no down migrations.
- `serve` refuses a store behind the cell's migrations (exit 2) and
  accepts a store ahead of them. A rolled-back binary serves on a newer
  schema without a warning.
- `schema_version` keeps a version and a name, not a checksum. A
  migration whose SQL changed under an applied version is skipped as
  applied (reproduced: a version 1 with different SQL was never run).
- The manifest and the migrations are independent: a release that changes
  both can apply its migrations (the Job, spec 032) and then fail to serve
  on the manifest.

Restore, **Verified**: no compatibility check at restore time (section 6).
Mixed versions during an N=3 rolling update: nothing specified.

The consumer procedure today, **Verified** as the only one that keeps the
chain:

1. Treat the manifest as immutable for the life of a volume.
2. Grant generously at first boot only if you accept the wider ceiling.
3. To change the ceiling, stand up a new volume with the new manifest; the
   old chain stays verifiable on the old volume with the old binary
   (`ledger verify`, `ledger export`), and the app's data moves by the
   consumer's own means. Both are losses this note does not recommend
   living with.

**Recommendation** (draft spec 036): a manifest transition record appended
by an explicit deploy verb, a boot check against the chain's current
manifest instead of its genesis parent, migration checksums, a refusal of
a store ahead of the binary unless the cell declares it compatible, and a
restore that checks compatibility before it writes. Spec 036 carries the
worked consumer example for an upgrade and a rollback.

## 8. The identity contract for a hosted control plane and a CLI

rauthy supplies the grants; the chassis validates what rauthy issued; a
command-line client implements its own client flow. **Verified**
(`crates/rahi-idp/src/{config,discovery,jwks,bearer,scope,session,refresh,registration,resource}.rs`,
specs 021, 022, 025; rauthy source at the pinned tag):

| Item | As built |
|---|---|
| issuer | `<RAHI_PUBLIC_URL>/auth/v1/`, trailing slash included (021 D-10) |
| discovery | `<origin>/auth/v1/.well-known/openid-configuration`, rauthy's own document through the raw proxy |
| key set | the document's `jwks_uri`; the cell fetches it over loopback, keeps the current and the previous set, refreshes hourly and once on an unknown `kid` |
| protected resource metadata | `<origin>/.well-known/oauth-protected-resource` (RFC 9728), served by the cell |
| audience | mandatory, exactly the cell's origin (025 D-2); rahi reads no `resource` parameter itself |
| algorithm | RS256 only |
| token type | access tokens on bearer routes; id tokens only inside the login flow |
| scopes | exact, space-delimited; a route declares one with `RequireScope` inside `with_bearer` |
| clock leeway | 60 seconds on `exp` and `nbf` |
| principal | `sub` verbatim; roles from the `roles` claim; for a bearer, rebuilt from the token on every request |
| browser session | `__Host-session` (`session` over http), no `Max-Age`; a cached assertion for 900 seconds, then a renewal round trip that re-reads roles |
| bearer lifetime | not configured by rahi; rauthy's client default, 1800 seconds in 0.36.2 |
| revocation, browser | at the next renewal, so within 900 seconds of a user being disabled |
| revocation, bearer | none before `exp`: the `jti` deny-list and its writer (`ResourceServer::deny`) exist, and nothing calls the writer outside a test (025 B-5 says logout and a verb would); `preflight` does not report the bound |
| introspection | rauthy has `/auth/v1/oidc/introspect`; the cell never calls it (025 D-1) |
| the cell's own client | one confidential client bootstrapped at first boot through rauthy's admin API; redirect `<origin>/session/callback` |
| dynamic registration | rauthy's `/auth/v1/clients_dyn` through the proxy, advertised in the resource metadata; `RAHI_IDP_REGISTRATION` defaults to `token` |
| device authorization | rauthy's `POST /auth/v1/oidc/device` and the page `GET /auth/v1/device` are reachable through the proxy; the chassis implements no leg of it (025 B-8) |
| client credentials | validated when a token arrives (`sub == azp` marks a service principal, 025 D-4); nothing provisions such a client, and the chassis never mints a key (025 B-1) |
| CSRF on a bearer route | 025 B-11 exempts bearer routes; the edge's CSRF layer exempts only `/auth/*` and the composer applies no bearer predicate (025 D-6 "what remains is wiring"), so a `POST`, `PUT`, or `DELETE` with a bearer token is refused `403 csrf` unless it also carries a `csrf` cookie and an equal `X-CSRF-Token` header; the pair is compared, not issued-checked, so any equal pair passes |

What a CLI device-code login needs that the chassis does not provide,
**Verified** against rauthy 0.36.2's source:

1. A public client with `device_code` in its enabled flows. Nothing
   provisions one; rauthy's device endpoint demands the secret from a
   confidential client, which a CLI cannot keep.
2. RS256 and the audience. rauthy signs a new client's tokens with EdDSA,
   and its device grant request has no `resource` parameter, so the only
   way to put the cell's origin in `aud` is the client's `default_aud`,
   set by an admin. A dynamically registered client needs the same admin
   call before its tokens pass (025 D-12).
3. Revocation of a CLI's token before expiry. Not available (above).
4. Writes without a CSRF pair. Not available until the exemption is wired
   (above).
5. A worked example. hello-cell mounts no bearer route, and its resource
   metadata advertises `scopes_supported: []`.

**Recommendation** (draft spec 038): wire the CSRF exemption for bearer
routes, provision the CLI's public client at first boot from the manifest,
bind it to RS256 and the cell's audience, set the access token lifetime
explicitly, wire the deny-list to logout and to an operator verb, and
report the bound in `preflight`.

**Unresolved** (owners: Statecraft and the CLI): the scopes the control
plane's API declares, the client id the CLI presents, whether refresh
tokens are held by the CLI (rauthy's device-grant refresh lifetime
defaults to 72 hours), and whether a runner authenticates as a user, as a
client-credentials service principal, or through a token the control
plane mints. The chassis never mints one (025 B-1), so the last option is
a consumer design outside the chassis. *Added 2026-09-12:* note 02 section
6.1 answers Statecraft R-1 and CLI D72, separates today's validation from
the proposed renewal, and recommends a client-credentials principal per
runner; 025 B-5's "fifteen minutes" is the revocation lag (900 s), not a
lifetime the chassis enforces, and it is shorter than rauthy's default
token lifetime.

## 9. Requests to other repositories

These are requests, not decisions made for those repositories.

**Statecraft**:

1. Decide whether the action journal must be tamper-evident in the cell's
   chain (5.1). If not, build it on `txn`, the outbox, and fenced leases
   and say so; if so, ask for the spec 5.1 sketches.
2. Review draft spec 036 before committing to a first manifest, and state
   how often the control plane expects its ceiling to change.
3. Name the control plane's API scopes and whether runners are users,
   service principals, or holders of a control-plane credential (8).
4. Treat rauthy's database as tier-one state: until spec 037 lands, a
   restore does not bring users back.
5. Pin a commit per 2.2 and say which release line you need; spec 039
   proposes the tags.

**statecraft-cli**:

1. Implement the device authorization grant (RFC 8628) against the
   discovery document at the cell's issuer: polling interval 5 seconds
   and code lifetime 300 seconds by rauthy's defaults, both from the
   response.
2. Present the public client id spec 038 provisions; request only the
   control plane's declared scopes; expect RS256 tokens whose `aud` is the
   cell's origin.
3. Treat `401` with `WWW-Authenticate: Bearer resource_metadata=...` as the
   bootstrap (025 B-6) and `403 insufficient_scope` as final.
4. Never send a session cookie with an `Authorization` header (`400`,
   025 B-10). Until spec 038 wires 025 B-11's exemption, send an equal
   `csrf` cookie and `X-CSRF-Token` header on every unsafe method.
5. Assume a stolen token stays valid until `exp` (up to 1800 seconds plus
   60) until spec 038 lands, and keep tokens out of logs and receipts.

**hqgit**:

1. Keep the repository evidence DAG out of the cell's store; the chassis
   gives an hqgit cell its own hiqlite and chain, nothing shared.
2. hqgit spec 003 B-2 lists the chassis crates without `rahi-cli`, the one
   that exports `Cell` and `run`; add it.
3. Note that the chain records denials, not allows, and loses queued
   denials at shutdown and colliding denials at N=3 until spec 035 lands;
   evidence that must be complete belongs in hqgit's own records.
4. Reference a rahi deployment epoch by its record hash, never by its
   number alone, which a restore can reuse (section 11, draft 041 B-7).

The runtime binding of section 11 adds these, which are requests for
agreement before either side implements, not decisions made here. They
continue the numbered lists above.

**Statecraft**, continued from item 5:

6. Name the deployment record's type URI and schema version and publish
   one fixture. The epoch record (draft 041 B-3) stores `{type, digest,
   id}` for it and never parses it.
7. Decide whether the composing envelope wraps a replica's `/binding`
   document or references it by digest, and name the schema you will
   accept (`rahi.binding/v0` in draft 040 is a placeholder).
8. Key runtime observations by `service.instance.id` and the epoch's
   record hash. Do not ask the chassis for a digest, an instance id, or a
   deployment id as a metric label (draft 040 B-9).
9. When you deploy a cell, set `RAHI_ARTIFACT_IMAGE` to the digest you
   pinned and `RAHI_DEPLOYMENT_REFS` on the migration Job, and after a
   restore run the Job before the replicas start (draft 041 B-12).
10. Keep effect-time revalidation in the broker: compare the epoch a
    permit was issued against with the cell's current one. The chassis
    reports the epoch; it evaluates no validity predicate.

**statecraft-cli**, continued from item 5 (as the proposed home of the
neutral verifier):

6. Agree the in-toto Statement and SLSA provenance versions and how a
   provenance names a cell's binary (its sha256, one subject per
   platform), which the verifier checks against the epoch record's
   `artifact.binary`.

**spec-spine**:

1. Name the authority snapshot's type URI and digest rules, so that a
   build provenance can list the snapshot as a material and the epoch
   record's `refs.authority` can carry it without rahi interpreting it.

**aicortex** (not in the assignment, surfaced because it is a consumer):
its spec 010 D-1 requires rahi from a registry by exact version, which
cannot be met until spec 039 publishes; its B-2 also omits `rahi-cli`.

## 10. Open decisions for the rahi maintainer

*Updated 2026-09-12:* note 02 section 8 carries these with a proposal and
the evidence for each, plus the decisions owned by Statecraft and hqgit.

| Decision | Options | Where |
|---|---|---|
| ~~backup against a real rauthy~~ | closed 2026-09-17: a dedicated passkey-only backup admin the verb logs in as | 037 B-1, D-3 |
| manifest transitions | the shape in draft 036, or a new volume per ceiling change | draft 036 |
| a store ahead of the binary | refuse by default, or accept as today | draft 036 |
| release channel | git tags only, or tags plus crates.io | draft 039 |
| image for out-of-tree cells | a published base image carrying rauthy and the entrypoint, or a documented Dockerfile each consumer copies | draft 039 |
| the raw store handle | keep, or replace with a facade factory in a release line | section 4 |
| a decision id across replicas | a node segment in the id (proposed), the node folded into the nonce, or a counter offset per node; each changes the shape 015 D-8 records | draft 035 B-6 |
| where `/binding` is exposed | beside `/metrics`, kept off the ingress (proposed), behind the operator role, or behind a bearer scope | draft 040 |
| manifest transition and deployment epoch | two record kinds (proposed, so 036 is not held by the cross-repository agreement 041 needs) or one | drafts 036, 041 |
| a binding mismatch at boot | a signal only (proposed) or refusable under a manifest option | draft 041 B-8 |
| the sequencing plan | the drafts sit outside thesis §5's waves; adding them there is a thesis change | spec 002 |

## 11. Runtime binding

The family's evidence chain (the September 11 realignment) wants a
control plane to correlate an immutable artifact, the authority snapshot
it was built under, a deployment and its epoch, and a running replica,
without a hash cycle. rahi's share is identity and metadata a replica
reports reliably, and a record in its own chain of each deployment it
serves under. Evaluating any of it is the consumer's.

### 11.1 What a replica can say about itself today

**Verified** at `444bcf8` by reading the code:

| Identifier | Where it exists | Readable by an operator or consumer |
|---|---|---|
| chassis version | `CARGO_PKG_VERSION` | `rahi version` prints `rahi 0.1.0`; a backup's `manifest.json` records it |
| build revision | nowhere: no `build.rs`, no `option_env!` | no |
| executable digest | nowhere | no |
| image digest | `image.yml` logs it and names a one-day workflow artifact after it; no provenance, no SBOM; the Dockerfile has no `LABEL`; `deploy/k8s` pins no digest | no |
| manifest hash | computed at boot (015 B-2) | only as the genesis record in `ledger export`, in a backup's `manifest.json`, and in every denial's payload; not on `/readyz`, `/metrics`, preflight, or a log line |
| `contract.version`, `app.org` | parsed and validated | read by nothing |
| chain identity | the genesis record; the ledger public key beside every record | `ledger export`, on a stopped volume only (11.4) |
| replica | hiqlite node id, the pod ordinal plus one | not reported; OTel resource carries `service.name` only |
| process incarnation | nothing names one | no |
| metrics | every label a closed vocabulary (023) | no build or identity family |
| traces | batch exporter, default sampler keeps every span, ring of 1,000 | export loss is not counted |

### 11.2 A record order with no cycle

**Recommendation** (drafts 040 and 041). Each record names only records
that exist before it:

```
authority snapshot   spec-spine                over source
build provenance     the build platform        in-toto Statement, SLSA predicate;
                                                subjects: image digest, cell binary sha256 per platform;
                                                materials: source revision, authority snapshot
deployment record    Statecraft or operator    names the provenance and the image
epoch record         rahi chain (041)          names deployment, provenance, snapshot,
                                                measured binary, declared image, current manifest
replica binding      rahi /binding (040, 041)  names its epoch, binary, manifest, instance
decisions            rahi chain (041 B-9)      name their epoch, instance, and manifest
observations         the consumer's collector  name instance, epoch, interval, coverage
```

The image contains none of these: it cannot contain its own digest, and
the chassis writes no deployment id, epoch, or snapshot into a built
file. The manifest stays the ceiling and the TOML it is written in; no
extracted application model replaces it. The genesis and every record
already in a chain stay byte for byte; epoch 0 is the genesis and needs
no backfill. A deployment outcome the consumer writes after the rollout
may name the epoch record's hash; that keeps the direction.

Every value a replica reports carries its basis: `measured` (computed by
the process from bytes it read: its executable's digest, the manifest
hash), `declared` (given to it and unchecked: the image digest, the build
revision, the deployment references), or `absent` with a reason. A digest
the process measures of itself catches the wrong image or a stale node;
it does not stand against an adversary inside the process.

### 11.3 Upgrade, changed manifest, rollback, restore

**Recommendation** (draft 041, whose worked example is the reference):

- **Upgrade.** The deploy step (the Job at N=3, the entrypoint at N=1)
  applies migrations, appends 036's transition when the manifest changed,
  then appends one epoch naming the artifact it measured and the
  references it was given. Replicas roll; until the rollout ends, old
  replicas report the previous epoch and their decisions say so.
- **Changed manifest.** Always through 036's transition, in the same
  step; the epoch names the transition. A replica on the old image that
  restarts after the transition is refused by 036, not by 041.
- **Rollback.** Deploying an older image is a new epoch whose artifact
  equals an earlier epoch's. An artifact never names an epoch; an epoch
  names an artifact, and one artifact can appear in many epochs.
- **Restore.** The archive names the epoch it was taken under. The next
  deploy step appends an epoch with `cause: restore` whose predecessor is
  that archived epoch. What the chain held after the backup is not in the
  restored chain; an epoch is identified by its record hash because a
  restore can reuse a number.
- **N=3, multiple architectures.** The Job measures one platform's
  binary; a replica on another platform reports its binary as `unknown`,
  and the consumer resolves it against the provenance's subjects.

### 11.4 What a consumer can do before 040 and 041

**Verified** as available today, with the limits named:

- Correlate a decision to a ceiling: every denial's payload names the
  manifest hash, and `ledger export` names the genesis.
- Pin the image by digest in the pod spec and read the kubelet's
  `imageID` from the pod status. That is the platform's observation of
  the image, not the cell's.
- Read a live cell's chain: **not possible** today, by reading the code.
  `migrate` and `backup` attach to a running node or connect as a store
  client (`Booted::open_or_attach`, `crates/rahi-cli/src/lib.rs`);
  `ledger verify` and `ledger export` use `Booted::open`, which starts a
  node on the data directory, so they run on a stopped volume only. A
  consumer cannot mark a deployment by the chain head without stopping a
  replica. Draft 041 B-14 proposes the attach path for both.
- Attribute a decision to a replica: **not possible** today. The id names
  no replica and collides at N=3 (section 5).

### 11.5 Trust windows and audit bundles

**Recommendation**. For a validity predicate revalidated at effect time,
the chassis supplies the current epoch's record hash on `/binding` and in
the chain as a freshness input; the broker evaluates the predicate. The
chassis evaluates no trust window (015 §6 leaves that to a later spec; the
sibling `trust-window` crate is not a chassis dependency). For an
independent audit bundle, the chassis supplies the `ledger export` lines
as the original bytes, the verifying public key, the segment references,
and, with draft 041 B-13, a coverage file that states what the export
does not contain: allows, lost denials, and archived segments. The
bundle's envelope is the consumer's.

## 12. Status reconciliation, 2026-09-17

**Verified** on 2026-09-17, during the spec-spine 0.20.0 governance upgrade.
This section reports state; it approves no draft and amends no spec. The
dated readings above are left as the record of their own days.

### 12.1 The corpus

| Item | State on `main` at `7c86771` |
|---|---|
| Specs | 29 total: 24 `approved`, 5 `draft` |
| Implemented since note 02 | 035 (`#46`) and 039 (`#47`, `#48`, `#49`), both `approved` and `implementation: complete` |
| Drafts remaining | 036, 037, 038, 040, 041, all `status: draft` and `implementation: pending` |
| Schedulable | `registry plan` calls 036, 037, 038 and 040 dependency-ready and 041 blocked on 036 and 040. Ready is not approved: `/next` subtracts every draft, so the approved-ready set is empty and no spec is buildable until a human flips one |
| Governance | `spec-spine 0.20.0 check`: registry fresh, index fresh, 142 unwitnessed claims, 142 allowed |

Note 02 section 0 recorded the drafts as living on unpushed local branches.
That is superseded by fact rather than by decision: 035 to 041 are on `main`
since `#45`, and 035 and 039 have since been built.

### 12.2 Spec 039's acceptance, re-measured

| Criterion | State |
|---|---|
| **AC-1** `make ci`, `cargo package --workspace --locked`, `scripts/k8s-validate.sh` | passes locally |
| **AC-2** a `v*` tag publishes both images and they pull anonymously | **not met.** Both images exist and both architectures were pushed, but the GHCR packages are private. Re-measured 2026-09-17 by anonymous registry token: `ghcr.io/statecrafting/rahi:0.1.0` and `ghcr.io/statecrafting/rahi-runtime:0.1.0` each answer `403`, against a `200` control on `ghcr.io/sebadob/rauthy:0.36.2` taken the same minute with the same method |
| **AC-3** spec 034's AC-2 procedure end to end against the published image | **not met, and blocked by AC-2.** The procedure cannot begin while an anonymous pull is refused, so this is unproven rather than failing |
| **AC-4** the nine crates resolve from crates.io at the tag's version, and `release.yml` proves the registry stanza | **met.** Re-measured 2026-09-17: all nine answer `0.1.0` from the crates.io API, and the `consumer-registry` job proved the stanza in the `v0.1.0` run |

AC-2 rests on one action no workflow can take: a package's visibility is a
GitHub package setting, not a property of the push, and GHCR creates a
package private. Until an owner makes both packages public, a consumer
building a cell from the crates is unblocked and a consumer pulling an image
is not. AC-4's passing does not carry AC-2 or AC-3, and spec 039 is not
release-ready on implementation status or on a local `make ci` alone.
