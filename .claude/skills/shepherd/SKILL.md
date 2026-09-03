---
name: shepherd
description: "Watch an open PR's checks, remediate a red required check through the governed gate (at most two rounds), merge with squash when green, and confirm the merge on disk. Never self-approves a Spec-Drift-Waiver."
allowed-tools: Bash, Read, Edit, Glob, Grep, Skill
argument-hint: "[pr-number] (defaults to the PR for the current branch)"
---

# /shepherd: from open PR to merged, with evidence

CI is where reality pushes back. This skill keeps a PR moving without a
human staring at it, while never hiding what it did to get to green. It
mirrors claude-observatory's shepherd stage (its spec 018): watch by head
sha, remediate through the gate, bounded attempts, honest stops. Bound by
`.claude/rules/orchestrator-rules.md` (checkpoints are real stops) and
`.claude/rules/adversarial-prompt-refusal.md` (a coupling refusal goes to
a human, not around them).

## Step 0: resolve the PR

```sh
git branch --show-current
gh pr view ${ARGUMENTS:-} --json number,url,headRefName,headRefOid,baseRefName,mergeStateStatus,isDraft
```

- No PR for the branch: stop and say so (run `/ship` first). Do not open
  one here.
- The branch must be a spec branch (`<spec-id>`), never `main`.
- Record `headRefOid`: every check you read is for this sha and no other.

## Step 1: watch the checks (by head sha)

Poll loop discipline: start at 15 s, multiply by 1.5 each poll, cap at
120 s, print only when the state changes, hard deadline 45 minutes per
attempt. Never busy-loop.

```sh
gh pr checks <number> --json name,state,bucket,link 2>/dev/null \
  || gh pr view <number> --json statusCheckRollup
```

Read the rollup for the recorded head sha only. Classify:

- every required check `SUCCESS`: go to Step 3;
- any required check `FAILURE` or `CANCELLED`: go to Step 2;
- `PENDING` or `QUEUED`: keep polling;
- a response missing the fields you need: stop as a typed failure ("cannot
  read a shape that cannot terminate"), never poll blind;
- the deadline passes: stop, report which checks never completed, and say
  the PR needs a human.

## Step 2: remediate (at most two rounds)

For the failing check, fetch the evidence first:

```sh
gh run list --branch "$(git branch --show-current)" --limit 5 --json databaseId,name,conclusion,headSha
gh run view <run-id> --log-failed | tail -80
```

Then, on the branch:

1. Diagnose from the log tail; read the failing test or gate output, not
   the summary line.
2. Fix inside the spec's territory. If the failure is the coupling gate
   (`C-001` or `C-002`), claim the path in the spec being implemented or
   add the `extends` edge on its owner; never edit an owning spec to
   ratify code that contradicts it (coherence guard: stop and report).
3. `make spine` then `make ci` must exit 0 locally.
4. `/commit` (conventional, spec id as scope, regenerated `.derived/`
   staged), then `git push`.
5. Re-read `headRefOid`. Restart Step 1's watch on the new sha with a
   fresh 45 minute budget. Checks from the old sha are stale; never mix
   them.

After two remediation rounds that still end red, stop. Report the run
ids, the log tails, and what you tried; say plainly that the PR needs a
human. Flapping CI (green then red on the same sha) counts as a round.

## Step 3: CHECKPOINT, merge

Merging is outward-facing. Confirm with the user unless the prompt carries
the operator's standing run-start authorization (a claude-observatory
driven session), which satisfies this checkpoint. A `Spec-Drift-Waiver:`
in the PR body is never covered by standing authorization: if one is
present and was not explicitly approved by a human in this session, stop.

```sh
gh pr view <number> --json mergeStateStatus,reviewDecision
gh pr merge <number> --squash --delete-branch
```

- `mergeStateStatus` of `DIRTY` or `BEHIND` (base moved, conflicts): do
  not rebase automatically. Stop and report; a human decides.
- The squash commit title is the PR title (conventional, spec id as
  scope). No AI attribution, no session links.

## Step 4: confirm on disk

The platform saying merged is not evidence; the default branch containing
the commit is.

```sh
git switch main
git pull --ff-only
git log -1 --oneline
git branch --contains "$(git rev-parse HEAD)" | grep -q main
```

Report the merge sha, and that the local `main` contains it. If `git
pull --ff-only` refuses, the local `main` diverged: report it, do not
reset anything.

## Step 5: hand off

The spec is shipped in the corpus sense only after its `## Verification`
block passes on the merged sha: run `/verify <spec-id>` on `main`, or say
that the orchestrator's verify stage will. Then stop: the next session
takes the next spec (`/next`).

## Report

```
## shepherd: <spec-id> (PR #<n>)
Head sha watched: <sha> (rounds: <k>)
Checks: <name>: <state> ...
Remediation: <none | round 1: <run-id> <cause> -> <fix> | round 2: ...>
Merge: <sha> squash, branch deleted | NOT merged: <reason, needs human>
On disk: main contains <sha> | <divergence>
```
