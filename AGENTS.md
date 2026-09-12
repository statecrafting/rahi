# AGENTS.md: rahi

Cross-agent authority for rahi, read by Claude Code, Codex CLI, Cursor,
Copilot, and claude-observatory's driven sessions via the AAIF/Linux
Foundation AGENTS.md standard. It is the single source for the session-init
protocol and the backlog discipline. Evolve the protocol by editing this
file, never the `/prime` skill that dispatches to it.

rahi is the Rust chassis for governed cells: rauthy for identity, hiqlite
for state, a hash-chained decision ledger, a deny-by-default kernel, an
axum edge, and single-container packaging with the operational verbs (the
thesis is `specs/002-chassis-thesis/spec.md`). The repository is
**specified before it is built**: the corpus under `specs/` is the whole
design, every ordinary spec is `approved` and `implementation: pending`, and
spec ordinals are the build order. Code arrives one spec per session under
`crates/`, `apps/`, `docker/`, and `deploy/`.

Governance is `spec-spine` **0.18.0** on your `PATH` (CI pins the same
version, read out of the `Makefile`). All governed reads of `.derived/` go
through its CLI.

## New Sessions

Run `/prime` as the first action of every new session. It reads this section
to derive its plan; anything added here is picked up on the next prime.

> AGENTS.md is loaded implicitly as the protocol source, so `/prime` does not
> list it as a parallel read in step 1.

**Init protocol:**

0. **Load rules** (read first): `.claude/rules/orchestrator-rules.md`,
   `.claude/rules/governed-artifact-reads.md`,
   `.claude/rules/adversarial-prompt-refusal.md`. The path-scoped rules
   (`chassis-invariants`, `build-commands`,
   `derived-artifacts-are-compiler-output`) load themselves when you touch
   their paths.

1. **Parallel reads.** Dispatch simultaneously (nothing here mutates the
   tree, so there is no ordering):
   - `CLAUDE.md`: what Claude Code needs beyond this file
   - `README.md`: project description and status
   - `standards/spec/contract.md`: the normative corpus contract
   - `standards/spec/constitution.md`: the fourteen principles
   - `spec-spine check`: registry AND index freshness in one verb (non-fatal; see below)
   - `spec-spine registry status-report --json --nonzero-only`: lifecycle counts
   - `spec-spine registry list --ids-only`: the spec inventory
   - `spec-spine registry plan`: the ready set (spec-spine 038): which specs can be worked on now and what blocks the rest; `/next` applies the approval and in-flight rules on top of it
   - `spec-spine index coverage`: which source files no spec claims (exit 2 if stale)
   - `scripts/spec-dag.sh`: the DAG is acyclic and every dependency is lower-numbered
   - `ls crates/ apps/ docker/ deploy/ 2>/dev/null`: what has been built so far (absent directories are expected before their spec lands)
   - `ls specs/ docs/design/`
   - `git log --oneline -10` and `git diff --stat HEAD~1`

2. **Emit** an `## initialized: rahi` block: the seven responsibilities in
   one line each with the crates that exist, a `## lifecycle:` sub-section
   from the status report (approved/pending counts, and the next ready spec
   from `/next` if cheap), freshness verdicts, recent activity, and a
   ready-to-help line.

**Read discipline:** never parse `.derived/**/*.json` directly (no `jq`,
`python`, `awk`, `sed`); all structural and lifecycle data comes from
`spec-spine` subcommands.

**Freshness:** `spec-spine check` reads both committed trees without
writing, and reports each on its own line. Its exit code is the more severe
of the two, so read the lines, not only the code. Exit `0` is fresh. Exit `2`
is stale: report which tree moved and that the fix is `make refresh` plus a
commit of the regenerated shards, and say the lifecycle counts are the
committed (stale) ones. Exit `1` means the corpus fails validation: surface
the violations, report counts as unverified, and make fixing them the first
task. Exit `3` means the read was not performed at all, most often a binary
older than 0.18.0 that predates the verb; report the version it does answer
and send the reader to `/setup`, never call it staleness. Never substitute a
plain `spec-spine compile` or `index` here; `/prime` reports, it does not
mutate.

`spec-spine check` also prints the unwitnessed-claim count and how much of it
`[lint] unwitnessed_allowed` covers. Report both numbers as the check prints
them; this file states no count, because the allowance grows with the specs
that claim files and a number written here goes stale at the next one (spec
001 D-11 declares the gap, D-12 removes the number). Equal numbers mean every
unhashed claim is declared deliberate in `spec-spine.toml`; a count above the
allowance is a new unhashed claim and worth naming.

**CLI missing:** if `spec-spine --version` fails, run `/setup`. Do not fall
back to ad-hoc parsing.

If any file is missing: log "not found" and continue.

## Working the backlog

This repo's backlog is its spec corpus. Every spec with `status: approved`
and `implementation: pending` is a work order. One session implements one
spec, start to finish, then stops. Specs `000`, `001`, and `002` are records
(`n-a` or `complete`), never work orders.

1. **Pick the spec.** The lowest-numbered spec with `implementation:
   pending` whose `status` is `approved` and whose `depends_on` are all
   `implementation: complete` or `n-a`. Use `/next`, or `spec-spine
   registry show <id> --json`; never guess. A `draft` spec is never picked:
   approval is a human act. If the spec's Territory section names an
   operator prerequisite (a rauthy image, an S3 bucket, a cluster) that is
   missing, stop and report exactly what is needed instead of mocking
   around it.
