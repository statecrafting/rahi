---
name: setup
description: "One-time contributor setup for rahi: install spec-spine 0.14.0, the pinned Rust toolchain once rust-toolchain.toml exists, the optional cargo-deny, and verify the governed loop with make spine."
allowed-tools: Bash, Read
---

# /setup

Get a fresh clone operational. After this completes, `/init` can report
lifecycle and structural counts through the `spec-spine` binary, never by
ad-hoc parsing of `.derived/**/*.json`
(`.claude/rules/governed-artifact-reads.md`).

## Process

### 1. Install spec-spine 0.14.0

The version is pinned in `AGENTS.md` and in `.github/workflows/govern.yml`
(`SPEC_SPINE_VERSION`); the three must agree. Either route:

```sh
cargo install spec-spine-cli --version 0.14.0 --locked   # with a Rust toolchain
npm i -g spec-spine@0.14.0                               # prebuilt binary, no toolchain
```

Verify: `spec-spine --version` prints `spec-spine 0.14.0`. A different
version is a halt: CI runs the pinned one and a local pass on another
version proves nothing.

### 2. Rust toolchain (once spec 010 has landed)

`rust-toolchain.toml` arrives with spec 010 and pins the channel and
components. When it exists:

```sh
rustup show          # installs the pinned toolchain on first run
cargo --version
```

Before spec 010 there is no `Cargo.toml`; every `make` target is guarded
for that, and this step is skipped with a note.

Optional, but CI runs it and `make deny` skips without it:

```sh
cargo install cargo-deny --locked
```

`jq` is used by the hooks in `.claude/settings.json`; each hook exits 0
and says what it skipped when `jq` is absent (spec 001 FR-003), so it is
a convenience, not a prerequisite. Install it with your package manager.

### 3. Fetch the base ref

The coupling gate diffs against `origin/main`:

```sh
git fetch origin main
```

### 4. Verify the governed loop

`make spine` is the gate chain CI re-runs (spec 001 B-2): `spec-spine
compile`, `spec-spine index`, `spec-spine lint --fail-on-warn`,
`spec-spine index check`, `spec-spine couple --base origin/main --head
HEAD`, `scripts/spec-dag.sh`.

```sh
make spine
```

On a clean checkout `compile` and `index` are deterministic no-ops. If
`git status --short -- .derived/` shows a diff afterwards the committed
shards were stale: commit the regenerated shards (`chore(derived): ...`)
before doing anything else. Halt on the first failing target and surface
its output verbatim; do not continue past it.

Then the lifecycle reads `/init` will use:

```sh
spec-spine registry status-report --json --nonzero-only
spec-spine index coverage
```

### 5. Emit summary

Report exactly:

```
## setup: rahi

**spec-spine:** {0.14.0 / wrong version <v> / failed at <step>}
**Rust toolchain:** {<channel> from rust-toolchain.toml / not yet (spec 010 pending)}
**Optional tools:** cargo-deny {present/absent}, jq {present/absent}
**Governed loop (make spine):**
  - compile: {ok / failed}
  - index: {ok / regenerated, commit .derived/}
  - lint --fail-on-warn: {clean / N diagnostics}
  - index check: {fresh / stale}
  - couple: {clean / drift surfaced}
  - spec-dag: {acyclic, N specs / violation}
**Lifecycle:** {N specs across <statuses>}  (from registry status-report)
**Coverage:** {N claimed, M unclaimed}  (from index coverage)

Next: run `/init` to load full session context.
```

Do not invent counts. Only report values that came back from a
`spec-spine` subcommand or a `make` target.

## Rules

- The loop runs through the installed `spec-spine` binary on your `PATH`.
- Halt on first failure. Do not silently continue past a missing
  prerequisite or a failing gate.
- Never parse `.derived/**/*.json` directly in any verification step; use
  the `spec-spine` subcommands.
- Idempotent: safe to re-run.
