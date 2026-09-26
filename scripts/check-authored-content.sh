#!/usr/bin/env bash
# The organization's authored-content rules (U+2014, session links).
#
# The two rules, made mechanical.
#
#   1. No authored file contains U+2014 (EM DASH).
#   2. No authored file carries an agent-session URL or a session-tracking
#      trailer.
#
# Scope is git-tracked files plus staged additions, minus the compiler-owned
# derived tree and minus this script for rule 2 (it necessarily contains the
# patterns it searches for). Findings are reported per rule, so one does not
# mask the other, and every finding names its file and line.
#
# `--text FILE...` applies the same two rules to text that is not a tracked
# file: a pull request's title and body, or a commit message, which become
# history under merge commits (AGENTS.md, "How a pull request is merged"). No
# file is exempt in that mode.
#
# Exit 0 clean, 1 findings, 3 usage/environment.

set -uo pipefail

SELF="scripts/check-authored-content.sh"
status=0
mode=tree
if [ "${1:-}" = "--text" ]; then
  mode=text
  shift
  [ "$#" -gt 0 ] || { echo "check-authored-content: --text needs at least one file" >&2; exit 3; }
  SELF=""
else
  cd "$(git rev-parse --show-toplevel 2>/dev/null)" || {
    echo "check-authored-content: not inside a git work tree" >&2
    exit 3
  }
fi

# Bash 3.2 is the floor (macOS system bash). Refuse anything older loudly rather
# than reporting a clean tree we never actually read.
if [ "${BASH_VERSINFO[0]:-0}" -lt 3 ]; then
  echo "check-authored-content: needs bash 3.2 or newer, found ${BASH_VERSION:-unknown}" >&2
  exit 3
fi

# Text files under version control. -z plus a NUL read keeps paths with spaces
# intact; the derived tree is compiler output and is not authored.
#
# Built with a read loop rather than `mapfile -d ''`, which needs bash 4.4.
# macOS ships /bin/bash 3.2, so a contributor whose PATH resolves to it would
# otherwise get an empty file list and a vacuous pass. This form runs on 3.2.
files=()
if [ "$mode" = text ]; then
  for f in "$@"; do
    [ -f "$f" ] || { echo "check-authored-content: no such file: $f" >&2; exit 3; }
    files+=("$f")
  done
else
  while IFS= read -r -d '' f; do
    case "$f" in
      .statecraft/derived/*|*.png|*.jpg|*.jpeg|*.gif|*.ico|*.pdf|*.node) continue ;;
    esac
    [ -f "$f" ] || continue
    files+=("$f")
  done < <(git ls-files -z --cached --others --exclude-standard)
fi

if [ "${#files[@]}" -eq 0 ]; then
  echo "check-authored-content: no authored files found" >&2
  exit 3
fi

# --- Rule 1: U+2014 ---------------------------------------------------------
# The byte sequence is built rather than written, so this file does not trip
# its own check and a reader can see exactly which codepoint is refused.
emdash=$(printf '\xe2\x80\x94')
em_hits=$(grep -n -F -- "$emdash" "${files[@]}" 2>/dev/null)
if [ -n "$em_hits" ]; then
  echo "U+2014 (EM DASH) is refused by the organization's authored-content rules (U+2014, session links):"
  printf '%s\n' "$em_hits" | sed 's/^/  /'
  echo "  Use a colon, semicolon, comma, parentheses, or two sentences."
  status=1
fi

# --- Rule 2: agent-session URLs and session-tracking trailers ---------------
scan=()
for f in "${files[@]}"; do
  [ "$f" = "$SELF" ] || scan+=("$f")
done

# One pattern per line, extended regex. Each is a link or trailer that ties
# repository content to an agent session; spec 001 section 3.6.2 refuses all of
# them, and refuses substituting another tracking link.
patterns='claude\.ai/(chat|code)/[0-9a-f-]{8}
Co-[Aa]uthored-[Bb]y:.*(Claude|Codex|Copilot|Gemini|noreply@anthropic)
Generated with \[?(Claude Code|Codex)
(Session|Agent-Session|Run)-(Id|URL): *[0-9a-zA-Z_-]{8}
https?://[a-z0-9.-]*(anthropic|openai)\.com/[a-z/]*session'

if [ "${#scan[@]}" -gt 0 ]; then
  link_hits=$(printf '%s\n' "$patterns" | grep -v '^$' \
    | while IFS= read -r p; do grep -n -E -- "$p" "${scan[@]}" 2>/dev/null; done)
  if [ -n "$link_hits" ]; then
    echo "agent-session links and session trailers are refused by the organization's authored-content rules (U+2014, session links):"
    printf '%s\n' "$link_hits" | sed 's/^/  /'
    status=1
  fi
fi

if [ "$status" -eq 0 ]; then
  echo "check-authored-content: ${#files[@]} authored $([ "$mode" = text ] && echo text || echo file)(s) clean (U+2014, session links)"
fi
exit "$status"
