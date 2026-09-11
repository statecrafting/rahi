# The consumer contract, as built

Version 0, 2026-09-11, for review. This note is a consumer's view of the
chassis: how to depend on it today, what it guarantees, what it does not,
and which of the gaps are proposed for change. The design truth stays the
corpus; nothing here amends a spec. The behavior changes this note asks
for are the draft specs 035 to 039, which a human approves or rejects.

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

### 2.1 What is published

**Verified** on 2026-09-11 (`gh repo view`, crates.io API, `git ls-remote`,
`gh run view`, anonymous GHCR queries):

| Artifact | State |
|---|---|
| source | public at `github.com/statecrafting/rahi`, Apache-2.0, clonable without credentials |
| crates on crates.io | none; the ten names are free |
| git tags, GitHub releases | none |
| container image | none; `image.yml` pushes `ghcr.io/statecrafting/rahi:<tag>` only on a `v*` tag, and no tag has been pushed |
| deploy manifests | name `ghcr.io/bartekus/rahi:latest`, which does not exist and which no workflow produces |
| rauthy | the image pins `ghcr.io/sebadob/rauthy:0.36.2` by digest; a Cargo consumer gets no rauthy from rahi |

A workspace version (`0.1.0` in every crate manifest) is a declaration, not
a release. Pin a commit.

### 2.2 The dependency stanza

**Verified**: a scratch consumer outside this repository, with its own
manifest and one migration, built from this stanza with rustc 1.96.0, then
ran `first-boot`, `migrate`, and `serve`, answered `/readyz`, performed one
governed write, was denied one ungranted operation with a decision id, and
passed `ledger verify` with the denial as the chain's last record.

```toml
[dependencies]
rahi-cli    = { git = "https://github.com/statecrafting/rahi", rev = "444bcf8e10d79345afc114b59ec2baaa2c65d324" }
rahi-edge   = { git = "https://github.com/statecrafting/rahi", rev = "444bcf8e10d79345afc114b59ec2baaa2c65d324" }
rahi-idp    = { git = "https://github.com/statecrafting/rahi", rev = "444bcf8e10d79345afc114b59ec2baaa2c65d324" }
rahi-kernel = { git = "https://github.com/statecrafting/rahi", rev = "444bcf8e10d79345afc114b59ec2baaa2c65d324" }
rahi-store  = { git = "https://github.com/statecrafting/rahi", rev = "444bcf8e10d79345afc114b59ec2baaa2c65d324" }
rahi-types  = { git = "https://github.com/statecrafting/rahi", rev = "444bcf8e10d79345afc114b59ec2baaa2c65d324" }
axum = "0.8"

[dev-dependencies]
rahi-harness = { git = "https://github.com/statecrafting/rahi", rev = "444bcf8e10d79345afc114b59ec2baaa2c65d324" }
```

`rahi-cli` is the crate that exports `Cell` and `run`; a consumer that
lists the other chassis crates and not this one cannot compose a cell.
`rahi-idp` is needed only for `Authenticated`, `RequireScope`, and the
bearer layers.

Constraints, all **Verified**:

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

**Verified** (test inventory at `444bcf8`, and `.github/workflows/`). CI
runs `cargo test --workspace --locked` with no rauthy: no workflow sets
`RAHI_TEST_RAUTHY` or pulls a rauthy for the tests. Every
rauthy-gated test prints `skipped` and passes. `docker/smoke.sh` boots the
pinned rauthy inside the built image on pushes to `main`, outside the
required checks, and covers readiness, discovery through the proxy, and a
clean SIGTERM; it runs no verb and no login.

