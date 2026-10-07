---
id: "048-store-clean-shutdown"
title: "Make every exit of a program that opened the app node pass through its shutdown"
status: approved
kind: kernel
domain: store
created: "2026-09-27"
authors: ["Bartek Kus"]
implementation: complete
risk: high
wave: 3
depends_on:
  - "011-store-hiqlite"
  - "030-operational-verbs"
  - "031-single-container-packaging"
  - "043-patched-dependency-adoption"
establishes:
  - "crates/rahi-store/src/lifecycle.rs"
  - "crates/rahi-store/tests/lifecycle.rs"
  - "crates/rahi-cli/tests/clean_shutdown.rs"
extends:
  - { spec: "011-store-hiqlite", unit: "crates/rahi-store/src/store.rs", nature: amending }
  - { spec: "011-store-hiqlite", unit: "crates/rahi-store/src/lib.rs", nature: additive }
  - { spec: "011-store-hiqlite", unit: "crates/rahi-store/Cargo.toml", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/src/serve.rs", nature: amending }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/src/lib.rs", nature: amending }
  - { spec: "030-operational-verbs", unit: "crates/rahi-ops/src/preflight.rs", nature: amending }
  - { spec: "039-release-and-out-of-tree-packaging", unit: "CHANGELOG.md", nature: additive }
references:
  - { unit: { kind: file, path: "docs/design/10-hiqlite-unclean-stop-upstream-issue.md" }, role: context }
  - { unit: { kind: file, path: "docs/design/evidence/048-probes/README.md" }, role: context }
  - { unit: { kind: file, path: "CHANGELOG.md" }, role: context }
obligations:
  - id: "I-1"
    kind: invariant
    text: "A program that opens the app node through Store::run shuts the node down before run returns or its panic resumes, whether the body returned Ok, returned Err, panicked, or was abandoned after a stop."
    anchor: "3-behavior"
  - id: "I-2"
    kind: invariant
    text: "No rahi verb that opens the app node returns between the open and the shutdown, and every such verb has SIGTERM and SIGINT armed before the open."
    anchor: "3-behavior"
  - id: "V-1"
    kind: verification
    text: "Real processes that error, panic, receive SIGTERM or SIGINT, ignore a stop, and are signalled from their first instant each leave a directory the next process opens; controls without the pattern leave the marker."
    anchor: "verification"
    inputs:
      - "cargo test -p rahi-store --locked --lib lifecycle"
      - "cargo test -p rahi-store --locked --test lifecycle"
      - "cargo test -p rahi-cli --locked --test clean_shutdown"
summary: >
  hiqlite creates an unclean-stop marker, state_machine/lock, when a node
  starts and removes it only in a graceful shutdown; without the auto-heal
  feature the next start refuses until someone repairs the directory by
  hand. The travel-memory service hit this with a ? between Store::open and
  Store::shutdown and a Ctrl-C-only handler. This spec gives rahi-store one
  scope for the node's lifetime, Store::run, which arms SIGTERM and SIGINT
  before the node starts and shuts it down on every way out of the body; it
  closes the three paths in rahi's own verbs that returned with the node
  open and the signal window in serve's boot; it documents the single-voter
  recovery and verifies it; and it records why auto-heal stays off after a
  probe showed its rebuild complete.
---

# 048: Clean shutdown of the app node

## 1. Purpose

hiqlite writes `state_machine/lock` into the app node's data directory when
the node starts and removes it only when `Client::shutdown` runs to its end.
The marker is how hiqlite knows the previous run may have applied writes past
the metadata it persists at shutdown. Without the `auto-heal` feature, which
rahi does not enable, a start that finds it refuses: hiqlite 0.14 panics with
"Lock file already exists ... needs manual interaction", and the patched 0.15
line rahi pins (spec 043) returns a typed refusal saying the directory "did
not stop cleanly". Either way an operator has to repair the volume.

A process skips the shutdown in three ordinary ways: an early return between
open and shutdown (the `?` operator), a panic, and a SIGTERM or SIGINT the
process has no handler for, whose default action ends it. The travel-memory
service (statecrafting/tm) did the first and the third: its `serve` used `?`
between `Store::open` and `Store::shutdown`, and it listened for Ctrl-C but not
for the SIGTERM a container stop sends. tm fixes its own code. This spec makes
the correct pattern the easy one for every program built on rahi, and holds
rahi's own binaries to it (constitution VIII, fail closed without leaving the
operator a trap; spec 031 D-4 already said "a lock file must never outlive the
process" for the supervisor).

