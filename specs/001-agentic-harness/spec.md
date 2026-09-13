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
  - ".github/workflows/ci.yml"
  - ".github/workflows/govern.yml"
  - ".github/dependabot.yml"
  - "CODEOWNERS"
  - ".gitattributes"
  - ".githooks/"
  - "scripts/spec-dag.sh"
summary: >
  The governed-development loop every human and every driven session runs
  inside: the cross-agent New Sessions protocol and the Working the backlog
  protocol in AGENTS.md, the Claude Code skills (the eight that sequence the
  backlog protocol, prime, setup, next, build, verify, spec, ship, shepherd,
  and the two it calls, commit and code-review), the four pipeline agents,
  the standing and path-scoped rules (including the chassis invariants
  checklist), the hooks that report derived-artifact freshness and block an
  ungated PR, the Makefile that is the one source of truth for what CI
  validates, and the CI workflow that re-runs the same gate. The harness
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
that `amends` it), the whole `.claude/` harness, the two CI workflows and the
dependabot config, `CODEOWNERS`, `.gitattributes`, the `.githooks/` merge
driver, and `scripts/spec-dag.sh`. The build session for any later spec is
granted authority to append a dated D-n note to this spec when it must
adjust a hook or a Makefile target to make its own territory buildable; it
may not change the protocol's substance without an amendment.

## 3. Behavior

- **B-1 (AGENTS.md is the protocol).** `AGENTS.md` carries a `## New
  Sessions` section that `/prime` executes verbatim, and a `## Working the
  backlog` section that the orchestrator extracts verbatim into every build
  prompt. Both are edited in `AGENTS.md`, never duplicated into a skill.
- **B-2 (the gate is one composite, and it never writes).** `make gate`
  runs `spec-spine check --fail-on-warn`, `spec-spine lint --fail-on-warn`,
  `spec-spine index coverage --fail-on-untraced`, `spec-spine couple --base
  $(BASE) --head HEAD`, and `scripts/spec-dag.sh`. Every verb is read-only:
  a gate that repairs the tree it is judging passes unconditionally, so
  staleness is reported and never fixed in place. `make refresh` is the
  separate writing half (`spec-spine compile`, `spec-spine index`) that a
  live session runs before committing the shards it regenerated. `BASE`
  resolves the default branch from the repository rather than assuming
  `origin/main`. `make ci` runs `make gate` and, whenever `Cargo.toml`
  exists, `cargo build --workspace --locked`, `cargo test --workspace
  --locked`, `cargo clippy --workspace --all-targets --locked -- -D
  warnings`, `cargo fmt --all --check`, and `cargo deny check` when
  `deny.toml` exists. Every target is guarded so the composite is green on
  the specify-only tree.
