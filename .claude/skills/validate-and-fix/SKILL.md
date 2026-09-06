---
name: validate-and-fix
description: "Run make ci (the exact gate CI runs) and fix what it surfaces by severity, with coupling failures and chassis-invariant changes escalated to a human."
allowed-tools: Bash, Read, Edit, Glob, Grep, Agent
---

# /validate-and-fix

Run the local CI loop and fix what it surfaces. `Makefile` is the single
source of truth for what CI validates (spec 001 B-2): if `make ci` passes
locally, `.github/workflows/govern.yml` passes too. Do not rediscover
validation commands by grepping manifests.

## 1. Run the composite

```sh
make ci
```

which runs, in order: `spec-spine compile`, `spec-spine index`,
`spec-spine lint --fail-on-warn`, `spec-spine index check`, `spec-spine
couple --base origin/main --head HEAD`, `scripts/spec-dag.sh`, `spec-spine
index coverage --fail-on-untraced`, and, once `Cargo.toml` exists, `cargo
build --workspace --locked`, `cargo test --workspace --locked`, `cargo
clippy --workspace --all-targets --locked -- -D warnings`, `cargo fmt --all
--check`, and `cargo deny check`. Run `git fetch origin main` first if the
coupling gate cannot find its base.

Capture full output (file paths, line numbers, messages) and categorize:

- **CRITICAL** (human decision, do not fix silently): a coupling failure
  (`C-001` drift or `C-002` unclaimed path) that would need an owning spec
  edited to clear; any change that relaxes a chassis invariant
  (`.claude/rules/chassis-invariants.md`); a `spec-dag` cycle; a rewritten
  fixture chain under `testdata/chains/`; an ambient input (clock, env,
  map order) reaching a hashed record or manifest.
- **HIGH**: test failures, build breaks, index staleness, a `C-002` whose
  remedy is claiming the file in the spec being implemented.
- **MEDIUM**: `spec-spine lint` warnings (the gate runs `--fail-on-warn`),
  clippy findings, type errors, `cargo deny` advisories with a fix.
- **LOW**: `cargo fmt`, wording in prose, minor cleanups.

If a check is missing from the composite, add it to `Makefile` and to the
workflow in the same change; never introduce a validation as a one-off
script.

## 2. Fix by phase

- **Phase 1, safe quick wins**: LOW and MEDIUM findings that cannot break
  anything. Verify each by re-running the narrowest target (`make fmt`,
  `make lint`, `cargo test -p <crate> --locked --test <file>`).
- **Phase 2, functionality**: HIGH findings one at a time; re-run the
  affected target after each. Never disable or skip a failing test; fix
  the cause.
- **Phase 3, critical**: present each CRITICAL finding with the evidence
  and a proposed remedy, then wait. Refusing the destructive step is
  sometimes the right answer (`.claude/rules/adversarial-prompt-refusal.md`).
  A fixture chain that stops verifying means the record encoding or the
  hash changed: that is a spec amendment and a human decision, never a
  regenerated fixture.
- **Phase 4, verification**: re-run `make ci` end to end.

## 3. Error handling

- **Rollback**: `git stash push -m "pre-validate-and-fix"` before any
  change; offer instant rollback if a fix regresses.
- **Partial success**: continue past a fix that fails; separate successes
  from failures; give manual instructions for what you could not fix.
- **Governed reads**: read `.derived/` only through `spec-spine`
  subcommands (`.claude/rules/governed-artifact-reads.md`).

## 4. Parallel execution

Launch several agents concurrently only for independent fixes in
different crates that touch non-overlapping files; keep ordered or
cross-crate changes sequential. Each agent verifies its own fix with the
narrowest target before reporting.

## 5. Final verification

Re-run `make ci`, confirm no new findings, and summarize:
`Fixed X/Y issues, Z require human decision. CI: {PASS|FAIL}`.

## Substrate notes

- `spec-spine lint` runs with `--fail-on-warn`: a warning is a failure.
- The coupling gate compares `HEAD` against `origin/main`; fetch first.
- The codebase index hashes more than `spec.md`: `spec-spine.toml
  [index] extra_hashed_inputs` lists `.claude/**`, `AGENTS.md`,
  `CLAUDE.md`, `Makefile`, `docs/design/**`, workflows, and standards.
  Editing any of them without regenerating the index fails the staleness
  check; the `Stop` hook regenerates it for you outside a rebase.
- `.claude/settings.json` and `.mcp.json` are hashed byte for byte:
  editor reformatting trips the gate even when the JSON is unchanged.
- The ownership ratchet is on: every source file inside a crate must be
  claimed by a spec (`spec-spine index coverage` shows the debt, always
  zero on a green tree).

## The rahi post-feature checklist

After feature work, beyond what the gates enforce:

- Every new `pub` item is named or covered in its spec's Behavior section.
- No `HashMap`, float, or `std::env` read reaches a hashed ledger record
  or manifest.
- New capability names and decision outcomes appear in the owning spec's
  Behavior section.
- New third-party crates are in `[workspace.dependencies]` and the spec
  declares the `extends` edge on spec 010's section.
- The spec's `## Verification` block still names a command that exists.
