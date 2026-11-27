---
id: "051-anonymous-bearer-challenge"
title: "Anonymous bearer challenge: a declared bearer route exempts a request that carries no session cookie from CSRF, so an anonymous unsafe request is answered with the 401 discovery challenge instead of 403 csrf"
status: approved
kind: feature
domain: identity
created: "2026-11-24"
authors: ["Bartek Kus"]
implementation: pending
risk: high
wave: 3
depends_on:
  - "020-edge-server"
  - "022-session-and-principal"
  - "025-api-tokens-and-resource-server"
  - "030-operational-verbs"
  - "038-native-clients-and-bearer-revocation"
summary: >
  Spec 025 B-6 says an unauthenticated request to a bearer route answers 401
  with the discovery challenge, and calls that challenge the entire
  bootstrap. Spec 038 B-1 wired the CSRF exemption for bearer routes only for
  a request that carries an Authorization header, so an anonymous POST meets
  the double-submit check first and is refused 403 csrf with no
  WWW-Authenticate. MCP clients open with exactly that request (the MCP
  authorization flow begins with an anonymous POST), so no MCP client can
  bootstrap against a cell. This spec makes the exemption depend on the
  absence of an ambient credential rather than the presence of a bearer one:
  a request on a declared bearer route that carries no session cookie is
  exempt, with or without an Authorization header.
establishes:
  - "crates/rahi-cli/tests/anonymous_challenge.rs"
extends:
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/src/serve.rs", nature: amending }
  - { spec: "025-api-tokens-and-resource-server", unit: "crates/rahi-idp/src/bearer.rs", nature: additive }
  - { spec: "025-api-tokens-and-resource-server", unit: "crates/rahi-idp/tests/bearer.rs", nature: additive }
  - { spec: "020-edge-server", unit: "crates/rahi-edge/src/middleware/csrf.rs", nature: amending }
  - { spec: "020-edge-server", unit: "crates/rahi-edge/tests/middleware.rs", nature: additive }
---

# 051: Anonymous bearer challenge

## 1. Purpose

Spec 025 B-6 makes the `401` challenge, `WWW-Authenticate: Bearer
realm=..., resource_metadata=...`, the whole bootstrap of a client that has
never seen this cell. Spec 038 B-1 then wired 025 B-11's CSRF exemption
with a predicate that is true only for a request that *carries* an
`Authorization` header. The two disagree for one request shape: an unsafe
request with no credential at all. It is not exempt, so the edge's
double-submit check refuses it `403 {"error":"csrf"}` before any layer that
could answer `401`, and the response carries no `WWW-Authenticate`. A safe
request is not checked and is challenged correctly, which is why 025's
FR-002 test, which uses `GET`, never saw it.

That request is how every MCP client starts. The MCP authorization flow
opens with an anonymous `POST` to the server endpoint and expects `401`
with a `WWW-Authenticate` header naming the protected resource metadata
(RFC 9728); a `403` ends the connection. The first consumer to hit it is
aicortex: its spec 020 D-10 recorded the limitation at rahi 0.7.1, and its
spec 021 (the MCP server) cannot meet its AC-2, a real MCP client
bootstrapping from nothing but a URL, until this lands. The official MCP
TypeScript SDK client's anonymous first `POST` ends with
`Streamable HTTP error: Error POSTing to endpoint: {"error":"csrf",...}`.

The CSRF check defends ambient credentials (020's `csrf.rs` module comment,
025 B-11). A request with no session cookie carries no ambient credential:
there is nothing a cross-origin page can cause the browser to attach that
this cell would act on. Exempting it gives an attacker nothing the request
did not already have, and lets the authentication layers answer it.

## 2. Territory

