---
id: "022-session-and-principal"
title: "Sessions and the principal: the signed envelope, renewal as a round-trip, the IdP subject as the only id"
status: approved
kind: "kernel"
domain: "identity"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: pending
risk: critical
wave: 2
depends_on:
  - "021-idp-proxy-and-discovery"
establishes:
  - "crates/rahi-idp/src/login.rs"
  - "crates/rahi-idp/src/session.rs"
  - "crates/rahi-idp/src/envelope.rs"
  - "crates/rahi-idp/src/refresh.rs"
  - "crates/rahi-idp/src/principal.rs"
  - "crates/rahi-idp/src/extractor.rs"
  - "crates/rahi-idp/tests/session.rs"
  - "crates/rahi-idp/tests/principal.rs"
extends:
  - { spec: "021-idp-proxy-and-discovery", unit: "crates/rahi-idp/src/lib.rs", nature: additive }
constrains:
  - { flavor: invariant-freeze, unit: "crates/rahi-idp/src/principal.rs", note: "sub is the only principal id; constitution VII" }
summary: >
  The app keeps the shell and gives up the authority. Login is the
  authorization-code flow with PKCE through the proxy; the session cookie is
  a signed envelope holding rauthy's refresh token and the pinned subject,
  httpOnly and same-origin, that grants nothing on presentation; the access
  assertion is short-lived and minted from the IdP's claims; renewal
  forwards the refresh token to rauthy and re-reads roles and email_verified
  from userinfo every time, so a role removed at the IdP takes effect within
  one access lifetime and a refused grant ends the session. No local account
  row is ever written. Carries enrahitu://004.
---

# 022: Sessions and the principal

## 1. Purpose

enrahitu's first auth system minted its own subject and kept its own
refresh table, and both were places for the app's answer to differ from the
IdP's (enrahitu://004 §1). This spec is the rewrite that fixed it, carried
into Rust unchanged in substance: rauthy's `sub` is the principal, the app
decides nothing about session validity, and the cookie is an envelope, not
a credential.

## 2. Territory

Six modules inside `crates/rahi-idp` and two tests. Extends 021's `lib.rs`.
The `Principal` type itself is spec 010's; this spec owns how one is
produced from a request.

## 3. Behavior

- **B-1 (login).** `GET /auth/login` (mounted by the app outside the raw
  proxy prefix, at `/session/login`) generates `state` and a PKCE verifier,
  stores both in a short-lived `__Host-login` cookie, and redirects to the
  discovery `authorization_endpoint` with S256. `GET /session/callback`
  validates `state`, exchanges the code at the token endpoint through the
  loopback base, verifies the id token against JWKS (issuer, audience,
  expiry, `nonce`), and establishes the session.
- **B-2 (envelope).** `Envelope { sub: Sub, refresh_token: Secret<String>,
  issued: UnixSeconds }` is serialized, signed with HMAC-SHA256 under the
  session key from `/data/keys/session.key`, and set as `__Host-session`
  (or `session` over plain http): httpOnly, SameSite=Lax, path `/`. The
  signature is integrity only; the envelope carries no roles and grants
  nothing.
- **B-3 (access assertion).** `Session { principal: Principal, expires:
  UnixSeconds }` is minted from the id token and userinfo at login and
  cached server-side in the store's cache group keyed by a random session
  id carried in the same cookie, TTL equal to the access token lifetime
  (default fifteen minutes). The cache group is non-durable, which is
  correct: a restart forces a renewal round-trip.
- **B-4 (principal).** `Principal.sub` is rauthy's `sub` verbatim. `email`
  is set only from a verified email claim; `email_verified` absent means
  false; `preferred_username` is never used as an email. `roles` come from
  rauthy's `roles` claim.
- **B-5 (renewal).** When the assertion is missing or expired, the
  extractor forwards the envelope's refresh token to the token endpoint.
  On success it fetches userinfo for the envelope's pinned `sub` (a
  renewal cannot change who the session belongs to), re-reads roles and
  `email_verified`, mints a new assertion, and rotates the envelope with
  the new refresh token. A refused grant clears both cookies and answers
  401. There is no local refresh table.
- **B-6 (extractor).** `Authenticated(Principal)` is an axum extractor
  that performs B-5 transparently. `RequireRole(role)` is a layer that
  answers 403 with a kernel Decision when the principal lacks the role.
- **B-7 (logout).** `POST /session/logout` revokes the refresh token at
  rauthy's revocation endpoint, clears the cookies, and redirects to the
  end-session endpoint.
- **B-8 (rate limits).** Login, callback, and renewal are in their own rate
  limit group (020 B-5), default 30 per minute per client identity.

## 4. Functional requirements

- **FR-001.** Against an in-process OIDC stub (recorded discovery, a test
  JWKS, a token endpoint that accepts one code and one refresh token):
  login round-trip establishes a session whose `sub` equals the stub's;
  a tampered envelope is rejected; an expired assertion triggers exactly
  one refresh call; a refused refresh answers 401 and clears cookies.
- **FR-002.** A role removed at the stub between two requests separated by
  an expired assertion is absent on the second request.
- **FR-003.** A userinfo response with `email` but no `email_verified`
  yields a principal with `email_verified == false` and
  `email() == None`.
- **FR-004.** A grep test asserts no `INSERT` into any user or account
  table exists in the crate.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p rahi-idp --locked` passes.
- **AC-2.** With `RAHI_TEST_RAUTHY` set, a browser-real login through the
  proxy succeeds (driven by the harness in 033; recorded here as the
  wave-2 exit condition, verified in 034).

## 6. Out of scope

The operator role and admin surfaces (024); MFA, passkeys, and account
self-service (rauthy's, reached through the proxy); agent principals and
delegation (a product spec in hqgit).

## 7. Resolved decisions

None yet.

## Verification

```verify:cli
cargo test -p rahi-idp --locked --test session
cargo test -p rahi-idp --locked --test principal
```