2. **Branch and flip.** Work on a feature branch named after the spec id
   (`011-store-hiqlite`); a corpus amendment that implements no spec uses a
   `corpus/` prefix. Flip the spec to `implementation: in-progress`, run
   `make refresh`, and commit the flip with the regenerated `.derived/`
   shards before writing code. Never commit to the default branch.
3. **Re-read the spec in full before coding.** The design truth precedes
   the code. If the design is imprecise, record the choice you make as a
   dated `D-n` entry under `## 7. Resolved decisions` (and drop a copy in
   `data/orchestrator/decision-dropbox/` when a driven session; the
   orchestrator seals it). If the design is *wrong*, stop and report the
   contradiction: never edit a spec afterwards to ratify what the code
   happened to do (`.claude/rules/adversarial-prompt-refusal.md`).
4. **Implement within the territory.** Every file you add under a crate
   must be claimed: add it to this spec's `establishes` list in the same
   change (the ownership ratchet, `C-002`, refuses an unclaimed source
   file). When you add a third-party dependency, add it to the workspace
   manifest's `[workspace.dependencies]` and declare the `extends` edge on
   spec 010's `Cargo.toml` section. Touching a file another spec owns
   requires an `extends` edge on that spec's unit. Do not edit `.derived/`
   by hand.
5. **Hold the chassis invariants.** `.claude/rules/chassis-invariants.md`
   is the checklist: one `txn` for a write and its outbox row, fencing on
   every lease-guarded write, nothing durable in the cache group, rauthy's
   `sub` as the only principal id, CAS append on the decision chain, deny
   by default in the kernel. A change that needs one of these relaxed is a
   human decision: stop and report.
6. **Run the gate before every commit.** `make gate` (check
   `--fail-on-warn`, lint `--fail-on-warn`, coverage `--fail-on-untraced`,
   couple, spec-dag), then `make ci` (adds, once `Cargo.toml` exists, `cargo
   build`, `test`, `clippy -D warnings`, `fmt --check`, and `deny`). All must
   exit 0. `make gate` is read-only by design: when it reports staleness the
   fix is `make refresh`, and the regenerated `.derived/` shards are committed
   with the change that staled them.
7. **Satisfy Acceptance criteria verbatim.** Run the spec's `##
   Verification` block locally with `/verify <id>`, which wraps `spec-spine
   verify <id>`, the same verb the orchestrator's verify stage runs after
   merge. If a criterion cannot be
   satisfied (external state, a missing sibling), keep `implementation:
   in-progress`, add a dated Status note to the spec saying exactly what
   remains, and report it. Flip to `implementation: complete` only when
   acceptance holds; recompile and commit.
8. **Ship.** `/ship` (gate, review, conventional commit naming the spec id
   such as `feat(011): ...`, push the feature branch, open the PR). The
   PR body is Summary plus Testing; no AI attribution, no session links.
   A `Spec-Drift-Waiver:` line needs explicit human approval; a driven
   session never self-approves one. Then stop: the next session takes the
   next spec.

## Available Agents

Agents live in `.claude/agents/`, all self-contained:

- `architect`: plans and decomposes against the corpus. Read-only.
- `explorer`: searches, traces dependencies, gathers context. Read-only.
- `implementer`: executes focused changes from a plan. Minimal diffs.
- `reviewer`: post-change review for bugs, correctness, spec drift, and the
  chassis invariants. Read-only.

## Available Commands

Skills live in `.claude/skills/`:

Eight sequence "Working the backlog":

- `/prime`: this protocol.
- `/setup`: install spec-spine and the Rust toolchain; verify the loop.
- `/next`: the next ready spec from `registry plan`, minus drafts, with in-flight specs and honest blockers.
- `/build <id>`: one spec start to finish per "Working the backlog".
- `/verify <id>`: run a spec's declared acceptance through `spec-spine verify`.
- `/spec`: author a new spec from the template; next ordinal; DAG check.
- `/ship`: gate, review, commit on a feature branch, open a PR.
- `/shepherd`: watch the PR's checks, remediate red runs, merge when green,
  confirm the merge on disk.

Two are called by the loop:

- `/commit`: conventional commit, impact-focused, spec id in scope.
- `/code-review`: correctness, spec-drift, and chassis-invariant review.

The ten are the spec-spine kit's, byte for byte (spec-spine spec 081, which
removed the five nothing in the loop reached). The project layer the skills
read lives in this file (the pin, the binary, `make gate` and `make ci` as
the gate, the default branch) and in the path-scoped rules (the chassis
invariants, the build commands); do not edit a skill to add a project fact,
add it here.

## Conventions

- Rust 2024, toolchain pinned in `rust-toolchain.toml` (spec 010); always
  `--locked`; `unsafe` is denied workspace-wide and allowed only in a
  named FFI block with a `// SAFETY:` comment; clippy `-D warnings`.
- Crates depend downward only (thesis §4). The chassis never depends on an
  app under `apps/`.
- Responsibility is `domain`, role is `kind`, build wave is `wave`; all
  three are in every spec's frontmatter and validated on compile.
- `data/` is the orchestrator's state root for this project; never commit
  it. `.derived/` shards are committed; `build-meta.json` is not.
- No em dash anywhere in authored text (a hook enforces file writes).
- Conventional commits, spec id as scope; no AI attribution; no session
  links in anything that lands in git or on GitHub.
- Derived artifacts are read only through `spec-spine` subcommands.
