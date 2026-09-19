# Changelog

Every change to the consumer contract, under the version that carries it
(spec 039 B-2). The consumer contract is the `Cell` trait, the manifest
schema, the environment surface, the exit codes, the archive format, the
chain record format, and the chassis's HTTP surfaces (`/auth`, `/session`,
`/.well-known`, `/operator`, and the probes).

Every chassis crate carries one version, and a release is an annotated tag
`vX.Y.Z` on a `main` commit whose `make ci` passed. Pre-1.0, a minor bump may
change the consumer contract and a patch bump may not.

## 0.2.0, release candidate (not published)

Prepared from the completed spec 037 merge
`ea6d0da125aaafe0927570410a8a1b0ee9d1286e`, including every change since
`v0.1.0` (`6e06c042df4e9f5b47dc86ee8d40167c2e668b5e`). All nine chassis
crates move together. The minor bump follows spec 039 B-1 because key-set,
backup authentication, recovery, and the export API change compatibility.
The coordinator must record the final tested main revision and release date
when creating the annotated `v0.2.0` tag and forge release. This candidate
is neither registry availability nor evidence of consumer deployment.

### Recovery and authentication (spec 037)

- Fresh key sets now carry `backup_passkey.json`, including an ES256 private
  key bound to the public origin and relying party. `supervise` provisions
  a dedicated passkey-only rauthy backup administrator. Backups authenticate
  with its MFA session; rauthy's admin API key alone cannot take a backup.
  `ADMIN_FORCE_MFA` stays enabled and no persistent backup password is kept.
- `first-boot --export` now requires `RAHI_PUBLIC_URL` so the exported key
  set is bound to the intended origin. The Rust API is now
  `rahi_ops::first_boot::export(&dyn EnvReader)`, replacing `export()`.
  Container export instructions use `--entrypoint /usr/local/bin/rahi`.
- App and rauthy backups each serialize local requests, wait out hiqlite's
  sixty-second suppression window, and accept only a new snapshot at or
  after their own trigger second. Existing and future-dated files cannot
  stand in for a fresh backup. One deadline per store covers queueing,
  suppression, trigger, and polling, plus rauthy's login and download:
  120 seconds by default, with `backup_within` for an explicit budget.
  Accepted work retains serialization until it settles after cancellation
  or timeout. Exhaustion returns `Error::Upstream`, not a stale archive.
  A rapid second backup can take about a minute. The two store snapshots
  remain separate instants, not a coordinated cross-store snapshot.
- Restore now hands the pending rauthy snapshot to rauthy's own hiqlite
  restore on the next supervised start. The marker gains optional
  `rauthy_snapshot_applied`; old markers deserialize as pending. A missing
  or unreadable pending source fails closed. Only health of the child given
  that exact source advances the marker, so a later start does not reapply
  it. Application code never opens rauthy's store.
- Additive library surfaces support passkey custody and admin sessions,
  bounded backups, and the prepared restore handoff. `RauthyApi` callers
  taking backups must supply `with_passkey(keys.backup_passkey()?)`.
  The `Cell` trait, manifest schema `1.0.0`, exit-code mapping, archive
  envelope, and chain record format do not change in this release; archive
  key payloads and restore-marker semantics do.

### Upgrade limits

- A pre-037 key set has no `backup_passkey.json`. Restart does not mint one
  into an existing key set. Supervised serving can continue with a named
  warning, but backup refuses until a usable credential is provisioned.
  Neither this release nor 037 supplies an automatic old-key-set migration.
  An old archive is not made recovery-compatible merely by changing the
  binary version. Do not replace an existing deployment's keys with a fresh
  export or delete a data volume as an upgrade procedure.
- Passkeys are origin-bound. The proven restore uses the original origin
  and ports and carries both the private key and its rauthy registration.
  Changing `RAHI_PUBLIC_URL` does not rebind an existing credential.
  Cross-origin recovery, key rotation, and N=3 restore are not delivered
  here. Manifest and schema evolution is now delivered, by spec 036 below.
  Specs 038, 040, and 041 remain unimplemented.

### Manifest and schema evolution (spec 036)

- A manifest change is a chain record. `rahi migrate --adopt-manifest`
  applies the cell's migrations and then, on the leader, appends a
  `manifest.transition` decision carrying `from`, `to`, the adopted
  manifest's canonical JSON, the store's schema version, the binary, and the
  actor. When the booted manifest already is the chain's current one it
  appends nothing and says so. `docker/entrypoint.sh` and
  `deploy/k8s/migrate-job.yaml` pass the flag, so an ordinary deployment
  adopts its manifest in the step that migrates it.
- The boot check moved. `Kernel::boot` compares the booted manifest against
  the chain's **current** manifest (its genesis parent until the first
  transition, then the `to` of the latest one) and a mismatch is
  `Error::Stale`, exit 2, naming both hashes and the command that clears it.
  Before this release a changed manifest was reported as an integrity
  failure and a deployed cell's manifest was frozen at its first boot.
- Verification is re-anchored, not relaxed. `Ledger::open` reads the genesis
  parent from the chain's own stored records rather than from the booted
  manifest, with the cell's ledger key's signatures as the anchor. A broken
  link, a bad signature, or a fork is still `Error::Integrity`, exit 1, and
  still fatal at boot.
