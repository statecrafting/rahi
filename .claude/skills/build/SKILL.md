---
name: build
description: "Implement one spec start to finish per AGENTS.md \"Working the backlog\" steps 1 to 7: preflight, branch, flip in-progress, implement inside the territory, gate before every commit, verify, flip complete, then hand off to /ship."
allowed-tools: Bash, Read, Edit, Write, Glob, Grep, Skill, Agent
argument-hint: "<spec-id>"
---

# /build <spec-id>: one spec, one session

The protocol is `AGENTS.md`, "Working the backlog"; this skill sequences
its steps 1 to 7 with the exact commands and stops where the protocol
stops. Step 8 is `/ship`. Bound by `.claude/rules/orchestrator-rules.md`
(one session, one spec; checkpoints are real stops) and
`.claude/rules/adversarial-prompt-refusal.md` (the coherence guard). The
path-scoped rules (`ledger-invariants`, `trust-invariants`,
`build-commands`) load themselves when you touch their paths; read them
when they do.

## Step 0: preflight

Halt on any of these; do not work around them.

- An argument is required: the full id (`017-ledger-entry-dag`). Without
  one, run `/next` and ask; never guess.
- `git status --porcelain` is empty. `git branch --show-current` is
  `main`. `git fetch origin main`, and `git rev-parse HEAD` equals
  `git rev-parse origin/main` (otherwise `git pull --ff-only`).
- `make spine` is green on `main` before any change.
- The spec is a work order: `spec-spine registry show <id> --json` says
  `status: approved` and `implementation: pending`, and every `dependsOn`
  is `complete` or `n-a` (what `/next` computes). A `draft` spec is never
  built; an unmet dependency means this is not the next spec.
- Read the spec's `## 2. Territory`. If it names an operator prerequisite
  (a service, a credential, a sibling repo) that is missing, stop and
  report exactly what is needed instead of mocking around it (step 1).

## Step 1: branch and flip (step 2)

```sh
git switch -c <spec-id>
```

Edit `specs/<spec-id>/spec.md`: `implementation: pending` becomes
`implementation: in-progress`. Nothing else in the file changes yet. Then:

```sh
spec-spine compile && spec-spine index
git add specs/<spec-id>/spec.md .derived/
git commit -m "chore(<NNN>): start <spec-id>"
```

`<NNN>` is the three-digit ordinal. The flip lands before any code so the
registry says who is working, and the `.derived/` shards travel with the
edit that changed them (`build-meta.json` is gitignored).

## Step 2: re-read the spec in full (step 3)

Read `specs/<spec-id>/spec.md` top to bottom, then the `## 7. Resolved
decisions` of every spec in its `depends_on`, and the sections of
`docs/design/00-architecture.md` it cites. The design truth precedes the
code.

- **The spec is imprecise:** make the choice and record it as a dated
  `D-n` under `## 7. Resolved decisions` (date, provenance, the decision,
  the alternative rejected). When `data/orchestrator/decision-dropbox/`
  exists this is a driven session: also drop one JSON file per decision
  there, `data/orchestrator/decision-dropbox/<spec-id>-D-n.json`, for the
  orchestrator to seal. The shape is fixed (claude-observatory spec 020
  B-1; unknown fields and floats are rejected):

  ```json
  {
    "id": "<spec-id>/D-n",
    "specId": "<spec-id>",
    "scope": ["<spec-id>", "crates/rahi-ledger/src/entry.rs"],
    "title": "one line",
    "decision": "what was chosen",
    "rationale": "why",
    "alternatives": ["what was rejected"]
  }
  ```

  `data/` is gitignored; never commit it.
- **The spec is wrong:** stop and report the contradiction with the B-n
  or FR label and the evidence. Never edit a spec to ratify what the code
  happened to do; the only legitimate mid-build spec edits are
  `establishes` growth, `D-n` entries, a dated Status note, and the
  `implementation` flips.

## Step 3: implement inside the territory (steps 4 and 5)

- Every new file under `crates/`, `fuzz/`, `executor/`, or `web/` is
  claimed in this spec's `establishes` in the same change (the ownership
  ratchet: `spec-spine index coverage --fail-on-untraced` refuses an
  unclaimed file and `couple` refuses a changed one).
- A new third-party dependency goes in the root `Cargo.toml`
  `[workspace.dependencies]` with an `extends` edge on spec 010's
  `Cargo.toml` section `workspace.dependencies`; crate manifests inherit
  with `workspace = true`. Always `--locked`.
- Touching a file another spec owns needs an `extends` edge on that
  spec's unit, declared in this spec's frontmatter.
- The frozen invariants (step 5): nothing that reaches a hashed byte may
  depend on a clock, an environment read, a float, or `HashMap`
  iteration. A change to any golden vector under
  `crates/rahi-types/testdata/vectors/` is a schema MAJOR and a human
  decision: stop and report, never regenerate.
- Do not edit `.derived/` by hand.

Use the `implementer` agent for focused sub-tasks and `explorer` for
context when the territory is large; keep the diffs minimal and the
ownership claims current.

## Step 4: gate before every commit (step 6)

```sh
make spine   # compile, index, lint --fail-on-warn, index check, couple, spec-dag
make ci      # spine + index coverage --fail-on-untraced + build, test, clippy -D warnings, fmt --check, deny
```

Both exit 0, or the commit waits. Then `/commit` with the spec ordinal as
scope (`feat(017): ...`), staging the regenerated `.derived/` shards with
the code they describe. Commit in coherent slices; a red gate is fixed,
not committed around.

## Step 5: acceptance criteria verbatim (step 7)

Run `/verify <spec-id>` (the spec's `## Verification` block through
`scripts/verify-spec.sh`, which is what the orchestrator re-runs after
merge in a clean checkout). Walk `## 5. Acceptance criteria` one by one
and cite the evidence for each.

- All hold: edit the frontmatter to `implementation: complete`, then
  `spec-spine compile && spec-spine index`, `make spine`, and commit
  (`chore(<NNN>): mark <spec-id> complete`, or fold the flip into the
  final `feat(<NNN>)` commit).
- One cannot be satisfied here (external state, a missing sibling): keep
  `implementation: in-progress`, add a dated Status note to the spec
  saying exactly what remains, recompile, commit, and report it.

## Step 6: hand off

Print a short summary (spec, branch, commits, D-n recorded, acceptance
evidence) and point at `/ship`. Then stop: the next session takes the
next spec.

## Halt conditions (report, do not route around)

- A dirty tree, the wrong branch, or a red `make spine` in preflight.
- A `draft` spec, an unmet dependency, or a missing operator prerequisite.
- A contradiction between the spec and what the code must do.
- A golden vector that would change.
- A coupling failure that only a spec rewrite or a `Spec-Drift-Waiver:`
  could clear: a driven session never self-approves a waiver.
- A `PreToolUse` hook refusal (exit 2): it is a stop, not an obstacle.
