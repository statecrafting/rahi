---
id: "052-self-registered-client-tokens"
title: "Self-registered client tokens: a client that registers itself through dynamic registration and asks for this cell with an RFC 8707 resource indicator receives a token the resource server admits, with no administrator call"
status: draft
kind: feature
domain: identity
created: "2026-11-24"
authors: ["Bartek Kus"]
implementation: pending
risk: critical
wave: 3
depends_on:
  - "025-api-tokens-and-resource-server"
  - "031-single-container-packaging"
  - "038-native-clients-and-bearer-revocation"
  - "043-patched-dependency-adoption"
  - "051-anonymous-bearer-challenge"
summary: >
  Spec 025 B-7 offers rauthy's dynamic client registration, and B-3 and D-2
  make the cell origin in `aud` mandatory. On the pinned rauthy those two
  cannot both hold for a client that registers itself: rauthy refuses any
  RFC 8707 `resource` from a dynamic client, and its only other route to an
  audience is `default_aud`, which only an administrator can set (025 D-12,
  037 D-7). Three more defaults stand between a self-registered client and
  admission: rauthy signs its access tokens EdDSA where the chassis accepts
  RS256 only, grants it only `openid profile email groups`, and gives it a
  thirty-minute lifetime above the cell's ceiling; and the packaged cell
  passes none of rauthy's dynamic client settings at all. This spec makes
  the packaged cell configure rauthy so a standards-only client (RFC 7591
  registration, RFC 8707 resource, PKCE) gets an admissible token, without
  relaxing the audience check.
establishes:
  - "crates/rahi-idp/tests/self_registered.rs"
references:
  - { unit: { kind: file, path: "crates/rahi-idp/src/registration.rs" }, role: context }
  - { unit: { kind: file, path: "crates/rahi-idp/src/bearer.rs" }, role: context }
  - { unit: { kind: file, path: "crates/rahi-idp/src/resource.rs" }, role: context }
  - { unit: { kind: file, path: "crates/rahi-ops/src/rauthy_env.rs" }, role: context }
  - { unit: { kind: file, path: "docker/Dockerfile" }, role: context }
---

# 052: Self-registered client tokens

## 1. Purpose

The MCP authorization flow, and any OAuth client that has never seen a
deployment, goes: `401` challenge (025 B-6, reachable on an unsafe request
once 051 lands), protected resource metadata (025 B-2), dynamic client
registration (RFC 7591, 025 B-7), authorization code with PKCE carrying an
RFC 8707 `resource` indicator naming the resource, and a token for that
resource. The user pastes a URL and nothing else.

Against this chassis that flow ends with a token the resource server
refuses. aicortex recorded it in its spec 020 D-14 and its spec 021 Status
(rahi 0.7.1, `rauthy-patched` 0.36.2-patched.3): its live tests reach a
token only because a fixture administrator sets the dynamic client's
`default_aud`, which is how rahi's own 025 AC-2 test works too (025 D-12,
037 D-7's correction). That is the administered path, and 037 D-7 says in
as many words that it "claims no universal RFC 8707 or MCP
interoperability". A client that registers itself has no administrator.

Four things stand in the way, each measured against rauthy's source at the
pinned tag (`v0.36.2-patched.3` of `bartekus/rauthy`) and against rahi at
`b1fa33f`:

1. **Audience.** `Client::validate_resource_request`
   (`src/data/src/entity/clients.rs` line 967) refuses every `resource` from
   a client whose id starts `dyn$` with `invalid_target`, "dynamic clients
   must not request a `resource`" (line 981). `default_aud` is set only
   through the admin API. So a self-registered client's token carries only
   its own client id in `aud`, and `ResourceServer::validate`
   (`crates/rahi-idp/src/bearer.rs` line 396) refuses it, as 025 B-3 and D-2
   require.
2. **Signature algorithm.** A dynamic client's access token algorithm is
   taken from the registration request's `token_endpoint_auth_signing_alg`
   (clients.rs line 1823) and defaults to `EdDSA`. The chassis accepts
   `RS256` only (bearer.rs line 361). A standards-only client does not send
   that field, so its token is refused on the algorithm before the audience
   is read.
3. **Scopes.** A dynamic client is granted `dynamic_clients.allowed_scopes`
   (clients.rs line 1854), default `openid profile email groups`. The scopes
   a cell's routes require (025 B-9) are not among them, so the token, even
   once admitted, passes no scope gate.
4. **Lifetime.** A dynamic client's access token lifetime is
   `dynamic_clients.default_token_lifetime` (clients.rs line 1879), default
   thirty minutes. The resource server refuses a token whose `exp - iat`
   exceeds the cell's ceiling (bearer.rs line 464, 043 B-6), which a cell
   may set lower (aicortex sets 600 seconds).