- `crates/rahi-cli/src/serve.rs` (030's; 038 extends it): the exemption
  predicate passed to `Edge::csrf_exemption` at lines 924 to 936 changes
  from "declared bearer route, an `Authorization` header, and not both
  credentials" to "declared bearer route and no session cookie".
- `crates/rahi-idp/src/bearer.rs` (025's): the predicate moves here as a
  named function, so `serve` and the tests share one definition instead of
  the edge test writing it out a second time (the drift that let 038 B-1
  and 025 B-6 disagree unnoticed).
- `crates/rahi-idp/tests/bearer.rs` (025's): the predicate's truth table.
- `crates/rahi-edge/tests/middleware.rs` (020's; 038 extends it): the
  exemption test at line 177 gains the anonymous case.
- `crates/rahi-edge/src/middleware/csrf.rs` (020's): its module comment's
  statement of the exemption is updated; the check itself does not change.
- `crates/rahi-cli/tests/anonymous_challenge.rs` (new): `serve`'s
  composition answering an anonymous unsafe request on a declared bearer
  route.

The approving change (D-4) turned the drafts' `references` edges into
`extends` edges on the owning specs' units.

## 3. Behavior

- **B-1 (the exemption is the absence of an ambient credential).** The
  CSRF exemption `serve` passes MUST be true for a request whose path is
  covered by a bearer declaration (025 B-11, `is_bearer_route`) and which
  carries no session cookie of this cell's scheme, whether or not it
  carries an `Authorization` header. Every other request is checked as
  today. This replaces 038 B-1's clause "that carries an `Authorization`
  header and no session cookie" with "that carries no session cookie".
- **B-2 (the challenge reaches an unsafe request).** An unsafe request
  (`POST`, `PUT`, `PATCH`, `DELETE`) with no credential on a declared
  bearer route MUST be answered as 025 B-6 answers a safe one: whatever
  the inner layers answer, and when that is `401`, with the
  `WWW-Authenticate` challenge naming the protected resource metadata. It
  MUST NOT be answered `403 csrf`.
- **B-3 (cookie sessions keep their protection).** A request on a declared
  bearer route that carries a session cookie and no `Authorization` header
  MUST still meet the double-submit check, and is refused `403 csrf`
  without an equal pair. A request carrying both credentials is still
  refused `400` by the bearer layer (025 B-10). A route no spec declared
  bearer is checked as today, with or without a credential.
- **B-4 (nothing else moves).** The exempt request MUST NOT be issued a
  CSRF cookie (the check returns before minting one, as it does for any
  exempt request), the bearer layer's handling of a presented token is
  unchanged, and an anonymous request is admitted to no route it was not
  already admitted to as a safe request: the exemption decides whether the
  double-submit proof is demanded, never whether the request is
  authenticated.

## 4. Functional requirements

- **FR-001.** `rahi_idp::bearer` exposes the predicate as one function of
  the path, the headers, and the cookie scheme; `serve` passes it and no
  test restates it.
- **FR-002.** A unit test in `crates/rahi-idp/tests/bearer.rs` covers the
  truth table: declared route with no credential (exempt), with a bearer
  token (exempt), with a session cookie (not exempt), with both (not
  exempt, and refused 400 by the bearer layer); undeclared route with each
  of the four (not exempt).
- **FR-003.** `crates/rahi-edge/tests/middleware.rs`'s exemption test adds
  the anonymous `POST` on a declared route and asserts it passes the check
  with no pair and no `Set-Cookie`.
- **FR-004.** `crates/rahi-cli/tests/anonymous_challenge.rs` drives `serve`'s
  composition (sessions inside the bearer layer, the edge inside both, as
  `serve.rs` lines 940 to 955 compose them) and asserts: an anonymous
  `POST` to an authenticated declared bearer route answers `401` with a
  `WWW-Authenticate` whose `resource_metadata` is this cell's
  `/.well-known/oauth-protected-resource`; the same `POST` with a session
  cookie and no pair answers `403` with `error` `csrf`; with a session
  cookie and an `Authorization` header answers `400`.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p rahi-cli --locked --test anonymous_challenge`
  passes.
- **AC-2.** `cargo test -p rahi-idp --locked --test bearer` and
  `cargo test -p rahi-edge --locked --test middleware` pass, including the
  cases of FR-002 and FR-003.
- **AC-3 (consumer check, never `verify:cli`).** Against a cell built on
  the release carrying this spec, an MCP client's anonymous `initialize`
  `POST` receives `401` with the challenge, recorded in the Status note
  with the client and version. aicortex 021 AC-2 is the expected first run.

## 6. Out of scope

What a self-registering client does after the challenge (dynamic
registration, the token's audience, its signature algorithm), which is spec
052. The session layer's own CSRF handling for cookie-authenticated
browser routes, which is unchanged. MCP transport semantics, which belong
to the product that speaks MCP (025 section 6).

## 7. Resolved decisions

- **D-1 (2026-11-24; why the absence of a cookie, not the presence of a
  token).** CSRF is an attack on ambient authority: a cross-origin page
  makes the browser send a credential it attaches automatically. In this
  chassis the only ambient credential is the session cookie (022), and the
  bearer layer and session layer are the only authenticators. A request
  without the session cookie carries no ambient authority whether or not it
  carries a token, so the presence of `Authorization` was never the fact
  the exemption needed; it was a proxy for "not a browser session", and it
  is wrong for the one request that has neither. Rejected: having the
  bearer layer short-circuit `401` for every anonymous unsafe request on a
  declared route before the inner layers run. It would answer for routes
  an app deliberately serves anonymously under a declared prefix, and it
  would split 025 B-6's decision across two places when the existing
  mechanism (the inner `401`, decorated by `challenged` in `bearer.rs`)
  already gives the right answer once the request can reach it. Rejected:
  exempting only `POST` to a path the app marks as an MCP endpoint, which
  would teach the chassis a protocol it deliberately does not know (025
  section 6).
- **D-2 (2026-11-24; the evidence, read against rahi at `b1fa33f`, the
  0.7.1 line).** `crates/rahi-cli/src/serve.rs` lines 924 to 936 pass
  `is_bearer_route(path) && headers.contains_key(AUTHORIZATION) &&
  !both_credentials(headers, scheme)`. `crates/rahi-edge/src/middleware/csrf.rs`
  line 194 returns early only for an exempt request, and line 216 refuses
  an unsafe request without an equal pair `403` `csrf`.
  `crates/rahi-idp/src/bearer.rs` line 723 passes an anonymous request on a
  declared route inward and attaches the challenge only to a `401`
  (`challenged`, line 862), so a `403` from the edge leaves without one.
  `crates/rahi-cli/src/serve.rs` lines 944 to 954 put the session layer
  outside the edge and the bearer layer outside both, which is why the
  edge's refusal is the answer. `crates/rahi-idp/tests/bearer.rs` line 325
  proves the challenge with `GET` only, and the predicate written out in
  `crates/rahi-edge/tests/middleware.rs` line 177 has the same
  `authorized` clause. The behavior is what 038 B-1 specified; the
  contradiction is between 038 B-1 and 025 B-6, and this spec resolves it
  in 025 B-6's favor.
- **D-3 (2026-11-24; a draft claims no unit).** As 050 D-2: the units of
  030, 025, and 020 named in section 2 are `references` edges while this
  spec is `draft`, so the ratification gate does not refuse other work on
  them. The approving change turns them into `extends` edges and records,
  on 038, that its B-1 clause is replaced by B-1 here.
- **D-4 (2026-11-26; owner approval).** The owner approved this spec on
  2026-11-26 as drafted. The approving change turns section 2's units into
  `extends` edges (`serve.rs` and `csrf.rs`'s module comment `amending`,
  because the exemption's stated rule changes; the rest `additive`) and
  records on 038, as its D-14, that its B-1 clause is replaced by B-1 here.

## Verification

```verify:cli
cargo test -p rahi-cli --locked --test anonymous_challenge
cargo test -p rahi-idp --locked --test bearer
cargo test -p rahi-edge --locked --test middleware
```