## 2. Territory

New: `crates/rahi-store/src/lifecycle.rs` (`Store::run`, `Store::run_until`,
`Store::close_after`, `Stopping`, `stop_on_signal`, `STOP_GRACE`,
`is_stopped`) and two process-level test files.

Extended: `rahi-store`'s `store.rs` (the open path, the shutdown flag, the
`Drop` warning), its `lib.rs` (the module, the re-exports, and the crate
documentation of the pattern) and its `Cargo.toml` (tokio's `signal` and
`macros` features); `rahi-cli`'s `serve.rs` and `lib.rs` (when the signals are
armed, and how a verb's work is bounded); `rahi-ops`'s `preflight.rs` (one
early return); `CHANGELOG.md`.

hiqlite is not changed (D-7).

## 3. Behavior

- **B-1 (the obligation).** Every way out of a program that opened the app
  node with `Store::open` passes through `Store::shutdown` before the process
  ends, except the ways no program can run code on: SIGKILL, an abort, power
  loss. A marker left by one of those is recovered by B-8.
- **B-2 (the scope).** `Store::run(cfg, body)` arms SIGTERM and SIGINT
  (B-4), opens the node, calls `body(StoreHandle, Stopping)`, and shuts the
  node down when the body returns `Ok`, returns `Err`, panics, or is
  abandoned under B-3. It answers the body's error, or the stopped error, or,
  when the body succeeded, the shutdown's result. A panic resumes after the
  shutdown. A shutdown error behind a failed body is written to stderr, since
  the body's error is the one the caller acts on. `Store::run_until` takes
  the caller's `Stopping` instead of arming signals; `Store::close_after`
  gives the same guarantee to a store the caller opened itself.
