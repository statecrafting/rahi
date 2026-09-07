---
id: "021-idp-proxy-and-discovery"
title: "rauthy behind a same-origin proxy: the raw /auth/* route, discovery, JWKS, client bootstrap"
status: approved
kind: "kernel"
domain: "identity"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: in-progress
risk: critical
wave: 2
depends_on:
  - "020-edge-server"
establishes:
  - "crates/rahi-idp/Cargo.toml"
  - "crates/rahi-idp/src/lib.rs"
  - "crates/rahi-idp/src/config.rs"
  - "crates/rahi-idp/src/proxy.rs"
  - "crates/rahi-idp/src/discovery.rs"
  - "crates/rahi-idp/src/jwks.rs"
  - "crates/rahi-idp/src/bootstrap.rs"
  - "crates/rahi-idp/tests/proxy.rs"
  - "crates/rahi-idp/tests/discovery.rs"
  - "crates/rahi-idp/tests/common/mod.rs"
  - "crates/rahi-idp/testdata/discovery/"
extends:
  - { spec: "010-workspace-and-core-types", unit: { kind: section, file: "Cargo.toml", anchor: "workspace.dependencies" }, nature: additive }
summary: >
  One public origin for app and IdP: rauthy binds loopback, the app
  reverse-proxies the /auth/* subtree to it raw and unfiltered with bodies
  streamed, the issuer and every OIDC endpoint live under the app's URL, the
  driver trusts rauthy's discovery document fetched through the loopback
  base and caches its JWKS with rotation, and the app's OIDC client is
  bootstrapped on first boot with authorization-code plus refresh, S256
  PKCE, RS256, and redirect URIs derived from the public URL. rauthy is a
  release binary in the same container and is never forked. Carries
  enrahitu://005.
---

# 021: rauthy behind a same-origin proxy

## 1. Purpose

Exactly one exposed port and no CORS between app and IdP (constitution VI).
The proxy is the sole route into rauthy, which is why it is raw: filtering
it would mean re-implementing rauthy's surface, and rauthy already
authenticates and rate-limits its own endpoints. The app's identity
decisions (022) are built on the discovery document and JWKS this spec
fetches, never on assumptions about rauthy's paths.

## 2. Territory

The crate `rahi-idp` after this spec: config, proxy, discovery, JWKS,
bootstrap. Sessions and the principal (022) add modules inside it.
**Operator prerequisite for integration tests:** a rauthy binary on `PATH`
or `RAHI_TEST_RAUTHY` naming one; unit tests use recorded discovery
documents under `testdata/discovery/` and need nothing.

## 3. Behavior

- **B-1 (config).** `IdpConfig` is derived from `rahi_types::Config`:
  `loopback_base` (`http://127.0.0.1:8080`), `issuer`
  (`<public_url>/auth/v1`), `client_id` (the app name from the manifest),
  `redirect_uri` (`<public_url>/auth/callback`), and the client secret path
  under `/data/keys/`.
- **B-2 (proxy).** `proxy_router() -> Router` mounts `/auth/*` and forwards
  method, path, query, headers (minus hop-by-hop), and a streamed body to
  `loopback_base`, returning the upstream status, headers, and streamed
  body unchanged. It sets `X-Forwarded-Proto` and `X-Forwarded-Host` from
  the public URL so rauthy builds correct redirects. No filtering, no
  buffering, no CSRF layer.
- **B-3 (discovery).** `Discovery::fetch(loopback_base) ->
  Result<Discovery>` retrieves
  `/auth/v1/.well-known/openid-configuration`, verifies `issuer` equals the
  configured issuer, and exposes the authorization, token, userinfo,
  end-session, and JWKS endpoints. It retries with backoff for up to sixty
  seconds at boot (rauthy starts in the same container) and is
  `Error::Upstream` after that.
- **B-4 (JWKS).** `Jwks::load(discovery)` fetches and caches keys; a token
  whose `kid` is unknown triggers one refresh before rejection; the cache
  refreshes on a timer (default one hour) and keeps the previous set for
  one interval for rotation.
- **B-5 (bootstrap).** `bootstrap_client(config, admin_token)` registers
  the OIDC client with rauthy's admin API on first boot (spec 031 supplies
  the admin token from `/data/keys/`): confidential client, `code` and
  `refresh_token` grants, S256 PKCE required, RS256, the redirect URI from
  B-1, post-logout redirect to the public URL. It is idempotent: an
  existing client with matching settings is left alone; a mismatch is
  `Error::Conflict` with the differing field named, never silently
  overwritten.
- **B-6 (cookie mode).** When the public URL is plain `http`, the config
  emits `COOKIE_MODE=danger-insecure` for rauthy (spec 031 writes it) so
  local trials work in Safari; with `https` it emits `PROXY_MODE=true`.

## 4. Functional requirements

- **FR-001.** A unit test proxies to an in-process stub upstream and
  asserts headers, a 2 MiB streamed body, a redirect status, and the
  forwarded proto and host.
- **FR-002.** Discovery tests over recorded documents: matching issuer
  parses; a mismatched issuer is `Error::Validation`; a document missing
  `jwks_uri` is `Error::Validation`.
- **FR-003.** JWKS tests: unknown `kid` refreshes once then rejects; a key
  removed upstream verifies for one interval and not after.
- **FR-004.** With `RAHI_TEST_RAUTHY` set, an integration test boots rauthy
  on loopback, bootstraps the client, fetches discovery through the proxy,
  and asserts the issuer is the public URL; otherwise it is skipped with a
  message.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p rahi-idp --locked` passes (integration test
  skipped or passed, never failed for absence of rauthy).

## 6. Out of scope

The login flow, sessions, and the principal (022); rauthy's own
configuration file and process supervision (031); mail delivery for the
IdP (an operator concern documented in 031).

## 7. Resolved decisions

- **D-1 (2026-09-07, build session; completes B-1's `client_id`).**
  `IdpConfig::derive(&Config, client_id)` takes the client id as an argument
  rather than reading a manifest. B-1 says the id is the manifest's app name
  and that stays true: the composer (spec 030) holds both documents and
  passes `manifest.app.name`. Taking a `&Manifest` here would put
  `rahi-kernel` under the identity crate for one string, and the layering
  puts identity beside the edge rather than above the kernel's document
  model. The id is validated against rauthy's own client id rule on the way
  in, so a name rauthy would refuse fails at derivation instead of at first
  boot. Rejected alternative: a `rahi-kernel` dependency for one field read.
- **D-2 (2026-09-07, build session; completes B-2's signature).**
  `proxy_router(Proxy)` takes the forwarder, and `Proxy::new(&IdpConfig)`
  builds it. B-2 writes `proxy_router() -> Router`, which cannot know the
  loopback base, the forwarded scheme, or the forwarded host, all three of
  which are per-cell. Building the HTTP client once in `Proxy::new` also
  keeps the connection pool for rauthy's whole lifetime instead of one per
  request. Rejected alternative: a global set at boot, which makes two cells
  in one process impossible and makes a test depend on process state.
- **D-3 (2026-09-07, build session; completes B-3's signature).**
  `Discovery::fetch(&IdpConfig)` takes the configuration rather than the
  loopback base alone, because B-3's own requirement is that it "verifies
  `issuer` equals the configured issuer" and the loopback base does not carry
  the issuer. `Discovery::parse(document, expected_issuer)` is the pure half,
  which is what the recorded documents are tested through, and
  `fetch_within` names the budget so a test does not wait sixty seconds to
  watch a failure. A document that arrives and is wrong is returned as
  `Error::Validation` immediately rather than retried: retrying a wrong
  answer only delays it.
- **D-4 (2026-09-07, build session; realises B-4's timer).** The refresh is
  lazy. An access that finds the interval elapsed refreshes before it
  answers, and the previous set is served until `refreshed_at + interval`.
  B-4 says the cache "refreshes on a timer", and a spawned task would be a
  timer that outlives the value that owns it and keeps a cell talking to
  rauthy after the router is gone. The lazy form gives the same guarantee a
  caller can observe: no key set older than one interval is ever used, and a
  key rotated away verifies for exactly one interval. The cost is that a cell
  serving no traffic does not refresh, which is the case where it does not
  matter. The single-flight lock means concurrent unknown key ids buy one
  refresh between them, not one each. Rejected alternative: a background task
  with a shutdown channel, which is a lifecycle for the app to own.
- **D-5 (2026-09-07, build session; completes B-5's mechanics).** The
  bootstrap is create-then-update over rauthy's own client document.
  rauthy's create request carries five fields, and the flows, the challenge
  methods, and the algorithms B-5 fixes are only settable through the update,
  so the sequence is POST, read back, then PUT the document that came back
  with this spec's fields written over it. A field this crate has never heard
  of is sent back untouched, so a rauthy that grows one keeps it. The match
  is coverage rather than equality: an operator who added a second redirect
  URI or a third grant has widened the client deliberately, and the chassis
  only insists that what it needs is present. The secret is returned, never
  written: `/data/keys` is spec 031's. Rejected alternative: a typed mirror
  of rauthy's update request, which silently drops every field this crate
  does not model.
- **D-6 (2026-09-07, build session; bounds the proxy's own failure).** The
  proxy answers 502 with spec 020's envelope when rauthy cannot be reached,
  built in this crate rather than through `rahi-edge`. The layering names
  identity and the edge as peers and permits neither to depend on the other,
  and this is one condition with one status (`Error::Upstream`, which spec
  020 D-2 already maps to 502) rather than a second copy of that spec's
  table. The coupling is real and is written down here: if spec 020's table
  moves `Upstream` off 502, this follows. Rejected alternative: a
  `rahi-edge` dependency for one status, which makes the identity crate
  unusable without the edge.
- **D-7 (2026-09-07, build session; bounds B-3's model).** `Discovery` holds
  the six values this chassis uses (the issuer and the five endpoints B-3
  names) and ignores the rest of rauthy's document. A field nobody reads is a
  field nobody has to keep true, and every one of the six is required, so a
  document missing `jwks_uri` fails at the deserializer rather than at the
  first token. Rejected alternative: keeping the whole document, which makes
  every rauthy release a potential parse change for fields no caller wants.

## 8. Status

- **2026-09-07 (build session).** `implementation` stays `in-progress`:
  FR-004's integration run is written as a skip in both directions, not only
  when `RAHI_TEST_RAUTHY` is unset. Booting rauthy from a test needs what
  spec 031 builds and this spec does not own: rauthy's configuration file,
  its admin API key, and the key set under `/data/keys` that B-5's
  `admin_token` comes from. The skip says so where a reader will find it
  (`tests/discovery.rs`). Everything else in this spec is built and covered:
  B-1 through B-3 and B-5 through B-6 by unit tests, FR-001 by five proxy
  tests against a real loopback listener, FR-002 by the recorded documents,
  FR-003 by a key set that rotates under a held clock, and AC-1 passes with
  the integration run skipped, which is what AC-1 says it must do. What
  remains is FR-004's boot, and it is a human's call whether it lands here
  after spec 031 or moves into 031's own territory: writing it now would mean
  writing a container arrangement no session can execute.

## Verification

```verify:cli
cargo test -p rahi-idp --locked --test proxy
cargo test -p rahi-idp --locked --test discovery
```