| Property | Proven in CI (no rauthy) | Proven against a real rauthy |
|---|---|---|
| browser login through the cell's origin | stub OIDC (`crates/rahi-idp/tests/session.rs`) | `apps/hello-cell/tests/e2e.rs`, `crates/rahi-harness/tests/boot.rs`, when `RAHI_TEST_RAUTHY` is set |
| bearer resource server | stub key set (`crates/rahi-idp/tests/bearer.rs`) | `bearer.rs` live test, when `RAHI_TEST_RAUTHY_URL` and four more variables are set; driven by hand once (025 D-12) |
| governed write with its outbox row | the pieces separately (`rahi-store/tests/outbox.rs`, `rahi-kernel/tests/adjudicate.rs`) | whole, as an authenticated user, in the e2e only |
| a ledgered denial | `rahi-kernel/tests/adjudicate.rs`, `rahi-edge/tests/stream.rs` | the e2e (`db.migrate` refused, the denial is the chain's last record) |
| streaming | `rahi-edge/tests/stream.rs`, ten tests | not exercised with identity |
| migration | `rahi-cli/tests/cli.rs`, `rahi-store/tests/migrate.rs` | the e2e |
| backup | stub rauthy backup routes everywhere (`rahi-ops/tests/backup.rs`, `rahi-cli/tests/cli.rs`) | **never**: the e2e answers rauthy's backup routes with a stub too (D-3) |
| restore | `rahi-ops/tests/restore.rs`, `rahi-cli/tests/cli.rs` | the app half only: the reboot after restore mounts no identity |
| restart with the chain verified | `rahi-ledger/tests/verify.rs`, `rahi-store/tests/cache.rs` (in process) | the e2e's reboot on the restored volume |

**Verified** (spec 034 §8, and the maintainer's local rauthy checkout):
the "driven green against a native rauthy 0.36.0" run used a debug build
of an unreleased rauthy branch (`feat/rfc9068-at-jwt`), not a release;
rahi does not depend on that branch's `at+jwt` header. The same run
passed again at `444bcf8` on 2026-09-11 (76 seconds, with the exported
chain verified by the independent `attest-ledger` CLI). The pinned
release, 0.36.2, has met the chassis in `docker/smoke.sh` and in the image
run of 2.5, never in a login. `crates/rahi-idp/tests/discovery.rs` line
307 is gated on `RAHI_TEST_RAUTHY` and never boots anything even when it
is set.

**Recommendation** (draft spec 037): a CI job that runs every
rauthy-gated test against the pinned release, and an end-to-end test that
also covers a bearer route, a stream, a restart without restore, and
identity after restore.

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

**Verified** (`crates/rahi-kernel/src/lib.rs`, `adjudicate.rs`,
`crates/rahi-edge/src/obs/mod.rs`, spec 015 B-6, D-5, D-7):

| Event | Recorded? |
|---|---|
| an ordinary allow | never (015 D-5: the chain is not the request log) |
| a deny or a degrade | queued, then appended by one background task |
| genesis | once, at the first open of an empty chain |
| anything a cell does outside a facade | never |

A deny or degrade is answered before its record is durable. The record
is lost in three ways:

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

Counted means one `kernel_ledger_failures_total` increment and one error
line at target `rahi.decision`. `/metrics` does not separate a dropped
record from a failed append; only the log line's text does.

Constitution X says the kernel "ledgers every denial". Under a graceful
shutdown it does not. **Recommendation** (draft spec 035): drain the queue
on shutdown within a bound, count what the bound abandons, and expose a
separate dropped counter.

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
  missing part is an error. rauthy 0.36 accepts only an admin session on
  its backup routes and the verb presents the admin API key, so **every
  backup against a real rauthy fails today** (030 D-3). Choosing between a
  rauthy change and a backup session is a human decision still open.
  Observed in the hello-cell image with rauthy 0.36.2: `rahi backup`
  inside the running container ends with `unauthorized: rauthy refused
  the admin token at .../auth/v1/backup (401 Unauthorized)` and writes no
  archive.
- The app half of the verb races. hiqlite 0.14 documents that
  `Client::backup` returns before the file exists, and `Store::backup`
  lists the backup directory once, immediately after, so it can report
  `upstream: backup completed but no new file was listed` (exit 3) for a
  backup that lands a moment later. Observed on two of four runs against
  the same running container. The tests pass because nothing there is
  large or busy.
- `rahi restore` runs on a stopped volume, checks every part's hash,
  resets the app node, restores the keys, and places rauthy's snapshot
  under `/data/restore/rauthy/`. **Nothing hands that snapshot to rauthy.**
  030 D-2 said spec 031's supervisor would; 031 does not, and the
  supervisor starts rauthy from its rendered environment only. The module
  comment of `crates/rahi-ops/src/restore.rs` still says the supervisor
  hands it over; the code does not. A restored
  cell keeps its rows, its chain, and its keys, and its rauthy starts from
  whatever `/data/rauthy` holds, which on a fresh volume is nothing.
- Every principal id in app rows is rauthy's `sub` (constitution VII). A
  restore without rauthy's state leaves every such row unreachable.
- Restore does not compare the archive's manifest hash or schema versions
  with the running binary; the next `serve` finds out (section 7).
- Sealed chain segments live in the ledger archive, not in the backup
  (spec 014 B-6).
- A restored volume must reopen on the ports it was written under
  (spec 034 D-5).

**Recommendation** (draft spec 037): resolve D-3, make `Store::backup`
wait for the file hiqlite writes in the background, hand the restored
snapshot to rauthy once through the supervisor, and prove identity after
restore against the pinned release.

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
a consumer design outside the chassis.

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
   denials at shutdown until spec 035 lands; evidence that must be
   complete belongs in hqgit's own records.

**aicortex** (not in the assignment, surfaced because it is a consumer):
its spec 010 D-1 requires rahi from a registry by exact version, which
cannot be met until spec 039 publishes; its B-2 also omits `rahi-cli`.

## 10. Open decisions for the rahi maintainer

| Decision | Options | Where |
|---|---|---|
| backup against a real rauthy | an upstream rauthy change accepting an API key with backup access on its backup routes, or a dedicated backup admin session the verb establishes | 030 D-3, draft 037 |
| manifest transitions | the shape in draft 036, or a new volume per ceiling change | draft 036 |
| a store ahead of the binary | refuse by default, or accept as today | draft 036 |
| release channel | git tags only, or tags plus crates.io | draft 039 |
| image for out-of-tree cells | a published base image carrying rauthy and the entrypoint, or a documented Dockerfile each consumer copies | draft 039 |
| the raw store handle | keep, or replace with a facade factory in a release line | section 4 |
| the sequencing plan | the drafts sit outside thesis §5's waves; adding them there is a thesis change | spec 002 |
