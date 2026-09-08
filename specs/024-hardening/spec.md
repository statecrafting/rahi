---
id: "024-hardening"
title: "Hardening: trusted client identity, the operator role gate, exposure review of every route"
status: approved
kind: "feature"
domain: "edge"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: in-progress
risk: high
wave: 2
depends_on:
  - "022-session-and-principal"
  - "023-observability"
establishes:
  - "crates/rahi-edge/src/client_identity.rs"
  - "crates/rahi-edge/src/operator.rs"
  - "crates/rahi-edge/src/exposure.rs"
  - "crates/rahi-edge/tests/hardening.rs"
extends:
  - { spec: "020-edge-server", unit: "crates/rahi-edge/src/lib.rs", nature: additive }
  - { spec: "020-edge-server", unit: "crates/rahi-edge/src/middleware/rate_limit.rs", nature: additive }
summary: >
  X-Forwarded-For is evidence, not identity: it becomes identity only when
  the operator states how many hops in front of the cell are theirs. This
  spec adds the client identity resolver the rate limiter keys on, the
  operator role gate that internal surfaces (the trace ring, ledger
  inspection, backup triggers) sit behind with no rate limiter on top, and
  an exposure table that every mounted route must appear in with its
  authentication class, asserted by a test so an unreviewed public route
  cannot land. Carries enrahitu://025.
---

# 024: Hardening

## 1. Purpose

enrahitu's exposure review found an unauthenticated data surface on a
container bound to all interfaces and a rate limiter that would have
silently failed open behind a capability scope (enrahitu://025). The
lessons are structural, so they are made structural: identity is declared,
operator surfaces are gated by role and not limited, and every route
declares its class.

## 2. Territory

Three modules in `crates/rahi-edge` and one test. Extends 020's `lib.rs`
and rate limiter (which now keys on the resolver).

## 3. Behavior

- **B-1 (client identity).** `ClientIdentity::resolve(headers, peer,
  trusted_proxy_hops) -> IpAddr`: with hops `0` the peer address; with
  hops `n`, the `n`-th address from the right of `X-Forwarded-For`, falling
  back to the peer when the header is shorter. Never the leftmost entry.
- **B-2 (operator gate).** `RequireOperator` is `RequireRole(manifest.auth
  .operator_role)`. Routes mounted through `EdgeBuilder::mount_operator`
  get it and are excluded from the rate limiter (an operator-only surface
  behind a role gate; a limiter there costs the ceiling it protects).
- **B-3 (exposure table).** `EdgeBuilder::build` records every route with
  its class: `Public`, `Authenticated`, `Operator`, `Probe`, `Proxy`.
  `exposure::report()` renders the table. A route mounted without a class
  is a build-time panic in debug and `Error::Config` in release.
- **B-4 (defaults).** Probes and `/metrics` are `Probe`; `/auth/*` is
  `Proxy`; the static slot is `Public`; everything an app mounts is
  `Authenticated` unless it names `Public` explicitly.
- **B-5 (no fail-open).** The rate limiter, on a store error, answers 503
  rather than admitting; a test asserts it.

## 4. Functional requirements

- **FR-001.** Resolver tests for hops 0, 1, 2 with a three-entry header, a
  one-entry header, and no header.
- **FR-002.** An operator route answers 403 with a decision id for a
  non-operator principal and 200 for an operator; 400 requests in a minute
  from one operator are all admitted.
- **FR-003.** `exposure::report()` lists every route the reference app
  mounts (checked again in 034) and a route without a class fails the build
  in the test.
- **FR-004.** With the store stopped, a rate-limited route answers 503.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p rahi-edge --locked --test hardening` passes.

## 6. Out of scope

TLS termination (the reverse proxy's, documented in 032); WAF rules; the
container's network posture (031).

## 7. Resolved decisions

None yet.

## Verification

```verify:cli
cargo test -p rahi-edge --locked --test hardening
```
