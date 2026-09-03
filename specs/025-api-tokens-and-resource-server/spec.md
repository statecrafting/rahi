---
id: "025-api-tokens-and-resource-server"
title: "The resource server: bearer tokens rauthy issued, audience bound, scope gated, never app minted"
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
  - "022-session-and-principal"
  - "024-hardening"
establishes:
  - "crates/rahi-idp/src/resource.rs"
  - "crates/rahi-idp/src/bearer.rs"
  - "crates/rahi-idp/src/scope.rs"
  - "crates/rahi-idp/src/registration.rs"
  - "crates/rahi-idp/tests/bearer.rs"
  - "crates/rahi-idp/testdata/tokens/"
extends:
  - { spec: "021-idp-proxy-and-discovery", unit: "crates/rahi-idp/src/lib.rs", nature: additive }
  - { spec: "022-session-and-principal", unit: "crates/rahi-idp/src/extractor.rs", nature: additive }
constrains:
  - { flavor: invariant-freeze, unit: "crates/rahi-idp/src/principal.rs", note: "the app mints no credential; rauthy is the only issuer, cookie or bearer" }
summary: >
  A browser is not the only client. Command line tools, agent runtimes, and
  MCP clients hold no cookie and cannot follow a redirect into a browser
  session, and the chassis has so far had nothing to say to them. This spec
  makes the app an OAuth 2.1 resource server over the same IdP: it publishes
  protected resource metadata so a client can discover the authorization
  server, validates rauthy-issued JWT access tokens locally against the
  cached JWKS with a mandatory audience check, gates routes on scope, and
  answers an unauthenticated request with the challenge that bootstraps
  discovery. Registration and the grants themselves stay rauthy's: the app
  issues no API key, ever.
---

# 025: The resource server

## 1. Purpose

Constitution VII says the IdP is the principal authority and spec 022
delivered that for browsers. Every other client the products in this family
must serve is not a browser: `hqgit` has a CLI that reviews and attests, an
agent runtime acts under delegation, and a memory service is reached by
four different MCP clients over HTTP. The failure mode this spec forecloses
is the one every such system reaches for first and the one that made the
predecessor system indefensible: a static shared key, minted by the app,
accepted forever, carried in a query string. The app must be able to say no
to that by having a real answer already built.

The real answer is that rauthy is an OAuth 2.1 authorization server with
authorization code and PKCE, device authorization for clients with no
browser, client credentials, dynamic client registration, and resource
indicators. All of it is already reachable through the same-origin proxy of
spec 021. What the chassis owes is the resource server half: discovery,
validation, and the challenge.

## 2. Territory

Four modules and one test in `crates/rahi-idp`, plus token fixtures.
Extends 021's `lib.rs` with the new exports and 022's `extractor.rs` with a
credential that is a bearer token rather than a cookie. Freezes on 022's
`principal.rs` the property that no credential originates in this codebase.

## 3. Behavior

- **B-1 (no app-minted credentials).** This codebase contains no code that
  creates, stores, hashes, or compares an API key, a personal access token,
  or any other bearer secret of its own. Every accepted credential is a
  rauthy artifact validated against rauthy's keys. A test greps the
  workspace for `api_key`, `apikey`, and `access_key` outside comments and
  fails on a match.
