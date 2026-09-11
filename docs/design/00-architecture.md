# rahi architecture, as built

The design truth is `specs/002-chassis-thesis/spec.md`; this note does not
restate it. It records what the running system looks like as the specs
land, and the facts a build session learned that the specs do not state.
Update it in the session that changes what it describes.

## The map

| Responsibility | Crate | Landed by | What it holds |
|---|---|---|---|
| types | `rahi-types` | 010 | `Error` and the four exit codes, `Principal`, `Revision`, `FenceToken`, `Config`, schema versions |
| store | `rahi-store` | 011, 012, 016 | hiqlite in-process: `Store::open`, `execute`, `txn`, `query`, `query_consistent`, `migrate`, backup; `Lease` with fencing, `Notify`, `Outbox`, `Watermark`; BLOB values, loadable extensions refused |
| ledger | `rahi-ledger` | 013, 014 | `Ledger`: open verifies the chain, append is a CAS on the parent index; Ed25519-signed records; sealing to segments; an `Archive` on disk or S3 |
| kernel | `rahi-kernel` | 015 | `Manifest` (parse, hash), `Kernel` (adjudication, the bounded denial queue), the `Governed` facades, the `verify!` build check |
| edge | `rahi-edge` | 020, 023, 024, 026 | the `Edge` builder, middleware (CSRF, rate limit, security headers, client identity), probes, the static slot, the exposure table, metrics and the trace ring, server-sent event streams |
| identity | `rahi-idp` | 021, 022, 025 | the raw `/auth` proxy, discovery, the key set, client bootstrap, the session envelope and renewal, the `Principal`, the bearer resource server and scope gate |
| ops | `rahi-ops` | 030, 031 | preflight, migrate, backup, restore, first boot, the key set, rauthy's rendered environment, the die-together supervisor |
| ops | `rahi-cli` | 030 | the `Cell` trait, `run(cell)`, the composition of `serve`, the verbs |
| ops | `rahi-harness` | 033 | a dev-dependency with no workspace dependency: boots a built binary, waits on `/readyz`, a cookie-jar client, a rauthy admin helper |
| reference | `apps/hello-cell` | 034 | the manifest, one migration, the notes resource, an operator route, a page, the end-to-end test |

Dependencies point downward only: `cargo tree -p <crate>` must show no
crate above it. `rahi-harness` depends on nothing in the workspace; it
drives a built binary over HTTP.

## The volume

```
/data
  hiqlite/                      the app's Raft state (rahi-store)
    state_machine/
      db/hiqlite.db             the SQLite group, the decision chain included
      backups/                  backup_node_<id>_<ts>.sqlite
      snapshots/
  rauthy/                       rauthy's directory; the app never opens its store
    rauthy.env                  rendered once by first-boot (spec 031 B-2)
    config.toml                 empty; the environment wins
  keys/                         the deployment's key set (spec 030 D-4)
  backups/                      the default target of `rahi backup`
  ledger-archive/               the default filesystem archive of sealed segments
  restore/rauthy/               where `rahi restore` places rauthy's snapshot
  restore.marker                names the archive a restore applied
```

The app's hiqlite refuses any data directory with a `rauthy` path
component. The two stores are separate Raft clusters on separate ports.

## Ports

| Listener | Default | Owner |
|---|---|---|
| public listener | `0.0.0.0:8443` (`RAHI_LISTEN_ADDR`), reached as `RAHI_PUBLIC_URL` | the app (spec 020, spec 030 D-6) |
| rauthy loopback | `127.0.0.1:8080` | rauthy (spec 021) |
| rauthy hiqlite | `8100` Raft, `8200` API | rauthy, rendered by first boot (spec 031) |
| app hiqlite | `127.0.0.1:8300` API, `127.0.0.1:8400` Raft | `rahi-store` |

## The environment surface

One variable is required, `RAHI_PUBLIC_URL`. An empty value is absent. The
cookie scheme is derived from the public URL's scheme and has no override.
Secrets never travel through `Config`: the store takes them as
`StoreSecrets`, and spec 030 reads them from the keys directory.

