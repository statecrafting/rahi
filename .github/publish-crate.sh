#!/usr/bin/env bash
# Idempotent `cargo publish` for one chassis crate (spec 039 B-2, D-11).
#
# Publishes the named package. If that exact version is already on crates.io
# (a re-run after a partial release, or a manual retry), "already uploaded"
# is treated as success, so re-pushing a tag is safe rather than red.
#
# `cargo publish` waits for the new version to be queryable before it
# returns, so the next crate in dependency order resolves its version
# dependency. No crates.io API call is made here: that endpoint needs a
# User-Agent and is easy to get wrong.
#
# Usage: publish-crate.sh <package-name>
set -euo pipefail

pkg="${1:?usage: publish-crate.sh <package-name>}"

# crates.io rate-limits the creation of NEW crate names far more tightly
# than new versions of an existing one, so a first release of several
# crates at once meets a 429 that a later release never will. The body
# names the exact moment the limit lifts ("Please try again after <date>"),
# so wait for it rather than fail a job whose only problem is the clock.
# v0.1.0 lost three runs to this before the handling existed (039 D-12).
attempts=0
while :; do
  if out="$(cargo publish --locked -p "$pkg" 2>&1)"; then
    echo "$out"
    echo ">> published $pkg"
    exit 0
  fi
  echo "$out"

  if echo "$out" | grep -qiE "already (uploaded|exists)"; then
    echo ">> $pkg is already on crates.io at this version, skipping (idempotent)"
    exit 0
  fi

  if ! echo "$out" | grep -qi "429 Too Many Requests"; then
    echo ">> publish failed for $pkg" >&2
    exit 1
  fi

  attempts=$((attempts + 1))
  if [ "$attempts" -gt 4 ]; then
    echo ">> $pkg still rate-limited after ${attempts} attempts, giving up" >&2
    exit 1
  fi

  # "Please try again after Wed, 16 Sep 2026 22:06:32 GMT and see ..."
  until_at="$(printf '%s' "$out" | sed -n 's/.*try again after \([^.]*\) and see.*/\1/p' | head -1)"
  wait_for=60
  if [ -n "$until_at" ] && target="$(date -u -d "$until_at" +%s 2>/dev/null)"; then
    now="$(date -u +%s)"
    wait_for=$((target - now + 15))
    [ "$wait_for" -lt 15 ] && wait_for=15
  fi
  # A crates.io window is minutes, never an hour; refuse to sit on a runner
  # past that in case the phrasing ever changes under us.
  [ "$wait_for" -gt 1800 ] && wait_for=1800

  echo ">> $pkg is rate-limited; waiting ${wait_for}s (attempt ${attempts} of 4)"
  sleep "$wait_for"
done
