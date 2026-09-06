---
id: "001-agentic-harness"
title: "Agentic engineering harness: session protocol, skills, agents, hooks, gate"
status: approved
kind: "governance"
domain: "governance"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: complete
risk: high
wave: 1
depends_on:
  - "000-rahi-bootstrap"
establishes:
  - "AGENTS.md"
  - "CLAUDE.md"
  - "Makefile"
  - "spec-spine.toml"
  - ".mcp.json"
  - "standards/spec/contract.md"
  - "standards/spec/templates/"
  - ".claude/settings.json"
  - ".claude/agents/"
  - ".claude/rules/"
  - ".claude/skills/"
  - ".github/workflows/govern.yml"
  - ".github/dependabot.yml"
  - "scripts/verify-spec.sh"
  - "scripts/spec-dag.sh"
summary: >
  The governed-development loop every human and every driven session runs
  inside: the cross-agent New Sessions protocol and the Working the backlog
  protocol in AGENTS.md, the Claude Code skills (init, setup, next, build,
  verify, spec, commit, code-review, ship, shepherd, validate-and-fix,
  cleanup, implement-plan, research, refactor-claude-md), the four pipeline
  agents, the standing and path-scoped rules (including the chassis
  invariants checklist), the hooks that keep the derived artifacts fresh and
  block an ungated PR, the Makefile that is the one source of truth for what
  CI validates, and the CI workflow that re-runs the same gate. The harness
  is what makes the corpus buildable by claude-observatory.
---

# 001: Agentic engineering harness

## 1. Purpose

The corpus is only as buildable as the loop that builds it. This spec owns
that loop so that a change to how sessions are steered (a skill, a hook, a
rule, the CI gate) is a governed change coupled to a spec, never an
uncommitted habit. It is the hqgit harness (its spec 001) carried over with
one substitution: the two hqgit domain specialists and their rules are
replaced by one path-scoped chassis-invariants rule that the reviewer agent
and the code-review skill apply.

## 2. Territory

`AGENTS.md` (the cross-agent protocol authority), `CLAUDE.md` (what Claude
Code needs beyond it), `Makefile` (the CI composite), `spec-spine.toml`,
`.mcp.json`, the contract and template under `standards/spec/` (the
constitution itself is in the bypass floor and is amended only by a spec
that `amends` it), the whole `.claude/` harness, the CI workflow, dependabot
config, and the two helper scripts. The build session for any later spec is
granted authority to append a dated D-n note to this spec when it must
adjust a hook or a Makefile target to make its own territory buildable; it
may not change the protocol's substance without an amendment.

## 3. Behavior

- **B-1 (AGENTS.md is the protocol).** `AGENTS.md` carries a `## New
  Sessions` section that `/init` executes verbatim, and a `## Working the
  backlog` section that the orchestrator extracts verbatim into every build
  prompt. Both are edited in `AGENTS.md`, never duplicated into a skill.
- **B-2 (the gate is one composite).** `make spine` runs `spec-spine
  compile`, `spec-spine index`, `spec-spine lint --fail-on-warn`,
  `spec-spine index check`, `spec-spine couple --base origin/main --head
  HEAD`, and `scripts/spec-dag.sh`. `make ci` runs `make spine`, `spec-spine
  index coverage --fail-on-untraced`, and, whenever `Cargo.toml` exists,
  `cargo build --workspace --locked`, `cargo test --workspace --locked`,
  `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo
  fmt --all --check`, and `cargo deny check` when `deny.toml` exists. Every
  target is guarded so the composite is green on the specify-only tree.
- **B-3 (CI is the same gate).** `.github/workflows/govern.yml` runs on
  pull requests: `spec-spine compile --check`, `index check`, `lint
  --fail-on-warn`, `couple` with the PR body as waiver source, `index
  coverage --fail-on-untraced`, the cargo gates when a workspace exists,
  and `spec-spine attest --with-coupling` uploaded as a build artifact. It
  pins `spec-spine` to the version named in `AGENTS.md`.
- **B-4 (hooks).** `.claude/settings.json` wires: `SessionStart` (report
  registry and index freshness), `PostToolUse` on `Edit|Write` (recompile
  after a spec edit; staleness check after any hashed-input edit),
  `PreToolUse` on `Bash` (block `gh pr create` unless the coupling gate is
  green or a `Spec-Drift-Waiver:` is inline in the body; block `git push`
  to the default branch), and `Stop` (auto-regenerate a stale index outside
  a rebase or merge). Permissions allow the read-only git verbs, `cargo`,
  `make`, and `spec-spine`; they deny publishing and destructive `gh`
  verbs.
- **B-5 (skills).** `.claude/skills/` ships fifteen skills: `/init`,
  `/setup`, `/next`, `/build <id>`, `/verify <id>`, `/spec`, `/commit`,
  `/code-review`, `/ship`, `/shepherd`, `/validate-and-fix`, `/cleanup`,
  `/implement-plan`, `/research`, `/refactor-claude-md`.
