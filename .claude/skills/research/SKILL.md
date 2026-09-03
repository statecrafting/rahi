---
name: research
description: Deep research with parallel sub-agents, query classification, and filesystem artifact passing; corpus questions read specs through spec-spine, external questions use the web
allowed-tools: Agent, Read, Write, Bash(git log:*), Bash(git diff:*), Bash(spec-spine:*), WebSearch, WebFetch, Glob, Grep
argument-hint: "<question or topic to investigate>"
---

# Research

Conduct deep, parallel research on a topic using specialized sub-agents.
Agents cost tokens: classify first, spawn the fewest that answer the
question, and pass reports through files rather than inline.

## Research query

`$ARGUMENTS`

## Phase 1: classify

| Type | Characteristics | Sub-agents | Depth each |
|---|---|---|---|
| Breadth-first | several independent aspects, surveys, comparisons | 3 to 6 | 5 to 10 searches |
| Depth-first | one topic needing thorough understanding | 2 to 3 | 10 to 15 searches |
| Simple factual | one fact, one lookup | 1 | 3 to 5 searches |

Decide: query type, agent count, domains (corpus, codebase, external
docs, papers, general web), and scope (corpus-only, web-only, hybrid).

- **Corpus questions** ("what does spec 020 say about erasure", "who owns
  `crates/rahi-ledger/src/order.rs`", "what depends on 064"): use the
  `explorer` agent with `spec-spine registry show|relationships <id>`,
  `spec-spine index render`, `Grep` over `specs/`, and `git log`. Never
  parse `.derived/` directly.
- **External questions** (REAPI digest functions, BAO range proofs,
  Biscuit datalog, Willow reconciliation, Sigstore bundle format, hiqlite
  Raft semantics): `WebSearch` and `WebFetch`, preferring primary sources
  (specifications, RFCs, the library's own docs).
- Many questions are hybrid; split them across agents by domain.

## Phase 2: parallel execution

Spawn all sub-agents in one message. Each prompt begins with a depth
trigger: "Quick check:", "Investigate:", or "Deep dive:".

Each sub-agent MUST write its full report to the session scratchpad
directory when one is listed in the system prompt (otherwise
`/tmp/rahi-research/`) as `research_<timestamp>_<slug>.md` and return
only: the file path, a two to three sentence summary, key topics, and the
source count.

Example, depth-first ("how should 111 bind the identity key to the QUIC
certificate?"):

```
Task 1: "Deep dive: raw public key TLS (RFC 7250) support in rustls and quinn; how iroh binds a node id to the endpoint certificate"
Task 2: "Investigate: what spec 060 and spec 111 in specs/ already fix about identity binding; use spec-spine registry show"
```

## Phase 3: synthesis

Collect the report paths, read them, merge (themes, deduplication,
contradictions flagged), consolidate sources, and write the final report to
the same directory as `research_final_<timestamp>.md`.

## Phase 4: deliver

```markdown
# Research Report: <topic>
## Executive Summary
## Key Findings
## Detailed Analysis
## Implications for the corpus
(which spec would change, and whether that is an amendment to a shipped
spec, which invalidates dependents, or an edit to a pending one)
## Sources
## Metadata (classification, agents, source count, artifact paths)
```

Show the summary and key findings inline, give the report path, list the
sub-agent report paths, and name contradictions and gaps explicitly.

## Quality

Prefer primary sources; cross-reference important claims; state what could
not be determined; separate fact from inference; prefer recent sources and
flag stale ones. Never use the em dash character in any written artifact.
