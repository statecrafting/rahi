---
id: "038-native-clients-and-bearer-revocation"
title: "Native clients and bearer revocation: the CSRF exemption wired, a public client for a CLI declared in the manifest, token lifetimes set, and a token that can be revoked"
status: draft
kind: feature
domain: identity
created: "2026-09-11"
authors: ["Bartek Kus"]
implementation: pending
risk: high
wave: 3
depends_on:
  - "015-kernel-manifest-and-adjudication"
  - "020-edge-server"
  - "022-session-and-principal"
  - "025-api-tokens-and-resource-server"
  - "030-operational-verbs"
  - "031-single-container-packaging"
  - "034-hello-cell"
establishes:
  - "crates/rahi-idp/src/native.rs"
  - "crates/rahi-idp/src/revoke.rs"
  - "crates/rahi-idp/tests/native.rs"
extends:
  - { spec: "020-edge-server", unit: "crates/rahi-edge/src/middleware/csrf.rs", nature: additive }
  - { spec: "025-api-tokens-and-resource-server", unit: "crates/rahi-idp/src/bearer.rs", nature: additive }
  - { spec: "021-idp-proxy-and-discovery", unit: "crates/rahi-idp/src/lib.rs", nature: additive }
  - { spec: "021-idp-proxy-and-discovery", unit: "crates/rahi-idp/src/bootstrap.rs", nature: additive }
  - { spec: "015-kernel-manifest-and-adjudication", unit: "crates/rahi-kernel/src/manifest.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/src/serve.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-ops/src/preflight.rs", nature: additive }
  - { spec: "031-single-container-packaging", unit: "crates/rahi-ops/src/supervise.rs", nature: additive }
  - { spec: "034-hello-cell", unit: "apps/hello-cell/manifest.toml", nature: additive }
  - { spec: "034-hello-cell", unit: "apps/hello-cell/src/notes.rs", nature: additive }
  - { spec: "034-hello-cell", unit: "apps/hello-cell/tests/e2e.rs", nature: additive }
  - { spec: "034-hello-cell", unit: "apps/hello-cell/README.md", nature: additive }
references:
  - { unit: { kind: file, path: "docs/design/01-consumer-contract.md" }, role: context }
