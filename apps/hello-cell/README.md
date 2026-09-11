# hello-cell

The smallest complete governed cell, and the thing a consumer copies from
(spec 034). It is an example, not a template: nothing stamps from it.

One binary composes the chassis. The app is a manifest, one migration, a
notes resource behind login, an operator route, and a page. Everything
else (serve, preflight, migrate, backup, restore, ledger verify, supervise,
first-boot) is the composer's (spec 030), and the app inherits it by
calling `rahi_cli::run`.

```text
apps/hello-cell
  manifest.toml        the ceiling: one table, one kv prefix, the notes service's grants
  src/main.rs          fn main() { rahi_cli::run(HelloCell) }
  src/cell.rs          impl Cell: manifest, migrations, routes, operator routes, static dir
  src/migrations.rs    the coordination tables (spec 012), then notes
  src/notes.rs         POST/GET /api/notes, DELETE /api/notes/{id}, POST /api/notes/migrate
  web/                 the page: login, list, add, logout; no framework
  tests/e2e.rs         the end-to-end proof through rahi-harness
  tests/verify.rs      the ceiling, verified at build
```

## The manifest

`manifest.toml` grants the `notes` service `db.read`, `db.write`, and
`db.txn` on `notes`, `kv.get` and `kv.put` under `hello:`, and
`notify.publish`. It deliberately does not grant `db.migrate`: the notes
service reaches for it through `POST /api/notes/migrate`, the kernel
refuses, the refusal is a record in the decision chain, and the 403
carries that record's id. That is the demonstration of deny by default,
not an oversight.

`tests/verify.rs` holds the crate to the manifest with
`rahi_kernel::verify!`. Because the demonstration is by design outside the
ceiling, the check is exact rather than empty: the verifier names exactly
one site, `(notes, db.migrate, notes)`, and nothing else. Remove a grant
while its call remains and a second name appears and the build fails.

## The notes resource

Every store call goes through a `Governed` facade whose triple (service,
kind, resource) is literal at the call site, so the build-time verifier and
the runtime adjudicator read the same three tokens.

- `POST /api/notes` inserts the principal's note, stamps the table's
  revision watermark, and stages an outbox envelope, all in one `txn`.
- `GET /api/notes` lists the principal's notes through the leader-agnostic
  read.
- `DELETE /api/notes/{id}` deletes only the caller's; another principal's
  note is not found, which says nothing about whether it exists.
- `POST /api/notes/migrate` is the denial.

## The operator surface

Behind the `hello_operator` role gate (spec 024 B-2):

- `GET /operator/traces`: the in-process trace ring (spec 023).
- `GET /operator/exposure`: the route table as the edge published it.

## Exposure

Every route is classified or the edge refuses to build (spec 024 B-3).
`exposure::report()` for this cell, as `GET /operator/exposure` serves it
and as the end-to-end test asserts it:

```text
class          path
-------------  ------------------------
Public         /*
Public         /.well-known/oauth-protected-resource
Public         /session
Authenticated  /
Operator       /operator
Probe          /healthz
Probe          /metrics
Probe          /readyz
Proxy          /auth
```

The table is by mount. The notes router is the cell's routes at the root,
authenticated; the page is the static slot, public; the session, proxy,
and metadata mounts are the chassis's.

## The verbs

```sh
cargo run -p hello-cell --locked -- --help
```

lists the composer's verbs: `serve`, `preflight`, `migrate`, `backup`,
`restore`, `ledger verify`, `ledger export`, `supervise`, `first-boot`,
with the four exit codes (0 ok, 1 failure, 2 stale, 3 infrastructure).

## Tests

```sh
cargo test -p hello-cell --locked
```

Without a rauthy the end-to-end test runs the unauthenticated subset and
reports the skipped steps by name: the probes, the page, the metrics, the
route classes as the edge answers them (401 ahead of adjudication, the
operator gate closed), and the ledger verbs on the stopped volume.

With `RAHI_TEST_RAUTHY` naming a rauthy binary the whole path runs: boot
with rauthy, log in, create two notes, list, delete one, be denied
`db.migrate` with a decision id, read the trace ring and the exposure
table as an operator, verify the ledger and assert the denial is the last
record, back up, restore into a fresh volume, boot again on the restored
volume, and assert the remaining note and the same ledger head.

Two parts of that path do not touch the real rauthy. The backup's rauthy
part is answered by a stub on rauthy's address, because a live rauthy
refuses the admin API key on its backup routes (spec 030 D-3); and the
boot on the restored volume mounts no identity, because nothing yet hands
the restored rauthy snapshot to rauthy. What the round trip proves is the
app's store, its chain, and its keys.

```sh
RAHI_TEST_RAUTHY=/path/to/rauthy cargo test -p hello-cell --locked
```

With `attest-ledger` on the PATH (or `RAHI_TEST_ATTEST_LEDGER` naming it)
the exported chain is also verified by that independent CLI.

A restored volume must be reopened on the ports it was written under:
hiqlite binds a node to the addresses in its stored membership, so a
volume reopened on other ports never elects. A deployment's ports are
fixed by contract; the test pins them with `BootSpec::with_ports`.

## Run it (the operator check, spec 034 AC-2)

The image spec 031 builds takes the package and binary as build
arguments; the compose file of spec 033 runs it as the N=1 topology with
the volume bind-mounted at `docker/.data`.

1. Build the image for this cell and start it:

   ```sh
   docker compose -f docker/compose.yml build \
     --build-arg RAHI_PACKAGE=hello-cell --build-arg RAHI_BIN=hello-cell
   docker compose -f docker/compose.yml up
   ```

2. Wait for readiness: `curl -s localhost:8080/readyz` answers `200`.
   First boot prints the rauthy admin credentials once:
   `docker compose -f docker/compose.yml logs rahi`.

3. Open `http://localhost:8080/`. The page is served from the static slot
   and shows a login button and an empty list.

4. Create a user in rauthy: open `http://localhost:8080/auth/v1/admin`
   with the admin credentials from step 2, add a user with a password,
   and give it the `hello_operator` role if you want the operator routes.

5. Click **Log in**. The cell sends you through `/session/login` to rauthy
   on the same origin, you authenticate, and the callback sets the
   session cookie and returns you to the page.

6. Add a note. The list shows it with its revision. Delete it.

7. Call the denied route with the session cookie:

   ```sh
   curl -s -X POST -b "session=<cookie>" localhost:8080/api/notes/migrate
   ```

   answers `403` with a `decision` id. As an operator,
   `localhost:8080/operator/traces` shows the request and
   `localhost:8080/operator/exposure` shows the route table above.

8. `docker compose -f docker/compose.yml down` stops the unit. The volume
   stays in `docker/.data`; the next `up` is a restart, not a first boot.
   With the stack down, the chain is verified with the cell's own binary
   on that volume, and the denial is its last record:

   ```sh
   RAHI_PUBLIC_URL=http://localhost:8080 RAHI_DATA_DIR=docker/.data \
     cargo run -p hello-cell --locked -- ledger verify
   RAHI_PUBLIC_URL=http://localhost:8080 RAHI_DATA_DIR=docker/.data \
     cargo run -p hello-cell --locked -- ledger export chain.jsonl
   ```
