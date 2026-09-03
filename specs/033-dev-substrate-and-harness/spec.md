---
id: "033-dev-substrate-and-harness"
title: "The dev substrate and the harness: compose for N=1, boot the real binary, wait on /readyz, a cookie jar"
status: approved
kind: "tooling"
domain: "ops"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: pending
risk: medium
wave: 3
depends_on:
  - "031-single-container-packaging"
establishes:
  - "docker/compose.yml"
  - "crates/rahi-harness/Cargo.toml"
  - "crates/rahi-harness/src/lib.rs"
  - "crates/rahi-harness/src/boot.rs"
  - "crates/rahi-harness/src/client.rs"
  - "crates/rahi-harness/src/cookies.rs"
  - "crates/rahi-harness/src/rauthy.rs"
  - "crates/rahi-harness/tests/boot.rs"
summary: >
  Two things a contributor needs: a compose file that runs the N=1 topology
  (the image with a bind-mounted volume, watched rebuilds) and a harness
  crate that boots a built cell binary under a throwaway data directory
  with fresh keys and OS-allocated ports, starts a rauthy on loopback when
  one is available, waits on /readyz and never on /healthz, and hands back
  an HTTP client with a cookie jar so authenticated flows are testable.
  One instance per test file, never per test. Carries enrahitu://033.
---

# 033: The dev substrate and the harness

## 1. Purpose

enrahitu's harness found real defects immediately and then hid one for six
weeks by waiting on liveness instead of readiness (enrahitu://033 §3.1).
The rahi harness waits on `/readyz` from its first version and records why.
It is a separate crate with no compile-time dependency on the chassis so
that it can drive any built cell, including an app in another repository.

## 2. Territory

The compose file and the whole of `crates/rahi-harness`. The reference
app's end-to-end test (034) is its first consumer.

## 3. Behavior

- **B-1 (compose).** `docker/compose.yml` builds the image from
  `docker/Dockerfile`, mounts `./.data` at `/data`, sets
  `RAHI_PUBLIC_URL=http://localhost:8080`, publishes one port, and uses
  `develop.watch` to rebuild on source changes. `docker compose up` is the
  whole developer setup.
- **B-2 (boot).** `Harness::boot(BootSpec { binary, manifest_dir,
  rauthy: RauthyMode }) -> Result<Instance>` creates a temp data dir, runs
  `first-boot`, allocates free ports for the app and both hiqlite
  instances, spawns `supervise` (with `RauthyMode::External(path)` or
  `RauthyMode::None`, in which case `serve` is spawned directly and login
  tests skip), and polls `/readyz` until 200 or a ninety-second budget.
  `Instance` exposes `base_url`, `admin_token`, `data_dir`, and `stop()`.
- **B-3 (client).** `Instance::client() -> Client` is a reqwest client with
  a cookie jar and a helper that fetches the CSRF cookie and sets the
  header on non-safe methods. `Instance::login_as(user) -> Result<()>`
  drives the authorization-code flow against rauthy's login form when
  rauthy is present (creating the user through the admin API first).
- **B-4 (environment).** The child inherits nothing from the test runner
  except `PATH` and `HOME`; the harness sets every `RAHI_*` variable it
  needs explicitly.
- **B-5 (cost stated).** Boot takes seconds (two elections); the crate
  docs say one instance per test file in a `OnceLock`, and `Instance` is
  `Send + Sync` for that purpose.

## 4. Functional requirements

- **FR-001.** `tests/boot.rs` boots the `rahi-cli` fixture cell with
  `RauthyMode::None`, asserts `/healthz` answers before `/readyz`, that
  `boot` returns only after `/readyz` is 200, and that `stop()` leaves no
  child process.
- **FR-002.** With `RAHI_TEST_RAUTHY` set, the same test uses
  `RauthyMode::External`, creates a user, logs in, and reads a protected
  route as that principal.
- **FR-003.** A boot with a deliberately broken manifest fails inside the
  budget with the binary's stderr in the error.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p rahi-harness --locked` passes.
- **AC-2.** `docker compose -f docker/compose.yml config` validates
  (skipped in `verify:cli` when docker is absent).

## 6. Out of scope

Browser-driven tests (the orchestrator's `verify:browser` blocks); load
testing.

## 7. Resolved decisions

None yet.

## Verification

```verify:cli
cargo test -p rahi-harness --locked
```