- **B-3 (the grace).** A body is not cancelled the moment a stop is
  requested: it observes the request through `Stopping` and has
  `STOP_GRACE` (ten seconds, spec 031 D-4's drain budget) to return. A body
  still running at the bound is dropped at its current await, and the answer
  is `Error::Io` beginning `stopped:` (exit `3`), which `is_stopped`
  recognises. A stop requested before the body starts, including one that
  arrives while the node is opening, means the body is never polled.
  `Stopping::bounded` is this rule on its own.
- **B-4 (arming).** `stop_on_signal` installs the SIGTERM and SIGINT handlers
  when it is called, not when its future is first polled, so a signal that
  arrives while the node starts is recorded instead of ending the process.
  Once armed, the process does not terminate on either signal by default for
  the rest of its life; it stops by returning. A binary arms it; a library
  does not.
- **B-5 (no early return inside the chassis).** Where the chassis itself
  opens the node, nothing that can fail runs between the open and the
  scope that shuts it down without stopping the node first:
  `Store::open` stops the node when creating the chassis tables fails,
  `preflight` stops it when the opened node is not healthy, and
  `backup --to` parses its destination before the node opens.
- **B-6 (the safety net).** Dropping a node-owning `Store` on which
  `shutdown` was never called prints a warning on stderr naming the data
  directory and the consequence. It does nothing else: shutting down is
  async, and a `Drop` is not.
- **B-7 (rahi's binaries).** `serve` arms both signals at its entry, before
  the cell's gate, the stop record, and the open; a signal during boot is
  held until the listener is up and then stops serve in spec 043 B-9's order.
  `migrate`, `backup`, `ledger verify`, `ledger reindex` and `ledger export`
  arm them before the open and run their work under `Stopping::bounded`;
  the node is shut down on every way out, with spec 043 FR-009's rule that a
  verb's own result is its exit. `preflight` and `upgrade-cache` hold the
  node only for bounded steps, so they arm the signals and let the stop wait
  until they end. `supervise` already polled its own signal future before the
  in-process `serve` opened the node, and is unchanged.
- **B-8 (single-voter recovery).** When a marker was left by B-1's
  exception, the directory of a single voter is recovered, with no process on
  it, by moving `state_machine/db` and `state_machine/lock` aside (keeping
  them as evidence) and starting the node: hiqlite restores its latest
  snapshot and replays the Raft log into a new database, and its
  startup-recovery gate keeps health false until the replay is applied.
  Restoring a backup remains the alternative. The crate documentation states
  this procedure.

## 4. Functional requirements

- **FR-001.** A process whose body returns an error after writing exits with
  that error's code, leaves no marker, and the next process reads the write.
- **FR-002.** A process whose body panics after writing leaves no marker and
  the next process reads the write.
- **FR-003.** A process that receives SIGTERM or SIGINT while its body
  watches `Stopping` finishes the body's final write and exits `0`; one whose
  body ignores the stop exits `3` no earlier than `STOP_GRACE` after the
  signal; one whose stop precedes the open never runs its body. None leaves
  a marker.
- **FR-004.** A process sent SIGTERM every 25 ms from the instant it is
  spawned leaves a directory the next process opens. Two controls show what
  the tests detect: a program with a `?` between open and shutdown, and a
  program with no handler that receives SIGTERM, each leave the marker, and
  the next open refuses with "did not stop cleanly". After the first control,
  B-8's procedure makes the next open succeed with the write intact.
- **FR-005.** `rahi serve` sent SIGTERM while it boots exits `0`, and the next
  `serve` starts and stops cleanly.
- **FR-006.** `rahi ledger verify` and `rahi migrate` sent SIGTERM every
  25 ms from spawn leave a volume the next `serve` starts on, and
  `rahi backup --to s3://` refuses with exit `1` without opening the node.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p rahi-store --locked --test lifecycle` passes,
  covering FR-001 to FR-004 against real processes.
- **AC-2.** `cargo test -p rahi-store --locked --lib lifecycle` passes,
  covering B-3's rules with a paused clock and B-2's panic capture.
- **AC-3.** `cargo test -p rahi-cli --locked --test clean_shutdown` passes,
  covering FR-005 and FR-006; the same file fails against the `rahi-cli`
  sources before this spec (recorded in D-5).
- **AC-4.** The workspace gate and `make ci` pass.

## 6. Out of scope

- Enabling hiqlite's `auto-heal` (D-3), and any automatic recovery.
- A panic inside a `rahi` verb's own work: the verbs use `Stopping::bounded`
  and spec 043's `stop_after`, not `Store::close_after`, because a verb's
  shutdown failure is a warning (043 FR-009); a panicking verb is a defect
  to fix, not a stop to shape.
- SIGKILL, abort, and power loss (B-1's exception): B-8 is their recovery.
- The legacy fence's creation on a fresh volume (D-6).
- Changing hiqlite (D-7).

## 7. Resolved decisions

- **D-1 (2026-09-27, owner).** The owner commissioned this work on
  2026-09-27 with the travel-memory incident as its cause, fixing its scope:
  audit rahi's binaries and fix any that leave the marker; give the API a
  structured shutdown, with a logging `Drop` allowed only as a safety net;
  evaluate `auto-heal` for a single voter without enabling it by default and
  record the decision; test an erroring and a SIGTERMed program; document the
  pattern in the crate and the changelog; leave hiqlite unchanged and draft,
  not file, any upstream issue. That commission is this spec's approval.
- **D-2 (2026-09-27, build session).** The scope is a function taking a
  closure rather than a guard object, because shutdown is async and a guard's
  `Drop` cannot await it. The body gets a `StoreHandle`, not `&Store`, so its
  future borrows nothing the scope must outlive. A stop leaves the body a
  grace instead of cancelling it at once, so one API serves a server that
  drains and a batch job that does not look; ten seconds is 031 D-4's drain
  budget and fits Kubernetes's default thirty-second grace with the
  fifteen-second shutdown wait (`SHUTDOWN_WAIT`) behind it. A second signal
  during the grace does not shorten it: the bound is already short, and a
  second path would be a second policy. The stopped error is `Error::Io`
  (exit `3`, the interrupted infrastructure code) with a `stopped:` prefix,
  following `is_timeout` and `is_terminal`, rather than a new variant of the
  workspace `Error`.
- **D-3 (2026-09-27, build session).** `auto-heal` stays off. The probe in
  `docs/design/evidence/048-probes/auto-heal/` SIGKILLed a single voter on
  `hiqlite-patched =0.15.0-patched.3` with `auto-heal`, snapshotting every 50
  entries, and restarted it: after 600 writes (log 602, snapshot 599, purged
  598), after kills at a snapshot boundary (10 runs) and 25 entries past one
  (5 runs), and before any snapshot, the rebuild held every acknowledged
  write with none skipped or applied twice, at the first read. The rebuild
  is complete for that shape. It stays off anyway: a Cargo feature enabled by
  `rahi-store` is enabled for every consumer and cannot be turned off per
  cell, so "not by default" can only mean "not in rahi"; an automatic rebuild
  would hide the defect this spec removes the causes of; the rebuild's cost
  grows with the store; and the probe does not cover a multi-voter cluster, a
  torn snapshot, or a full disk. The same probe on upstream `hiqlite =0.15.0`
  also converged, but upstream reported the node healthy before the replay
  was applied: the first read after a rebuild held 21 to 25 of 30
  acknowledged rows in four of seven pre-snapshot runs, and 419 of 425 in
  one of seven mid-write runs. The fork's recovery gate is what makes B-8
  safe on rahi's pin.
- **D-4 (2026-09-27, build session).** The audit of B-7's binaries found four
  paths that left the marker: `serve` armed SIGTERM only when axum's graceful
  shutdown first polled its future, after the open, the chain verification
  and the bind; `backup --to` parsed its destination with `?` after the open;
  `preflight` returned the health check's error with `?` and dropped the open
  node; and `Store::open` returned the chassis tables' error with `?` after
  the node started. Every node-owning verb also had no handler at all.
  `supervise` was not affected: its signal future is polled from the first
  `select!`, before Rauthy is ready and so before `serve` opens the node.
- **D-5 (2026-09-27, build session).** `crates/rahi-cli/tests/clean_shutdown.rs`
  was run against the `rahi-cli` sources before this spec: all three tests
  failed. `serve` died by the signal (exit status `None`); `backup --to
  s3://` left the marker, with B-6's warning printed; and the burst test's
  verb was killed in its first milliseconds, before any handler, which is D-6.
- **D-6 (2026-09-27, build session; recorded, not changed).** The pre-spec
  burst run killed a first `ledger verify` on a fresh volume between
  `create_legacy_fence` creating `<data>/hiqlite/state_machine/` and
  publishing the fence marker in it. Every later start then refused the
  volume as "written by a pre-043 rahi" (`state_machine/lock is absent`).
  With B-7 a SIGTERM there is held, so this spec closes the path it opened,
  but SIGKILL or power loss at the same instant still leaves a volume only an
  operator can clear. The fence is spec 043's, and whether an empty
  `state_machine/` with no marker and no database may be treated as absent is
  a judgment about pre-043 volumes this spec does not make. It is reported to
  the owner.
- **D-6a (2026-10-05, owner decision).** D-6's question is answered:
  an empty `state_machine/` with no marker, no database and nothing beside
  it is read as absent and fenced at the next start. Spec 043 D-32 records
  the decision and its mechanism.
- **D-7 (2026-09-27, build session).** The marker's behavior should change
  upstream: hiqlite 0.14.0 and 0.15.0 both `panic!` inside `start_node*` on a
  stale marker without `auto-heal`, choose the rebuild only at compile time,
  and 0.15.0 reports health before the rebuild's replay is applied (D-3).
  `docs/design/10-hiqlite-unclean-stop-upstream-issue.md` is the proposed
  issue for `sebadob/hiqlite`, with this spec's evidence. It is not filed;
  filing is the owner's act.
- **D-8 (2026-09-27, build session).** tm pins rahi's 0.14 line (hiqlite 0.14,
  `sebadob/hiqlite@8f3b9bd`), where a stale marker panics in `Store::open`.
  `Store::run` lands on the 0.15-patched line, where it is a typed refusal;
  the pattern, not the refusal's form, is what a consumer adopts when it bumps
  rahi, and the changelog says so.
- **D-9 (2026-09-27, build session).** Under the full workspace suite a
  child of `tests/lifecycle.rs` twice failed to bind a port the shared
  allocator had just handed its parent ("Address already in use"), once on a
  first start and once on the reader. A start that loses its port opens and
  writes nothing, so the test starts it again, up to five times; any other
  failure still fails. The child also stopped calling `common::config`,
  which claimed two ports it then discarded. The allocator itself (spec 011
  D-13) is not changed here.

## Verification

```verify:cli
cargo test -p rahi-store --locked --lib lifecycle
cargo test -p rahi-store --locked --test lifecycle
cargo test -p rahi-cli --locked --test clean_shutdown
```
