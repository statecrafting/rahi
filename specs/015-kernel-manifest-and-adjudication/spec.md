---
id: "015-kernel-manifest-and-adjudication"
title: "The kernel: a declared capability ceiling, verified at build, enforced at runtime, denials ledgered"
status: approved
kind: "kernel"
domain: "kernel"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: complete
risk: critical
wave: 1
depends_on:
  - "013-ledger-decision-chain"
establishes:
  - "crates/rahi-kernel/Cargo.toml"
  - "crates/rahi-kernel/src/lib.rs"
  - "crates/rahi-kernel/src/manifest.rs"
  - "crates/rahi-kernel/src/capability.rs"
  - "crates/rahi-kernel/src/verify.rs"
  - "crates/rahi-kernel/src/adjudicate.rs"
  - "crates/rahi-kernel/src/facade.rs"
  - "crates/rahi-kernel/src/observe.rs"
  - "crates/rahi-kernel/tests/manifest.rs"
  - "crates/rahi-kernel/tests/adjudicate.rs"
  - "crates/rahi-kernel/testdata/manifests/"
extends:
  - { spec: "010-workspace-and-core-types", unit: { kind: section, file: "Cargo.toml", anchor: "workspace.dependencies" }, nature: additive }
constrains:
  - { flavor: invariant-freeze, unit: "crates/rahi-kernel/src/adjudicate.rs", note: "deny by default; constitution X" }
summary: >
  The governance seam. An app declares a manifest (TOML, embedded at compile
  time): identity, services, resources by name, a capability catalog, per
  service grants, the gate roster, ledger and observability policy. The
  kernel canonicalizes and hashes it, verifies at build that every governed
  facade the app links is covered by a grant, adjudicates every store,
  lock, notify, secret, and egress operation at runtime through the
  action-gate primitive, denies anything not granted, and appends a
  Decision for every denial without the request path awaiting the ledger.
  Carries enrahitu://020 and enrahitu://021 with the parser-based extractor
  replaced by a declared manifest.
---

# 015: The kernel

## 1. Purpose

Encore's lasting idea was a static application model separated from runtime
configuration. enrahitu extracted that model from TypeScript with a
parser; rahi declares it, because a declared ceiling that the build
verifies against observed usage is the property that mattered
(declare-verify-enforce, enrahitu://020 §3.1), and a parser is a second
implementation of the compiler. The manifest's hash is the genesis parent
of the decision chain (spec 013 B-2), so the ledger commits to what the
cell was permitted to do.

## 2. Territory

The whole of `crates/rahi-kernel`. It exposes `Manifest`, `Kernel`, the
`Governed<T>` facade wrappers that every store and idp operation passes
through, and the `observe` hook that the tracer (023) subscribes to.

## 3. Behavior

- **B-1 (manifest).** `Manifest` is parsed from TOML with members `app
  {name, org}`, `resources {tables[], kv[], counters[], secrets[] (names
  only), egress[] (host patterns)}`, `capabilities [{id, kind, resource,
  constraints?}]`, `services {<name> = {capabilities[]}}`, `gate {checks[]}`,
  `ledger {schema_version, max_record_bytes}`, `observability
  {metrics_path, otel}`, `auth {operator_role}`, and `contract {version}`.
  Kinds are the closed vocabulary `db.read`, `db.write`, `db.txn`,
  `db.migrate`, `kv.get`, `kv.put`, `kv.delete`, `counter.get`,
  `counter.add`, `lock.acquire`, `notify.publish`, `notify.listen`,
  `secret.read`, `http.egress`. Unknown keys are `Error::Validation`.
- **B-2 (hash).** `Manifest::hash()` is sha256 over canonical-keysort-json
  bytes of the parsed model plus the gate's `config_hash()`. Never over the
  TOML text.
- **B-3 (verify at build).** `rahi_kernel::verify!(manifest)` is a build
  step (a `build.rs` helper and a test) that walks the app crate's use of
  the governed facades (each facade call site names its service and kind
  via a `#[governed(service = "...", kind = "...")]` attribute or an
  explicit `Governed::new(...)`) and asserts every observed
  `(service, kind, resource)` is covered by a declared grant. Uncovered
  usage fails the build with the missing grant named. Absence is never
  permission.
- **B-4 (adjudicate).** `Kernel::adjudicate(ctx: ActionContext) ->
  Decision` is pure over the manifest and the `action-gate` check roster;
  `Allow`, `Deny(reason)`, or `Degrade(reason)`. Constraints (`key_prefix`,
  `table`, `host`) are matched here.
- **B-5 (facades).** `Governed<Store>`, `Governed<Lease>`, `Governed<Notify>`,
  `Governed<Secrets>`, `Governed<Egress>` wrap the underlying operation:
  adjudicate, then perform on `Allow`, else return `Error::Denied` and
  emit a Decision. There is no path to the store from application code
  except through a facade; the raw `Store` is not re-exported by the kernel.
- **B-6 (denials ledgered).** Every `Deny` builds a Decision with the
  actor's `Sub` (or `system`), capability, reason, and a caller-supplied
  wall time in the payload, and hands it to a bounded channel drained by
  one appender task into spec 013's `Ledger::append`. The request path
  never awaits the append; an `Error::Integrity` from the appender is
  logged with the decision id and raises a `kernel_ledger_failures` metric.
  The typed `Error::Denied` carries the decision id.
- **B-7 (observe).** `observe::on_decision(f)` registers a synchronous
  observer invoked with each Decision before it is queued; the tracer
  (023) uses it and the kernel imports nothing from the edge.
- **B-8 (genesis).** `Kernel::boot(manifest, store, ledger)` passes
  `manifest.hash()` as the genesis parent; a ledger whose genesis parent
  differs from the booted manifest's hash is `Error::Integrity` (the
  manifest changed without a deploy genesis record).

