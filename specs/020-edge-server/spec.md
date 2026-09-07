---
id: "020-edge-server"
title: "The edge: axum router, middleware chain, probes, static slot, error mapping"
status: approved
kind: "kernel"
domain: "edge"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: in-progress
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
  - "crates/rahi-edge/tests/common/mod.rs"
  - "crates/rahi-edge/testdata/"
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

- **D-1 (2026-09-06, build session; realises B-8's signature).** The status
  table is `error::status_of` and `EdgeError` is the newtype that carries a
  workspace error into a response. B-8 writes the mapping as
  `impl IntoResponse for rahi_types::Error`, which the orphan rule forbids in
  this crate: `IntoResponse` belongs to `axum-core` and `Error` to
  `rahi-types`, so neither is local here and the impl cannot be written.
  Writing it in `rahi-types` instead would put axum under the crate every
  other crate depends on and invert constitution XIV's direction. What B-8
  fixes, one mapping in one place, is unchanged: `status_of` is the table,
  `error::response` builds the body, and a handler returns
  `Result<T, EdgeError>` so `?` reaches it. Rejected alternative: an
  extension trait over `Result`, which leaves the mapping reachable only
  through the sugar and lets a handler assemble a status by hand.
- **D-2 (2026-09-06, build session; settles B-8's "502 or 500").**
  `Upstream` is 502; `Io` and `Config` are 500. `Error::Upstream` names a
  co-deployed or remote dependency that failed (spec 010 B-4), which is what
  a gateway status is for, and the cell is the gateway in front of rauthy and
  of the store. `Io` and `Config` are failures of this process, which is what
  500 says. Rejected alternative: 502 for all three, which tells a client to
  retry against another upstream when the fault is local and will not move.
- **D-3 (2026-09-06, build session; realises B-5's "counters with TTL").**
  The window ordinal is part of the counter's key
  (`rl:<group>:<identity>:<unix seconds / 60>`) rather than an expiry on the
  counter. The store's counters take no TTL argument (spec 011 exposes
  `counter_add` and `counter_get`), so a fixed window is expressed by never
  naming a spent window again. The cost is that a spent window's key lingers
  in memory until the node restarts, and that a restart forgives every window
  in progress; the cache group is memory-resident, which bounds both.
  Rejected alternatives: read-modify-write over `kv_put`'s TTL, which is not
  atomic and undercounts exactly when it matters; adding a TTL to the store's
  counter API, which is spec 011's unit and a wider hiqlite surface.
- **D-4 (2026-09-06, build session; completes B-5's failure behavior).** A
  store failure inside the limiter admits the request. Constitution IX puts
  rate limits in the derived cache group precisely because nothing whose loss
  changes a decision may live there, so a limiter that refused traffic when
  that group blinked would make derived state a hard dependency of the whole
  edge and turn a cache blip into an outage. The cost is that a cell whose
  cache group is down admits unmetered traffic until it returns; spec 023's
  metrics are where that becomes visible. Rejected alternative: answering
  503, which fails closed on a group whose contents are disposable by design.
- **D-5 (2026-09-06, build session; defines B-7's "hashed assets").** An
  asset is hashed when its last path segment is not `index.html`, has an
  extension, and its stem carries a `.` or `-` separated tail of at least
  eight alphanumeric characters with a digit among them: `index-BsX9k2Lp.js`
  and `main.4f3a2b1c.css` are immutable, `vendor-bootstrap.css` and `app.js`
  revalidate. The rule errs toward revalidating because the two errors are
  not symmetric: a false positive pins a stale asset in every cache for a
  year, and a false negative costs one conditional request. Rejected
  alternative: immutable for every non-HTML file, which is true only of build
  output the chassis cannot verify it is serving.
- **D-6 (2026-09-06, build session; completes B-1's state for B-6).**
  `AppState` also carries the ledger, and the extension slot is a type-erased
  `http::Extensions` read by type. B-1 lists the kernel, the store handle,
  the config, and the slot, but B-6 makes readiness an answer about the store
  *and* the ledger, and the kernel exposes no ledger accessor (spec 015), so
  the probe reaches the chain from the state or not at all. Readiness reads
  `Ledger::head` rather than re-verifying the chain: verification is a
  boot-time property that has already failed closed (constitution XI), and
  re-running it per probe would be a signature check over the whole hot
  window on a path a kubelet calls every few seconds. The slot is
  type-erased because a named type would be `rahi-idp`'s and AC-2 forbids
  that edge; spec 022 inserts its extractor and reads it back by type.
  Rejected alternative: a `Kernel::ledger()` accessor, which amends spec
  015's crate to answer spec 020's probe.
- **D-7 (2026-09-06, build session; completes B-4's issuance).** The CSRF
  layer mints the token itself: a safe request that arrives without the
  cookie is issued one on the way out, and the cookie is readable by script.
  B-4 fixes the pair and the header but names no issuer, and a double submit
  is only a proof if the page can read the cookie to echo it; what makes the
  pair evidence is that a cross-origin caller can send the cookie and cannot
  read it. The cookie is `SameSite=Lax`, `Path=/`, undomained, and `Secure`
  under the `__Host-` prefix. An entropy failure answers 500 rather than
  issuing a guessable token. Rejected alternative: minting in the session
  layer (spec 022), which leaves every route unprotected until a principal
  exists and makes CSRF depend on identity.

## Verification

```verify:cli
cargo test -p rahi-edge --locked --test probes
cargo test -p rahi-edge --locked --test middleware
```
