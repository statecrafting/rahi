---
id: "031-single-container-packaging"
title: "Single-container packaging: one image, one volume, first boot, keys, die-together supervision"
status: approved
kind: "tooling"
domain: "ops"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: in-progress
risk: critical
wave: 3
depends_on:
  - "030-operational-verbs"
establishes:
  - "crates/rahi-ops/src/first_boot.rs"
  - "crates/rahi-ops/src/keys.rs"
  - "crates/rahi-ops/src/supervise.rs"
  - "crates/rahi-ops/src/rauthy_env.rs"
  - "crates/rahi-ops/tests/first_boot.rs"
  - "crates/rahi-ops/tests/supervise.rs"
  - "docker/Dockerfile"
  - "docker/entrypoint.sh"
  - "docker/rauthy.env.template"
  - ".github/workflows/image.yml"
extends:
  - { spec: "030-operational-verbs", unit: "crates/rahi-ops/src/lib.rs", nature: additive }
  - { spec: "001-agentic-harness", unit: ".github/workflows/govern.yml", nature: additive }
summary: >
  One container plus one volume is a complete authenticated application. The
  image carries the app binary and the rauthy release binary. First boot
  generates every key (ledger, session, backup, rauthy ENC_KEYS, the admin
  token), writes rauthy's environment derived from one public URL, and
  bootstraps the OIDC client; a restart regenerates nothing. The app is the
  supervisor: it starts rauthy on loopback, waits for its health, starts
  serve, and when either exits the other is stopped and the container
  exits. The volume layout is /data/hiqlite, /data/rauthy, /data/keys, and
  /data/backups. Carries enrahitu://007.
---

# 031: Single-container packaging

## 1. Purpose

Constitution VI. The die-together supervisor is in Rust because the
container has no other runtime; the entrypoint shell script only execs the
binary. First boot is a verb so that it is testable without a container.

## 2. Territory

Four modules and two tests in `crates/rahi-ops`, the `docker/` directory,
and the image workflow. Extends 030's `lib.rs` and the CI workflow (the
image job runs after the cargo gate).

## 3. Behavior

- **B-1 (volume).** `/data/hiqlite` (the app's Raft state), `/data/rauthy`
  (rauthy's, owned by the rauthy process, never opened by the app),
  `/data/keys` (mode `0700`; files `0600`), `/data/backups`. One mount.
- **B-2 (first boot).** `rahi first-boot` (idempotent, run by the
  entrypoint before `supervise`): if `/data/keys` is empty, generate the
  ledger Ed25519 key, the session HMAC key, the backup age key, rauthy's
  `ENC_KEYS` and `ENC_KEY_ACTIVE`, and a rauthy admin token; write
  `/data/rauthy/rauthy.env` from `docker/rauthy.env.template` with the
  public URL derivations (`PUB_URL`, `RP_ID`, `RP_ORIGIN`, `PROXY_MODE` or
  `COOKIE_MODE`, hiqlite ports `8100`/`8200`, loopback bind); print the
  admin credentials exactly once to stdout. If keys exist, verify their
  modes and do nothing else.
- **B-3 (supervise).** `rahi supervise`: spawn rauthy with its env, poll
  its health on loopback for up to sixty seconds, then run `serve` in the
  same process. If rauthy exits, stop serving and exit with rauthy's code;
  if serve fails, SIGTERM rauthy, wait five seconds, SIGKILL, and exit with
  serve's code. SIGTERM to the supervisor propagates to both. The client
  bootstrap (021 B-5) runs after rauthy is healthy and before serve.
- **B-4 (image).** A multi-stage `Dockerfile`: build the workspace with
  `--locked --release`, copy the binary and the pinned rauthy release
  binary (version and sha256 in the Dockerfile), a non-root user, `/data`
  as the only volume, port `8443` (or the configured one) exposed, no
  shell beyond `entrypoint.sh`. `linux/amd64` and `linux/arm64`.
- **B-5 (one input).** `RAHI_PUBLIC_URL` is the single required
  environment variable; everything else has a default or is generated.
- **B-6 (image workflow).** `.github/workflows/image.yml` builds the image
  on `main` and tags, runs a smoke test (start with a temp volume, wait for
  `/readyz`, fetch discovery through the proxy, assert the issuer), and
  pushes to the registry on tags only.

## 4. Functional requirements

- **FR-001.** `first-boot` in a temp dir creates every key with the right
  mode and a valid `rauthy.env`; a second run changes no file (hash
  compare).
- **FR-002.** `supervise` with a stub child that exits after two seconds
  stops serve and exits with the child's code; SIGTERM reaches both (a
  test with a stub that records signals).
- **FR-003.** `rauthy.env` for an `http` public URL contains
  `COOKIE_MODE=danger-insecure` and for `https` contains
  `PROXY_MODE=true`.
- **FR-004.** The image smoke test in CI passes on both architectures.
- **FR-005.** The smoke test of B-6 boots rauthy in the running container,
  bootstraps the OIDC client (021 B-5), fetches discovery through the
  proxy, and asserts the issuer is the public URL. This is spec 021's
  FR-004, moved here by D-1; 021 owns the proxy and the discovery client,
  this spec owns the only place either can be exercised against a live
  rauthy.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p rahi-ops --locked --test first_boot` and
  `--test supervise` pass.
- **AC-2.** `docker build -f docker/Dockerfile .` succeeds and the smoke
  test of B-6 passes locally (skipped in `verify:cli` when docker is
  absent), including the discovery assertion of FR-005.

## 6. Out of scope

N=3 topology and key injection across replicas (032); the compose file
for development (033).

## 7. Resolved decisions

- **D-1 (2026-09-07, corpus amendment; inherits 021's FR-004).** Spec 021's
  FR-004 required an integration test that boots rauthy on loopback,
  bootstraps the client, fetches discovery through the proxy, and asserts
  the issuer. Booting rauthy needs three things 021 does not own and this
  spec builds: rauthy's configuration file (B-2), its admin token
  (B-2, from `/data/keys`), and the container the two processes share
  (B-3, B-4). The requirement is therefore this spec's, and it arrives as
  FR-005. It costs nothing to carry: B-6's smoke test already fetched
  discovery through the proxy and asserted the issuer, so FR-005 names an
  assertion the smoke test was going to make anyway and binds it to the
  spec that needed it.

  Spec 021 was held `in-progress` for this one requirement while every
  other claim in it was built and covered, and specs 022, 024, 030, and
  this one sit behind 021 in the declared DAG, so the requirement's
  misplacement stalled the whole of waves 2 and 3. The two specs' own
  status notes named the contradiction and left it for a human, which is
  what the coherence guard asks; this entry is that decision. Spec 021 is
  flipped to `implementation: complete` in the same change. Rejected
  alternative: leaving FR-004 in 021 and holding that spec open until this
  one merges, which keeps a spec in flight for a year of build order and
  blocks the four specs behind it for the same reason.

## Verification

```verify:cli
cargo test -p rahi-ops --locked --test first_boot
cargo test -p rahi-ops --locked --test supervise
```
