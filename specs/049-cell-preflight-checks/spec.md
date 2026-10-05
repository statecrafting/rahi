---
id: "049-cell-preflight-checks"
title: "Let a cell contribute named, bounded, read-only checks to preflight"
status: draft
kind: kernel
domain: ops
created: "2026-10-05"
authors: ["Bartek Kus"]
implementation: pending
risk: medium
wave: 3
depends_on:
  - "015-kernel-manifest-and-adjudication"
  - "030-operational-verbs"
  - "048-store-clean-shutdown"
establishes:
  - "crates/rahi-ops/src/preflight_app.rs"
  - "crates/rahi-cli/tests/preflight_app.rs"
extends:
  - { spec: "030-operational-verbs", unit: "crates/rahi-ops/src/preflight.rs", nature: amending }
  - { spec: "030-operational-verbs", unit: "crates/rahi-ops/src/lib.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/src/cell.rs", nature: additive }
  - { spec: "030-operational-verbs", unit: "crates/rahi-cli/src/lib.rs", nature: additive }
summary: >
  `rahi preflight` answers whether serve would start here, but only for the
  chassis: an application cannot report its own readiness facts or refuse a
  deployment whose application configuration the manifest ceiling does not
  cover. This spec adds a default-empty `Cell::preflight_checks` declaration
  of named checks that preflight runs after its own twelve, against a
  read-only view of the opened app store, the parsed manifest, and the same
  environment reader, each under a time bound. Results are reported as
  `app.<name>` lines with optional bounded report lines; any failing app
  check fails preflight with exit 1, and no app check can mask, reorder, or
  replace a chassis check.
---

# 049: Cell preflight checks

## 1. Purpose

Spec 030 B-3 makes `preflight` the operator's answer to "would `serve` start
here, and why not", without mutating anything. The answer stops at the
chassis. An application whose readiness depends on its own state (an active
model revision, a backlog of undelivered outbox rows, a dead-letter count)
or on its own configuration (a remote provider the manifest ceiling does not
grant) has no way to say so before `serve` runs, so the operator learns it
from the first failed request instead of from the verb built to tell them.

This spec gives the cell that seam without making the chassis understand the
application. It serves constitution XIII by putting application readiness on
the same named, diffable report as the chassis's, constitution XIV by letting
an app extend the verb without the chassis depending on the app, and the
deny-by-default anchor by letting an app refuse, at preflight, configuration
its own manifest ceiling does not cover. The first consumer is aicortex's
embedding pipeline; the contract is general.

## 2. Territory

This spec establishes `rahi-ops`'s public app-check types and runner
(`crates/rahi-ops/src/preflight_app.rs`) and the process-level fixture
(`crates/rahi-cli/tests/preflight_app.rs`). It amends spec 030's
`preflight::run` to accept a cell's declared checks and run them after the
chassis checks, and adds a `Verdict::Warn` variant. It extends `Cell` with an
empty-by-default declaration, and `rahi-cli` re-exports the types and passes
the cell's declaration to the verb.

It does not change the chassis checks, their names, their order, or their
conditions; it does not change `serve` or any other verb; it adds no
registry, no store API, and no kernel path. Metrics collectors are out of
scope: an application registers them through the existing
`rahi_edge::obs::current()` and `Obs::metrics().registry()` surface.

## 3. Behavior

- **B-1 (the declaration).** `Cell` MUST expose
  `preflight_checks() -> Vec<AppCheck>` with a default of an empty vector. An
  `AppCheck` MUST carry a name and one function from an owned
  `PreflightContext` to a `Send + 'static` future whose output is an
  `AppVerdict`. The declaration is an associated function, called without a
  store, so the check names are known before any check runs.
- **B-2 (a valid declaration).** A declaration MUST hold at most 32 checks,
  and each check name MUST match `[a-z][a-z0-9_]{0,47}` and be unique within
  the cell. Preflight reports app check `x` as `app.x`, so an
  app name can never equal or shadow a chassis check name. A declaration
  of more than 32 checks, or with an invalid or duplicate name, MUST produce exactly one line,
  `FAIL app: <reason naming the offending name, or the count>`, run none of the cell's
  checks, and fail preflight.
- **B-3 (the context).** `PreflightContext` MUST carry exactly: a read-only
  store view over the opened app store, exposing only spec 011's query
  reads (`query`, `query_consistent`, `query_paged`) and no write,
  transaction, migration, or shutdown path; the parsed `Manifest`; and the
  same `EnvReader` the verb was given, so the app reads its own
  configuration and Rahi interprets none of it. The view owns a clone of
  the store handle; app code MUST NOT mutate the volume, and preflight's
  "never mutates" (030 B-3) applies to the cell's checks as to the
  chassis's.
- **B-4 (the verdict).** An `AppVerdict` is one of `Pass`, `Warn`, or `Fail`,
  each with a one-line detail and zero or more report lines. `Warn` MUST NOT
  fail preflight. A `Fail` that the app builds with
  `AppVerdict::capability_missing(service, kind, resource)` MUST read
  `capability <kind> on <resource> for service <service> is not in the
  manifest ceiling`, so a provider absent from the ceiling is refused by a
  named capability error in one fixed form. Rahi does not decide which
  capabilities an app's configuration needs; the app asks
  `Manifest::covers` and reports.
- **B-5 (order and masking).** Chassis checks MUST be reported first, in
  spec 030's `CHECKS` order, with unchanged verdicts. App checks follow in
  declaration order. App checks run only when the `hiqlite` chassis check
  passed; otherwise every declared app check MUST be reported
  `SKIP app.<name>: skipped: the store did not open`. A failing chassis
  check fails preflight whatever the app checks report, and an app check
  MUST NOT be able to change, remove, or reorder a chassis line.