## 4. Functional requirements

- **FR-001.** Fixture manifests under `testdata/manifests/`: a valid one, one
  with an unknown kind, one with a service referencing an undeclared
  capability, one with an uppercase secret name; each invalid one fails
  with a named error.
- **FR-002.** `Manifest::hash()` is byte-identical across key order and
  whitespace changes in the TOML and changes when any grant changes.
- **FR-003.** Adjudication tests: an ungranted kind is denied; a grant with
  `key_prefix = "demo:"` allows `demo:x` and denies `rl:x`; a denial
  produces exactly one Decision with the request's actor.
- **FR-004.** A test app with an uncovered facade call fails `verify!`
  naming `(service, kind, resource)`.
- **FR-005.** The appender drains under a failing ledger without blocking
  the request path (a timed test).

## 5. Acceptance criteria

- **AC-1.** `cargo test -p rahi-kernel --locked` passes.
- **AC-2.** `cargo tree -p rahi-kernel` shows `rahi-ledger`, `rahi-store`,
  and `rahi-types` as its only workspace dependencies.

## 6. Out of scope

HTTP-level authorization (roles on routes) is the edge's (020) using the
`Principal`; the operator role gate (024); trust windows and privilege
degradation (a later spec if a product needs them).

## 7. Resolved decisions

- **D-1 (2026-09-06, build session; refines B-2).** `Manifest::hash()` is
  fallible and builds its own gate, so the hash stays pure over the
  document: `hash(&self) -> Result<Hash, Error>` serialises the parsed
  model, canonicalises it, assembles the gate from the manifest itself
  (`Manifest::gate()`), and hashes the model bytes followed by the gate's
  `config_hash()`. No `Gate` argument is taken, so a caller cannot root a
  chain at a gate the manifest does not describe. B-2 names
  `config_hash()` as an input and B-8 needs the hash *before*
  `Ledger::open`, which runs before `Kernel::boot`; both hold only if the
  gate is derivable from the manifest alone. The `Result` is what keeps
  the crate inside the workspace's `unwrap_used` and `expect_used`
  denials. Rejected alternative: an infallible `hash()` with an internal
  `expect`, which the workspace clippy configuration denies.
