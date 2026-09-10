---
id: "033-dev-substrate-and-harness"
title: "The dev substrate and the harness: compose for N=1, boot the real binary, wait on /readyz, a cookie jar"
status: approved
kind: "tooling"
domain: "ops"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: complete
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
extends:
  - { spec: "031-single-container-packaging", unit: "crates/rahi-ops/src/rauthy_env.rs", nature: additive }
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

- **D-1 (2026-09-10, build session; reads B-2, FR-003).** `BootSpec`
  carries `manifest_dir`, but a cell's manifest is compiled into its
  binary (spec 030 `Cell::manifest`), so the harness cannot break it. The
  field is the child's working directory, for a cell that resolves files
  relative to it, and FR-003 is exercised with a configuration the binary
  refuses at start (`RAHI_RAUTHY_MODE=bogus`, through `BootSpec::env`):
  the boot fails inside the budget and the error carries the binary's
  stderr, which is what the requirement is for. Rejected: a fixture
  binary with a broken manifest, which would make this crate depend on
  the chassis it must not depend on.
- **D-2 (2026-09-10, build session; reads B-2).** The boot runs
  `first-boot`, then `migrate`, then `supervise` or `serve`: the
  container's entrypoint order (spec 031 B-3, D-5). B-2 names no
  `migrate`, but `serve` refuses a store behind its cell's migrations
  (spec 030), so a harness that skipped it could boot only a cell with
  none.
- **D-3 (2026-09-10, build session; reads B-2).** The public origin is
  `http://localhost:<port>`, with the listener on `127.0.0.1` at that
  port, because rauthy derives its WebAuthn relying-party id from the
  public host and refuses an IP literal (`Invalid webauthn.rp_id`, found
  with a live rauthy). Same shape as the compose file's origin.
- **D-4 (2026-09-10, build session; reads B-3, FR-002).** The harness
  administers users with the bootstrap API key, which spec 031 D-8 left
  for this spec to widen: `Users` read, create, and update join the
  rendered `BOOTSTRAP_API_KEY` access (an `extends` edge on 031's unit),
  and rauthy re-applies it at every start. The empty cell (spec 030)
  mounts no protected route, so FR-002's read as the principal is
  asserted on what every cell carries: the callback that issued the
  session exchanged the code as that user, and a logout with the session
  is honoured. The read of an app route as the principal is hello-cell's
  end-to-end test (spec 034), this crate's declared first consumer.
  Rejected: an admin session, because rauthy forces MFA on admin sessions
  by default and the harness must not weaken the rendered configuration
  to log in.
- **D-5 (2026-09-10, build session; reads FR-002, AC-1).** FR-002 runs
  only with `RAHI_TEST_RAUTHY` naming a rauthy binary, and there is none
  on a macOS host. It was driven green here through a wrapper that runs
  `serve -c <config>` in the pinned image with the rendered environment
  forwarded and the listeners opened to the container network; that
  wrapper is session tooling, not part of the tree. On a Linux host the
  binary is the image's `/app/rauthy`.

## 8. Status

- **2026-09-10.** B-1 to B-5, FR-001 to FR-003, AC-1, and AC-2 hold.
  `cargo test -p rahi-harness --locked` passes: `tests/boot.rs` boots the
  empty cell of spec 030 with `RauthyMode::None`, asserts liveness never
  answered later than readiness and that `boot` returned after `/readyz`
  was `200` (FR-001), that `stop()` leaves no child process, that a
  refused configuration fails inside the budget with the binary's stderr
  (FR-003, per D-1), and that the client carries the cookie jar and the
  CSRF proof. FR-002 (`with_a_rauthy_at_hand_a_user_logs_in_and_reads_a_protected_route`)
  was driven green against the pinned rauthy image through the wrapper
  D-5 describes: the user is created with the widened API key (D-4), the
  proof of work is solved, rauthy's `202` carries the callback, the cell
  exchanges the code and issues its session cookie, and a logout with it
  is honoured. `docker compose -f docker/compose.yml config` validates
  (AC-2). The workspace's tests, clippy, fmt, and deny pass. Not
  exercised here: FR-002 on a Linux host with the binary itself, and the
  `develop.watch` rebuild loop, which needs a running daemon and an
  editing developer.

## Verification

```verify:cli
cargo test -p rahi-harness --locked
```
