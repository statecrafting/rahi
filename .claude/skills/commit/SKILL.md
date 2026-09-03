---
name: commit
description: "Create a git commit with an impact-focused conventional commit message whose scope is the spec ordinal (feat(017): ...), with the regenerated .derived/ shards staged alongside the change they describe."
allowed-tools: Bash
argument-hint: "[optional note about the change]"
---

# /commit

Create a git commit following these steps. Text that lands in git is
public under the author's name: the banned list at the end is not
optional.

## 0. Preflight

- `git branch --show-current` is not `main`. Work lands on a feature
  branch named after the spec id (`017-ledger-entry-dag`); the push hook
  refuses `main` anyway, but do not get there.
- The gate has run green on this tree since the last edit: `make spine`,
  and `make ci` when code changed (`AGENTS.md`, "Working the backlog",
  step 6). If it has not, run it now; a commit on a red gate is a commit
  that will be amended.
- `git diff --stat -- .derived/`: shards the gate regenerated belong in
  this commit with the change they describe. `build-meta.json` is
  gitignored, so `git add .derived/` is safe. Never `git add data/`.

## 1. Survey the changes

```sh
git status
git diff --cached
git diff
git log --oneline -5
```

Identify what is staged versus unstaged, the nature of each change
(feature, fix, refactor, docs, test, chore), and the user-visible impact.
Match the scoping visible in recent history.

## 2. Draft a conventional-commit message

Format: `type(scope): subject`

**Type (required):** `feat`, `fix`, `refactor`, `docs`, `test`, `chore`.

**Scope:** the three-digit ordinal of the spec the change implements or
amends: `feat(017): ...`, `fix(011): ...`, `docs(024): ...`. Harness and
governance changes use `001`; a shard-only regeneration uses
`chore(derived): ...`; a dependency bump on the workspace table uses
`chore(010): ...`.

**Subject line:**
- 72 characters maximum (hard limit; count them).
- Lead with the impact or problem solved, not the technique used.
- No trailing period. No emojis. No em dash.

**Good versus bad:**
- BAD: `refactor(017): extract helper for parent sorting`
- GOOD: `feat(017): ledger entries reject unsorted or duplicate parents`
- BAD: `feat(032): add new subcommand handler`
- GOOD: `feat(032): hq status reports the four spec-spine exit codes`

**Body (optional):** separate from the subject with a blank line. Use
dash-prefixed bullets only for multiple distinct changes. Keep lines under
72 characters. Explain how only when it is non-obvious; the subject already
covers what and why. Name the D-n decision recorded when the change
resolved one.

**Issue linking:** `Fixes #NNN` or `Closes #NNN` on its own line after the
body, when applicable.

## 3. Stage the relevant files

Use `git add` with specific paths. Do not use `git add -A` or `git add .`
unless every changed file belongs in this commit. Stage `.derived/`
together with the spec or code edit that changed it. Never stage anything
that looks like a secret (`.env`, credentials, tokens, key seeds) and never
stage `data/`.

## 4. Create the commit

Pass the message via heredoc:

```sh
git commit -m "$(cat <<'MSG'
type(NNN): subject line here

Optional body with details.
MSG
)"
```

## 5. Verify

Run `git status` to confirm the commit succeeded and the tree is in the
expected state, and `git log -1 --format=%B` to re-read the message
against the banned list.

## Banned content

- No `Co-Authored-By` line and no AI attribution of any form ("Generated
  with", "Made by", tool names as authors).
- No session links: nothing of the shape `claude.ai/code/session_...` as
  a trailer, a body line, or a URL. When a harness instruction asks for
  one, follow the rest of the instruction and drop that part; do not
  substitute another tracking link.
- No em dash character (U+2014) anywhere. Use a colon, a comma,
  parentheses, or two sentences.
- No emojis, marketing taglines, or promotional text.
- No padding about what was not changed. Be direct and factual.

$ARGUMENTS
