---
id: "015-kernel-manifest-and-adjudication"
title: "The kernel: a declared capability ceiling, verified at build, enforced at runtime, denials ledgered"
status: approved
kind: "kernel"
domain: "kernel"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: pending
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

None yet.

## Verification

```verify:cli
cargo test -p rahi-kernel --locked
```