summary: >
  A hosted control plane built as a cell and a command-line client that
  logs in to it need four things the chassis does not yet give. The CSRF
  layer exempts only /auth/*, so a POST with a bearer token is refused 403
  csrf (025 B-11 promised the exemption; D-6 left the wiring). No public
  client exists: the one client first boot creates is confidential, and a
  rauthy client defaults to EdDSA with no audience binding, so no CLI token
  passes the resource server without an admin's hand. Token lifetime is
  rauthy's default, 1800 seconds, not the fifteen minutes 025 B-5 states.
  And nothing writes the jti deny-list, so a bearer token cannot be
  revoked before it expires. This spec wires the exemption, declares
  native clients in the manifest and provisions them at boot bound to
  RS256 and the cell's audience, sets lifetimes explicitly, adds
  revocation by token and by subject, reports the bound in preflight, and
  gives hello-cell a scope-gated bearer route and a device-code login in
  its end-to-end test.
---

# 038: Native clients and bearer revocation

## 1. Purpose

Spec 025 made the cell a resource server: rauthy issues, the cell
validates, and the chassis never mints a credential. It left four gaps
that a device-code login by a CLI falls into, each verified on 2026-09-11
at `444bcf8`:

1. **CSRF.** `crates/rahi-edge/src/middleware/csrf.rs` exempts only
   `/auth/*`; `serve` passes no bearer predicate. An unsafe-method request
   carrying `Authorization: Bearer` is refused `403 csrf` unless it also
   carries an equal `csrf` cookie and `X-CSRF-Token` header. 025 B-11 says
   the layer "does not apply" to a bearer route; 025 D-6 records that the
   seam was never built.
2. **No public client.** First boot bootstraps one confidential client
   for the browser flow. rauthy's device endpoint demands the secret from
   a confidential client, which a CLI cannot keep. A new rauthy client
   signs with EdDSA (the resource server accepts RS256 only), and rauthy's
   device grant takes no `resource` parameter, so the cell's origin
   reaches `aud` only through the client's `default_aud`, set by an admin.
   A dynamically registered client needs the same admin call (025 D-12).
3. **Lifetimes.** rahi sets no access token lifetime; rauthy 0.36.2's
   default is 1800 seconds. 025 B-5 states fifteen minutes.
4. **Revocation.** `ResourceServer::deny` exists and has no caller;
   logout does not write it and no verb or route does. `preflight` does
   not report the bound 025 B-5 says it reports.

This spec closes the four without minting a credential (025 B-1) and
without introspecting on the request path (025 D-1).

## 2. Territory

`rahi-idp` gains `native.rs` (declared native clients, provisioned through
rauthy's admin API) and `revoke.rs` (revocation by `jti` and by subject,
and the routes that write it). Additive changes to the CSRF layer (020),
the resource server (025), the client bootstrap and the crate root (021),
the manifest's `[auth]` table (015), the serve composition and preflight
(030), the supervisor's custody step (031), and hello-cell (034).

## 3. Behavior

- **B-1 (the exemption, wired).** The CSRF layer takes an exemption
  predicate; `serve` passes one that is true for a request on a route
  declared bearer (025 D-6's `is_bearer_route`) that carries an
  `Authorization` header and no session cookie. Every other request is
  checked as today. A request with both credentials is still refused
  (025 B-10).
- **B-2 (native clients in the manifest).** `[[auth.native_clients]]`
  declares `id`, `flows` (`device_code`, `authorization_code`, or both),
  `scopes`, and, for `authorization_code`, loopback `redirect_uris`
  (RFC 8252). The manifest validates them: ids unique, scopes a subset of
  the scopes the cell's bearer routes declare, redirect URIs loopback
  only. A native client is part of the declared ceiling and moves the
  manifest hash (spec 036 governs the change).
- **B-3 (provisioned at boot).** The supervisor's custody step, after the
  cell's own client, upserts each declared native client in rauthy as a
  public client: no secret, PKCE `S256`, RS256 access tokens,
  `default_aud` the cell's origin, the declared flows enabled, the
  declared scopes created if missing and allowed. It never deletes a
  client it did not declare.
- **B-4 (lifetimes set).** `[auth] access_token_lifetime_secs` (default
  600) is applied to the cell's own client and every native client, and
  is the deny-list's TTL. The browser session's assertion stays at 900
  seconds (022).
- **B-5 (revocation).** Two deny-lists in the cache group, each with the
  lifetime as TTL: by `jti`, and by subject with an instant (a token for
  that `sub` issued before the instant is refused). A bearer request to
  `POST /session/token/revoke` deny-lists its own `jti`; an operator route
  `POST /operator/tokens/revoke` takes a `jti` or a `sub`; the browser
  logout deny-lists the subject when the manifest asks for it
  (`[auth] logout_revokes_bearer = true`). A revoked token is refused
  `401` with the challenge (025 B-6) within one cache read.
- **B-6 (the bound, reported).** `preflight` reads each declared client's
  lifetime back from rauthy and prints it with the deny-list TTL; a
  lifetime above the manifest's is a failure.
- **B-7 (the worked example).** hello-cell declares a bearer route
  `POST /api/v1/notes` behind scope `notes:write`, and a native client
  `hello-cli` with the device flow. Its end-to-end test, on the rauthy
  path, runs the device grant against rauthy through the cell's origin,
  approves it as the test user, polls the token, posts a note with the
  bearer token and no CSRF pair, revokes the token, and is refused.

## 4. Functional requirements

- **FR-001.** An edge test: a bearer request to a bearer route passes the
  CSRF layer with no pair; the same request with a session cookie is
  refused 400; a cookie request without a pair is refused 403.
- **FR-002.** A manifest test refuses a native client whose scope no route
  declares, and one whose redirect URI is not loopback.
- **FR-003.** `tests/native.rs` provisions a declared client against a
  stub admin API and asserts the upsert body (public, S256, RS256,
  `default_aud`, flows, scopes, lifetime).
- **FR-004.** A resource-server test refuses a deny-listed `jti` and a
  token for a deny-listed subject issued before the instant, and admits
  one issued after.
- **FR-005.** hello-cell's end-to-end test does B-7 against the pinned
  rauthy release.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p rahi-edge --locked`, `cargo test -p rahi-idp
  --locked`, `cargo test -p rahi-kernel --locked`, and `cargo test -p
  hello-cell --locked` pass.
- **AC-2.** Spec 025 AC-1 still holds, and the workspace grep of 025 B-1
  still finds no minted key.
- **AC-3.** `docs/design/01-consumer-contract.md` section 8 and the CLI
  requests of section 9 are updated to the built behavior.

## 6. Out of scope

- Minting tokens or API keys in the chassis (025 B-1 stands).
- Token exchange (RFC 8693) and delegation chains (025 §6).
- Introspection on the request path (025 D-1).
- How a runner authenticates to a control plane: a consumer contract.
- The CLI's own implementation of the device grant.

## 7. Resolved decisions

None yet. Before approval a human decides:

- the default access token lifetime (600 seconds proposed);
- whether native clients belong in the manifest (proposed, since they are
  part of what the cell permits) or in deployment configuration;
- whether `RAHI_IDP_REGISTRATION` should default to `off` once declared
  native clients exist, since a dynamically registered client cannot pass
  the resource server without an admin call anyway (025 D-3 chose
  `token`);
- whether the operator revocation surface is the route proposed here or
  a verb, which would amend spec 030 AC-2's exact verb list.

## Verification

```verify:cli
cargo test -p rahi-edge --locked
cargo test -p rahi-idp --locked
cargo test -p rahi-kernel --locked
cargo test -p hello-cell --locked
```
