#!/bin/sh
# Rendered by Statecraft from profile github-actions-rust revision 22.
# The policy-isolation guard (spec 044 section 3.3): a pull request that
# changes a dependency or supply-chain policy file and anything else must be
# split, so a dependency change is never reviewed inside a feature.
#
# usage: guard-policy-files.sh <base> <head>
#
# The changed paths are those between the merge base of the two revisions and
# the head, never between the two tips, so a change present on both sides
# cannot cancel out. Each path falls in exactly one class:
#
#   policy     deny.toml, .cargo/audit.toml, anything under supply-chain/, any
#              Cargo.toml or Cargo.lock at any depth, spec-spine.toml and
#              docs/adoption/spec-spine.md;
#   generated  anything under .statecraft/derived/, and
#              .statecraft/environment.json, which their own freshness checks
#              guard; the guard ignores them;
#   other      every remaining path.
#
# Exit 0 when policy and other paths did not both change; 1 when they did,
# listing each class's paths; 2 when the guard cannot decide (bad usage, a
# revision that does not resolve, no merge base). It never fetches, so it
# cannot hang or half-decide.
#
# The exit and merge-base rule are adapted from preloop-ci's
# supply-chain/guard-policy-files.sh. That part: MIT License, Copyright (c)
# 2026 Preloop contributors. Permission is hereby granted, free of charge, to
# any person obtaining a copy of this software and associated documentation
# files (the "Software"), to deal in the Software without restriction,
# including without limitation the rights to use, copy, modify, merge,
# publish, distribute, sublicense, and/or sell copies of the Software, and to
# permit persons to whom the Software is furnished to do so, subject to the
# following conditions: The above copyright notice and this permission notice
# shall be included in all copies or substantial portions of the Software.
# THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
# IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
# FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
# AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
# LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
# OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
# SOFTWARE.
set -eu

# The family exit contract (revision 7), with this guard's own table: 2 is
# also bad usage here (spec 044 section 3.3), because every way the guard
# cannot decide is one refusal. A command `set -e` stopped on is an operation
# that broke, reported as 4; never 0.
leave() {
  trap - EXIT
  exit "$1"
}
trap 'rc=$?; if [ "$rc" -ne 0 ]; then echo "guard-policy-files.sh: a command failed (exit $rc); reported as failed (4)" >&2; exit 4; fi' EXIT

undecided() {
  echo "guard-policy-files.sh: $1; the guard cannot decide, and fails closed" >&2
  leave 2
}

[ "$#" -eq 2 ] || undecided "usage: guard-policy-files.sh <base> <head>"
for rev in "$1" "$2"; do
  case "$rev" in
    "" | -*) undecided "usage: guard-policy-files.sh <base> <head>, each a revision (got '$rev')" ;;
  esac
done
git rev-parse --is-inside-work-tree > /dev/null 2>&1 || undecided "not inside a git work tree"
base=$(git rev-parse --verify --quiet "$1^{commit}") || undecided "the base revision $1 does not resolve (was it fetched?)"
head=$(git rev-parse --verify --quiet "$2^{commit}") || undecided "the head revision $2 does not resolve (was it fetched?)"
merge_base=$(git merge-base "$base" "$head") || undecided "$1 and $2 have no merge base"

# No renames: a renamed path counts as both its old and its new path, so a
# move into or out of a policy path is seen on both sides.
changed=$(git -c core.quotePath=true diff --no-renames --name-only "$merge_base" "$head") \
  || undecided "cannot list the paths changed between $merge_base and $head"

# One pass over the paths, so the guard is linear in their number. A path git had to quote is unquoted only enough to
# classify it: the class depends on its directory prefix and its final name.
rc=0
printf '%s\n' "$changed" | awk '
  NF == 0 { next }
  {
    p = $0
    if (p ~ /^".*"$/) p = substr(p, 2, length(p) - 2)
    n = split(p, part, "/")
    name = part[n]
    if (p ~ /^\.statecraft\/derived\// || p == ".statecraft/environment.json") {
      class = "generated"
    } else if (p == "deny.toml" || p == ".cargo/audit.toml" || p ~ /^supply-chain\// ||
               name == "Cargo.toml" || name == "Cargo.lock" ||
               p == "spec-spine.toml" || p == "docs/adoption/spec-spine.md") {
      class = "policy"
    } else {
      class = "other"
    }
    count[class]++
    # Listed exactly as git printed it, quoted where git quoted it, so a
    # name with a newline or a control character cannot forge a line here.
    list[class] = list[class] "  " $0 "\n"
  }
  END {
    printf "policy-isolation: %d policy, %d generated (ignored), %d other\n", count["policy"], count["generated"], count["other"]
    if (count["policy"] > 0 && count["other"] > 0) {
      printf "this pull request changes policy paths and other paths together; split it, so the dependency or policy change is its own pull request\n"
      printf "policy:\n%s", list["policy"]
      printf "other:\n%s", list["other"]
      mixed = 1
    }
    exit (mixed ? 1 : 0)
  }
' || rc=$?
case "$rc" in
  0) leave 0 ;;
  1) leave 1 ;;
  *) echo "guard-policy-files.sh: classifying the paths failed (exit $rc); reported as failed (4)" >&2; leave 4 ;;
esac
