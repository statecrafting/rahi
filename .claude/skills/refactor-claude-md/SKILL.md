---
name: refactor-claude-md
description: Tighten CLAUDE.md by extracting context-specific guidance into docs and path-scoped rules under .claude/rules, keeping the harness spec (001) coupled
argument-hint: "[path to CLAUDE.md, default ./CLAUDE.md]"
---

# Refactor CLAUDE.md

Reduce the size of `CLAUDE.md` while preserving guidance, by moving
context-specific sections into `docs/` and loading them through
path-scoped rules. In rahi, `CLAUDE.md`, `.claude/rules/`, and
`AGENTS.md` are spec 001's territory and hashed inputs of the codebase
index: every change here couples to `specs/001-agentic-harness/spec.md`
(a dated D-n note under Resolved decisions) and stales the index until
`spec-spine index` runs.

## Process

1. **Read and analyze** the current `CLAUDE.md` in full. Compare it with
   `AGENTS.md`: anything duplicated between them belongs in `AGENTS.md`
   only (it is the cross-agent authority; `CLAUDE.md` carries only what
   Claude Code needs beyond it).

2. **Identify extraction candidates**: sections that are cross-cutting
   patterns rather than core setup, specific to certain crates or file
   types, long and detailed, or better loaded only when relevant.

3. **For each candidate** recommend: the doc name under `docs/`, its
   scope, the `paths:` globs that should trigger it, and the one-line
   reference to keep in `CLAUDE.md`.

4. **Create the files** in this order: extract to `docs/<name>.md`;
   create `.claude/rules/<name>.md` with `paths:` frontmatter and a short
   reminder pointing at the doc; replace the extracted section in
   `CLAUDE.md` with the reference; update any documentation table.

   Path-scoped rule format (the key is `paths`, a YAML list of globs):

   ```markdown
   ---
   paths:
     - "crates/rahi-eval/**"
   ---

   Two or three key points, and the doc to read: `docs/<name>.md`.
   ```

5. **Couple the change**: add a dated D-n entry to spec 001's `## 7.
   Resolved decisions` naming the extraction, then `spec-spine compile &&
   spec-spine index` and stage `.derived/`.

## Key principles

- Extract only context-specific guidance; keep universal rules in
  `CLAUDE.md`.
- Preserve critical information in `CLAUDE.md`: the frozen invariants,
  the commands, the architecture table, the governance mechanics, house
  style.
- Meaningful globs: a rule that loads everywhere is a `CLAUDE.md` section
  in disguise.
- Keep replacements brief; the reader needs to know where to look.

## Good extraction candidates in this repository

- Per-crate implementation notes once a crate has shipped (for example
  the REAPI digest mapping for `crates/rahi-eval/**`).
- Testing patterns (fixture ledgers, the `testing` feature builders).
- The web client's conventions (`web/**`).

## Keep in CLAUDE.md

- The frozen invariants and the hash-stability rule.
- Commands and exit codes.
- The layer-to-crate table.
- Governance mechanics (ownership ratchet, committed `.derived/`, hooks).
- House style.

## After extraction

Report the size change (lines before and after), list the files created,
confirm `spec-spine index check` is fresh, and offer `/commit`.

`$ARGUMENTS`