- **B-6 (bounds).** Checks run one at a time. Each MUST be bounded at five
  seconds and the app phase as a whole at thirty seconds, both measured by
  the verb. A check past its bound MUST be aborted and reported
  `FAIL app.<name>: timed out after 5s`; once the phase bound is spent, every
  remaining check MUST be reported `SKIP app.<name>: skipped: the app check
  budget is spent`. A panicking check MUST be reported
  `FAIL app.<name>: panicked`. Every app future MUST be completed or aborted,
  and its task joined, before preflight shuts the store down, so spec 048's
  invariant I-2 holds with the cell's checks in it.
- **B-7 (report shape).** Each app check prints one line,
  `<PASS|WARN|FAIL|SKIP> app.<name>: <detail>`, followed by each of its
  report lines as `  | <line>`. Rahi MUST replace control characters in a
  detail or report line with `?`, truncate each to 240 characters, and keep
  at most 32 report lines per check, printing `  | (<n> more lines
  omitted)` when it drops any.
- **B-8 (exit).** Preflight exits `0` when no chassis or app check failed
  and `1` otherwise, through spec 030's existing `Error::Validation` path.
  No new exit code is introduced.
- **B-9 (compatible absence).** A cell that does not override
  `preflight_checks` MUST produce output byte-identical to what it produced
  before this spec, and the same exit code.

## 4. Functional requirements

- **FR-001.** A fixture cell declares three checks: one passing with two
  report lines read from a row its migration seeded, one warning, one
  failing with `capability_missing`. Preflight prints the twelve chassis
  lines, then the three `app.` lines in order with their report lines, then
  `preflight: failed`, and exits 1.
- **FR-002.** The same cell with the failing check removed exits 0 with the
  warning printed.
- **FR-003.** A check that sleeps past five seconds is reported timed out,
  the following check still runs, and the verb's elapsed time stays under
  the phase bound plus the chassis checks' own time. A check that panics is
  reported `panicked` and the following check still runs.
- **FR-004.** A declaration with a duplicate name, an invalid name, or 33
  checks produces the single `FAIL app:` line and runs no check (each
  check's side channel in the fixture records zero calls).
- **FR-005.** With the store prevented from opening (a key set that does
  not read), every app check is reported skipped by name and none is
  called.
- **FR-006.** A detail containing a newline and a check returning 40 long
  report lines are printed sanitized, truncated, and capped as B-7 says.
- **FR-007.** After a preflight whose check timed out, the store's owner
  lock is released and a second preflight opens the store; the store's
  content is unchanged byte for byte under the view.
- **FR-008.** `EmptyCell` and an external-style cell implementing only the
  pre-049 methods produce the same preflight output as before this spec.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p rahi-cli --locked --test preflight_app` passes and
  covers FR-001 through FR-008 by driving the binary over argv.
- **AC-2.** `cargo test -p rahi-ops --locked` passes, including unit tests of
  name validation, sanitization, and the capability-missing wording.
- **AC-3.** The store view's public API, as compiled, offers no method that
  writes, migrates, or shuts down the store (a compile-fail or API-listing
  test proves it).
- **AC-4.** All existing `rahi-cli` preflight tests pass unmodified, proving
  B-9.

## 6. Out of scope

Machine-readable (`--json`) preflight output, which spec 030 does not have
for chassis checks either; parallel app checks; app checks in any verb
other than `preflight`; app checks that write, repair, or migrate; and
metrics collectors, which already register through
`Obs::metrics().registry()`. Interpreting application configuration,
including which remote providers an app uses, stays the application's.

## 7. Resolved decisions

- **D-1 (2026-10-05, this draft).** App check lines are namespaced `app.<name>`
  rather than refused on collision with a chassis name. Namespacing makes a
  collision impossible by construction, including with a chassis check a
  later spec adds; a refusal list would have to grow with `CHECKS`.
- **D-2 (2026-10-05, this draft).** Names are declared before any check runs,
  so a skipped check is still listed by name and two reports diff line for
  line, which is spec 030's stated reason for reporting skips.
- **D-3 (2026-10-05, this draft).** The context is a read-only query view, the
  parsed manifest, and the verb's `EnvReader`. Passing `StoreHandle` would
  hand app code a write path on a verb that never mutates; passing parsed
  app configuration would make Rahi interpret it.
- **D-4 (2026-10-05, this draft).** App checks run only when the store opened,
  and after every chassis check, rather than being interleaved. A chassis
  failure is never hidden behind app output, and app checks never run on a
  store preflight could not open.
- **D-5 (2026-10-05, this draft).** Sequential, five seconds each, thirty in
  all. Preflight holds the node under spec 048 B-7 for its duration; an
  unbounded app phase would make that hold unbounded.
- **D-6 (2026-10-05, this draft).** `Warn` is added to the shared `Verdict`
  so one report type carries both chassis and app lines. No chassis check
  emits it.
- **D-7 (2026-10-05, this draft).** `crates/rahi-cli/src/lib.rs` is also
  claimed by drafts 040, 041 and 047; the build session confirms the
  governance gate admits a co-claimed additive edit before touching it, or
  passes the declaration through the existing `Verb::Preflight` arm with
  the smallest diff the gate accepts.

## Verification

```verify:cli
cargo test -p rahi-cli --locked --test preflight_app
cargo test -p rahi-ops --locked
cargo test -p rahi-cli --locked
```
