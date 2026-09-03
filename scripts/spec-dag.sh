#!/usr/bin/env bash
# spec-dag.sh: the DAG check the coupling gate does not do (spec 001 FR-001).
#
# Reads `spec-spine registry list --json` (a typed CLI read, never the
# .derived/ shards) and refuses:
#   - a depends_on target that is not in the corpus,
#   - a depends_on target with a higher or equal ordinal (build order is the
#     ordinal; claude-observatory schedules lowest-numbered-ready),
#   - any cycle, naming the path.
# Exit 0 clean, 1 on a violation, 3 when spec-spine (or python3) is absent.
set -u

if ! command -v spec-spine >/dev/null 2>&1; then
  echo "spec-dag: spec-spine not on PATH (run /setup)" >&2
  exit 3
fi
if ! command -v python3 >/dev/null 2>&1; then
  echo "spec-dag: python3 not available" >&2
  exit 3
fi

repo="${1:-.}"
tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT
if ! spec-spine registry list --json --repo "$repo" > "$tmp"; then
  echo "spec-dag: spec-spine registry list failed (run spec-spine compile first?)" >&2
  exit 3
fi

# The program arrives on stdin (the heredoc), so the registry JSON travels by
# file path, never by the same stream.
python3 - "$tmp" <<'PY'
import json, sys

with open(sys.argv[1], encoding="utf-8") as fh:
    specs = json.load(fh)
deps = {s["id"]: list(s.get("dependsOn") or []) for s in specs}
ordinal = {sid: int(sid[:3]) for sid in deps}
violations = []

for sid, ds in sorted(deps.items()):
    for d in ds:
        if d not in deps:
            violations.append(f"{sid} depends on unknown spec {d}")
        elif ordinal[d] >= ordinal[sid]:
            violations.append(f"{sid} depends on {d}, which is not lower-numbered (build order is the ordinal)")

WHITE, GREY, BLACK = 0, 1, 2
color = {sid: WHITE for sid in deps}
stack = []
cycle = None

def visit(sid):
    global cycle
    color[sid] = GREY
    stack.append(sid)
    for d in deps.get(sid, []):
        if d not in deps:
            continue
        if color[d] == GREY:
            cycle = stack[stack.index(d):] + [d]
            return True
        if color[d] == WHITE and visit(d):
            return True
    stack.pop()
    color[sid] = BLACK
    return False

for sid in sorted(deps, key=lambda s: ordinal[s]):
    if color[sid] == WHITE and visit(sid):
        break

if cycle:
    violations.append("dependency cycle refuses scheduling: " + " -> ".join(cycle))

if violations:
    for v in violations:
        print(f"spec-dag: {v}", file=sys.stderr)
    sys.exit(1)

roots = [s for s, ds in deps.items() if not ds]
print(f"spec-dag: {len(deps)} spec(s), acyclic, every dependency lower-numbered, {len(roots)} root(s)")
PY