- **D-2 (2026-09-06, build session; names B-4's types).**
  `Kernel::adjudicate(&ActionContext)` returns
  `action_gate_types::Decision`, re-exported as `Verdict`. B-4 writes the
  return type as `Decision` with `Allow` / `Deny(reason)` /
  `Degrade(reason)`, but `Decision` in this chassis already means the
  ledgered record (spec 013) and this crate depends on `rahi-ledger`;
  aliasing the gate's own type keeps one name per concept and makes the
  value a check returns the value the kernel returns, `check_ids` and
  `blocking` included. `Request` is the typed form of an `ActionContext`,
  lowered by `Request::to_context()`, because an `ActionContext` is a
  string map and a typo in an attribute key would read as a permit.
  Rejected alternative: a third `Allow`/`Deny`/`Degrade` enum in this
  crate, which would collide with `rahi_ledger::Outcome` at every import.
- **D-3 (2026-09-06, build session; completes B-1's resource families).**
  A capability's resource must appear in the `resources` list its kind
  draws from (`db.*` in `tables`, `kv.*` in `kv`, `counter.*` in
  `counters`, `secret.read` in `secrets`, `http.egress` in `egress`).
  `lock.acquire`, `notify.publish`, and `notify.listen` draw from no list
  and name their resource directly, validated for shape only: B-1 declares
  no lock or topic family, and a lock key or a topic is an ephemeral
  coordination name rather than a durable resource an operator would
  inventory. Narrowing them is what `key_prefix` is for. The cost is that
  a typo in a lock key is caught by the grant that names it and not by the
  resource list. Rejected alternative: adding `locks[]` and `topics[]`,
  which would widen a schema B-1 fixes.
- **D-4 (2026-09-06, build session; completes B-1's `gate.checks`).** The
  roster is a closed catalog, seeded here with one entry: `secrets`
  (`action-gate`'s own `SecretsCheck`). The capability check, id `grants`,
  is always the first check in every gate and a roster that names it is
  `Error::Validation`; an unknown id is refused when the manifest is
  parsed rather than skipped when the gate is built. Deny by default
  cannot be a roster entry, because a manifest that omitted it would
  assemble a gate that admits everything. An open roster resolved against
  app-registered checks would also leave `Manifest::hash()` undefined
  until registration, which contradicts D-1 and B-8's ordering. Adding a
  check is a spec amendment. Rejected alternative: an app-supplied check
  registry, which makes the manifest hash depend on runtime state.
- **D-5 (2026-09-06, build session; extends B-6 to `Degrade`).** `Deny`
  and `Degrade` are both ledgered and both return `Error::Denied` carrying
  the decision id; `Allow` is not ledgered. The record's outcome is the
  gate's, so a degrade reads as `degrade` in the chain. B-6 names `Deny`
  because that is the case constitution X is about, but a degrade is
  equally a governance event, and `action-gate` is explicit that `Degrade`
  is a signal the consumer interprets: the chassis has no reduced form of
  a store write to perform, so the only honest interpretation at a facade
  is refusal with the reason recorded. Ledgering allows would turn the
  chain into the request log, which spec 014's hot window is not sized
  for. Rejected alternative: treating `Degrade` as `Allow`, which performs
  in full the operation the gate asked to reduce.
- **D-6 (2026-09-06, build session; resolves B-3's two forms).** One
  facade is one capability:
  `Governed::new(kernel, service, kind, resource, inner)` binds the
  triple, and each operation method refuses a facade declared for another
  kind. `verify!` reads those three literals from the call site, so the
  declaration the build checks and the triple the runtime adjudicates are
  the same three tokens and cannot drift. A site that builds its triple at
  runtime carries a `#[governed(service = .., kind = .., resource = ..)]`
  marker written as a comment above the call and read from the source
  text; Rust rejects an unknown attribute without a proc-macro crate, and
  a companion macro crate would appear in `cargo tree` and break AC-2. A
  site with neither is `Error::Validation`, not an unverified call.
  Rejected alternative: a facade carrying only the service, with the kind
  implied by the method, which leaves no literal for the walk to read.
- **D-7 (2026-09-06, build session; widens B-6's failure handling).**
  Every failed `Ledger::append` raises `kernel_ledger_failures`, not only
  `Error::Integrity`: a store failure that loses a denial is equally a
  governance failure and equally must not be silent. The denial queue is
  bounded (`DEFAULT_QUEUE_CAPACITY` 1024) and `emit` uses `try_send`, so a
  full queue drops the record and raises `kernel_decisions_dropped`; B-6
  forbids the request path from awaiting the append, and under a stalled
  leader the alternative to dropping is a stalled cell. Both paths reach
  `observe::on_failure`, and with no observer registered the kernel writes
  the failure to stderr, because the chassis has no logging facility until
  spec 023 and this is the one failure that must never be silent. Rejected
  alternatives: blocking on a full queue; an unbounded queue, which turns
  a stalled appender into unbounded memory growth.
- **D-8 (2026-09-06, build session; completes B-6's decision id).** The
  kernel reads no clock. A decision id is
  `kernel:<last 16 hex of the chain head at boot>:<12-digit counter>`, and
  wall time is optional and supplied by the app as `KernelOptions::clock`,
  a closure the kernel calls when building a payload; with no clock the
  payload has no `wall_time` key. B-6 says the wall time is
  caller-supplied and spec 013 D-3 keeps the record's timestamp slot on
  the store revision, so a clock of the kernel's own would contradict
  both. The chain head is the one per-boot value already in hand, needs no
  randomness, and makes an id reproducible from the chain; two boots can
  mint the same id only if the earlier one appended nothing, in which case
  the id it minted never landed. Rejected alternatives: a uuid or random
  nonce, which adds a dependency and makes ids unreproducible;
  `SystemTime::now()`, in a crate whose decisions are meant to be
  re-derivable.

## Verification

```verify:cli
cargo test -p rahi-kernel --locked
```
