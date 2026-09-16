#!/usr/bin/env bash
# Build the assembled consumer cell, boot it, and prove it is governed
# (spec 039 B-7, AC-4). Run from the assembled cell's directory.
#
# Two release jobs run this over the same cell: `consumer` against the git
# stanza a consumer writes before a version is published, and
# `consumer-registry` against the crates.io stanza it writes after (D-11).
# One script, so the two stanzas are proven by the same evidence.

set -euo pipefail
cargo build --release
bin=target/release/consumer-cell
export RAHI_PUBLIC_URL=http://127.0.0.1:18443
export RAHI_DATA_DIR="${PWD}/data"
export RAHI_LISTEN_ADDR=127.0.0.1:18443
export RAHI_HIQLITE_API_ADDR=127.0.0.1:18301
export RAHI_HIQLITE_RAFT_ADDR=127.0.0.1:18401
export RAHI_RAUTHY_MODE=none
mkdir -p "$RAHI_DATA_DIR"
"$bin" first-boot
"$bin" migrate
"$bin" serve &
serve=$!
ready=0
for _ in $(seq 1 120); do
  if curl -fsS http://127.0.0.1:18443/readyz > /tmp/readyz 2>/dev/null; then ready=1; break; fi
  sleep 1
done
[ "$ready" = "1" ] || { echo "the cell never answered /readyz" >&2; exit 1; }
grep -q '"status":"ready"' /tmp/readyz

# One denial, answered with the id of the record the chain holds.
# This safe request also mints the CSRF cookie the next one echoes
# (spec 020 B-4); the probes sit outside that layer and mint none.
code="$(curl -s -c /tmp/jar -o /tmp/deny -w '%{http_code}' http://127.0.0.1:18443/api/deny)"
[ "$code" = "403" ] || { echo "the denial answered ${code}"; cat /tmp/deny; exit 1; }
grep -q '"decision":"kernel:' /tmp/deny

# One governed write, admitted, with the double-submit pair a page
# sends: the cookie back, and its value in X-CSRF-Token.
token="$(awk '$6 == "csrf" { print $7 }' /tmp/jar)"
[ -n "$token" ] || { echo "no CSRF cookie was minted" >&2; cat /tmp/jar; exit 1; }
write="$(curl -s -o /tmp/write -w '%{http_code}' -b /tmp/jar \
  -H "x-csrf-token: ${token}" -X POST http://127.0.0.1:18443/api/write)"
[ "$write" = "200" ] || { echo "the write answered ${write}"; cat /tmp/write; exit 1; }
grep -q "wrote 1" /tmp/write

kill -TERM "$serve"
wait "$serve" || { echo "serve did not stop cleanly" >&2; exit 1; }
"$bin" ledger verify
echo "the consumer stanza builds, boots, writes, is denied, and verifies"
