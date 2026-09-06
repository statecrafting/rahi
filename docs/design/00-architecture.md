# rahi architecture, as built

The design truth is `specs/002-chassis-thesis/spec.md`; this note does not
restate it. It records what the running system looks like as the specs
land, and the facts a build session learned that the specs do not state.
Update it in the session that changes what it describes.

## The map

| Responsibility | Crate | Landed by | What it holds |
|---|---|---|---|
| types | `rahi-types` | 010 | `Error` and the four exit codes, `Principal`, `Revision`, `FenceToken`, `Config`, schema versions |
| store | `rahi-store` | 011 | hiqlite in-process: `Store::open`, `execute`, `txn`, `query`, `query_consistent`, `migrate`, backup |

Every crate below the store is still specification only. Dependencies
point downward only: `cargo tree -p <crate>` must show no crate above it.

## The volume

```
/data
  hiqlite/                      the app's Raft state (rahi-store)
    state_machine/
      backups/                  backup_node_<id>_<ts>.sqlite
      snapshots/
  rauthy/                       rauthy's Raft state; the app never opens it
  keys/                         the deployment's key set (spec 030)
```

The app's hiqlite refuses any data directory with a `rauthy` path
component. The two stores are separate Raft clusters on separate ports.

## Ports

| Listener | Default | Owner |
|---|---|---|
| public origin | from `RAHI_PUBLIC_URL` | the app (spec 020) |
| rauthy loopback | `127.0.0.1:8080` | rauthy (spec 021) |
| rauthy hiqlite | `8100` API, `8200` Raft | rauthy, never configured by the app |
| app hiqlite | `127.0.0.1:8300` API, `127.0.0.1:8400` Raft | `rahi-store` |

## The environment surface

One variable is required, `RAHI_PUBLIC_URL`. The overrides are
`RAHI_DATA_DIR`, `RAHI_HIQLITE_API_ADDR`, `RAHI_HIQLITE_RAFT_ADDR`,
`RAHI_RAUTHY_ADDR`, `RAHI_TRUSTED_PROXY_HOPS`, and `RAHI_OTLP_ENDPOINT`.
An empty value is absent. The cookie scheme is derived from the public
URL's scheme and has no override. Secrets never travel through `Config`:
the store takes them as `StoreSecrets`, and spec 030 reads them from the
keys directory.

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
  the only caller. A cell can serve on an old schema; it cannot migrate
  itself into a surprise.
- **Backups are leader-only and named by hiqlite.** The file is
  `backup_node_<id>_<ts>.sqlite` under `state_machine/backups`; with an S3
  target it is encrypted with the active key and pushed. Restore is not a
  store operation; it is a cluster reset performed at boot by spec 030.
