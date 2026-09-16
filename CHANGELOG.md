# Changelog

Every change to the consumer contract, under the version that carries it
(spec 039 B-2). The consumer contract is the `Cell` trait, the manifest
schema, the environment surface, the exit codes, the archive format, the
chain record format, and the chassis's HTTP surfaces (`/auth`, `/session`,
`/.well-known`, `/operator`, and the probes).

Every chassis crate carries one version, and a release is an annotated tag
`vX.Y.Z` on a `main` commit whose `make ci` passed. Pre-1.0, a minor bump may
change the consumer contract and a patch bump may not.

## 0.1.0, unreleased

No tag exists yet, and nothing is published. The entries below are what the
first release will carry; a consumer pinning a git commit today already has
them.

### The contract

- **`Cell`** declares a manifest, migrations, routes, optional operator
  routes, an exposure table, and an optional static directory. `rahi_cli::run`
  turns it into a binary with `serve`, `preflight`, `migrate`, `backup`,
  `restore`, `first-boot`, `supervise`, and `ledger verify | export`.
- **Exit codes**: `0` ok, `1` failure, `2` stale, `3` infrastructure.
- **The manifest names its schema.** `schema_version = "1.0.0"` is required
  at the top of the document, and a manifest naming another major is refused
  (spec 039 B-8). The value is part of what the decision chain is rooted at,
  so a volume whose chain predates this key must be recreated until spec 036
  lands (039 D-4).
- **Decision ids** are `kernel:<nonce>:<node>:<counter>`: the chain head at
  boot, the replica's hiqlite node id, and a counter, so two replicas of one
  chain never mint one id (spec 035 B-6).
- **The environment** gained `RAHI_STATIC_DIR` (the directory the static slot
  serves, spec 039 B-5) and `RAHI_DENIAL_DRAIN_TIMEOUT_SECS` (how long a stop
  drains queued denials, spec 035 B-1).
- **`/metrics`** gained `kernel_decisions_dropped_total` and
  `kernel_decisions_abandoned_total`; `kernel_ledger_failures_total` now
  counts failed appends only (spec 035 B-3).

### Packaging

- All nine chassis crates package: `rahi-types`, `rahi-store`, `rahi-ledger`,
  `rahi-kernel`, `rahi-idp`, `rahi-edge`, `rahi-ops`, `rahi-cli`, and
  `rahi-harness`. The rauthy environment template moved inside `rahi-ops` to
  make that true (spec 039 B-6).
- A tag publishes `ghcr.io/statecrafting/rahi:X.Y.Z` and
  `ghcr.io/statecrafting/rahi-runtime:X.Y.Z`, both architectures, never
  `latest` (spec 039 B-3, D-3).
- An out-of-tree cell builds on the runtime image: its binary at
  `/usr/local/bin/rahi`, its page at `/usr/local/share/rahi/static`
  (spec 039 B-4).
