#!/usr/bin/env bash
# verify-spec.sh <spec-id>: run a spec's `verify:cli` blocks locally (spec 001
# FR-002). This is what claude-observatory's verify stage runs after merge, in
# a clean checkout of the merged sha: every non-comment, non-blank line inside
# a ```verify:cli fence, from the repo root, in order, stopping at the first
# non-zero exit. A spec with no `## Verification` section prints
# `not-declared` and exits 0. `verify:browser` blocks are reported and skipped;
# only the orchestrator drives those.
set -u

id="${1:-}"
if [ -z "$id" ]; then
  echo "usage: scripts/verify-spec.sh <spec-id>" >&2
  exit 2
fi

root="$(cd "$(dirname "$0")/.." && pwd)"
spec="$root/specs/$id/spec.md"
if [ ! -f "$spec" ]; then
  echo "verify: no such spec: $spec" >&2
  exit 2
fi

# The Verification section: from the exact heading to the next H2.
section="$(awk '
  /^## Verification[[:space:]]*$/ { on = 1; next }
  on && /^## / { exit }
  on { print }
' "$spec")"

if [ -z "$section" ]; then
  echo "verify: $id: not-declared (no ## Verification section)"
  exit 0
fi

# Fenced blocks: tag on the opening line, body until a bare closing fence.
commands="$(printf '%s\n' "$section" | awk '
  /^```verify:cli[[:space:]]*$/ { inblock = 1; next }
  /^```verify:browser[[:space:]]*$/ { browser = 1; next }
  /^```[[:space:]]*$/ { inblock = 0; browser = 0; next }
  inblock { print }
  END { if (browser_seen) {} }
')"

browser_count="$(printf '%s\n' "$section" | grep -c '^```verify:browser' || true)"
if [ "${browser_count:-0}" -gt 0 ]; then
  echo "verify: $id: $browser_count verify:browser block(s) are driven by the orchestrator; skipped here"
fi

ran=0
while IFS= read -r line; do
  trimmed="${line#"${line%%[![:space:]]*}"}"
  case "$trimmed" in
    ""|\#*) continue ;;
  esac
  ran=$((ran + 1))
  echo "[verify] \$ $trimmed"
  (cd "$root" && sh -c "$trimmed")
  code=$?
  echo "[verify] exit $code"
  if [ "$code" -ne 0 ]; then
    echo "verify: $id: FAILED at command $ran" >&2
    exit "$code"
  fi
done <<EOF
$commands
EOF

if [ "$ran" -eq 0 ]; then
  echo "verify: $id: not-declared (Verification section holds no verify:cli commands)"
  exit 0
fi
echo "verify: $id: passed ($ran command(s))"
