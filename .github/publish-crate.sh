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

if out="$(cargo publish --locked -p "$pkg" 2>&1)"; then
  echo "$out"
  echo ">> published $pkg"
else
  echo "$out"
  if echo "$out" | grep -qiE "already (uploaded|exists)"; then
    echo ">> $pkg is already on crates.io at this version, skipping (idempotent)"
  else
    echo ">> publish failed for $pkg" >&2
    exit 1
  fi
fi
