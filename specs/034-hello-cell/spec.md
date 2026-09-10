---
id: "034-hello-cell"
title: "hello-cell: the reference app that composes every crate and proves the chassis end to end"
status: approved
kind: "feature"
domain: "edge"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: in-progress
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
  - "apps/hello-cell/README.md"
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

## Verification

```verify:cli
cargo test -p hello-cell --locked
cargo run -p hello-cell --locked -- --help
```
