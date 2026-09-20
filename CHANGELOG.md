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

Every change since `v0.1.0` (`6e06c042df4e9f5b47dc86ee8d40167c2e668b5e`).
All nine chassis crates move together. The minor bump follows spec 039 B-1
because key-set, backup authentication, recovery, and the export API change
compatibility. The coordinator must record the final tested main revision
and release date when creating the annotated `v0.2.0` tag and forge
release. This candidate is neither registry availability nor evidence of
consumer deployment.

**Provenance, reconciled 2026-09-20.** This section was first prepared on
2026-09-17 from the spec 037 merge
`ea6d0da125aaafe0927570410a8a1b0ee9d1286e` and described a tree that no
longer matches `main`. The candidate now carries, in the order they
landed: **037** identity recovery and the live proof; **036** manifest and
schema evolution, with the four fail-open repairs of PR #62 and the
one-snapshot manifest read of PR #64; **042** lifetime identity for
decisions, with the coverage, recovery, single-snapshot and
resident-verification corrections of PRs #66, #67 and #68; and the two
corrections of 2026-09-20, **026**'s stream gauge pairing and **033**'s
boot-budget measurement. The 2026-09-17 preparation text is kept where it
records what was decided then; where it described the release's contents,
it is corrected here.

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
  release carries no `schema`, and its compatibility is established from the
  archived database itself: `restore` reads `schema_version` out of the app
  snapshot (which is the destination's database byte for byte) and judges it
  exactly as it judges a recorded history. There is no restore that was not
  judged. An archive whose payload yields no such evidence is refused with
  `Error::Stale`, exit 2, and the destination is untouched. `--adopt`
  authorizes a manifest difference only; it never bypasses the schema check
  or an integrity check.
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
- Legacy archives are supported exactly this far: an archived database with
  no `schema_version` table restores on the baseline it proves, and one
  whose `schema_version` table predates this release's `checksum` and
  `additive` columns is refused with `Error::Stale`, exit 2, nothing
  written. A pre-036 archive taken from a store that had applied a migration
  is therefore refused rather than restored.
- `restore` gains `--adopt` and refuses, writing nothing, an archive whose
  schema is ahead across a non-additive migration or whose chain names a
  manifest this binary does not. The Rust API is now
  `rahi_ops::restore::run(&config, &archive, &key, &Compatibility)`;
  `restore::Outcome::Restored` carries a `restore::SchemaEvidence` naming
  what the restore was judged on, `restore::check_compatible` takes the
  archive's parts and returns that evidence, and `restore::schema_checked`
  is gone because a restore that was not checked no longer happens.
- `serve` reports an applied migration whose SQL differs from this binary's
  as `Error::Integrity`, exit 1, naming that version, even when the store is
  also behind the binary. Earlier in this release cycle such a store was
  reported only as stale, and running the named command would have applied
  the pending migration on top of a history the binary cannot vouch for.
- `Ledger::current_manifest` refuses rather than answering an older manifest
  from a read that did not answer. The sealed headers and the resident
  records are read in one statement, each carrying its own row count from
  that snapshot, and neither falls back past what `Ledger::open` verified,
  so a replica cannot boot on a superseded ceiling because a read came back
  empty or because a seal committed while the chain was being read. The
  resident chain is ordered against the root the same snapshot names, so
  evidence taken from two moments is `Error::Integrity` rather than an
  answer. `Ledger::current_manifest_model` is the supported way to recover
  the previous ceiling's text; `migrate --adopt-manifest` reports the grant
  diff as unavailable only when the chain genuinely holds no earlier
  manifest text, never because a read failed.
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

### Lifetime identity for decisions (spec 042)

- Every decision now carries a narrow resident identity row that **survives
  sealing**: `(id, identity_digest, record_hash, segment_hash)` in
  `kernel_decision_identity`, written in the same `txn` as the record. The
  identity table's primary key is the arbitration for a duplicate id, the way
  the unique parent index is the arbitration for a lost compare-and-swap.
  Nothing decides a duplicate by reading first.
- The defect this closes: classification used to ask whether the id was
  *resident*, and sealing deletes the row, so past the hot window an appender
  that lost its acknowledgement and retried wrote a **second record under one
  id** while full verification still passed. `Ledger::append` is now lifetime
  idempotent on a covered chain, and a retry of a sealed decision returns the
  original record hash.
- `Ledger::lookup(id)` answers `Resident`, `Sealed`, `Absent`, `Unproven` and
  `Ambiguous` as five different answers. An archive that is missing, corrupt
  or unreadable is `NotFound`, `Integrity` or `Io`, and never `Absent`:
  absence is proven from complete coverage or it is not claimed.
  `Ledger::recover(id, archive)` fetches exactly the one segment body the row
  names and verifies it at full depth before taking a record out of it.
- `Ledger::append_once` returns `Landing { hash, appended_now, sealed_in }`.
  `hash` is durable presence and holds across lost acknowledgements, retries,
  restarts and replicas. `appended_now` is knowledge about the invocation and
  is never `true` for a commit this process did not observe. This is
  exactly-once **append**, never exactly-once **delivery**: a caller that
  needs a side effect exactly once still records its intent in its own
  transaction.
- A negative coverage observation degrades this node's cached verdict where
  it is computed: `Ledger::coverage()` reporting an unstamped resident record
  or an uncovered segment moves the verdict to incomplete, so `lookup` stops
  answering `Absent` and `append` stops admitting an id the chain can no
  longer prove free, on that handle and on every clone sharing it. An append
  already in flight re-consults the backstop before each retry. Only
  `Ledger::recheck_coverage()` restores confidence, and `serve` never calls
  it.
- `Ledger::recover(id, archive)` spans a legitimate seal, wherever the seal
  lands. A record archived between the identity-row read and the
  resident-chain read is recovered from the archive under its original hash
  rather than reported as an integrity failure, by revalidating the same row
  exactly once. A seal landing *inside* the resident-chain read is settled
  differently and needs no revalidation: `Ledger::records()` now takes the
  resident records, their completeness census, and the segments its root is
  decided from in one statement, so the answer comes from one snapshot and a
  seal can only land wholly before or wholly after it. `Ledger::resident_root()`
  takes its unclaimed segments and the archive's size in the same way, and a
  read of either that carries no witness row is refused rather than read as
  an empty archive. Every reader of the resident chain gains this, so
  `ledger verify` and `ledger export` no longer fail on an intact chain that
  a concurrent seal crossed. A record no seal archived and the resident chain
  does not hold is still `Error::Integrity`, no integrity error is retried,
  and no archive failure is converted into an absence.
- `Ledger::recover(id, archive)` now **verifies the resident evidence it
  returns**, which it previously did only for the archived half. A resident
  record whose signature was forged, whose payload was tampered with, or
  whose envelope disagrees with its payload was returned as a recovered
  decision, because the resident path matched the identity row's hash against
  the hash the record *stores* and checked nothing else. Content hashes are
  now recomputed, every signature is checked against the key the cell holds,
  and every payload is checked against its envelope, over the same snapshot
  the returned record comes out of. Ordinary resident recovery still fetches
  no archived body.
- Verification takes its **whole evidence from one snapshot**. The segment
  headers, the hash the resident chain is rooted at, and the resident records
  were three separate reads, and a real seal between the last two made an
  intact chain report an integrity failure. `Ledger::verify_chain` at either
  depth, `Ledger::records`, `Ledger::export_jsonl`, the manifest read and
  resident recovery now all read one statement, with a completeness census
  for each relation from that same snapshot. Genuine damage is unchanged: a
  forged signature and a tampered payload are refused at both depths, a
  corrupt archived body at full depth, and a missing record, a broken link, a
  fork, an ambiguous id and the three archive failures answer exactly what
  they answered before.
- A chain that already spent an id more than once is recorded in
  `kernel_decision_collisions` and never resolved: lookup of such an id
  answers `Ambiguous` with every copy, appends under it are refused, no copy
  is selected, no archived byte is written, and no row of either table is
  ever deleted. Coverage counts records, so such a chain is still fully
  accounted for and `serve` starts on it with the containment per affected id.

### Upgrade limits (spec 042)

- **This is a stop-the-world upgrade.** A chain sealed before this release
  has no identity rows for its archived ids, so `Ledger::open` refuses with
  `Error::Stale` (exit 2) naming `rahi ledger reindex <archive>`, and `serve`
  and every verb that appends refuse with it. Stop every replica, run the
  reindex against the archive while nothing appends, then start the cluster.
- The cutover is **operationally guaranteed, not enforced**. Baseline DDL
  writes no `schema_version` row, so no store version check fences this
  release in either direction, and spec 036 fences nothing here. A new binary
  *detects* an old writer after the fact (a seal that had to create an
  identity row, a resident record met without one), which makes the damage
  visible rather than undone. No mixed-version guarantee is offered.
- There is **no override**: no `--allow-uncovered`, no other flag, argument or
  environment variable starts normal service on incomplete historical
  coverage. `ledger verify`, `ledger export` and `ledger reindex` keep working
  on an uncovered chain and each prints the uncovered count; `preflight`
  reports coverage as a named check and fails on it. A chain whose archive
  has permanently lost a body can never be proven covered and therefore never
  serves again under this release; recovering from that is a separate,
  reserved decision.
- **New permanent resident cost**: one row per decision, forever, on every
  replica and in every snapshot, backup, restore and cluster join. Estimated
  at about 400 bytes per decision; the acceptance ceiling is 800. No supported
  lifetime-decision limit is declared. Size against **every** append to the
  chain, kernel denials included, not only an application's own acts.
- New verb: `rahi ledger reindex <archive>`, the only mutating verb under
  `ledger`. `rahi ledger verify` additionally prints the uncovered segment
  count, the unstamped resident count, the identity row count and the
  collision count at both depths, and exits 1 when a collision is recorded.
- Rolling back to a binary without this release is a one-way door in the
  other direction: it appends unstamped records, coverage breaks again, and
  returning requires another stop and another reindex.

### Streaming metrics (spec 026)

- `rahi_streams_open` is paired with the stream's attachment rather than
  with the process-global observability context. A stream that opened
  before `rahi_edge::obs::init` installed that context and closed after it
  decremented a gauge it had never incremented, so the gauge could go
  negative and `rahi_streams_closed_total` could count a close whose open
  was never counted. A stream is now counted open and counted closed, or
  neither. No metric name, label, or type changed, and the composer, which
  installs observability before the listener accepts, was never exposed to
  the ordering; a library consumer that serves an `Edge` before calling
  `obs::init` was.

### Test and packaging corrections (specs 033, 039)

- The harness's boot-budget test measured a compilation against a boot
  deadline: it opened its measured window before `binary()`, which shells
  out to `cargo build -p rahi-cli`. The budget, the refused configuration
  and the stderr assertion are unchanged; the window now opens after the
  binary is built (033 D-6). No chassis behaviour changes, and no consumer
  surface is affected.
- A tag now publishes `ghcr.io/statecrafting/rahi-hello-cell:X.Y.Z`
  alongside the cell and runtime images, both architectures, never
  `latest`, and smokes the pushed digest rather than a second local build
  (039 D-14). The hello-cell **crate** stays `publish = false`; this is the
  reference app's image, which 039 AC-3 runs spec 034's AC-2 procedure
  against and which no release had ever published.

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

- **Lease release:** published hiqlite 0.14.0 can fail after a stale lease
  is released following TTL takeover, and this release does not repair it
  for a consumer. Corrected 2026-09-20: the sentence that stood here said
  "no git, vendor, or patch substitute is introduced", which was true when
  written on 2026-09-17 and was falsified the same day. rahi's workspace
  root now carries a temporary, scoped `[patch.crates-io]` pinning
  `hiqlite` and `hiqlite-wal` to the merge commit of upstream PR #352
  (spec 011 D-12), and `Cargo.lock` resolves them from that git source.
  **A consumer does not inherit it.** `[patch]` is root-workspace metadata,
  carried neither by `cargo publish` nor along a dependency edge, so every
  published crate still declares `hiqlite = "0.14"` and a registry or git
  consumer resolves 0.14.0 without the fix; a consumer that wants it
  declares the same patch in its own workspace root. Rahi's own green test
  runs are therefore evidence about rahi's builds and about nothing a
  downstream consumer executes. Re-checked 2026-09-20: the newest published
  hiqlite is still 0.14.0 of 2026-07-06 and the release request
  [#366](https://github.com/sebadob/hiqlite/issues/366) is still open, so
  011 D-12's removal condition is unmet and the exception stands. See the
  upstream [repair](https://github.com/sebadob/hiqlite/pull/352) for
  context, not as proof that any published package contains it.
- **Full-node restart and a second lease:** stopping a whole node while a
  lease is held, restarting it, and acquiring on that key again within a
  bounded time is **unverified**. No approved spec carries that acceptance.
  It is a different property from the restart 037 proves, which is a reboot
  on a restored volume with the chain verified, and passing that does not
  establish this.
- **Ledger retry identity: closed by spec 042, and carried by this
  release.** Corrected 2026-09-20. This entry previously said lifetime
  idempotence "require[s] a separately governed design", contradicting the
  042 section above, and it predated 042 landing on `main`. The governed
  design exists and shipped: every decision carries a resident identity row
  that survives sealing, the identity table's primary key arbitrates a
  duplicate id inside the append transaction, `lookup` answers presence,
  proven absence, unproven history and proven ambiguity as different
  answers, and `rahi ledger reindex` rebuilds the accounting of a chain
  sealed before it. The limitation as written remains true of **0.1.0 and
  of any artifact built before 042 landed**, and the upgrade is not free:
  see "Upgrade limits (spec 042)" above for the stop-the-world reindex, the
  absence of a mixed-version guarantee, and the permanent per-decision
  resident cost.

Statecraft can evaluate 037's N=1 recovery once 0.2.0 is published, at N=1
only: this release establishes nothing about Kubernetes N=3, which has never
run. Aicortex gains the ledger identity repair from this release and does
**not** gain the hiqlite lease repair or full-node second-lease evidence
from it. Publishing new artifacts changes no consumer dependency or rollout
target.

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