- **B-2 (protected resource metadata).** `GET
  /.well-known/oauth-protected-resource` returns RFC 9728 metadata:
  `resource` (the public URL, which is also the audience value),
  `authorization_servers` (the public URL plus rauthy's issuer path),
  `bearer_methods_supported: ["header"]`, `scopes_supported` (the union of
  scopes any mounted route requires), and `resource_documentation`. It is
  unauthenticated, cached for one hour, and derived from `Config` so it
  cannot disagree with the deployment.
- **B-3 (validation).** `Bearer` credentials are validated locally: RS256
  signature against the JWKS cache of 021, `iss` equal to the discovery
  issuer, `exp` and `nbf` with 60 seconds of leeway, and `aud` MUST contain
  the `resource` value of B-2. A token without the audience is rejected
  even when otherwise valid, because a token minted for another resource
  must not be replayable here (RFC 8707). There is no introspection call
  on the request path.
- **B-4 (principal from a token).** The `Principal` is built from the same
  claims as 022 B-4, with `sub` verbatim. A token with no `sub` claim is
  rejected. A token whose `sub` names a confidential client rather than a
  person (the client credentials grant) is accepted only on routes a spec
  has explicitly marked service-callable; every other route answers 403
  with a ledgered Decision. The validated `client_id` is carried on the
  principal for audit and for rate limiting.
- **B-5 (revocation lag).** Access tokens are short-lived and not
  introspected, so revocation is bounded rather than immediate. A `jti`
  deny-list lives in the store's cache group with a TTL of the maximum
  access token lifetime, written by the logout path of 022 and by an
  operator verb; validation consults it. The bound (default fifteen
  minutes) is reported by `preflight` and stated in the deployment
  documentation.
- **B-6 (the challenge).** An unauthenticated or invalid-credential
  request to a bearer-protected route answers `401` with
  `WWW-Authenticate: Bearer realm="<resource>",
  resource_metadata="<public_url>/.well-known/oauth-protected-resource"`,
  and, for a scope failure, `403` with `error="insufficient_scope",
  scope="<required>"`. This challenge is the entire bootstrap: a client
  that has never seen this deployment learns the authorization server from
  it.
- **B-7 (dynamic client registration).** rauthy's registration endpoint is
  reachable through the raw proxy and is never reimplemented here.
  `Config.idp.registration` is one of `off`, `token` (default), or `open`,
  it is passed through to rauthy's configuration by the packaging spec,
  and B-2 advertises `registration_endpoint` only when the value is not
  `off`. The default is `token` because an open registration endpoint on a
  public origin is an abuse surface.
- **B-8 (device authorization).** The device code grant is rauthy's and is
  reachable through the proxy. The chassis documents it as the supported
  flow for a client with no browser redirect and implements nothing.
- **B-9 (scope gate).** `RequireScope(&'static str)` is a layer beside
  022's `RequireRole`. A denial is a kernel Decision with the capability,
  the required scope, and the presented scopes, ledgered by 015 and
  answered per B-6. Scopes are space-delimited per RFC 6749 and matched
  exactly; no prefix or wildcard matching exists.
- **B-10 (one credential per request).** A request presenting both a
  session cookie and an `Authorization` header is rejected with 400. The
  ambiguity is never resolved in favor of one, because a resolution rule is
  a confused-deputy generator.
- **B-11 (cookie-free routes).** A bearer-authenticated route sets no
  cookie, and the CSRF layer of 020 does not apply to it, since CSRF
  defends ambient credentials and a bearer token is not ambient. The route
  registration carries the credential kind, so the exemption is declared
  rather than inferred from the request.
- **B-12 (rate limiting).** Token-authenticated requests are limited in
  their own group keyed by `(client_id, sub)` rather than by the client
  identity of 024, so one user's agent runtime cannot exhaust another's
  budget from the same egress address.

## 4. Functional requirements

- **FR-001.** Against the OIDC stub of 022: a token with the correct
  audience authenticates; the same token with `aud` set to another
  resource is rejected; an expired token is rejected; a token signed by a
  key absent from the JWKS is rejected; a `sub`-less token is rejected.
- **FR-002.** A 401 from a bearer route carries a `WWW-Authenticate`
  header whose `resource_metadata` URL is fetchable and returns B-2's
  document, and that document's `authorization_servers[0]` resolves to a
  discovery document through the proxy.
- **FR-003.** A request with both a valid cookie and a valid bearer token
  answers 400 and writes no Decision that would suggest either succeeded.
- **FR-004.** A token whose `jti` is on the deny-list is rejected within
  the lag, and the deny-list entry expires on its own.
- **FR-005.** A scope-gated route with an insufficient token answers 403,
  emits exactly one Decision, and names the required scope in the header.
- **FR-006.** The B-1 grep test fails when a fixture introduces an
  `api_key` column into a migration.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p rahi-idp --locked --test bearer` passes.
- **AC-2.** With `RAHI_TEST_RAUTHY` set, a client registered through
  rauthy's dynamic registration endpoint completes authorization code with
  PKCE against a loopback redirect, presents the resulting access token to
  a scope-gated route, and is admitted; the same token is refused by a
  route requiring a scope it lacks.

## 6. Out of scope

Delegation chains and agent principals, which are a product concern in
`hqgit` and wrap the `Sub` this spec produces rather than replacing it.
Per-tool authorization semantics and consent screens, which belong to the
product that defines the tools. MCP transport and session semantics, which
belong to the product that speaks MCP. Token exchange (RFC 8693), which no
consumer needs yet.

## 7. Resolved decisions

- **D-1 (2026-09-03, this spec).** Local JWT validation with a bounded
  revocation lag, rather than introspection on every request. Introspection
  makes every API call a round-trip to rauthy and makes the IdP a hot
  dependency of the data path, which contradicts the packaging model where
  rauthy may be restarting under supervision. Rejected alternative:
  introspect on every request; rejected fallback: introspect only for
  long-lived tokens, which reintroduces long-lived tokens.
- **D-2 (2026-09-03, this spec).** Audience is mandatory, not optional.
  A deployment that cannot mint audience-bound tokens is misconfigured and
  `preflight` says so, rather than the resource server relaxing.

## Verification

```verify:cli
cargo test -p rahi-idp --locked --test bearer
```
