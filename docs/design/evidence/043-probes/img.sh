#!/bin/bash
# Published pre-043 verbs (v0.2.0 image by digest) against a fenced volume; spec 043 D-P14.
S=${1:?usage: img.sh <disposable work dir>}; SP=$(cd "$(dirname "$0")" && pwd)
I=ghcr.io/statecrafting/rahi-hello-cell:0.2.0@sha256:494a566d1ea97aa348a0ccbe0adda4a87522f0b67a87518a46f980d68b66f06b; SNAP=$SP/snap.sh
V=$S/img; rm -rf $V; mkdir -p $V/hiqlite/state_machine $V/app-store/state_machine/db $V/rauthy
printf 'rahi-upgrade-cache 7f3a' > $V/hiqlite/state_machine/lock; echo sentinel > $V/app-store/state_machine/db/hiqlite.db
r(){ docker run --rm --name "v-$$" -v $V:/data -e RAHI_PUBLIC_URL=http://localhost:8080 --entrypoint rahi $I "$@"; }
r first-boot >/dev/null 2>&1; echo "first-boot exit=$?"
for verb in "serve" "restore /data/none.age" "preflight" "ledger verify"; do
  $SNAP $V/hiqlite > $S/b1; $SNAP $V/app-store > $S/b2
  start=$(date +%s); ( r $verb > $S/out.txt 2>&1 ) & pid=$!
  for i in $(seq 1 45); do kill -0 $pid 2>/dev/null || break; sleep 1; done
  if kill -0 $pid 2>/dev/null; then docker rm -f "v-$$" >/dev/null 2>&1; wait $pid; echo "=== $verb: STILL RUNNING at 45s (killed)"; else wait $pid; echo "=== $verb: exit=$? after $(( $(date +%s)-start ))s"; fi
  grep -Eiv "^\s*$" $S/out.txt | grep -Ei "error|panic|lock|refus|conflict|attach|listen|rauthy" | cut -c1-200 | head -4
  $SNAP $V/hiqlite > $S/a1; $SNAP $V/app-store > $S/a2
  cmp -s $S/b1 $S/a1 && echo "  legacy path unchanged" || { echo "  legacy path CHANGED:"; diff $S/b1 $S/a1 | head -8; }
  cmp -s $S/b2 $S/a2 && echo "  app-store unchanged" || echo "  app-store CHANGED"
done
