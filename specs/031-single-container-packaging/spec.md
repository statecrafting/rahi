---
id: "031-single-container-packaging"
title: "Single-container packaging: one image, one volume, first boot, keys, die-together supervision"
status: approved
kind: "tooling"
domain: "ops"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: complete
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
  - "docker/smoke.sh"
  - ".github/workflows/image.yml"
extends:
  - { spec: "030-operational-verbs", unit: "crates/rahi-ops/src/lib.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-ops/Cargo.toml", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/src/lib.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/src/serve.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/tests/cli.rs", nature: additive }
  - { spec: "010-workspace-and-core-types", unit: { kind: section, file: "Cargo.toml", anchor: "workspace.dependencies" }, nature: additive }
  - { spec: "025-api-tokens-and-resource-server", unit: "crates/rahi-idp/tests/bearer.rs", nature: additive }
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

- **D-2 (2026-09-09, build session; reads B-2 and B-3 against rauthy
  0.36).** rauthy is configured through a TOML file it insists exists, with
  every key overridable by an environment variable that wins over the file.
  `first-boot` therefore writes an empty `config.toml` beside the rendered
  `rauthy.env`, and `supervise` runs `rauthy serve -c <that file>` with the
  rendered pairs as the child's whole environment (plus `PATH`, `HOME`,
  `TZ`). The template is `docker/rauthy.env.template`, compiled into the
  binary with `include_str!` so the file in the tree and the file first
  boot writes cannot drift; placeholders are `${NAME}` and an unrendered one
  is a build defect. Three of rauthy's rules shape the values: `PUB_URL`
  carries no scheme, `RP_ORIGIN` needs an explicit port, and `HQL_NODE_ID`
  must be `1` (its default of `0` panics inside hiqlite). rauthy's own
  secrets (its encryption key, hiqlite secrets, bootstrap password, and API
  key secret) live in `keys/rauthy.json`, so a backup carries what decrypts
  rauthy's store (spec 030 B-5); the admin token spec 030 names is
  `<name>$<secret>`, the form rauthy's `BOOTSTRAP_API_KEY` and
  `BOOTSTRAP_API_KEY_SECRET` provision at rauthy's first start.
- **D-3 (2026-09-09, build session; two composition defects the first live
  boot found, fixed in spec 030's `serve.rs` under an `extends` edge).**
  The key set was fetched from `jwks_uri` as rauthy publishes it, on the
  public origin, which nothing inside the container can reach; the composer
  now rewrites it to the loopback base exactly as `Sessions::back_channel`
  rewrites every other back-channel call (spec 021 B-1). And the proxy
  router carries its full `/auth/...` paths, so nesting it under `/auth`
  doubled the prefix and every proxied request was a 404; it merges at the
  root and is named in the exposure table by prefix. Neither changes what
  spec 030 requires; both are what B-2 of 030 always meant.
- **D-4 (2026-09-09, build session; reads B-3's "when either exits").**
  `supervise` stops serve cooperatively rather than dropping its future: a
  dropped serve never shuts its hiqlite node down, the lock file outlives
  the process, and the next start refuses to open the store. The supervisor
  hands serve a stop signal, fires it when rauthy exits or when its own
  SIGTERM arrives, and waits up to fifteen seconds for serve to finish. On
  the propagated SIGTERM, serve stops first and rauthy is terminated
  second, with ten seconds before SIGKILL rather than B-3's five: rauthy's
  own graceful stop finishes its open connections and its Raft node and
  takes two to five seconds in practice, and five seconds belongs to the
  serve-failure path, where nothing is waiting on a clean stop. An
  orchestrator's termination budget must cover both (Kubernetes gives
  thirty seconds by default; `docker stop -t 30`). Verified by four
  consecutive restarts of one volume with no lock file left on either side.
  Two refinements from review: under the supervisor, serve reacts to the
  supervisor's stop signal only, so one SIGTERM has one listener and one
  ordered shutdown; and serve bounds its own drain at ten seconds, after
  which the listener is dropped and the node shut down regardless, so the
  fifteen seconds the supervisor waits always suffice.
- **D-8 (2026-09-09, build session; reads B-2's "a rauthy admin token").**
  The bootstrap API key is granted exactly what the built specs present it
  for: clients read, create, and update (spec 021 B-5) and secrets read
  (the supervisor's custody of the client secret). rauthy re-applies the
  access from `BOOTSTRAP_API_KEY` at every start, so a spec that needs more
  (033's harness administers users) widens the grant by re-rendering the
  environment rather than by this spec minting a broad key ahead of its
  use.
- **D-5 (2026-09-09, build session; reads B-3 with constitution IX).** The
  entrypoint runs `rahi migrate` between `first-boot` and `supervise`. In
  the single-container topology a container start is the deployment, so
  the deploy step and the start are the same moment; `serve` still refuses
  a store that is behind (spec 030 B-2), so the invariant that serve never
  migrates holds, and the empty cell's one migration is what proved a fresh
  container could not otherwise start. Spec 032's cluster runs the same
  verb as a Job before the rollout, and spec 033's harness must run it
  between `first-boot` and `supervise` as the entrypoint does.
- **D-6 (2026-09-09, build session; reads B-2's "do nothing else").** A
  present key set is verified and nothing is regenerated, but rauthy's
  empty config and rendered environment are written when absent: a volume
  restored from a backup (spec 030 B-6) carries the keys and not those two
  files, and without this a restore needed a second first boot on a
  non-empty key set, which B-2 forbids. FR-001's hash comparison still
  holds: a second run on a complete volume changes no file.
- **D-7 (2026-09-09, build session; reads B-4 and B-6).** The image is
  `debian:bookworm-slim` with the two binaries, `ca-certificates`, a
  non-root user, and `entrypoint.sh`; rauthy's release is taken from its
  published image by the digest of its multi-architecture index (the
  project ships no bare binary), which is the version and sha256 B-4 asks
  for, pinned in one `ARG`. The two architectures build on their own native
  runners rather than under QEMU, and are joined into one tag by digest on
  a version tag. The smoke test is `docker/smoke.sh`, one script the
  workflow and a developer run alike, so AC-2's local run and B-6's CI run
  are the same assertions: `/readyz` within ninety seconds, first boot and
  the client custody in the log, the issuer through the proxy equal to the
  public URL, and exit `0` on SIGTERM.

## 8. Status

- **2026-09-09.** B-1 to B-6, FR-001 to FR-003, FR-005, AC-1, and AC-2
  hold locally: `cargo test -p rahi-ops --locked --test first_boot` (4) and
  `--test supervise` (7) pass, `docker build -f docker/Dockerfile .`
  succeeds, and `docker/smoke.sh` passes against it (issuer
  `http://localhost:18443/auth/v1/` through the proxy, clean exit on
  SIGTERM), on this machine's architecture. FR-004, both architectures in
  CI, is asserted by `image.yml` on the first push to `main` that carries
  this spec; it is not something a build session can run before its merge.

## Verification

```verify:cli
cargo test -p rahi-ops --locked --test first_boot
cargo test -p rahi-ops --locked --test supervise
```
