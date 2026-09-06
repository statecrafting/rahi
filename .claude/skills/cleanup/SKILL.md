---
name: cleanup
description: Run dead-code and duplicate-code detection across the rahi crates, investigate each finding in context, and return categorized recommendations that respect spec ownership
allowed-tools: Agent, Read, Bash, Glob, Grep, Edit
---

# /cleanup: Cleanup Analysis

## Purpose

Spawn one analyzer sub-agent that runs dead-code and duplicate-code
detection across `crates/` and `apps/`, reads each
finding in context, and returns a structured report. Optional detectors
are used when available and skipped visibly when not.

## Usage

```
/cleanup              # dead code + duplicates
/cleanup dead-code
/cleanup duplicates
```

## Execution

### Step 1: parse arguments

`$ARGUMENTS` selects detectors; default is both. Valid tokens: `dead-code`,
`duplicates`.

### Step 2: spawn the analyzer

Use the `Agent` tool (type `explorer`, read-only) with this prompt, passing
the selected detectors:

---

You are a cleanup analyzer for rahi. Analyze and report; change nothing.

**Detectors to run:** [selected]

**A. Dead code.** Rust first: `cargo clippy --workspace --all-targets
--locked -- -W dead_code -W unused 2>&1 | grep -E "unused|dead|never
used"`; `cargo udeps --workspace` if installed (nightly), otherwise a
manual pass over each crate's `[dependencies]` against its `use` lines.
Fallback for orphan files: a source file under `crates/*/src/` that no
`mod` declaration or `use` path references.

**B. Duplicates.** A duplicate detector if installed (`simian` or `cpd`
for Rust); otherwise surface near-identical `pub fn`
signatures across crates with `grep -rn "^pub fn" crates/*/src | awk -F:
'{print $3}' | sort | uniq -d`. Treat results as hints.

**C. Investigate every finding** by reading the source before
categorizing.

**D. Categorize.**

Keep (false positives): anything under `.derived/` (compiler output);
generated code (`build.rs` outputs, prost modules under `target/`); trait
implementations reached only through dynamic dispatch seams (`Cell`,
`LedgerSigner`, injected clocks and stores); public API of library crates
consumed by `rahi-cli` or an app under `apps/`; migrations; test fixtures
and fixture chains under `testdata/`; workflow and hook scripts.

Safe to remove: private items clippy flags as never used with no
suppression; dependencies with zero usage in their crate; files no spec
claims and nothing references (check `spec-spine index coverage` first).

Needs review: exported items flagged unused inside their crate; files
recently added (`git log` shows planned work); ambiguous dependency usage
(a build script, a feature gate).

Duplicates by priority: high (more than 15 lines of logic), medium (10 to
15 lines of utilities), low (under 10 lines or test setup, keep).

**E. Return exactly this report:**

```markdown
## Cleanup Analysis Report

### Dead Code
#### Safe to remove
| Item | Type | Location | Owning spec | Reason |
#### Needs review
| Item | Type | Location | Owning spec | Context |
#### Keeping
| Item | Reason |

### Duplicate Code
#### High priority
- **[description]** ([N lines]): locations; recommendation
#### Medium priority
#### Keep as-is

### Detectors
- clippy unused: ran / skipped
- cargo udeps: ran / skipped: reason
- duplicate detector: ran / skipped: reason

### Summary
- N safe to remove, N need review, N duplicate blocks, N confirmed intentional
```

Rules: read code before categorizing; be conservative; name the owning
spec of every path via `spec-spine registry show <id> --json` (never parse
`.derived/`); never recommend removing a migration, a chassis invariant test, or
a seam implementation; make no changes.

---

### Step 3: present the report.

### Step 4: offer next steps

Ask whether to remove the safe items, walk the review items, or keep the
report. Removing a spec-claimed path is a change to that spec's territory:
the owning spec's `establishes` list must drop the path in the same
change, and if the owner is a shipped spec that is an amendment (a dated
`## Amendments received` entry) that invalidates its dependents. Say so
before removing anything.
