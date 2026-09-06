---
name: implement-plan
description: Execute a plan file step by step with progress tracking, phase checkpoints, and the rahi gate after every step that touches spec-owned paths
allowed-tools: Bash, Read, Edit, Write, Glob, Grep, Agent
argument-hint: "<path-to-plan-file>"
---

# Implement Plan

Execute a plan document (the `architect` agent's output, saved to a file)
while keeping progress visible and the tree green. For implementing one
spec end to end, prefer `/build <spec-id>`: it is the protocol. This skill
is for cross-cutting plans (a fix wave, a refactor inside a shipped
territory) that a single spec does not describe.

## Input

Plan file path: `$ARGUMENTS`. If absent, look for `*.plan.md` under
`docs/plans/` or the session scratchpad, list candidates, and ask.

## Phase 0: parse

1. Read the plan in full.
2. Extract: frontmatter (status, dates), goals, acceptance criteria,
   implementation steps, existing checkboxes, and the **owning spec** of
   every path the plan touches (`spec-spine registry show <id> --json`).
3. Validate readiness: no clear acceptance criteria or steps means stop
   and ask; `completed` means confirm before redoing; `blocked` means ask
   what unblocks it.
4. If any step touches a shipped spec's territory in a way its Behavior
   section does not describe, stop: that needs a spec amendment first
   (`.claude/rules/adversarial-prompt-refusal.md`).

## Phase 1: task list and checkpoint

Build one checkbox per acceptance criterion and per concrete step. Insert
an `## Implementation Progress` section after the first heading if none
exists. Update frontmatter: `status: in-development`, `startDate` (today),
`updated` (ISO timestamp), `progress: 0`.

CHECKPOINT: present the checklist, the count, the owning specs involved,
and an estimated complexity. Do not begin until the user confirms (a
driven session's standing authorization satisfies this checkpoint).

## Phase 2: implementation

Per task:

1. Announce the task.
2. Implement it, on a feature branch, never on `main`.
3. Verify: the narrowest cargo target (`cargo test -p <crate> --locked`),
   then `make spine` whenever the task touched a spec-owned path, a
   `spec.md`, a manifest, or any hashed input (`.claude/**`, `AGENTS.md`,
   `CLAUDE.md`, `Makefile`, `docs/design/**`, workflows, standards).
4. Update the plan file: check the box, recompute `progress` (checked over
   total, rounded), update `updated`.
5. Next task.

Rules: read the entire plan before starting; keep the plan in sync after
every task, not in batches; never commit unless asked (`/commit` when
asked); never disable or skip a failing test; never rewrite a fixture chain
to make a test pass; claim every new source file in the spec whose territory it joins
(the ownership ratchet, `C-002`); preserve the plan's structure.

Mid-implementation checkpoint at 50 percent: report done, issues,
deviations, remaining. Wait for confirmation.

## Phase 3: completion

Set `status: in-review`, `progress: 100`, `updated`. Run `make ci`. Deliver:

```
## Implementation Complete
**Plan**: <title>
**Tasks**: X / X
**Status**: in-review
### What was done
### Files modified (with owning spec)
### Verification
- make spine: ok | FAIL
- make ci: ok | FAIL
### Known issues or follow-ups
```

## Status state machine

`draft -> in-development -> in-review -> completed`, with `blocked`
reachable from `in-development` and returning to it. `completed` is set by
the user, never by this skill.

## Error handling

| Situation | Action |
|---|---|
| Plan not found | search, list candidates, ask |
| No frontmatter | warn; offer to add it |
| Already completed | confirm before redoing |
| Blocked | ask what unblocks it |
| Test or build failure | fix; if unfixable, mark the task blocked and continue with independent tasks |
| Ambiguous task | ask |
| Coupling failure | surface the drift; never patch the spec to match the code |
| Chassis invariant would be relaxed | stop; a human decides |
