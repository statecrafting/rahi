# rahi spec contract (normative summary)

A one-page operational summary of the bootstrap spec
(`specs/000-rahi-bootstrap/spec.md`), for quick reference. The bootstrap spec
and the constitution are authoritative; where this summary is terser, they
govern.

## Inputs (authored truth: markdown only)

- `specs/NNN-slug/spec.md`: one spec per directory; directory name equals `id`;
  `NNN` is a unique three-digit ordinal and the build order.
- `standards/spec/`: the constitution, this contract, and the template.
- `spec-spine.toml`: the compiler configuration (owned by spec 001).

## Outputs (machine truth: compiler-owned JSON, read via `spec-spine` only)

- `.derived/spec-registry/by-spec/<id>.json`: spec-as-source shards (`compile`).
- `.derived/codebase-index/by-spec/<id>.json`, `by-package/<slug>.json`:
  code-as-source shards (`index`).
- `.derived/**/build-meta.json`: wall-clock metadata; gitignored.

Both shard trees are **committed**. `spec-spine compile --check` and
`spec-spine index check` refuse a stale tree in CI.

## Required frontmatter

`id`, `title`, `status` (`draft`/`approved`/`superseded`/`retired`), `created`
(`YYYY-MM-DD`), `summary`, `authors`, `implementation`, `risk`, `wave`, plus
`domain` and `kind` from the closed taxonomies in `spec-spine.toml` (a missing
value is an `L-002`/`L-003` warning, and CI fails on warnings). Every ordinary
spec has a non-empty `depends_on` naming only lower-numbered specs.

## Extra keys (rahi-specific, declared in `frontmatter.extra_known_keys`)

- `wave`: the build-order wave (1 to 3) from spec 002 §5. Wave 1 is ordinals
  010 to 019, wave 2 is 020 to 029, wave 3 is 030 to 039.

## Lifecycle in a specify-first corpus

| status / implementation | meaning | unresolved owned unit |
|---|---|---|
| `draft` | proposed, not yet ratified by a human; never scheduled | `W-001` warning |
| `approved` + `pending` | a work order | `W-001` warning |
| `approved` + `in-progress` | being built | `W-001` warning |
| `approved` + `complete` | built and verified | **error** (`I-00x`) |
| `approved` + `n-a` | a record that owns no code (000, 002) | n/a |
| `superseded` / `retired` | history | n/a |

## Typed edges (8; `references` is the only non-owning one)

`establishes`, `extends`, `refines`, `supersedes`, `amends`, `co_authority`,
`constrains`, `references`. `origin` is a bootstrap marker, not an edge.

## Authority units

`file` (bare string shorthand; trailing slash = subtree), `section`
(`{file, anchor}`), `symbol` (`{id}`, resolved by tree-sitter), `directory`
(`{path}`), `crate` (`{id}`), `module` (`{id}`).

## Linkage from code to spec

1. Every Cargo package carries `[package.metadata.spec-spine] spec = "NNN-slug"`
   naming its founding spec (the manifest floor: drift, not coverage).
2. The owning spec declares every source file on an owning edge
   (`establishes`, or `extends` into another spec's crate).

`require_ownership` is on: a changed source file no spec specifically claims
is a `C-002` refusal.

## The gate chain

`make gate`, read-only throughout: `check --fail-on-warn` → `lint
--fail-on-warn` → `index coverage --fail-on-untraced` → `couple --base
<resolved default branch> --head HEAD` → `scripts/spec-dag.sh`.
`make ci`: `make gate` → the cargo gates when `Cargo.toml` exists. CI
(`govern.yml`) runs the same set. A gate never writes: it judges the
committed trees, and `make refresh` (`compile` → `index`) is the separate
writing half a live session runs before committing the shards it regenerated.
The escape valve is a scoped `Spec-Drift-Waiver:` line in the PR body,
approved by a human.

## Verification

Every ordinary spec ends with `## Verification` holding `verify:cli` fenced
blocks: one shell command per line, run from the repo root after merge by
the verify stage (`spec-spine verify <id>` locally, which is the same verb).
A spec with no observable command says so in that section rather than
omitting it.

## Determinism

Pure function of `(config, file contents)` → byte-identical output; the ledger
is diffable and mechanically mergeable; staleness is detected by content-hash
comparison alone.
