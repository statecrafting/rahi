---
id: "020-edge-server"
title: "The edge: axum router, middleware chain, probes, static slot, error mapping"
status: approved
kind: "kernel"
domain: "edge"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: pending
risk: high
wave: 2
depends_on:
  - "015-kernel-manifest-and-adjudication"
establishes:
  - "crates/rahi-edge/Cargo.toml"
  - "crates/rahi-edge/src/lib.rs"
  - "crates/rahi-edge/src/router.rs"
  - "crates/rahi-edge/src/state.rs"
  - "crates/rahi-edge/src/middleware/mod.rs"
  - "crates/rahi-edge/src/middleware/security_headers.rs"
  - "crates/rahi-edge/src/middleware/csrf.rs"
  - "crates/rahi-edge/src/middleware/rate_limit.rs"
  - "crates/rahi-edge/src/probes.rs"
  - "crates/rahi-edge/src/static_files.rs"
  - "crates/rahi-edge/src/error.rs"
  - "crates/rahi-edge/tests/probes.rs"
  - "crates/rahi-edge/tests/middleware.rs"
extends:
  - { spec: "010-workspace-and-core-types", unit: { kind: section, file: "Cargo.toml", anchor: "workspace.dependencies" }, nature: additive }
summary: >
  The one public listener of a cell: an axum Router builder that an app
  mounts its services on, a fixed middleware order (observation outermost,
  then security headers, CSRF double-submit, and a per-node rate limiter on
  the cache group), liveness and readiness probes that answer different
  questions, a static file slot for the app's SPA, and one mapping from
  rahi_types::Error to HTTP status. Everything an app serves goes through
  this router; there is no second listener.
---

# 020: The edge

## 1. Purpose

Encore's router and gateway were the part of the runtime the app actually
used. This spec replaces them with axum and tower, keeping enrahitu's
middleware order (observation outermost, enrahitu://022) and its probe
separation (`/healthz` touches no dependency, `/readyz` checks the store and
the ledger, enrahitu://025 and enrahitu://033), because the harness (033)
depends on that separation to know when a cell can serve rather than merely
listen.

## 2. Territory

The whole of `crates/rahi-edge` after this spec. Observability (023) and
hardening (024) add modules inside it. The identity crate (021, 022) is
separate; the app composes the two.

## 3. Behavior

- **B-1 (router).** `Edge::builder(state: AppState) -> EdgeBuilder` with
  `.mount(prefix, Router)`, `.static_slot(dir)`, `.build() -> Router`.
  `AppState` carries the kernel, the store handle, the config, and an
  `Extension` slot for the principal extractor (022). The build order of
  layers is fixed and not configurable by the app.
- **B-2 (middleware order).** Outermost to innermost: observation (023's
  layer, a no-op until it lands), security headers, CSRF, rate limit,
  then the app's routers. Probes and `/metrics` are mounted outside the
  CSRF and rate-limit layers.
- **B-3 (security headers).** `Content-Security-Policy` (default-src
  'self'; frame-ancestors 'none'), `X-Content-Type-Options: nosniff`,
  `Referrer-Policy: strict-origin-when-cross-origin`,
  `Strict-Transport-Security` when the public URL is `https`, and
  `Permissions-Policy` denying camera, microphone, and geolocation.
- **B-4 (CSRF).** Double-submit: a `__Host-csrf` cookie (or `csrf` over
  plain http, following the cookie scheme rule) and an `X-CSRF-Token`
  header that must match on every non-safe method under the app's
  prefixes. `/auth/*` (the rauthy proxy) is exempt; rauthy has its own.
- **B-5 (rate limit).** A fixed-window limiter keyed by client identity
  (024 supplies the resolver; until then the peer address) using the
  store's counters with TTL, per node, documented as per-node (N replicas
  admit N times the ceiling). Limits are per-route-group and declared by
  the app; the default group is 300 requests per minute.
- **B-6 (probes).** `GET /healthz` returns 200 with no dependency touched.
  `GET /readyz` returns 200 only when `store.health()` is up and the
  ledger reports verified; otherwise 503 with the failing component named.
- **B-7 (static slot).** `static_slot(dir)` serves files with immutable
  caching for hashed assets and `no-cache` for `index.html`, falling back to
  `index.html` for unknown paths under the slot's prefix (SPA routing).
- **B-8 (errors).** `impl IntoResponse for rahi_types::Error` in one place:
  `Validation` 400, `Unauthorized` 401, `Denied` 403 (with the decision id
  in the body), `NotFound` 404, `Conflict` 409, `Stale` 503, `Integrity`
  500 (body says integrity), `Io`/`Config`/`Upstream` 502 or 500. Messages
  are the error's string; no stack traces.

## 4. Functional requirements

- **FR-001.** Tests boot the router in-process with a temp store and
  assert: `/healthz` is 200 with the store stopped; `/readyz` is 503 with
  the store stopped and 200 after `Store::open`.
- **FR-002.** A POST without a matching CSRF pair is 403 under an app
  prefix and passes under `/auth/`.
- **FR-003.** The security headers are present on every response including
  errors and static files.
- **FR-004.** The limiter returns 429 on request 301 within a minute for one
  identity and admits another identity.
- **FR-005.** Every `Error` variant maps to the status in B-8 (a table
  test).

## 5. Acceptance criteria

- **AC-1.** `cargo test -p rahi-edge --locked` passes.
- **AC-2.** `cargo tree -p rahi-edge` shows no dependency on `rahi-idp`.

## 6. Out of scope

Metrics and tracing (023); trusted proxy hops and operator gating (024);
the rauthy proxy route (021); sessions (022).

## 7. Resolved decisions

None yet.

## Verification

```verify:cli
cargo test -p rahi-edge --locked --test probes
cargo test -p rahi-edge --locked --test middleware
```
