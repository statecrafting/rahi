---
name: next
description: "Compute the next ready spec exactly as AGENTS.md \"Working the backlog\" step 1 defines it, from a typed spec-spine registry read, with honest blockers when nothing is ready."
allowed-tools: Bash, Read
---

# /next: the next work order

The backlog is the spec corpus. The rule is step 1 of `AGENTS.md`,
"Working the backlog": the lowest-numbered spec with `status: approved`
and `implementation: pending` whose every `depends_on` target is
`implementation: complete` or `n-a`. A `draft` spec is never offered:
approval is a human act (claude-observatory's scheduler, its spec 012
D-3, enforces the same edge). Specs `000`, `001`, and `002` are records,
never work orders.

## Step 0: the DAG must be executable

```sh
scripts/spec-dag.sh
```

Exit 0 continues. Exit 1 names the violation (a cycle with its path, a
dependency on a higher-numbered or unknown spec): refuse to name a next
spec, print the script's stderr verbatim, and stop; scheduling from a
broken graph is guessing. Exit 3 means `spec-spine` or `python3` is
absent: run `/setup`.

## Step 1: compute readiness from a typed read

`spec-spine registry list --json` is a typed CLI read; parsing its output
with `python3` or `jq` is allowed (`.claude/rules/governed-artifact-reads.md`).
Reading `.derived/` is not. The registry JSON travels by file, never on
the same stdin as the program:

```sh
tmp="$(mktemp)" && trap 'rm -f "$tmp"' EXIT
spec-spine registry list --json > "$tmp" || { echo "registry read failed (run spec-spine compile?)"; exit 3; }
python3 - "$tmp" <<'PY'
import json, sys

with open(sys.argv[1], encoding="utf-8") as fh:
    specs = json.load(fh)
by_id = {s["id"]: s for s in specs}
DONE = {"complete", "n-a"}

def blockers(s):
    out = []
    if s.get("status") != "approved":
        out.append(f"status {s.get('status')} (approval is a human act)")
    for d in s.get("dependsOn") or []:
        dep = by_id.get(d)
        if dep is None:
            out.append(f"depends on {d}, which is not in the corpus")
        elif dep.get("implementation") not in DONE:
            out.append(f"depends on {d}, which is {dep.get('implementation')}")
    return out

pending = sorted((s for s in specs if s.get("implementation") == "pending"), key=lambda s: s["id"])
ready = [s for s in pending if not blockers(s)]
if ready:
    pick = ready[0]
    wave = (pick.get("extraFrontmatter") or {}).get("wave", "?")
    print(f"## next: {pick['id']}")
    print(f"title: {pick['title']}")
    print(f"wave: {wave}; status: {pick['status']}; implementation: {pick['implementation']}")
    print("depends_on:")
    for d in pick.get("dependsOn") or []:
        print(f"  - {d}: {by_id[d].get('implementation')}")
    rest = [s["id"] for s in ready[1:]]
    print("also ready (higher-numbered): " + (", ".join(rest) if rest else "none"))
else:
    print("## next: none ready")
    for s in pending:
        print(f"- {s['id']}: blocked; " + "; ".join(blockers(s)))
    if not pending:
        print("(no spec is implementation: pending)")
PY
```

Cross-check the pick with the typed single-spec read before handing it to
`/build`, and read its `## 2. Territory` for operator prerequisites (a
service, a credential, a sibling repo) that would make the session stop
at step 1 of the backlog protocol:

```sh
spec-spine registry show <id> --json
```

## Step 2: report

Print the block from Step 1 as-is. When nothing is ready, every pending
spec is listed with its honest blockers (`status draft`, or the dependency
by id and its current `implementation` value); never hide an unapproved
spec and never offer one. An `in-progress` spec is not pending: mention
it separately as "in flight" when one exists, since the one-session,
one-spec rule means it belongs to another session or to an interrupted
one.

## Rules

- Never guess an id from `ls specs/`; the registry is the source.
- Never read `.derived/**/*.json` directly.
- `/next` is read-only. It does not flip, branch, or commit; `/build <id>`
  does.
