# CLAUDE.md

Read `AGENTS.md` first: it carries the session protocol (`## New Sessions`)
and the backlog discipline (`## Working the backlog`). This file only holds
what Claude Code needs beyond it.

## What this is

rahi is the Rust chassis every statecrafting product stands on (hqgit,
aicortex): a set of library crates and one binary composer that give an
application identity (rauthy, same origin), replicated state (hiqlite,
in-process), a hash-chained decision ledger, a deny-by-default kernel, an
axum edge with probes, metrics, and tracing, and single-container packaging
with preflight, migrate, backup, and restore. An app is a binary that
composes the crates and declares a manifest; it extends the chassis and
never forks it. Read `specs/002-chassis-thesis/spec.md` for the seven
responsibilities, the crate topology, and the build order;
`docs/design/00-lineage.md` for what was carried from enrahitu and why.

The repository is specified before it is built. Every ordinary spec is
`approved` + `implementation: pending`, spec ordinals are the build order,
and code lands one spec per session. Before spec 010 lands there is no
`Cargo.toml`; every Makefile target and CI step is guarded for that.

## Commands

```sh
make gate       # read-only: spec-spine check --fail-on-warn, lint --fail-on-warn, index coverage --fail-on-untraced, couple, spec-dag
make refresh    # the writing half: spec-spine compile + index, for a session that can commit the shards
make ci         # make gate + the cargo gates (when Cargo.toml exists)
make build      # cargo build --workspace --locked
make test       # cargo test  --workspace --locked
make lint       # cargo clippy --workspace --all-targets --locked -- -D warnings
make fmt        # cargo fmt --all --check
make deny       # cargo deny check (when deny.toml exists)
make coverage   # spec-spine index coverage
make attest     # spec-spine attest --with-coupling -> .derived/attestation/
make verify SPEC=<id>         # one spec's declared acceptance (what the verify stage runs after merge)
scripts/spec-dag.sh           # depends_on is acyclic and only points to lower-numbered specs

# One crate, one test:
cargo test -p rahi-store --locked --test txn
```

Exit codes of `spec-spine`: `0` ok, `1` validation failure or drift, `2`
stale, `3` I/O, parse, schema, or config. The `rahi` binary (spec 030)
adopts the same four.

## Architecture in one screen

| Responsibility | Crates | Founding specs |
|---|---|---|
| types | `rahi-types` | 010 |
| store | `rahi-store` | 011, 012 |
| ledger | `rahi-ledger` | 013, 014 |
| kernel | `rahi-kernel` | 015 |
| edge | `rahi-edge` | 020, 023, 024 |
| identity | `rahi-idp` | 021, 022 |
| ops | `rahi-ops`, `rahi-cli`, `docker/`, `deploy/`, `rahi-harness` | 030, 031, 032, 033 |
| reference app | `apps/hello-cell` | 034 |

Dependencies point downward only: types, then store, then ledger and
kernel, then idp and edge, then ops and cli. An app depends on the chassis;
the chassis never depends on an app.

## Invariants that shape every change

- **One deployment unit.** One container, one volume, one origin. rauthy
  is co-deployed in the same unit and reached only through the app's
  origin.
- **The IdP is the principal authority.** rauthy's `sub` is the only
  principal id; no local account row; roles re-read on every renewal.
- **Two stores, never shared.** rauthy's hiqlite and the app's hiqlite are
  separate Raft clusters with separate data directories; app code never
  opens rauthy's.
- **One `txn` is the atomic unit.** A write and its outbox row commit
  together; notify is a hint outside it; the revision column is truth;
  nothing durable lives in the cache group.
- **Deny by default.** The manifest declares a ceiling, the build verifies
  observed usage is within it, the kernel enforces it, every denial is
  ledgered.
- **The chain is linear and verified at boot.** Append is a CAS on the
  unique parent index; integrity failure at init is process-fatal.
- **Backup is one artifact with its keys.** Restore is a cluster reset and
  is single-shot by construction.

## Governance mechanics

- Every source file inside a crate must be specifically claimed by a spec
  (`require_ownership` is on). Add new files to the implementing spec's
  `establishes` in the same change.
- `.derived/` shards are committed; regenerate with `spec-spine compile &&
  spec-spine index` and commit them with the change. `build-meta.json` is
  gitignored.
- Derived artifacts are read only through `spec-spine` subcommands.
- Hooks in `.claude/settings.json` recompile after spec edits, check
  staleness after hashed-input edits, block `gh pr create` on a red
  coupling gate, and block `git push` to `main`.
- The coherence guard: never edit an owning spec to make the gate pass on
  code that contradicts it. Surface the contradiction.

## House style

- No em dash character anywhere (chat, code, comments, specs, commits).
- Conventional commits with the spec id as scope: `feat(011): ...`.
- No AI attribution and no session links in commits, PR bodies, or comments.
- Specs follow `standards/spec/templates/spec-template.md`: Purpose,
  Territory, Behavior (B-n), Functional requirements (FR-nnn), Acceptance
  criteria (AC-n), Out of scope, Resolved decisions (D-n), `## Verification`.
