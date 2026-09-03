---
name: spec
description: "Author a new spec from standards/spec/templates/spec-template.md at the next free ordinal in its wave, born status draft, validated by compiling a temporary copy and by scripts/spec-dag.sh. Approval stays a human flip."
allowed-tools: Bash, Read, Write, Edit, Glob, Grep
argument-hint: "[wave] [short title]"
---

# /spec: author a new spec

A new spec is a design change, not code. It is born `status: draft`, it
claims territory only in prose and typed edges, and it schedules nothing
until a human flips it to `approved` (`/next` never offers a draft). The
contract is `standards/spec/contract.md`; the shape is
`standards/spec/templates/spec-template.md`; the layer model and the
wave plan are `specs/002-chassis-thesis/spec.md`.

## Step 1: gather the inputs

Ask for what is not in `$ARGUMENTS`; do not invent any of these:

- **title** (short, imperative) and a **slug** (kebab-case, from the
  title)
- **wave** 1 to 3, which fixes the ordinal range (spec 002 §5):

  | wave | ordinals |
  |---|---|
  | 1 | 010 to 019 |
  | 2 | 020 to 029 |
  | 3 | 030 to 039 |

- **kind**: one of `kernel`, `feature`, `tooling`, `governance`
  (`constitutional-bootstrap` and `thesis` are taken; `spec-spine.toml`
  `[kind] allowed` is the closed enum)
- **domain**: `governance`, `types`, `store`, `ledger`, `kernel`,
  `identity`, `edge`, `ops`
  (`[domains] allowed`, also closed)
- **risk**: `low`, `medium`, `high`, `critical` (critical touches hashed
  bytes or trust decisions)
- **depends_on**: existing ids only, every one lower-numbered than the
  new ordinal. Check against `spec-spine registry list --ids-only`.
- **summary** (one paragraph) and the **territory** (crate, modules,
  seams), enough to fill `establishes` with real paths

## Step 2: pick the ordinal

```sh
ls specs/ | cut -c1-3 | sort -n
```

The id is the lowest unused ordinal inside the wave's range, zero-padded
to three digits, joined to the slug: `NNN-slug`. Refuse when the range is
full (say so; renumbering is a human decision). The directory name must
equal the id (spec 000, "directory name equals id").

## Step 3: create the file

```sh
mkdir specs/<id>
cp standards/spec/templates/spec-template.md specs/<id>/spec.md
```

Then edit `specs/<id>/spec.md`:

- Frontmatter: `id`, `title`, `status: draft`, `kind`, `domain`,
  `created` (today, `YYYY-MM-DD`), `authors`, `implementation: pending`,
  `risk`, `wave`, `depends_on`, `summary`. Replace the commented
  typed-edge examples with real `establishes` paths (the manifest, every
  source file, every test, fixtures as a subtree) and any `extends`,
  `constrains`, or `references` edges the territory needs. Drop the
  template's inline comments.
- Body: the numbered sections in template order (Purpose, Territory,
  Behavior as B-n with MUST/SHOULD/MAY, Functional requirements as
  FR-nnn, Acceptance criteria as AC-n, Out of scope, Resolved decisions,
  `## Verification`). The Verification block holds the `verify:cli`
  commands that prove the AC-n; `scripts/verify-spec.sh` runs them after
  merge, so each must exist once the spec is built.
- No em dash anywhere in the text.

The `PostToolUse` hook recompiles the registry after a spec edit; that is
expected.

## Step 4: validate in a temporary copy, then the DAG

Compile and lint a copy so nothing is judged against a half-written
working tree:

```sh
T=$(mktemp -d) && cp -R spec-spine.toml standards specs "$T"/ \
  && spec-spine compile --repo "$T" && spec-spine lint --fail-on-warn --repo "$T"
scripts/spec-dag.sh "$T"
```

`--fail-on-warn` is what the gate runs. If `compile` complains about a
path referenced from outside `specs/` (a `references` edge into
`docs/design/`), add that directory to the copy and re-run. Fix every
diagnostic in the real file, re-copy, re-run, until all three exit 0.
`scripts/spec-dag.sh` refuses a cycle (naming it), a dependency on a
higher-numbered id, and an unknown id.

Then the real gate, which regenerates and checks the committed shards:

```sh
make spine
```

## Step 5: commit and report

On a feature branch named after the new id (`git switch -c <id>`),
`/commit` as `docs(<NNN>): draft spec <id>` with `.derived/` staged
alongside, then `/ship` when the draft is ready for review.

Report the id, wave, kind, domain, risk, dependencies, and:

- `status: draft` is deliberate. Approval (`status: approved`) is a human
  flip made in the file after review; nothing in this skill or in a
  driven session performs it.
- Spec 002's sequencing plans list each wave's specs. Adding the new id
  there is a change to the thesis and is a human call; say so rather than
  editing spec 002.
