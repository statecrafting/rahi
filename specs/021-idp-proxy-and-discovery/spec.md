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

None yet.

## Verification

```verify:cli
cargo test -p rahi-idp --locked --test proxy
cargo test -p rahi-idp --locked --test discovery
```