- **B-6 (agents).** Four pipeline agents: `architect`, `explorer`,
  `implementer`, `reviewer`. The reviewer applies the chassis-invariants
  rule to any diff under the store, ledger, identity, or kernel crates.
- **B-7 (rules).** Three standing rules (orchestrator, governed artifact
  reads, adversarial prompt refusal) and two path-scoped rules
  (`chassis-invariants` on `crates/rahi-{store,ledger,idp,kernel}/**`,
  `build-commands` on `crates/**`, `apps/**`, `docker/**`, `deploy/**`).
- **B-8 (house style).** No em dash anywhere; conventional commits naming
  the spec id (`feat(011): ...`); no AI attribution; no session links in
  commits, PR bodies, or comments.

## 4. Functional requirements

- **FR-001.** `scripts/spec-dag.sh` reads `spec-spine registry list --json`
  (a typed read), refuses any `depends_on` cycle naming the path, refuses a
  dependency on a higher-numbered spec, and refuses a dependency on an
  unknown id. Exit 0 clean, 1 on a violation, 3 when spec-spine is absent.
- **FR-002.** `scripts/verify-spec.sh <id>` extracts every `verify:cli`
  fenced block from `specs/<id>/spec.md`, runs each non-comment line in
  order from the repo root, prints command and exit code, and exits non-zero
  on the first failure; a spec with no `## Verification` section exits 0
  and prints `not-declared`.
- **FR-003.** Every hook exits 0 when `spec-spine` or `jq` is absent,
  printing what was skipped, so a missing tool never blocks a session.
- **FR-004.** The `PreToolUse` PR gate refreshes the index before coupling
  and blocks when `.derived/` is left uncommitted by that refresh.

## 5. Acceptance criteria

- **AC-1.** `make spine` exits 0 on the specify-only tree (zero packages,
  every owning unit `W-001`).
- **AC-2.** `scripts/spec-dag.sh` exits 0 on this corpus.
- **AC-3.** `scripts/verify-spec.sh 001-agentic-harness` runs this spec's
  block below and exits 0.

## 6. Out of scope

The orchestrator itself (claude-observatory owns its stages); the Rust
toolchain pins and lints (spec 010); the container image workflow (spec 031
extends `.github/workflows/`).

## 7. Resolved decisions

D-1 (2026-09-03, authoring). The observatory's build stage runs only the
spec-spine commands as its post-session gate on a Rust target (it gates bun
commands on a root `tsconfig.json`), so cargo correctness reaches the
pipeline through `make ci` inside the session and the CI workflow that
shepherd watches. This repo therefore never places a `tsconfig.json` or a
`package.json` at the root; there is no npm anywhere in it.

D-2 (2026-09-03, authoring). The constitution is in spec-spine's bypass
floor and is deliberately not claimed here: it changes only through a spec
that `amends` it.

D-3 (2026-09-03, authoring). The hqgit `ledger-guardian` and
`trust-reviewer` agents were not carried. rahi's invariants are fewer and
checkable by a rule; a specialist agent per crate would cost a subagent
spawn per review for a checklist the reviewer can hold.

D-4 (2026-09-05, first CI runs). B-3's "cargo gates when a workspace
exists" is guarded by an output of the `spine` job, not by `hashFiles` in
a job-level `if`. GitHub allows `hashFiles` only inside a step (a
job-level `if` is evaluated before any checkout), and the workflow fails
at startup with `calling function "hashFiles" is not allowed here`, which
reports as a run with no checks rather than as a failed gate; every
govern run on this repository failed that way until this fix. The `spine`
job probes for `Cargo.toml` and `deny.toml` after its checkout and
publishes `has_cargo` and `has_deny`; the `cargo` and `deny` jobs gate on
those. The guard's meaning is unchanged. The same review dropped the
dependabot `npm` entry for `/web`, which D-1 already rules out and which
failed on every scheduled run. Both were learned from hqgit's spec 001
D-3, where the identical workflow first hit the failure.

D-5 (2026-09-06, first multi-repo session). B-4's hooks resolve the repo
they act on from the action, not from the session. Both began with
`cd "${CLAUDE_PROJECT_DIR:-.}"`, which binds a hook to the session's
project: with sibling checkouts open, an edit under another repo
recompiled and staleness-checked this one, and a `git push` or `gh pr
create` aimed at a sibling was judged against this repo's branch and
coupling state. The staleness hook now derives the root from the edited
file (`git -C "$(dirname "$fp")" rev-parse --show-toplevel`); the push and
PR gate honours an explicit `cd <dir>` prefix in the command and otherwise
its own `cwd`; and both exit quietly unless `$root/specs` exists, so a
non-corpus repo is never gated. The same fix made the PR gate read-only.
It ran `spec-spine index`, a write, into the tree it was judging, then
blocked on the uncommitted `.derived/` its own write had just produced;
that is unrecoverable from inside the hook and mutates a tree another
session may be mid-build in. It now runs `index check` and reports a stale
index for the session to regenerate and commit. B-4's meaning is
unchanged.

## Verification

```verify:cli
scripts/spec-dag.sh
scripts/verify-spec.sh 000-rahi-bootstrap
make spine
```
