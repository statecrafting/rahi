---
id: "034-hello-cell"
title: "hello-cell: the reference app that composes every crate and proves the chassis end to end"
status: approved
kind: "feature"
domain: "edge"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: complete
risk: medium
wave: 3
depends_on:
  - "033-dev-substrate-and-harness"
  - "032-cluster-topology"
establishes:
  - "apps/hello-cell/Cargo.toml"
  - "apps/hello-cell/manifest.toml"
  - "apps/hello-cell/src/main.rs"
  - "apps/hello-cell/src/cell.rs"
  - "apps/hello-cell/src/notes.rs"
  - "apps/hello-cell/src/migrations.rs"
  - "apps/hello-cell/web/index.html"
  - "apps/hello-cell/web/app.js"
  - "apps/hello-cell/tests/e2e.rs"
  - "apps/hello-cell/tests/verify.rs"
  - "apps/hello-cell/README.md"
extends:
  - { spec: "010-workspace-and-core-types", unit: { kind: section, file: "Cargo.toml", anchor: "workspace.dependencies" }, nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/src/serve.rs", nature: additive }
  - { spec: "033-dev-substrate-and-harness", unit: "crates/rahi-harness/src/boot.rs", nature: additive }
  - { spec: "022-session-and-principal", unit: "crates/rahi-idp/src/envelope.rs", nature: additive }
  - { spec: "031-single-container-packaging", unit: "crates/rahi-ops/src/rauthy_env.rs", nature: additive }
  - { spec: "033-dev-substrate-and-harness", unit: "crates/rahi-harness/src/rauthy.rs", nature: additive }
  - { spec: "001-agentic-harness", unit: "spec-spine.toml", nature: additive }
summary: >
  The smallest complete governed cell and the thing consumers copy from: a
  manifest declaring one table, one kv prefix, and the grants a notes
  service needs; a Cell implementation whose main is one line; a notes
  resource with create, list, and delete behind login, each write staged
  with an outbox row and stamped with a revision, each denial ledgered; a
  static page that logs in through the proxy and lists notes; an operator
  route exposing the trace ring; and an end-to-end test through the
  harness that logs in, writes, reads, is denied an ungranted operation,
  verifies the ledger, takes a backup, and restores it into a fresh
  directory.
---

# 034: hello-cell

## 1. Purpose

Thesis §2 names this app as the proof. Every responsibility is exercised
by a path a reader can follow in a few hundred lines, and the end-to-end
test is the wave-3 exit condition. It is an example, not a template:
nothing stamps from it.

## 2. Territory

The whole of `apps/hello-cell`. It depends on the chassis crates and
`rahi-harness` (dev). The chassis does not depend on it.

## 3. Behavior

- **B-1 (manifest).** `manifest.toml` declares `app { name = "hello-cell"
  }`, `resources.tables = ["notes"]`, `resources.kv = ["hello:"]`,
  capabilities for `db.read`, `db.write`, `db.txn` on `notes`, `kv.get`
  and `kv.put` with `key_prefix = "hello:"`, `notify.publish`, and grants
  them to service `notes`; `auth.operator_role = "hello_operator"`. It
  deliberately omits `db.migrate` for `notes` so that the e2e test can
  show a denial.
- **B-2 (cell).** `struct HelloCell; impl Cell for HelloCell` returns the
  embedded manifest, one migration creating `notes (id, sub, body,
  revision, fence, created_at)`, the notes router, and an operator router
  exposing `GET /operator/traces`. `fn main() { rahi_cli::run(HelloCell) }`.
- **B-3 (notes).** `POST /api/notes` (authenticated) inserts a note for the
  principal's `sub` with `Watermark::next` and an outbox envelope in one
  `txn` through the governed store; `GET /api/notes` lists the principal's
  notes with `query`; `DELETE /api/notes/:id` deletes only the caller's.
  `POST /api/notes/migrate` calls a `db.migrate` facade and is therefore
  denied by the kernel with a ledgered Decision (the demonstration).
- **B-4 (page).** `web/index.html` and `app.js`: a login button to
  `/session/login`, a list of notes, an add form using the CSRF helper, a
  logout button. No framework.
- **B-5 (e2e).** `tests/e2e.rs` through the harness with
  `RauthyMode::External`: boot, log in, create two notes, list them, delete
  one, call the migrate route and assert 403 with a decision id, run `rahi
  ledger verify` and assert the denial is the last record, run `backup`,
  restore into a fresh data dir, boot again, and assert the remaining note
  and the same ledger head. Without rauthy the test runs the unauthenticated
  subset (probes, exposure table, denial via a test principal header the
  harness enables) and reports the skipped steps.
- **B-6 (exposure).** The README shows `exposure::report()` output for the
  app; the e2e test asserts every route is classified.

## 4. Functional requirements

- **FR-001.** `cargo run -p hello-cell -- --help` lists the composer's
  verbs.
- **FR-002.** `verify!` at build fails if a grant is removed from the
  manifest while the facade call remains (asserted by a `compile_fail`
  doc test).
- **FR-003.** The e2e test's ledger assertions pass with an independent
  `attest-ledger-cli verify` over the exported chain when the CLI is
  installed.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p hello-cell --locked` passes (rauthy-dependent
  steps skipped or passed, never failed for absence).
- **AC-2.** The end-to-end check is written into the README as a runnable
  operator procedure: `docker compose -f docker/compose.yml up`, open the
  public URL, see the page, complete a login through rauthy. Recording
  that procedure satisfies this criterion. Running it needs a rauthy
  image, so it is an operator check and never a `verify:cli` command.
- **AC-3.** `make ci` exits 0 with every crate and the app present, and
  `spec-spine index coverage --fail-on-untraced` exits 0 across the whole
  workspace.

## 6. Out of scope

Anything a real product does with the cell: hqgit and aicortex own their
own manifests, migrations, and routes in their own repositories.

## 7. Resolved decisions

- **D-1 (2026-09-05, human decision).** AC-2 is satisfied by recording the
  compose-and-login procedure in the README, not by executing it, on the
  same reasoning as 032 D-1: a build session has no rauthy image and no
  public origin, and a criterion no session can run must not hold the last
  spec in the chain at `implementation: in-progress`. AC-1 and AC-3 remain
  fully mechanical and are what a session proves.
- **D-2 (2026-09-10, build session; reads B-3, extends 030).** The
  composer's `serve` mounted the session router but never wrapped the
  application in spec 022's session layer, so every `Authenticated` route
  answered 401 to a logged-in user: the empty cell of spec 030 carries no
  such route, and this is the first app with one. `compose` now wraps
  the built router in `with_sessions` when rauthy is required, outermost,
  so the cookie is opened and the `Principal` is in the extensions before
  any route or the operator gate reads it. Additive on 030's unit; 030's
  text and 022's are unchanged.
- **D-3 (2026-09-10, build session; reads B-2).** The cell's migration
  list is spec 012's coordination migration at version 1 and the notes
  table at version 2: the store's own fence and outbox tables are applied
  by the app that composes it, as spec 012 provides for, and `Outbox::stage`
  in `create` needs the outbox table to exist. B-2 names one migration;
  the notes table is still the one migration the app declares.
- **D-4 (2026-09-10, build session; reads B-5, extends 022).** One boot in
  roughly fifty refused to serve with a session key of 31 bytes: the key
  file is 32 raw bytes and `SessionKey::load` trimmed trailing ASCII
  whitespace, so a key whose last byte happened to be a whitespace value
  lost it. A file of exactly the minimum length is now used whole; a
  longer, text-shaped key still loses its line ending. Additive on 022's
  unit, with a test that pins both readings.
- **D-5 (2026-09-10, build session; reads B-5, extends 033).** A restored
  volume reopened on other ports never elects: hiqlite binds a node to the
  addresses in its stored membership. A deployment's ports are fixed by
  contract (the container, the StatefulSet), so the harness gained
  `BootSpec::on_data_dir` and `BootSpec::with_ports`, and the reboot after
  restore pins the ports of the cell the archive came from; the direct
  read of the restored store does the same. The harness also gained
  `Instance::command` and `Instance::env`, so a test can run a verb on the
  stopped cell with the cell's own environment and nothing inherited
  (033 B-4). Additive on 033's unit.
- **D-6 (2026-09-10, build session; reads B-1, B-3, FR-002, spec 015
  B-3).** `verify!` reads every `Governed::new` literal in this crate, and
  the demonstration's `db.migrate` facade is by B-1 outside the ceiling,
  so a build check that demanded an empty report would refuse the cell B-3
  describes. The check in `tests/verify.rs` is exact instead: the verifier
  names exactly one site outside the ceiling, `(notes, db.migrate, notes)`,
  and nothing else; removing a grant while its call remains adds a second
  name and fails the build, which is what FR-002 asks. FR-002 says
  `compile_fail` doc test; the verifier walks source at test time, not
  compile time, so a `cargo test` is the mechanism and the requirement's
  text is left as written. Rejected: a `#[governed]` marker or a runtime
  triple for the migrate site, which the verifier refuses by design
  (015 D-6), and dropping the demonstration, which B-5 reads.
- **D-7 (2026-09-10, build session; reads B-2, B-6, spec 024 FR-003).**
  `GET /operator/exposure` renders `exposure::report()` behind the
  operator gate, next to `/operator/traces`. B-6 wants the report in the
  README and the table asserted by the e2e test; the table is published
  process-wide by the serving process, so the only faithful copy is one
  the cell serves, and the operator route is where a cell's internals are
  exposed (024 B-2). The e2e test logs a second user in with the
  `hello_operator` role, reads both, and prints the table the README
  carries.
- **D-8 (2026-09-10, build session; reads B-5, D-7, extends 031 and
  033).** rauthy keeps of a user's roles only those that already exist
  (`Role::sanitize`), so the harness's operator, granted `hello_operator`
  before any such role existed, logged in with no roles and met the
  gate's 403. The harness now creates a missing role before it grants it,
  and the bootstrap API key's access gains `Roles` read and create, on the
  same reasoning as 031 D-8 and 033 D-4: the key is widened by the spec
  that first presents it for the call, by re-rendering the environment.
  Additive on both units.
- **D-9 (2026-09-10, build session; reads B-5).** B-5's unauthenticated
  subset names "denial via a test principal header the harness enables".
  No such header exists: spec 033 established none, and a header that
  minted a principal would be a second principal authority beside the IdP
  (constitution VII, spec 022 B-1). Without rauthy the subset is the
  probes, the page, the metrics, the route classes as the edge answers
  them (401 ahead of adjudication, the operator gate closed), and the
  ledger verbs on the stopped volume; the ledgered denial is read on the
  rauthy path only, and the skipped steps are reported by name. B-5's
  text is left as written.
- **D-10 (2026-09-10, build session; reads AC-3, spec 001 D-11, spec 010
  D-3, extends 001).** The app's claimed files are bare `file` units and
  so in no content hash, exactly as the crates' sources are; `L-008`
  named every one. `[lint] unwitnessed_allowed` gains the app's Rust
  sources, manifest, page, and README on the reading spec 001 D-11
  recorded for `crates/**/*.rs`: the coupling gate and `require_ownership`
  are what defend them. `apps/.gitkeep`, which spec 010 D-3 committed so
  the `apps/*` member glob resolved while `apps/` was empty, and which
  spec 001 D-11 lists as the placeholder this spec deletes when it lands,
  is deleted here, with its claim removed from spec 010's `establishes`
  and its entry from the allowance. That is the one edit to a complete
  spec in this change, and it removes a claim on a file both specs said
  this spec would delete; no requirement of 010 changes.

## 8. Status

- **2026-09-10.** B-1 to B-6, FR-001 to FR-003, AC-1, and AC-3 hold;
  AC-2 per D-1. The full B-5 path (login, notes, the ledgered denial,
  backup, restore, reboot on the restored volume) was driven green
  against a native rauthy 0.36.0 with `RAHI_TEST_RAUTHY`; CI runs the
  unauthenticated subset.

## Verification

```verify:cli
cargo test -p hello-cell --locked
cargo run -p hello-cell --locked -- --help
```