- **B-3 (CI is the same gate, behind one check).**
  `.github/workflows/ci.yml` is the only workflow in the gate chain with event
  triggers (`pull_request`, `push` to the default branch, and `merge_group`); a
  release workflow that publishes no PR status check is outside this chain and
  keeps its own triggers (spec 031's `image.yml`). It runs
  the cargo gates when a workspace exists, the supply-chain gate when
  `deny.toml` exists, and calls `.github/workflows/govern.yml`, a reusable
  workflow (`on: workflow_call`) that runs `spec-spine check
  --fail-on-warn`, `lint --fail-on-warn`, `couple` with the PR body as waiver
  source and both endpoints as the event's frozen SHAs, `index coverage
  --fail-on-untraced`, `scripts/spec-dag.sh`, and `spec-spine attest
  --with-coupling` uploaded as a build artifact. `jobs.ci-gate` (`needs`
  every other job, `if: always()`) is the **single** status check branch
  protection requires: it fails when any job reports `failure` or
  `cancelled`, and counts a skipped job as a pass, so a conditional gate
  cannot leave a required check permanently unreported. Whichever gates a run
  did not exercise are announced as notices, so green never silently means
  "never ran". CI pins `spec-spine` to `SPEC_SPINE_VERSION` in the `Makefile`,
  which is the one place the pin is stated.
- **B-9 (derived-artifact merge hygiene).** `.gitattributes` normalizes text
  to LF on checkout and assigns the sharded `.derived/` globs to the
  `spec-spine-derived-regen` merge driver; `.githooks/merge-derived-index.sh`
  is that driver and `.githooks/enable-merge-driver.sh` registers it in a
  clone. Registration lives in `.git/config`, which is not committed, so the
  driver is opt-in per clone and the CI staleness gate stays the source of
  truth. `CODEOWNERS` names a reviewer for the corpus, the standards, the
  harness, and everything that runs with a token.
- **B-4 (hooks).** `.claude/settings.json` wires: `SessionStart` (report
  registry and index freshness through `spec-spine check`), `PostToolUse` on
  `Edit|Write` (recompile after a spec edit; freshness check after any
  hashed-input edit), `PreToolUse` on `Bash` (block `gh pr create` unless the
  freshness read answered fresh, `.derived/` is committed, and the coupling
  gate is green or a `Spec-Drift-Waiver:` is inline in the body; block a
  `git push` that would update the default branch, which a tag push does
  not), and `Stop` (report a stale tree, never regenerate one). Every hook
  resolves the repository the command acts on and the binary that governs it
  ($SPEC_SPINE_BIN, then that repository's own release build, then PATH), and
  the PR gate reads `check`'s four exit codes distinctly, so a binary too old
  to answer is reported as a read that did not happen rather than as
  staleness. Permissions allow the read-only git verbs, `cargo`, `make`, and
  `spec-spine`; they deny publishing and destructive `gh` verbs.
- **B-5 (skills).** `.claude/skills/` ships ten skills, the closed graph
  the protocol reaches: eight that sequence "Working the backlog" (`/prime`,
  `/setup`, `/next`, `/build <id>`, `/verify <id>`, `/spec`, `/ship`,
  `/shepherd`) and the two that graph calls (`/commit`, `/code-review`).
  Each is byte-identical to the spec-spine kit's copy; a project fact belongs
  in `AGENTS.md` or a path-scoped rule, never in a skill.
- **B-6 (agents).** Four pipeline agents: `architect`, `explorer`,
  `implementer`, `reviewer`. The reviewer applies the chassis-invariants
  rule to any diff under the store, ledger, identity, or kernel crates.
- **B-7 (rules).** Three standing rules (orchestrator, governed artifact
  reads, adversarial prompt refusal) and three path-scoped rules
  (`chassis-invariants` on `crates/rahi-{store,ledger,idp,kernel}/**`,
  `build-commands` on `crates/**`, `apps/**`, `docker/**`, `deploy/**`, and
  `derived-artifacts-are-compiler-output` on `.derived/**`). The scoped
  reinforcement never replaces the standing rule it echoes: reaching for
  `jq` instead of a subcommand is a mistake whose whole shape is not opening
  a shard, so `governed-artifact-reads` stays unconditional.
- **B-8 (house style).** No em dash anywhere; conventional commits naming
  the spec id (`feat(011): ...`); no AI attribution; no session links in
  commits, PR bodies, or comments.

## 4. Functional requirements

- **FR-001.** `scripts/spec-dag.sh` reads `spec-spine registry list --json`
  (a typed read), refuses any `depends_on` cycle naming the path, refuses a
  dependency on a higher-numbered spec, and refuses a dependency on an
  unknown id. Exit 0 clean, 1 on a violation, 3 when spec-spine is absent.
- **FR-002.** `spec-spine verify <id>` is the one implementation of the
  verification protocol: it extracts every `verify:cli` fenced block from
  `specs/<id>/spec.md`, runs each non-comment line in order from the
  repository root, stops at the first non-zero exit, and reports
  `not-declared` as an honest zero for a spec with no `## Verification`
  section. `make verify SPEC=<id>` and `/verify <id>` both wrap that verb,
  and the orchestrator's verify stage runs the same one after merge. This
  repository ships no second implementation of it.
- **FR-003.** Every hook exits 0 when `spec-spine` or `jq` is absent,
  printing what was skipped, so a missing tool never blocks a session.
- **FR-004.** The `PreToolUse` PR gate refreshes the index before coupling
  and blocks when `.derived/` is left uncommitted by that refresh.

## 5. Acceptance criteria

- **AC-1.** `make gate` exits 0 on the specify-only tree (zero packages,
  every owning unit `W-001`) and leaves the working tree unmodified.
- **AC-2.** `scripts/spec-dag.sh` exits 0 on this corpus.
- **AC-3.** `spec-spine verify 001-agentic-harness` runs this spec's block
  below and exits 0.
- **AC-4.** Branch protection on `main` requires exactly one check, `ci-gate`,
  and additionally sets: signed commits required, linear history required,
  enforcement for administrators, force pushes and deletions refused. This
  asserts a repository setting rather than a property of the checkout, so it is
  verified with `gh api repos/statecrafting/rahi/branches/main/protection`, not
  in the block below.

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

D-6 (2026-09-06, the stall before 016). B-4's Stop hook reports a stale
codebase index; it does not regenerate one. It used to run `spec-spine
index`, a write, and then print "review git diff .derived/ and include it
in your commit" to a session that had already stopped. Nothing committed
the result, so the tree was left dirty. claude-observatory's build stage
refuses to start on an unclean tree and the refusal pauses the run. The
pause is not terminal on its own: standby re-drives a project when its
corpus signature changes (that repo's D-4), and the run resumes once the
precondition clears. What turns it into a stall is that nothing can clear
it. Only a session or a human cleans the tree, and a session only starts on
a clean tree, so the pipeline cannot recover from dirt it produced itself.
The same dirt also short-circuits the daemon's checkout normalization, so
the tree cannot heal itself back to the default branch. After spec 015
merged and verified, rahi sat paused for eleven hours with the daemon
healthy, logging "driving rahi, paused, flight slot released, idle,
rescan"; it resumed on its own within minutes of the tree being made clean
again. The hook now
runs `index check` and prints the command to run, which lets a stale index
reach CI as a gate failure fixed deliberately: the same read-only stance
D-5 gave the PR gate. It also exits quietly outside a spec corpus, so a
non-corpus project never sees the message.

D-7 (2026-09-06, pin bump). The `spec-spine` pin moves from 0.11.0 to
0.14.0 in every site that states it (`govern.yml`, `AGENTS.md`, `README.md`,
`/setup`, the architect agent). The corpus was verified byte-compatible
first: 0.14.0's `compile --check` and `index check` both report fresh
against shards written by 0.11.0. The bump matters here more than
anywhere: spec-spine 044 was written for this corpus's `approved` +
`in-progress` state, which 0.11.0 treats as not in flight, so every
not-yet-written unit of the spec under build was a hard error for the
whole build window. Also gained: `registry plan` (038, which `/next`
reimplemented in Python), `--json` verdicts (037), `layout.state_dir`
(039, the declared home for `data/` instead of `resolver_exclusions`),
and the 046 kit hooks, which are the D-5/D-6 fixes ported upstream.
`spec-spine index` now prints the `W-001` warnings it always recorded; on
a pending spec that is one line per not-yet-written unit and is expected.
Follow-ons (`registry plan` in the init reads, `state_dir`, retiring the
Python in `/next`) are their own change.

D-8 (2026-09-06, kit adoption). The fifteen skills under `.claude/skills/`
are the spec-spine kit's own (spec-spine spec 048), taken byte for byte,
and the three standing rules are the kit's spec 047 text. The kit's hooks
are the ones this repository wrote under D-5 and D-6, ported upstream as
spec-spine spec 046 and returned here with two additions: the `specs`
guard on `SessionStart`, and the "a waiver is a human instrument"
sentence in the PR gate's refusal. B-4's meaning is unchanged. The kit
moved every project fact out of the skills into `AGENTS.md` and the
path-scoped rules, which this repository already held (`make spine`,
`make ci`, the 0.14.0 pin, `chassis-invariants`), so nothing was lost in
the swap and a future kit update is a copy. In substance: `/next` wraps
`spec-spine registry plan` and drops the Python readiness script (the D-7
follow-on), `/spec` derives the ordinal from the registry and the enums
from `spec-spine.toml`, `/code-review` uses `compile --check` so a review
never writes, `/commit` carries the session-link and em-dash bans, and
`scripts/verify-spec.sh` is the kit's copy, which also accepts a numbered
`## N. Verification` heading.

D-9 (2026-09-07, pin bump). The `spec-spine` pin moves from 0.14.0 to
0.15.0 in every site that states it (`govern.yml`, `AGENTS.md`,
`README.md`, the architect agent); `/setup` states the pin as `<pin>`
since D-8 and needs no edit. The corpus was verified byte-compatible
first: under 0.15.0 `compile --check`, `index check`, `lint
--fail-on-warn`, `index coverage --fail-on-untraced`, and `couple` all
pass against shards written by 0.14.0, with no shard rewritten. The
fifteen kit skills, the three standing rules, and `scripts/verify-spec.sh`
were diffed against the v0.15.0 kit and are byte-identical, so D-8's "a
future kit update is a copy" held with nothing to copy; the four agent
files differ only where this repository localized the kit's placeholders,
which is what D-8 intended. Gained: `spec-spine verify <id>` (spec-spine
spec 049), the verb that retires the `verify-spec.sh` three adopters each
wrote, and `index diagnostics` with `index check --fail-on-unresolved`
(spec 050), which gives the `W-001` counts a reader and a gate. The 78
`W-001` this corpus records all belong to pending specs claiming units
not yet written, which is what a specify-first corpus is; none belong to
a spec in flight.

This change also closes the `state_dir` follow-on D-7 deferred:
`layout.state_dir = "data"` replaces `"data"` in `resolver_exclusions`,
which declares what the directory *is* (claude-observatory's state root
for this project) rather than hiding it from the resolver alone.
spec-spine's own adopter audit names this repository's `"data"` entry as
the case the knob was added for.

Not adopted here: `spec-spine verify <id>` does not yet replace
`scripts/verify-spec.sh`. The v0.15.0 kit still ships the script, FR-002
and AC-3 of this spec require it by name, and changing what a spec
requires is not a bump's business. Retiring it is its own change, and it
waits on the kit retiring its copy.

D-9 (2026-09-07, sibling parity audit). The gate chain ran on every PR and
was never binding. Branch protection on `main` required **no** status check
at all (`required_status_checks.contexts` was empty), so GitHub would merge
a PR whose CI was red; the only thing enforcing green was the `/shepherd`
skill, which is the agent that wants to merge. A corpus whose thesis is
that done is never self-authored cannot leave its merge gate to the
merger's own discretion.

It could not be fixed in the settings alone. `govern.yml` published four
separately-named checks, two of them conditional on `has_cargo` and
`has_deny`, and a skipped required check never reports a conclusion, so an
enumerated required-context list would have blocked every PR forever the
first time a guard closed. This is butler-ai's D-2 with different names:
there, two checks both literally called `gate` made a required-context list
unable to distinguish them, and branch protection was consequently never
configured at all.

B-3 now takes spec-spine's and butler-ai's shape. `ci.yml` is the only
workflow with event triggers and holds the language gates; `govern.yml`
becomes `on: workflow_call` and is called as `jobs.govern`, publishing no
check of its own; `jobs.ci-gate` aggregates every job into the one required
context, counting a skip as a pass and any `failure` or `cancelled` as a
refusal. `merge_group` is added and is inert until the merge queue is
enabled. Nothing about *what* the chain runs moved; only how many checks it
publishes and whether GitHub is willing to merge without them.

Three defects surfaced in the same audit and are fixed here rather than
filed:

- The coupling gate diffed `--base <base.sha> --head HEAD`. On a
  `pull_request` the checkout is `refs/pull/N/merge`, which re-resolves
  against the current base on every run, so the diff folded in commits
  merged after this PR opened and reported them as this PR's drift, or
  silently widened a waiver's scope. Both endpoints are now the event's
  frozen SHAs, which is what spec-spine's own CI documents.
- `.gitattributes` carried no merge driver for the sharded `.derived/`
  trees. PR #22 conflicted on exactly three of those shards, resolved by
  hand. B-9 ports spec-spine spec 020's driver, opt-in per clone.
- The `spec-spine` pin was hardcoded in `govern.yml` and restated in prose
  in `AGENTS.md` and `README.md`, which is the hand sweep D-7 had to
  perform. `SPEC_SPINE_VERSION` now lives in the `Makefile` and CI reads
  that literal, so the pin moves in one place; a `make setup` target
  consumes it too. The prose sites still name the version for a human
  reader and are no longer what CI installs.

`CODEOWNERS` is added with the same reading: the corpus, the standards, the
harness, and everything that runs with a token.

D-10 (2026-09-08, corpus amendment; the coherence guard gains a second
branch). The guard told a build session what to do when its code and its own
spec disagree, and said nothing about the case that actually arose three
times in wave 2: the spec being built requires something a `complete` spec
forbids. Sessions read the silence in opposite directions. Spec 024's session
resolved two such contradictions on its own authority (D-2, D-3), left both
specs' text intact, declared `extends` edges, and shipped. Spec 022's and
spec 025's sessions refused the same shape and held at `in-progress`, which
cost two sessions and a human PR each time. Both readings were defensible
against the guard as written, which is the defect: a rule that licenses
opposite behaviors in the same situation is not governing.

The guard now turns on whether a mechanism exists that honors both
requirements. When one does, the session takes it, records a dated decision
naming both requirements and the rejected alternatives, declares the
`extends` edge, and leaves every spec's text as it found it; a complete
spec's acceptance criterion outranks a later spec's behavior text, because
the first has been verified and the second has not. When none does, because
another spec's requirement text is itself false, the session refuses and
surfaces it, and the reconciliation is a human's. Spec 024 D-2 and spec 025
D-11 are named in the rule as the worked examples of each branch.

This ratifies 024's behavior rather than reversing it: its decisions are
sealed, its reading is now the rule, and 025's refusal remains correct under
the same rule because no mechanism could have made spec 021 B-1's issuer
text true. Rejected alternative: requiring refusal in every case, which is
safest per instance and pays for it in stalled backlog every time, and which
would have made 024's two contradictions into two more human PRs for
reconciliations that changed nothing anyone needed to adjudicate.

D-11 (2026-09-09, corpus amendment; spec-spine kit v18 adoption). The pin
moves from 0.15.0 to 0.18.0 in every site that states it (`Makefile`, which
CI reads, plus the prose in `AGENTS.md`, `README.md` and the architect
agent). Unlike D-7 and D-9 this is not only a bump: three of the four things
0.18.0 changed are protocol substance, which the Territory section reserves
for an amendment, so B-2, B-3, B-4, B-5, B-7, FR-002, AC-1, AC-3, the
`establishes` list and the summary are amended here rather than annotated.

**The gate stops writing.** `make spine` ran `compile` and `index`, both
writes, and then `index check`, so it repaired the tree it was about to
judge and the freshness half could not fail. It is replaced by `make gate`,
read-only throughout, and `make refresh`, the writing half a live session
runs before committing the shards it regenerated. `spec-spine check`
(spec-spine spec 075) reads both committed trees in one verb; the CI job
runs the same one in place of `compile --check` and `index check`.
`--fail-on-warn` is passed, which is now reachable from CI at all
(spec-spine 077). `--fail-on-unresolved` is deliberately not: on a corpus
specified before it is built, every unit of every pending spec is
unresolved by design, which is 57 of them today. spec-spine's own CI opts in
because that repository builds what it claims inside one PR; rahi will not
until the last wave lands, and taking the flag would refuse every PR.

**`scripts/verify-spec.sh` is retired.** D-9 deferred this with a named
trigger: "it waits on the kit retiring its copy". spec-spine spec 074 did
exactly that, so the trigger has fired and FR-002 and AC-3 now name
`spec-spine verify <id>`, the verb spec-spine 049 added and the one the
orchestrator's verify stage already runs after merge. Two implementations of
one protocol is the drift the verb exists to remove; this repository now
ships none of its own. `make verify SPEC=<id>` and `/verify <id>` wrap it.

**The kit ships ten skills, not fifteen.** spec-spine spec 081 audited the
set and removed the five nothing in the loop reaches: `implement-plan`
restates `build`, `validate-and-fix` restates `ship` step 1 and `shepherd`,
and `cleanup`, `research` and `refactor-claude-md` are generic recipes whose
one governed sentence each is already a standing rule. A skill nobody calls
is a rule that goes stale unwatched. `/init` is renamed `/prime`
(spec-spine 075, which spent the name `init` on the scaffolding verb). The
ten are byte-identical to the kit, which is what D-8's "a future kit update
is a copy" promised and what held here: the ten needed no local edit, only
the copy. Two files deliberately did NOT take the kit's version.
`.claude/rules/adversarial-prompt-refusal.md` is a superset of the kit's
since D-10 added the second branch, which is not upstream yet; overwriting
it would silently revoke D-10. The four agents carry this repository's
localizations, which is what D-8 intended, so they took the changed project
facts and not the kit's text. `derived-artifacts-are-compiler-output.md`,
the kit's one worked example of a path-scoped rule, is added.

**The harness surfaces were never hashed.** Every `[index]
extra_hashed_inputs` entry that ended in `/**` matched directories only, and
the hasher keeps files, so `standards/**`, `.claude/agents/**`,
`.claude/rules/**`, `.claude/skills/**`, `.github/workflows/**`,
`docs/design/**`, `docker/**` and `deploy/**` had contributed zero bytes to
any content hash since this repository was scaffolded, along with eight more
in `[index.slices]`. Editing a skill or a workflow never staled the index,
and `spec-spine attest` has been sealing a ledger that had not read the
constitution, the contract or the harness. spec-spine's `L-010` (its specs
074 and 079) is what surfaced it, and rahi is the adopter that lint was
written about: fifteen dead patterns, seven in the table the lint could
already read and eight in the table 079 taught it to read. All fifteen are
corrected to file-matching form. They are written narrowly, by shape rather
than as `dir/**/*`, because this list hashes whatever is on disk: a broad
pattern would fold in the `.DS_Store` files this tree already carries under
`.claude/skills/` and `.github/`, and make the shard hashes differ per
machine. Hashing the workflows is safe as of spec-spine 073, which folds a
workflow as a governance projection with each `uses:` pinned ref stripped,
so a Dependabot action bump moves no hash while a changed step still does;
before 073 it would have walled every bot PR on exit 2.

**97 claimed files are declared out of the hash.** With the globs live,
`L-008` reports every path a spec claims that no content hash covers. Eleven
of the pre-fix 108 were governance files the corrected globs now hash. Of the
97 that remain, 96 are Rust sources claimed as bare `file` units: a `file`
unit carries no span, and only span-backing files enter a shard hash. The
97th is `apps/.gitkeep`, a placeholder spec 034 deletes when it lands the
reference app; `deploy/README.md` is listed with them for the day spec 032
creates it.
`[lint] unwitnessed_allowed = ["crates/**/*.rs", "apps/.gitkeep",
"deploy/README.md"]` declares the gap deliberate, which is the same reading
spec-spine reached for the identical situation in its spec 057. Rejected
alternative: folding `crates/**/*` into `extra_hashed_inputs`, which the
`L-008` message itself warns against and spec-spine 057 §3.4 rules out. It
would make every code edit stale every shard, so every PR would rewrite all
22 registry shards and reintroduce exactly the conflicts sharding removed,
and `index check` would start refusing for a condition `couple` already
refuses better. The gap is not undefended: `couple` refuses a changed source
file whose owning spec did not change, and `require_ownership` refuses an
unclaimed one. What it costs is written down here, and the allowance
suppresses the warning without suppressing the count, so `spec-spine check`
still prints 97 of 97 on every run and a 98th would be visible.

**Known residue, not fixed here.** `specs/000-rahi-bootstrap/spec.md` §8
names "the `spec-dag` check in `make spine`". Spec 000 is
`implementation: n-a` and pinned at the sha256 of its normalized `spec.md`,
and the reference is prose about a target this spec owns, not a requirement
of 000. Editing another spec's text to match a rename this spec made is the
move the coherence guard exists to refuse, so it is reported rather than
taken.

D-12 (2026-09-12, corpus amendment; human decision RH-08; corrects the count
D-11 put in `AGENTS.md`). D-11 wrote "97 of 97" into the New Sessions
protocol as the unwitnessed-claim count a primed session should expect. Spec
034 then widened `[lint] unwitnessed_allowed` to the reference app's sources,
manifest, page, and README and deleted `apps/.gitkeep` (its D-10), which is
the growth D-11 anticipated, and `AGENTS.md` kept saying 97: 034 does not own
the file, and the coupling gate refuses an `AGENTS.md` edit that its owner,
this spec, does not accompany. On 2026-09-12 `spec-spine check` at `c13cc70`
printed `unwitnessed claims: 138 (138 allowed by [lint] unwitnessed_allowed)`,
so every primed session reported a discrepancy that was only stale prose.

The owner decided that the correction is routine coupled work, not a waiver,
and that dated or generated evidence is preferred over another hardcoded
count. `AGENTS.md` therefore states no number: it tells a session to report
both numbers exactly as the check prints them, that equal numbers mean every
unhashed claim is declared, and that a count above the allowance is a new
unhashed claim. The check is the generated evidence, and this entry is the
dated evidence: 138 of 138 on the day of the change. D-11's "97" stays as the
record of its own day. A verification line keeps a count from returning to
the protocol. Rejected alternatives: replacing 97 with 138, which goes stale
at the next spec that claims a file outside `crates/`; a `Spec-Drift-Waiver:`
line, which is a human instrument for a contradiction and this is a
correction the owning spec can simply make.

## Verification

```verify:cli
scripts/spec-dag.sh
spec-spine verify 000-rahi-bootstrap
make gate
# B-2: the gate never writes. Anchored on end of line, so `index coverage`,
# which is a read, is deliberately not matched; the writing verbs are the two
# that end their line, and they belong to `refresh` alone. No \t in these
# patterns: BSD grep reads it as a tab and GNU grep as a literal `t`, which
# would make the negation below pass vacuously on the runner that matters.
sh -c '! sed -n "/^gate:/,/^$/p" Makefile | grep -qE "SPEC_SPINE\\) (compile|index)$"'
sh -c 'sed -n "/^refresh:/,/^$/p" Makefile | grep -qE "SPEC_SPINE\\) compile$"'
sh -c 'sed -n "/^refresh:/,/^$/p" Makefile | grep -qE "SPEC_SPINE\\) index$"'
# B-5: ten skills, and exactly the ten the protocol reaches.
sh -c 'test "$(ls -d .claude/skills/*/ | wc -l | tr -d " ")" = 10'
sh -c 'for s in build code-review commit next prime setup shepherd ship spec verify; do test -f ".claude/skills/$s/SKILL.md" || exit 1; done'
# B-7: the kit's one worked example of a path-scoped rule.
test -f .claude/rules/derived-artifacts-are-compiler-output.md
# D-12: the protocol states no unwitnessed-claim count; the check prints it.
sh -c '! grep -A7 "unwitnessed-claim count" AGENTS.md | grep -qE "[0-9]+ of [0-9]+"'
# FR-002: one verification protocol, one implementation of it.
sh -c '! test -e scripts/verify-spec.sh'
sh -c 'sed -n "/^verify:/,/^$/p" Makefile | grep -q "SPEC_SPINE) verify"'
# B-3: exactly one aggregate gate, named ci-gate, in the one triggered workflow.
grep -q '^  ci-gate:' .github/workflows/ci.yml
# B-3: the governance chain is reusable only (no event triggers of its own).
grep -q 'workflow_call' .github/workflows/govern.yml
sh -c '! grep -qE "^  (push|pull_request|merge_group):" .github/workflows/govern.yml'
# B-3: ci.yml is the single triggered entry point and calls that chain.
grep -q 'uses: ./.github/workflows/govern.yml' .github/workflows/ci.yml
grep -q '^  merge_group:' .github/workflows/ci.yml
# B-3: the coupling gate diffs the event's frozen SHAs, never the merge ref.
grep -q 'HEAD_SHA: ..{ github.event.pull_request.head.sha }' .github/workflows/govern.yml
sh -c '! grep -q -- "--head HEAD" .github/workflows/govern.yml'
# B-3: the pin is stated in the Makefile, and CI reads it from there.
sh -c 'test -n "$(sed -n "s/^SPEC_SPINE_VERSION ?= //p" Makefile)"'
grep -q "SPEC_SPINE_VERSION ?= " .github/workflows/govern.yml
# B-9: LF normalization, the merge-driver attribute, and the driver itself.
grep -q 'text=auto eol=lf' .gitattributes
grep -q 'merge=spec-spine-derived-regen' .gitattributes
test -x .githooks/merge-derived-index.sh
test -x .githooks/enable-merge-driver.sh
test -f CODEOWNERS
```