And underneath all four: `Registration::rauthy_env`
(`crates/rahi-idp/src/registration.rs` line 126), which 025 B-7 says the
packaging passes to rauthy, has no caller outside its own unit tests, and
`crates/rahi-ops/src/rauthy_env.rs` writes no `ENABLE_DYN_CLIENT_REG` or
`DYN_CLIENT_*` key. A packaged cell advertises `registration_endpoint`
(025 B-2, B-7) while its rauthy runs with dynamic registration at rauthy's
default, which is off. aicortex's fixture enables it itself, which is why
this was not seen there.

Upstream rauthy closed the audience half: `v0.37.0` adds
`dynamic_clients.allowed_resources` (`DYN_CLIENT_ALLOWED_RESOURCES`,
upstream commit `0eb1b589`, "let operators allow-list RFC 8707 resources
for dynamic/ephemeral clients", sebadob/rauthy#1686), an operator allow-list
resolved from the live configuration on every request, written for exactly
this: "an MCP client, which must send one, would otherwise get
`invalid_target`". The access token's `aud` is then the client id plus the
granted `resource` (`src/service/src/token_set.rs`, `build_access_token`).
No rauthy release has a dynamic-client default for the access token
algorithm.

## 2. Territory

- `crates/rahi-ops/src/rauthy_env.rs` (031's): the rauthy environment gains
  the dynamic client settings of B-1 to B-4, derived from the cell's
  configuration and manifest.
- `crates/rahi-idp/src/registration.rs` (025's): `Registration::rauthy_env`
  grows into the full set of B-1 to B-4 and is what `rauthy_env.rs` calls,
  so the setting 025 B-7 names reaches rauthy.
- `crates/rahi-idp/src/bearer.rs` and `resource.rs` (025's): only if D-3 or
  D-4 is decided toward the chassis; otherwise untouched.
- `docker/Dockerfile` (031's, pinned under 043): the rauthy image moves to
  a build carrying `dynamic_clients.allowed_resources`. That move is its own
  adoption under 043's rules and its own pull request; this spec names it as
  a prerequisite and does not perform it.
- `crates/rahi-idp/tests/self_registered.rs` (new): the live proof.

While `draft`, every unit but the new test is a `references` edge (050
D-2); the approving change turns the ones the build touches into `extends`
edges.

## 3. Behavior

- **B-1 (registration reaches rauthy).** The packaged cell MUST pass
  `Config.idp.registration` (025 B-7, `RAHI_IDP_REGISTRATION`) to rauthy:
  `off` disables dynamic registration, `token` enables it with the
  registration token from the key set, `open` enables it without one. The
  protected resource metadata MUST NOT advertise a registration endpoint
  rauthy is not serving.
- **B-2 (audience by resource indicator).** When registration is not
  `off`, the packaged cell MUST set rauthy's
  `dynamic_clients.allowed_resources` to this cell's resource value (025
  B-2's `resource`, the public origin) and nothing else. A dynamic client
  that requests that `resource` on authorization and on the code exchange
  then receives a token whose `aud` contains it, and 025 B-3's audience
  check admits it unchanged. A dynamic client that requests another
  resource is refused `invalid_target` by rauthy; one that requests none
  receives a token without the audience and is refused by B-3, as today.
  The audience check MUST NOT be relaxed to accept the client id, a
  wildcard, or a prefix (025 D-2).
- **B-3 (an algorithm the chassis verifies).** A token issued to a
  dynamic client that sent only RFC 7591 metadata MUST be signed with an
  algorithm the resource server verifies. The mechanism is D-3's open
  decision; whichever is chosen, the resource server MUST keep refusing
  `none`, any symmetric algorithm, and any token whose header `alg`
  disagrees with the published key's.
- **B-4 (scopes and lifetime from the manifest).** When registration is not
  `off`, the packaged cell MUST set `dynamic_clients.allowed_scopes` to
  `openid` plus the scopes the manifest declares for bearer routes (the set
  025 B-2 advertises as `scopes_supported`), and
  `dynamic_clients.default_token_lifetime` to no more than the cell's
  access token lifetime ceiling (043 B-6). Each scope MUST exist in rauthy's
  catalog before a client can be granted it, created the way 038's native
  client provisioning creates scopes.
- **B-5 (no administrator in the loop).** The flow of section 1 MUST
  complete with no admin API call, no `default_aud`, and no client-specific
  configuration, against the packaged cell's own rauthy.

## 4. Functional requirements

- **FR-001.** A unit test asserts the rauthy environment for each of `off`,
  `token`, and `open`: the enable flag, the registration token's presence
  only under `token`, and, when not `off`, `DYN_CLIENT_ALLOWED_RESOURCES`
  equal to the resource value, `DYN_CLIENT_ALLOWED_SCOPES` equal to B-4's
  set, and `DYN_CLIENT_DEFAULT_TOKEN_LIFETIME` within the ceiling.
- **FR-002.** `preflight` reports a configuration that advertises
  registration while the rauthy environment does not enable it, or that
  enables it on a rauthy build without `dynamic_clients.allowed_resources`
  (read from the pinned version), as a failure.
- **FR-003.** `crates/rahi-idp/tests/self_registered.rs`, opted in by
  `RAHI_TEST_RAUTHY_URL` as 025 D-12's test is, registers a public client
  through `POST /auth/v1/clients_dyn` with RFC 7591 metadata only (redirect
  URI, grant types, `token_endpoint_auth_method: none`), completes
  authorization code with S256 PKCE and `resource` set to the protected
  resource metadata's `resource`, and presents the token: a scope-gated
  route admits it `200`, a route requiring an undeclared scope refuses it
  `403 insufficient_scope`. The test makes no admin API call.
- **FR-004.** The same test requests a `resource` that is not this cell
  and asserts rauthy refuses it `invalid_target`, and omits `resource` and
  asserts the resulting token is refused `401 invalid_token` on audience.
- **FR-005.** The resource value a client sends MUST be measured, not
  assumed: a client that derives it with a URL parser may send the origin
  with a trailing `/`, which rauthy matches verbatim (D-4). FR-003 runs
  with the value as published and as a URL parser serializes it, and the
  Status note records what the reference MCP client sends.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p rahi-idp --locked --test self_registered`
  passes, with the live half run against the pinned rauthy in `live.yml`
  (037 B-5) and recorded in the Status note.
- **AC-2.** `cargo test -p rahi-ops --locked` passes, including FR-001, and
  `rahi preflight` fails on each misconfiguration FR-002 names.
- **AC-3 (consumer check, never `verify:cli`).** With 051 also released, a
  real MCP client given only the cell's URL completes the bootstrap,
  registration, authorization, and an authenticated tool call. aicortex
  021 AC-2 is the expected first run; the client and version are recorded.

## 6. Out of scope

The rauthy image change itself, which is its own adoption under 043 (a
rauthy-patched build on upstream `v0.37.x`, or a backport of `0eb1b589`
onto the 0.36.2-patched line). Client ID Metadata Documents (rauthy's
ephemeral clients), which rauthy 0.37 also allow-lists but no consumer
needs yet. Consent screens and per-tool authorization (025 section 6).
Token exchange. The CSRF exemption for the anonymous first request, which
is 051.

## 7. Resolved decisions

- **D-1 (2026-11-24; the audience mechanism).** A resource indicator
  allow-listed by the operator (B-2), rather than either alternative the
  consumer named. Rejected: accepting the client id as an audience, which
  would admit any token rauthy issues to any client of this cell's rauthy,
  for any resource, and undo 025 D-2 and RFC 8707's replay protection.
  Rejected: a configured default audience for every dynamic client, which
  rauthy has no setting for, and which would stamp this cell on tokens a
  client never asked for it; the resource indicator is the client stating
  which resource it wants, which is what the MCP authorization flow already
  sends.
- **D-2 (2026-11-24; a draft claims no unit).** As 050 D-2: section 2's
  existing units are `references` edges while this spec is `draft`.
- **D-3 (2026-11-24; open, for the approving owner: the signature
  algorithm).** B-3 needs one of three, and this draft does not choose.
  (a) The resource server also verifies `EdDSA` (Ed25519) under a key
  rauthy publishes, which widens a critical validator and amends 025 B-3's
  "RS256 signature". (b) A rauthy setting giving dynamic clients a default
  access token algorithm, which no upstream release has; it would be a
  patch on `bartekus/rauthy` offered upstream, carried in the same rauthy
  adoption as `allowed_resources`, and keeps the chassis validator
  untouched. (c) Rauthy signing every token `RS256` by configuration, if a
  global default exists in the adopted build; not found in `v0.37.0`. The
  drafting session leans to (b): the chassis does not relax for a
  dependency's default (025 D-2's reasoning), and the rauthy build has to
  move for B-2 anyway.
- **D-4 (2026-11-24; open, for the build session to measure: the resource
  string).** rauthy compares `resource` verbatim against the allow-list,
  and the chassis compares `aud` verbatim against the origin (bearer.rs
  line 396, `resource.rs` line 122). If a reference client sends
  `https://cell.example.com/` for a published `https://cell.example.com`,
  B-2 fails on the string. FR-005 measures it. If it does, the choice is
  between allow-listing both forms and having the audience check treat an
  origin and the origin plus `/` as one resource (RFC 3986 section 6.2.3,
  scheme-based normalization of an empty path), and nothing wider; that
  amends 025 B-3 and is a human decision.

## Verification

```verify:cli
cargo test -p rahi-idp --locked --test self_registered
cargo test -p rahi-ops --locked
```
