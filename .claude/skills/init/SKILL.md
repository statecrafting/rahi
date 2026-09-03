---
name: init
description: "Initialize a session by executing the cross-agent New Sessions protocol declared in AGENTS.md."
allowed-tools: Bash, Read, Glob, Grep
---

# /init: session bootstrap

Thin dispatcher. The canonical protocol lives in `AGENTS.md` under
`## New Sessions`, the cross-agent AAIF/Linux Foundation standard read by
Claude Code, Codex CLI, Cursor, Copilot, and claude-observatory's driven
sessions alike.

## What to do

1. Read `AGENTS.md`: the section from `## New Sessions` inclusive to the
   next `## ` heading exclusive. That section is the step list.
2. Load the standing rules it names first, then execute the protocol,
   using parallel tool calls wherever it says "dispatch simultaneously".
3. Emit the structured summary the protocol prescribes: the
   `## initialized: rahi` block (the layer model with the crates that
   exist, a `## lifecycle:` sub-section, freshness verdicts, recent
   activity, and a ready-to-help line).

This dispatcher deliberately does not duplicate the step list (spec 001
B-1): `AGENTS.md` is the single source of truth. Evolve the protocol by
editing `AGENTS.md`, never this file, so every agent stays in sync.

## Rules

- The protocol's governed reads go through the `spec-spine` binary on your
  `PATH`. If `spec-spine --version` fails, run `/setup` first; never fall
  back to parsing `.derived/` by hand.
- `/init` reports, it does not mutate: `spec-spine compile --check` and
  `spec-spine index check` are the freshness reads, never a bare `compile`
  or `index`.
- A file the protocol names but cannot find is logged as "not found" and
  the protocol continues.
