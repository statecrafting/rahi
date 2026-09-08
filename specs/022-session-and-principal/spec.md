---
id: "022-session-and-principal"
title: "Sessions and the principal: the signed envelope, renewal as a round-trip, the IdP subject as the only id"
status: approved
kind: "kernel"
domain: "identity"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: complete
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
  - "crates/rahi-idp/tests/oidc/"
  - "crates/rahi-idp/testdata/oidc/"
extends:
  - { spec: "021-idp-proxy-and-discovery", unit: "crates/rahi-idp/src/lib.rs", nature: additive }
  - { spec: "021-idp-proxy-and-discovery", unit: "crates/rahi-idp/Cargo.toml", nature: additive }
  - { spec: "021-idp-proxy-and-discovery", unit: "crates/rahi-idp/src/discovery.rs", nature: additive }
  - { spec: "015-kernel-manifest-and-adjudication", unit: "crates/rahi-kernel/src/lib.rs", nature: additive }
  - { spec: "010-workspace-and-core-types", unit: { kind: section, file: "Cargo.toml", anchor: "workspace.dependencies" }, nature: additive }
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

- **D-1 (2026-09-07, build session; completes B-2's struct).** The envelope
  carries a fourth field, the session id: `Envelope { sid, sub,
  refresh_token, issued }`. B-2 lists three, and B-3 requires the assertion
  to be "keyed by a random session id carried in the same cookie". The
  cookie's only payload is the envelope, so the id has to be a field of it;
  the two clauses are otherwise unsatisfiable together. It is drawn from the
  operating system rather than derived from the seal, because a key into the
  cache group that an attacker can compute from a cookie they can see is a
  key they can read. `rotated()` carries it through a renewal unchanged,
  which is what makes a renewal replace an assertion rather than orphan one.
  Rejected alternative: keying the cache on the HMAC tag, which is
  unguessable but changes on every rotation and would leave a dead entry per
  renewal.

- **D-2 (2026-09-07, build session; completes B-6's "kernel Decision").**
  `RequireRole` holds a `Kernel` and refuses through a new additive
  `Kernel::refuse(kind, actor, reason, payload)`, declared as an `extends`
  edge on spec 015's `lib.rs`. A role is not a capability: the manifest
  declares a ceiling over resources and the gate has nothing to say about
  who a person is, so `Kernel::admit` would have had to record "no grant
  covers this" for a refusal that was really "this principal lacks a role",
  which is a false record in an audit chain. What the two refusals share is
  everything after the answer, and spec 015 B-6 fixes that: the record is
  built, observed, and queued, and the request path never awaits the append.
  A second appender inside `rahi-idp` would be a second copy of that
  mechanism with its own failure semantics, and a synchronous
  `Ledger::append` in a middleware would put a Raft write on the request
  path that B-6 exists to keep off it. Spec 015's "out of scope" hands the
  *policy* (which routes need which role) elsewhere; it does not hand the
  recording elsewhere, and `refuse` records without deciding. Rejected
  alternatives: adjudicating a role through the gate, which lies in the
  chain; a ledger handle in `rahi-idp`, which blocks the request path.

- **D-3 (2026-09-07, build session; realises B-6's "transparently").** The
  renewal round-trip happens in a layer, `extractor::resolve`, and
  `Authenticated` reads the principal that layer resolved. **An axum
  extractor cannot write a `Set-Cookie`**: it observes the request and
  returns a value, and the response is built after it. B-5 requires the
  envelope to be rotated onto the new refresh token and requires a refused
  grant to clear both cookies, and both are response headers, so the
  round-trip must happen somewhere holding both halves. The handler's view is
  what B-6 describes: write `Authenticated(principal)` and get the principal
  the IdP describes now. The cost is that a route outside `with_sessions`
  answers 401 to everything, which is the failure direction this chassis
  prefers. Rejected alternative: a renewing extractor with a companion
  response layer to flush the cookie, which is the same layer plus a way for
  the two halves to disagree.

- **D-4 (2026-09-07, build session; names a corpus conflict, not a
  choice).** The redirect URI this spec's login sends and this spec's
  callback answers is `<public_url>/session/callback`, derived in
  `Sessions::new` rather than read from `IdpConfig::redirect_uri`. B-1 fixes
  the callback route at `/session/callback` and says why: `/auth/*` is
  rauthy's own subtree, forwarded raw (spec 021 B-2), so an app route inside
  it is a route the proxy swallows. Spec 021 B-1 fixes
  `IdpConfig::redirect_uri` at `<public_url>/auth/callback`, and spec 021
  B-5's `bootstrap_client` registers exactly that with rauthy. **The two
  cannot both be the redirect URI of a working cell.** This spec implements
  its own B-1, which is the one whose reason is stated; the reconciliation is
  a human's, and it is recorded in Status below rather than resolved by
  editing spec 021 (`.claude/rules/adversarial-prompt-refusal.md`). Rejected
  alternative: answering the callback at `/auth/callback` on a route more
  specific than the proxy's wildcard, which makes the proxy no longer raw and
  contradicts B-1's own parenthetical.

- **D-5 (2026-09-07, build session; completes B-7's endpoint).**
  `Discovery` grows `revocation_endpoint: Option<String>`, an `extends` edge
  on spec 021's `discovery.rs`. B-7 revokes the refresh token "at rauthy's
  revocation endpoint", and spec 021 B-3's principle is that every endpoint
  comes from the document rather than from an assumption about rauthy's
  paths. Spec 021 D-7 kept the model to the endpoints the chassis calls; a
  logout that revokes is a call, so the seventh field is that decision
  applied rather than reversed. `Option` because RFC 8414 makes the field
  optional; rauthy publishes it, and a server that does not gets
  `Error::Upstream` naming the absence. Rejected alternative: deriving
  `<issuer>/oidc/revoke`, which is the assumption B-3 exists to avoid.

- **D-6 (2026-09-07, build session; names B-2's `Secret<String>`).**
  `Secret<T>` is defined in `envelope.rs` rather than taken from the
  `secrecy` crate. B-2 writes the type but the workspace has none, and what
  the spec needs from it is one property: a refresh token that never renders
  in a `Debug`, a log line, or an error, and whose every read is the
  greppable `expose()`. That is twenty lines. A dependency for it would put
  a third-party type in the shape of a cookie the chassis serialises, which
  makes its serde representation somebody else's version policy. Rejected
  alternative: `secrecy`, whose `SecretBox` deliberately does not implement
  `Serialize`, which is the one thing this envelope must do.

- **D-7 (2026-09-07, build session; extends spec 021 D-6's reasoning).**
  `session::answer` transcribes spec 020 B-8's status table into this crate.
  Spec 021 D-6 named exactly one status here on the grounds that one
  condition is not a table; this spec raises 400, 401, 403, 502, and 500 on
  the session routes, so it is a table now. It is transcribed rather than
  called because the layering makes identity and the edge peers and permits
  neither to depend on the other, and a `rahi-edge` dependency would make the
  identity crate unusable without the edge. The coupling is real and is
  written down here as it was there: if spec 020 B-8 moves a status, this
  follows. Rejected alternative: a `rahi-edge` dependency, which reverses an
  arrow the thesis fixes.

- **D-8 (2026-09-07, build session; completes B-3's cache access).** The
  assertion is written through `StoreHandle::kv_put` directly rather than
  through a `Governed<Store>` facade. Spec 015 B-5's rule is that
  *application* code reaches the store only through a facade; the chassis's
  own edge already reads and writes the same cache group directly for the
  rate limiter (spec 020 B-5), and the session cache is the same kind of
  derived state serving the same kind of chassis concern. Governing it would
  mean every app's manifest declaring a `kv` resource for a namespace the app
  never names, which is a grant that cannot be refused without breaking
  login. Rejected alternative: a facade over a chassis-reserved namespace,
  which is a ceiling nobody can lower.

- **D-9 (2026-09-07, third build session; reverses D-4's mechanism, whose
  reason is now void).** `Sessions::new` reads `IdpConfig::redirect_uri`
  verbatim rather than rebuilding `<origin>/session/callback` from
  `SESSION_PREFIX` and `session::CALLBACK_PATH`, and returns `Error::Config`
  when that value is not the callback route `login_router` mounts. A unit test
  pins `config::CALLBACK_PATH` to `SESSION_PREFIX` + `session::CALLBACK_PATH`.
  D-4 derived the URI independently because spec 021 B-1 held a value this
  spec could not send; spec 021 D-9 corrected that constant, so the reason is
  gone and only the duplication remained. rauthy matches a `redirect_uri`
  literally, so the string the authorization request sends must equal the
  string `bootstrap_client` registers, and two derivations of it agreed only
  by coincidence: an edit to either constant refused every login, and the only
  check that caught it was AC-2, which this spec defers to 034. Verified by
  mutation: restoring `/auth/callback` now fails twelve of the thirteen
  session integration tests and the new unit test, where before it passed the
  whole suite. Rejected alternatives: asserting the equality in a test only,
  which misses a composer that hand-builds `IdpConfig`; leaving both
  derivations, which is the coincidence that produced D-4's conflict.

## 8. Status

- **2026-09-07 (build session).** `implementation` stays `in-progress` on one
  point, and it is not in this spec's territory. Everything this spec owns is
  built and covered: B-1 through B-8 by seventeen integration tests against
  an in-process OIDC stub that signs real RS256 id tokens over a real socket
  and a real single-voter store, plus twenty-six unit tests. FR-001 is four
  assertions in `tests/session.rs` (the round trip, the tampered envelope,
  exactly one refresh call, and the refused grant that clears both cookies);
  FR-002 and FR-003 are `tests/principal.rs`; FR-004 is a scan of the crate's
  own source that asserts something stronger than it asks for, that the crate
  contains no SQL at all. **AC-1 passes.**

  What remains is the conflict D-4 names. A cell bootstrapped by spec 021
  B-5's `bootstrap_client` registers `<public_url>/auth/callback` as its only
  redirect URI, and this spec's login sends `<public_url>/session/callback`,
  which rauthy will refuse. AC-2 is precisely the run that would surface it,
  and AC-2 is by its own text recorded here and verified in 034, so nothing
  in this session can close it. A human decides which of two shapes to take:
  amend spec 021 B-1 so `IdpConfig::redirect_uri` is the session callback
  (one line, and `bootstrap_client` follows), or leave 021 B-1 as it is and
  make spec 030's composer register both. Neither is an edit this session may
  make: the first amends a spec this session did not implement, and the
  second belongs to a spec that does not exist yet. Once it is settled, this
  flips to `complete` with no code change here.

- **2026-09-07 (second build session; confirms the above, adds no code).**
  The whole governed gate is green on this branch: `spec-spine compile`,
  `index check`, `lint --fail-on-warn`, `couple --base origin/main`
  (18 paths, no drift), and `make ci` all exit 0. Both `verify:cli` blocks
  pass, and AC-1 passes as 79 tests across the crate. The only tree change
  this session carries is a doc-comment correction in `envelope.rs`:
  `Envelope.issued` was documented as enabling an age-out without a
  round-trip, and nothing reads it for that purpose, so the comment now
  says what the field is (the seal time, rewritten by a rotation).

  D-4's conflict was re-derived from the code rather than taken on trust,
  and it is real and narrower than D-4 states: it reduces to the single
  constant `config::CALLBACK_PATH` (`crates/rahi-idp/src/config.rs:20`,
  spec 021's territory). `config.rs:70` builds `IdpConfig::redirect_uri`
  from it and `bootstrap.rs:75` registers exactly that string with rauthy,
  while this spec's `Sessions::new` (`session.rs:239`) independently builds
  `<origin>/session/callback` from its own `session::CALLBACK_PATH`. The
  two literals differ and rauthy matches a `redirect_uri` literally, so a
  browser-real login is refused. Spec 021 B-1's value is moreover unusable
  on its own terms: `/auth/callback` falls inside the raw proxy prefix that
  021 B-2 forwards verbatim, so rauthy would receive its own redirect back
  and answer 404. This spec's B-1 is the clause carrying the stated reason.

  So the human fix in the first of D-4's two shapes is one line: set spec
  021 B-1's `redirect_uri` to `<public_url>/session/callback` and
  `config::CALLBACK_PATH` to `/session/callback`; `bootstrap_client` and
  the registration follow with no further change, and this crate's tests
  are unaffected. That edit belongs to whoever owns spec 021, not to a
  session implementing 022 (`.claude/rules/adversarial-prompt-refusal.md`),
  which is why this stays `in-progress`.

- **2026-09-07 (third build session; the blocker cleared, and this flips to
  `complete`).** The conflict D-4 named and the second session narrowed to one
  constant was reconciled by a human on `main` as spec 021 D-9 (commit
  `9ecf19e`, PR #24): `config::CALLBACK_PATH` is now `/session/callback`, so
  `IdpConfig::redirect_uri` and this spec's login send the same string and
  `bootstrap_client` registers it. Spec 021 is `implementation: complete`.
  Nothing this spec owns needed changing, which is what the second session
  predicted.

  The one thing this session changed is D-9, and it closes the trap the
  reconciliation left behind rather than the conflict it resolved: the two
  constants now agree, but nothing held them together, so the next edit to
  either would have refused every login with no test to say so. `Sessions::new`
  now sends the registered URI and refuses to boot on a mismatch. The gate is
  green (`compile`, `index check`, `lint --fail-on-warn`, `couple --base
  origin/main` over 18 paths, `make ci`, all exit 0), both `verify:cli` blocks
  pass, and **AC-1 passes as 80 tests** across the crate.

  **AC-2 is not this session's to run and does not hold it open.** By its own
  text it is "driven by the harness in 033; recorded here as the wave-2 exit
  condition, verified in 034", so it names where it is discharged. Every
  criterion this spec owns is met, so `implementation` is `complete`.

## Verification

```verify:cli
cargo test -p rahi-idp --locked --test session
cargo test -p rahi-idp --locked --test principal
```
