---
id: "024-hardening"
title: "Hardening: trusted client identity, the operator role gate, exposure review of every route"
status: approved
kind: "feature"
domain: "edge"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: complete
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
  - { spec: "020-edge-server", unit: "crates/rahi-edge/src/router.rs", nature: additive }
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

- **D-1 (2026-09-07, build session; reads B-1's last sentence).**
  `ClientIdentity::resolve` implements B-1's arithmetic literally: with hops
  `n`, the entry at index `len - n`; the peer when the list is shorter, when
  the entry does not parse, or when hops is `0`. "Never the leftmost entry"
  is the property that arithmetic guarantees, not a further guard on index
  zero. Reading from the right is self-correcting: a client that forges `k`
  entries makes the list `k` longer, so `len - n` still names what the
  outermost trusted hop observed. Index zero is reached only when the hop
  count exactly accounts for the list, which is the case where the client
  sent no header of its own and the leftmost entry is the honest client
  address a trusted proxy wrote. Rejected alternative: refusing index zero
  outright, which breaks the single-proxy deployment 032 B-3 declares (every
  client collapses into the proxy's bucket) and which a client can bypass by
  prepending one junk entry, so it is both weaker and incoherent.
- **D-2 (2026-09-07, build session; mechanises B-2).** `RequireOperator` is
  the role gate rebuilt in `rahi-edge` rather than `rahi_idp::RequireRole`
  imported: it reads the `rahi_types::Principal` spec 022's layer leaves in
  the request extensions, compares it against
  `kernel.manifest().auth.operator_role`, and ledgers through
  `Kernel::refuse` under the kind `edge.operator`. B-2 says what the gate
  *is*; spec 020 AC-2 says `cargo tree -p rahi-edge` shows no dependency on
  `rahi-idp`, and that criterion belongs to a complete spec. Everything the
  gate needs is already in the edge's dependency set. Rejected alternatives:
  taking the dependency (contradicts 020 AC-2); injecting the gate through
  `AppState`'s extension slot (more machinery than the twenty-five lines it
  would save, and the slot holds values, not middleware).
- **D-3 (2026-09-07, build session; completes B-3's failure mode).**
  `EdgeBuilder::try_build() -> EdgeResult<Router>` is the fallible build and
  returns `Error::Config` naming every unclassified path. `build()` keeps
  spec 020 B-1's signature: it panics in a debug build and, in release,
  returns a router whose fallback answers that same `Error::Config` on every
  request. B-3 asks for both answers but 020 B-1 fixes `build`'s return type,
  so the error needs an additive sibling; and the release fallback has to be
  a `Router`, so the only fail-closed one serves nothing. Rejected
  alternative: logging and serving the unclassified route in release, which
  is the silent fail-open this spec exists to remove.
- **D-4 (2026-09-07, build session; reconciles B-3 with B-4).**
  `exposure::default_class` is total over every mount prefix, so no `mount`,
  `mount_as`, `mount_public` or `mount_operator` can produce an unclassified
  row: B-4's default covers a root merge too. The unclassified state is
  reached through `EdgeBuilder::expose(Route::unclassified(path))`, the call
  an app makes to name individual routes inside a mount, which is also the
  shape 034 B-6 prints. `exposure::publish` accumulates into a process-wide
  union rather than replacing, so `report()` stays meaningful in a test
  binary that builds many routers while remaining, in a cell that builds one,
  that router's table. Rejected alternatives: making a root merge the
  unclassified case (spec 020 documents root merges as legal); replacing on
  publish (correct for a cell, racy for the test that must assert on it).
- **D-5 (2026-09-07, build session; places B-2's gate).** `with_operator`
  applies the gate with `Router::route_layer`, not `Router::layer`, and
  returns a router with no routes untouched; `build` skips the operator
  branch entirely when the app mounted none. axum's `layer` covers the
  fallback, and merging two routers that both carry the default fallback
  keeps the merged-in one, so a gated operator branch turned every unmatched
  path in the cell into 401 (spec 020's own middleware test caught it).
  `route_layer` is the documented answer to exactly this case, and it is the
  right semantics regardless: a gate that answers the fallback tells every
  caller that every path exists. Rejected alternatives: gating each mounted
  router before nesting (same problem for a root merge); giving the operator
  branch its own 404 fallback (a second fallback, which axum refuses).

## Verification

```verify:cli
cargo test -p rahi-edge --locked --test hardening
```