- **Chain record format**: a new decision kind, `manifest.transition`. No
  existing record gained a field, and a chain with no transition verifies
  byte for byte as before.
- **Archive format**: `manifest.json` gains `schema`, the store's migration
  history at backup time, and its `manifest_hash` is now the chain's current
  manifest rather than the booted one. An archive written before this
  release carries no `schema`; `restore` reports that its schema could not
  be checked rather than treating silence as a passed check.
- **`Cell` surface**: `Migration` gains `additive` and the builder
  `Migration::additive()`. A migration that does not declare itself additive
  is not additive. `serve` accepts a store ahead of the binary only when
  every applied version above the binary's last is recorded additive, and
  otherwise exits 2 naming the first that is not. Before this release a
  store ahead was served in silence.
- `schema_version` gains `checksum` and `additive` columns, added to an
  existing store in place. A recorded version whose SQL differs from the
  binary's is `Error::Integrity` naming the version, rather than being
  skipped as applied. A row written before this release carries no checksum;
  the first `migrate` under it records the binary's.
- `restore` gains `--adopt` and refuses, writing nothing, an archive whose
  schema is ahead across a non-additive migration or whose chain names a
  manifest this binary does not. The Rust API is now
  `rahi_ops::restore::run(&config, &archive, &key, &Compatibility)`.
- A transition retains the adopted manifest whole or adoption is refused:
  a record that would exceed the cell's `ledger.max_record_bytes` is
  `Error::Validation` (exit 1) naming the measured size and the bound, with
  no migration applied and no record appended. Nothing is truncated, moved
  to another field, or reduced to hash-only evidence.

### Upgrade limits (spec 036)

- Everything above is enforced by a binary that carries spec 036. A binary
  built before it has none of these checks and cannot be given them from
  outside; an older binary that *does* carry 036 is a different thing from a
  pre-036 one, and only the former honors the additive rule.
- At N=3 one transition is appended per deploy, by the migration Job, before
  the rollout. Replicas already running the old image keep serving and keep
  stamping the old manifest hash on their decisions: this release offers no
  protection against a replica that is already up, and claims none. A
  replica that **restarts** on the old image after the transition refuses to
  boot.
- `preflight` is unchanged and does not report an unadopted manifest; `serve`
  and `migrate --adopt-manifest` are where that is answered.

### Evidence and release tooling since 0.1.0

- The digest-pinned rauthy 0.36.2 live suite refuses missing fixtures with
  `RAHI_REQUIRE_RAUTHY=1`. It proves login, an administered cell audience,
  scoped bearer admission, insufficient-scope refusal, real signed-token
  wrong/missing-audience refusal, restart, and N=1 restore with the original
  user, password, `sub`, note, and ledger head. The production audience
  validator is unchanged. The empty discovery test was removed; image
  smoke tests carry that proof. Live CI remains advisory.
- Prior merged evidence is for `ea6d0da`, not for a published 0.2.0:
  [required CI](https://github.com/statecrafting/rahi/actions/runs/35285071910),
  [amd64/arm64 image builds and smoke tests](https://github.com/statecrafting/rahi/actions/runs/35285071745),
  and the matching implementation's
  [pinned-rauthy suite](https://github.com/statecrafting/rahi/actions/runs/35283741716).
  Tag-based consumer proof, registry consumer acceptance, anonymous image
  pulls, and the published-image walkthrough still require the new release.
- The publisher handles crates.io new-name rate limits with bounded retries.
  The prior 0.1.0 registry proof passed; its recorded GHCR visibility failure
  remains historical evidence, not a claim about new artifacts.
- Governance now pins spec-spine 0.20.0 and checks freshness, staging, and
  coupling at the commit boundary. The consumer status and decision handoff
  documents distinguish implementation, publication, and operational proof.

### Independent limitations, not fixed by this release

- **Lease release:** the locked, published hiqlite 0.14.0 can fail after a
  stale lease is released following TTL takeover. Full-node restart followed
  by a second lease is also unverified. This release changes neither lease
  implementation nor the published dependency pin; no git, vendor, or patch
  substitute is introduced. See the upstream
  [repair](https://github.com/sebadob/hiqlite/pull/352) for context, not proof
  that the locked package contains it.
- **Ledger retry identity:** ID/content classification covers resident
  records. Sealing removes those rows, so a retry after sealing can append a
  duplicate decision ID while chain verification still passes. Lifetime
  idempotence, atomic retry versus sealing, unavailable-history handling,
  and migration/backfill require a separately governed design. An archive
  scan alone cannot establish atomic absence.

Statecraft can evaluate 037's N=1 recovery once 0.2.0 is published. Aicortex
must not adopt this release as a fix for either independent limitation.
Publishing new artifacts changes no consumer dependency or rollout target.

## 0.1.0, 2026-09-16

The first release: the seven responsibilities of spec 002, carried by nine
crates at one version. A tag publishes the crates to crates.io and the
images to ghcr.io (spec 039 D-11), so a consumer pins `= "0.1.0"` rather
than a commit.

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