| Group | Variables |
|---|---|
| the cell | `RAHI_DATA_DIR`, `RAHI_LISTEN_ADDR`, `RAHI_TRUSTED_PROXY_HOPS`, `RAHI_LEDGER_ARCHIVE_DIR` |
| the app store | `RAHI_HIQLITE_API_ADDR`, `RAHI_HIQLITE_RAFT_ADDR`, `RAHI_STORE_CLIENT` |
| cluster topology (spec 032) | `RAHI_HIQ_NODE_ID`, `RAHI_POD_INDEX`, `RAHI_HIQ_NODES`, `RAHI_RAUTHY_HQL_NODES`, `RAHI_RAUTHY_HQL_LISTEN_ADDR` |
| identity | `RAHI_RAUTHY_MODE` (`required` or `none`), `RAHI_RAUTHY_ADDR`, `RAHI_RAUTHY_BIN`, `RAHI_RAUTHY_HQL_RAFT_PORT`, `RAHI_RAUTHY_HQL_API_PORT`, `RAHI_IDP_REGISTRATION` |
| observability and streams | `RAHI_OTLP_ENDPOINT`, `RAHI_TRACE_RING_CAPACITY`, `RAHI_MAX_CONCURRENT_STREAMS`, `RAHI_STREAM_DRAIN_TIMEOUT_SECS` |
| backup | `RAHI_BACKUP_S3_ENDPOINT`, `RAHI_BACKUP_S3_REGION`, `RAHI_BACKUP_S3_ACCESS_KEY`, `RAHI_BACKUP_S3_SECRET_KEY`, `RAHI_BACKUP_S3_PATH_STYLE` |

## Facts the specs do not state

- **Writes are deterministic bytes.** hiqlite panics on `unixepoch()`,
  `time()`, `random()`, and any other non-deterministic SQL function on
  the write path, because every follower must apply the same statement to
  the same effect. A timestamp or a nonce that reaches the store is a
  caller-supplied parameter. This shapes the ledger (013): record
  timestamps are values in the record, never SQL defaults.
- **A `txn` is one Raft entry and one SQLite transaction.** hiqlite rolls
  the whole batch back when any statement fails, which is the mechanism
  behind constitution IX. The store surfaces the failing statement's error
  and no partial result.
- **Consistent reads are owned rows.** A `query_consistent` comes back
  from the leader as owned values and is mapped through serde; a local
  `query` maps the borrowed rusqlite row. Both reach the same caller type.
- **Migrations never run at boot.** `schema_version` is created by the
  first `migrate` call (version 0), and the `migrate` verb (spec 030) is
  the only caller. `serve` refuses a store behind the cell's migrations
  (exit 2, spec 030 B-2) and accepts one ahead of them, so an older binary
  serves on a newer schema without a word. `schema_version` records a
  version and a name, not the SQL, so a migration whose SQL changed under
  a version already applied is skipped as applied.
- **The manifest hash roots the chain.** `Ledger::open` compares the
  chain's first record with the booted manifest's hash, and the hash
  covers the whole parsed model (spec 015 B-2), so any manifest change on
  an existing volume refuses to boot with `integrity` (exit 1). No
  mechanism re-roots or extends a chain onto a new manifest yet
  (`docs/design/01-consumer-contract.md`, "Manifest evolution").
- **Backups are leader-only and named by hiqlite.** The file is
  `backup_node_<id>_<ts>.sqlite` under `state_machine/backups`; with an S3
  target it is encrypted with the active key and pushed. Restore is not a
  store operation: it is the `restore` verb on a stopped volume, which
  resets the app node at the file level (spec 030 D-2) and places rauthy's
  snapshot under `restore/rauthy/`. Nothing hands that snapshot to rauthy
  on the next start yet, so a restore recovers the app's store, its chain,
  and its keys, and not rauthy's users.
- **A restored volume keeps its ports.** hiqlite binds a node to the
  addresses in its stored membership, so a volume reopened on other
  addresses never elects (spec 034 D-5).
- **rauthy keeps only roles that exist.** Granting a user a role rauthy
  has not created succeeds and drops the role (spec 034 D-8).
