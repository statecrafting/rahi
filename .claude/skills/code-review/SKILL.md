---
name: code-review
description: "Review the current diff for correctness bugs and spec drift with make spine as the gate, check the store, ledger, identity, and kernel paths against the chassis-invariants rule, and emit an evidence-oriented findings list"
allowed-tools: Read, Grep, Glob, Agent, Bash(git status:*), Bash(git diff:*), Bash(git log:*), Bash(git show:*), Bash(git rev-parse:*), Bash(git fetch:*), Bash(spec-spine:*), Bash(make:*), Bash(cargo:*), Bash(grep:*)
argument-hint: "[scope] - e.g. \"branch\", \"working tree\", \"crates/rahi-ledger\""
---

# /code-review: correctness, spec drift, chassis invariants

Reviews the current diff against three questions: does the change have
correctness or edge-case bugs, does it still match its owning spec's
contract, and does it relax a chassis invariant (one `txn` per write and
its outbox row, CAS append on the chain, the IdP subject as the only
principal id, deny by default). Output is an evidence-oriented findings list, each
line citing `file:line`. Nothing authored is modified; `make spine`
regenerates `.derived/` deterministically, and a diff it leaves is itself
evidence (stale shards).

## Step 0: scope the diff

```sh
git fetch origin main
git status --short && git diff --stat && git log --oneline -10
git diff origin/main...HEAD --stat    # committed delta
git diff HEAD --stat                  # uncommitted delta
git diff origin/main...HEAD --name-only; git diff HEAD --name-only
```

Note which classes changed: crate source (`crates/**`, `apps/**`, `docker/**`, `deploy/**`), specs (`specs/**/spec.md`), standards
(`standards/**`), the harness (`.claude/**`, `AGENTS.md`, `CLAUDE.md`,
`Makefile`, `.github/**`), scripts, docs.

## Step 1: the gate stays green

```sh
make spine                  # compile, index, lint --fail-on-warn, index check, couple, spec-dag
spec-spine index coverage   # ownership: zero unclaimed, zero floor-only
```

- A `couple` failure is the headline finding: cite the file the gate
  named and the owning spec whose declared edges fail to cover it.
- An unclaimed file from `coverage` is a finding against the implementing
  spec's `establishes`.
- A `lint`, `index check`, or `spec-dag` failure is a corpus finding:
  cite the diagnostic verbatim.
- A `.derived/` diff after the run means the shards were stale: a
  finding whose fix is to commit them.

## Step 2: spec-contract match

For each changed source file, confirm the change is consistent with the
contract of its owning spec rather than only with the gate's mechanical
pass. Governed reads, through the CLI:

```sh
spec-spine registry show <spec-id> --json          # declared surface and edges
spec-spine registry relationships <spec-id>        # its typed neighborhood
```

- Does the code do what B-n says and nothing B-n forbids? Cite the label.
- Are the AC-n satisfied verbatim and is `## Verification` runnable?
- If a spec was edited: only `establishes` growth, dated `D-n` entries, a
  dated Status note, and the `implementation` flip are legitimate
  mid-build edits. Anything that changes what the spec requires is a
  coherence-guard finding (`.claude/rules/adversarial-prompt-refusal.md`),
  severity CRITICAL.
- Flag drift where code does something the spec's narrative does not
  describe even when `couple` passes (an over-broad edge).

## Step 3: correctness pass

Read the changed source and look for each of the following, with a
`file:line` and a one-sentence evidence claim:

- Logic and edge-case bugs (off-by-one, unhandled `None` or `Err`, empty
  input, boundary values, integer overflow on untrusted lengths).
- Error-path correctness: the right `Error` variant and the right exit
  code (`0` ok, `1` validation failure or drift, `2` stale, `3` I/O,
  parse, schema, or config; `rahi` adopts the same four).
- Rust hygiene the lints enforce: no `unsafe`; no `unwrap`, `expect`, or
  slice indexing in library code; owned data at public boundaries;
  dependencies point downward only; a new crate carries
  `[package.metadata.spec-spine] spec = "<id>"`.
- Determinism hazards anywhere: `HashMap` or `HashSet` iteration reaching
  output, locale- or platform-dependent behavior, unstable ordering in
  emitted JSON.
- Hygiene: stray debug prints, commented-out code, dead branches, secrets
  or seeds in logs.
- House style in authored text: no em dash (`grep -rn $'\xe2\x80\x94'`
  over the changed files), no session links, no AI attribution.

## Step 4: chassis-invariants pass

Decide from the changed paths in Step 0. If any path is under
`crates/rahi-store/`, `crates/rahi-ledger/`, `crates/rahi-idp/`, or
`crates/rahi-kernel/`, read `.claude/rules/chassis-invariants.md` and check
the diff against every rule it names (transaction atomicity, fencing on
lease-guarded writes, no durable data in the cache group, the IdP subject
as the only principal id, no local account rows, CAS append on the chain,
deny-by-default adjudication). Report each rule as held, violated, or not
applicable under `### Chassis invariants`.

No such path touched: say "not applicable" under `### Chassis invariants`;
do not skip the section silently.

## Step 5: findings report

```
## Review: <scope>
Base: origin/main | Head: <branch> | Files: <n> | +<a>/-<d>
Gate: make spine <ok|FAIL at target> | coverage <n unclaimed> | derived <clean|stale>
Owning spec: <id> | Mid-build spec edits: <legitimate|coherence-guard finding>

### Findings (severity-ordered)
- [CRITICAL|CORRECTNESS|SPEC-DRIFT|GATE|HYGIENE] <claim> at `file:line`
  Evidence: <one sentence, cited>
  Fix: <specific recommendation>

### Chassis invariants
- <rule>: <held | violated | not applicable>

### Clean
- <dimensions checked with nothing found>
```

If nothing is found, say so plainly and report the gate result and the
specialist verdicts as the evidence. To proceed with fixes, the user (or
`/ship`) names the findings to apply.
