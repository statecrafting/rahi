#!/bin/sh
# The live upgrade legs of spec 043 (FR-010, AC-3, AC-3a, AC-4 (d), AC-5,
# AC-9): the real v0.2.0 image, by digest, against the image built from this
# change, on real volumes.
#
#   docker/upgrade-live.sh <new-image> [old-image]
#
# Every leg prints PASS or FAIL with what it checked; legs this script cannot
# execute are printed as UNEXECUTED with the reason, so a reader sees what
# was and was not run. Exit 1 when any executed leg failed. Needs docker,
# curl and jq.
set -eu

new="${1:?usage: docker/upgrade-live.sh <new-image> [old-image]}"
old="${2:-ghcr.io/statecrafting/rahi-hello-cell:0.2.0@sha256:494a566d1ea97aa348a0ccbe0adda4a87522f0b67a87518a46f980d68b66f06b}"
run_id="upg$$"
portfile="$(mktemp)"
echo 18600 > "$portfile"
failures=0
results=""

say() { printf '%s\n' "$*" >&2; }
record() {
  results="${results}$1  $2
"
  say "$1  $2"
}
pass() { record PASS "$1"; }
# What changed between two tree listings, on stderr.
show_change() {
  printf '%s\n' "$1" > "${portfile}.a"; printf '%s\n' "$2" > "${portfile}.b"
  diff "${portfile}.a" "${portfile}.b" >&2 || true
  rm -f "${portfile}.a" "${portfile}.b"
}
fail() {
  record FAIL "$1"
  failures=$((failures + 1))
}
unexecuted() { record UNEXECUTED "$1"; }

# The host port, and so the public origin, of `volume`: fixed for the
# volume's life, because Rauthy's client is registered with that origin.
vol_port() {
  f="${portfile}.port.$1"
  [ -f "$f" ] || next_port > "$f"
  cat "$f"
}

# The next host port; a file, because most callers run in a subshell.
next_port() {
  n=$(($(cat "$portfile") + 1))
  echo "$n" > "$portfile"
  echo "$n"
}

cleanup() {
  for c in $(docker ps -aq --filter "name=${run_id}-"); do docker rm -f "$c" >/dev/null 2>&1 || true; done
  for v in $(docker volume ls -q --filter "name=${run_id}-"); do docker volume rm "$v" >/dev/null 2>&1 || true; done
  rm -f "$portfile" "${portfile}".port.*
}
trap cleanup EXIT

# A container of `image` on `volume`, detached, with its default entrypoint
# or with `rahi <args>` when args are given.
start() {
  name="$1"; image="$2"; volume="$3"; p="$4"; shift 4
  if [ "$#" -gt 0 ]; then
    docker run -d --name "$name" -p "${p}:8443" -e RAHI_PUBLIC_URL="http://localhost:${p}" \
      -v "${volume}:/data" --entrypoint /usr/local/bin/rahi "$image" "$@" >/dev/null
  else
    docker run -d --name "$name" -p "${p}:8443" -e RAHI_PUBLIC_URL="http://localhost:${p}" \
      -v "${volume}:/data" "$image" >/dev/null
  fi
}

# One verb of `image` to completion on a stopped volume; its exit code.
verb() {
  image="$1"; volume="$2"; shift 2
  docker run --rm -e RAHI_PUBLIC_URL="http://localhost:$(vol_port "$volume")" -v "${volume}:/data" \
    ${CRASH_AT:+-e RAHI_TEST_UPGRADE_CRASH_AT="$CRASH_AT"} \
    --entrypoint /usr/local/bin/rahi "$image" "$@"
}

# Whether /readyz answers 200 within `seconds`.
ready_within() {
  p="$1"; seconds="$2"; name="$3"; i=0
  while [ "$i" -lt "$seconds" ]; do
    if curl -fsS "http://localhost:${p}/readyz" >/dev/null 2>&1; then return 0; fi
    if [ "$(docker inspect -f '{{.State.Running}}' "$name" 2>/dev/null)" != "true" ]; then return 1; fi
    sleep 1; i=$((i + 1))
  done
  return 1
}

# Whether a process named rauthy runs in the container.
rauthy_running() {
  docker top "$1" -eo comm 2>/dev/null | grep -q '^rauthy'
}

# Every path under /data/<sub> (default: all of it) with its content hash,
# the lock files the gates take left out (spec 043 B-5a names them).
tree() {
  volume="$1"; sub="${2:-.}"
  docker run --rm -v "${volume}:/data" --entrypoint sh "$new" -c "
    cd /data && [ -e '$sub' ] || exit 0
    find '$sub' -type d ! -path './rauthy-env' ! -path './rauthy-env/*' | sort | sed 's/^/dir /' \
      | { if [ -d app-store ] && [ -z \"\$(ls -A app-store)\" ]; then grep -vx 'dir ./app-store'; else cat; fi; }
    find '$sub' -type f ! -name cell.lock ! -name transition.lock ! -path './rauthy-env/*' \
      -exec sha256sum {} + | sort -k2"
}

# A v0.2.0 volume that served, with an archive taken while it ran; echoes
# the archive's path inside the volume.
old_volume() {
  volume="$1"; p="$(vol_port "$volume")"; name="${run_id}-prep-${volume}"
  docker volume create "$volume" >/dev/null
  start "$name" "$old" "$volume" "$p"
  if ! ready_within "$p" 180 "$name"; then
    docker logs "$name" >&2 2>&1 || true
    docker rm -f "$name" >/dev/null 2>&1 || true
    say "the v0.2.0 image did not become ready on $volume"
    return 1
  fi
  docker exec "$name" rahi backup >&2
  archive="$(docker exec "$name" sh -c 'ls -1 /data/backups/*.tar.age 2>/dev/null | tail -1')"
  docker stop -t 50 "$name" >/dev/null
  docker rm "$name" >/dev/null
  # The ledger verbs of v0.2.0 open the node themselves, so they run on the
  # stopped volume.
  verb "$old" "$volume" ledger export /data/pre-upgrade-chain.jsonl >&2
  echo "$archive"
}

# The app store's tree and the fence marker's bytes: what AC-3a and AC-4 (d)
# hold unchanged. Debris a refused old start leaves beside the fence is
# allowed (B-5) and not compared.
guarded_state() {
  tree "$1" app-store
  tree "$1" hiqlite/state_machine/lock
}

# The v0.2.0 image, in each of its three forms, on `volume` for `seconds`:
# it must serve nothing, spawn no Rauthy, and change neither the app store
# nor the fence.
old_serves_nothing() {
  label="$1"; volume="$2"; seconds="$3"
  before="$(guarded_state "$volume")"
  for form in entrypoint supervise serve; do
    p="$(vol_port "$volume")"; name="${run_id}-old-${form}-${p}"
    case "$form" in
      entrypoint) start "$name" "$old" "$volume" "$p" ;;
      *) start "$name" "$old" "$volume" "$p" "$form" ;;
    esac
    served=no; spawned=no; i=0
    while [ "$i" -lt "$seconds" ]; do
      if curl -fsS "http://localhost:${p}/readyz" >/dev/null 2>&1; then served=yes; fi
      if rauthy_running "$name"; then spawned=yes; fi
      sleep 1; i=$((i + 1))
    done
    docker rm -f "$name" >/dev/null
    after="$(guarded_state "$volume")"
    [ "$after" = "$before" ] || show_change "$before" "$after"
    if [ "$served" = no ] && [ "$spawned" = no ] && [ "$before" = "$after" ]; then
      pass "$label: v0.2.0 $form serves nothing, spawns no Rauthy, changes no path"
    else
      fail "$label: v0.2.0 $form served=$served spawned_rauthy=$spawned tree_changed=$([ "$before" = "$after" ] && echo no || echo yes)"
    fi
  done
}

# The new image's default entrypoint on `volume` reaches ready and `done`.
new_reaches_done() {
  label="$1"; volume="$2"; p="$(vol_port "$volume")"; name="${run_id}-new-${p}-$$-$(date +%s)"
  start "$name" "$new" "$volume" "$p"
  if ! ready_within "$p" 180 "$name"; then
    docker logs "$name" >&2 2>&1 || true
    fail "$label: the new image did not become ready"
    docker rm -f "$name" >/dev/null
    return
  fi
  i=0; phase=""
  while [ "$i" -lt 30 ]; do
    phase="$(docker exec "$name" sh -c 'cat /data/upgrade-cache.json 2>/dev/null' | jq -r '.phase // empty' 2>/dev/null || true)"
    [ "$phase" = "done" ] && break
    sleep 1; i=$((i + 1))
  done
  if [ "$phase" = "done" ]; then pass "$label: the new image serves and records done"; else fail "$label: phase is '$phase'"; fi
  docker stop -t 50 "$name" >/dev/null
  docker rm "$name" >/dev/null
  if verb "$new" "$volume" ledger verify --full >/dev/null 2>&1; then
    pass "$label: ledger verify --full"
  else
    fail "$label: ledger verify --full"
  fi
  verb "$new" "$volume" ledger export /data/post-upgrade-chain.jsonl >/dev/null 2>&1 || true
}

# ---------------------------------------------------------------- AC-3
ac3() {
  vol="${run_id}-ac3"
  archive="$(old_volume "$vol")" || { fail "AC-3: the v0.2.0 volume could not be prepared"; return; }
  keys_before="$(tree "$vol" keys)"
  before="$(tree "$vol")"

  p="$(vol_port "$vol")"; name="${run_id}-ac3-refused"
  start "$name" "$new" "$vol" "$p"
  docker wait "$name" >/dev/null 2>&1 &
  waiter=$!
  i=0
  while [ "$i" -lt 90 ] && [ "$(docker inspect -f '{{.State.Running}}' "$name")" = "true" ]; do sleep 1; i=$((i + 1)); done
  code="$(docker inspect -f '{{.State.ExitCode}}' "$name")"
  running="$(docker inspect -f '{{.State.Running}}' "$name")"
  if docker logs "$name" 2>&1 | grep -q "rauthy is healthy"; then spawned=yes; else spawned=no; fi
  docker rm -f "$name" >/dev/null; wait "$waiter" 2>/dev/null || true
  after="$(tree "$vol")"
  [ "$after" = "$before" ] || show_change "$before" "$after"
  if [ "$running" = false ] && [ "$code" != 0 ] && [ "$spawned" = no ] && [ "$after" = "$before" ]; then
    pass "AC-3: the new image without the verb refuses (exit $code) before spawning Rauthy and changes nothing"
  else
    fail "AC-3: the new image without the verb: running=$running exit=$code spawned=$spawned changed=$([ "$(tree "$vol")" = "$before" ] && echo no || echo yes)"
  fi

  if verb "$new" "$vol" upgrade-cache --backup /data/no-such-archive.age >/dev/null 2>&1; then
    fail "AC-3: the verb accepted an archive that does not exist"
  elif [ "$(tree "$vol")" = "$before" ]; then
    pass "AC-3: the verb without a verifying archive refuses and changes nothing"
  else
    show_change "$before" "$(tree "$vol")"
    fail "AC-3: the refused verb changed the volume"
  fi

  if out="$(verb "$new" "$vol" upgrade-cache --backup "$archive" 2>&1)" && printf '%s' "$out" | grep -q "is floored at"; then
    pass "AC-3: the verb with the archive reaches floored"
  else
    say "$out"
    fail "AC-3: the verb with the archive did not reach floored"
    return
  fi
  if printf '%s' "$out" | grep -q "Every old process is stopped"; then
    pass "AC-5: the verb's output carries B-4's preconditions"
  else
    fail "AC-5: the verb's output lacks B-4's preconditions"
  fi

  new_reaches_done "AC-3" "$vol"
  fence="$(docker run --rm -v "${vol}:/data" --entrypoint sh "$new" -c 'find /data/hiqlite -type f | sort; cat /data/hiqlite/state_machine/lock 2>/dev/null')"
  # B-5: on an upgraded volume the guard T1 wrote is kept as the fence.
  if printf '%s' "$fence" | grep -q "^rahi-upgrade-cache "; then
    pass "AC-3: the legacy path is the fence (the guard, kept)"
  else
    fail "AC-3: the legacy path is not the fence: $fence"
  fi
  aside="$(docker run --rm -v "${vol}:/data" --entrypoint sh "$new" -c 'find /data -maxdepth 4 -path "*aside*" -type d | sort')"
  if [ -n "$aside" ]; then pass "AC-3: the caches are aside ($(printf '%s' "$aside" | tr '\n' ' '))"; else fail "AC-3: no aside directory"; fi
  if [ "$(tree "$vol" keys)" = "$keys_before" ]; then pass "AC-3: the key set is unchanged"; else fail "AC-3: the key set changed"; fi
  prefix="$(docker run --rm -v "${vol}:/data" --entrypoint sh "$new" -c '
    n=$(wc -l < /data/pre-upgrade-chain.jsonl)
    head -n "$n" /data/post-upgrade-chain.jsonl | cmp -s - /data/pre-upgrade-chain.jsonl && echo same' || true)"
  if [ "$prefix" = same ]; then pass "AC-3: the chain is unchanged"; else fail "AC-3: the chain changed"; fi

  p="$(vol_port "$vol")"; name="${run_id}-ac3-second"
  start "$name" "$new" "$vol" "$p"
  if ready_within "$p" 180 "$name" && ! docker logs "$name" 2>&1 | grep -q "the transition is"; then
    pass "AC-3: a second boot needs nothing"
  else
    fail "AC-3: the second boot"
  fi
  docker stop -t 50 "$name" >/dev/null; docker rm "$name" >/dev/null
}

# --------------------------------------------------------------- AC-3a
ac3a() {
  vol="${run_id}-ac3a"
  docker volume create "$vol" >/dev/null
  p="$(vol_port "$vol")"; name="${run_id}-ac3a-new"
  start "$name" "$new" "$vol" "$p"
  if ready_within "$p" 180 "$name"; then
    pass "AC-3a: the new image's first boot on an empty volume reaches ready without the verb"
  else
    docker logs "$name" >&2 2>&1 || true
    fail "AC-3a: the new image did not become ready on an empty volume"
  fi
  docker stop -t 50 "$name" >/dev/null; docker rm "$name" >/dev/null
  fences="$(docker run --rm -v "${vol}:/data" --entrypoint sh "$new" -c '
    cat /data/hiqlite/state_machine/lock 2>/dev/null | head -c 11; echo
    cat /data/rauthy/rauthy.env/FENCE 2>/dev/null | head -c 11; echo')"
  if [ "$(printf '%s' "$fences" | grep -c '^rahi-fence ')" = 2 ]; then
    pass "AC-3a: both fences exist"
  else
    fail "AC-3a: the fences are not both there: $fences"
  fi
  rauthy_before="$(tree "$vol" rauthy)"
  old_serves_nothing "AC-3a" "$vol" 45
  if [ "$(tree "$vol" rauthy)" = "$rauthy_before" ]; then
    pass "AC-3a: no path under Rauthy's storage changed"
  else
    fail "AC-3a: Rauthy's storage changed"
  fi
}

# ---------------------------------------------------------- AC-4 (d)
ac4d() {
  for state in begin guarded t2.move.0 relocated floored; do
    vol="${run_id}-ac4-$(printf '%s' "$state" | tr '.' '-')"
    archive="$(old_volume "$vol")" || { fail "AC-4 (d) $state: preparation"; continue; }
    code=0
    CRASH_AT="$state" verb "$new" "$vol" upgrade-cache --backup "$archive" >/dev/null 2>&1 || code=$?
    if [ "$code" != 137 ]; then fail "AC-4 (d) $state: the verb did not stop there (exit $code)"; continue; fi
    if [ "$state" = begin ]; then
      # No guard yet: the old image may serve on the untouched legacy store.
      p="$(vol_port "$vol")"; name="${run_id}-ac4-begin-old"
      start "$name" "$old" "$vol" "$p"
      if ready_within "$p" 180 "$name"; then
        pass "AC-4 (d) begin: v0.2.0 serves on the untouched legacy store"
      else
        fail "AC-4 (d) begin: v0.2.0 did not serve before the guard"
      fi
      docker stop -t 50 "$name" >/dev/null; docker rm "$name" >/dev/null
    else
      old_serves_nothing "AC-4 (d) $state" "$vol" 30
    fi
    if verb "$new" "$vol" upgrade-cache --backup "$archive" >/dev/null 2>&1; then
      pass "AC-4 (a) $state: a rerun of the verb reaches floored"
    else
      fail "AC-4 (a) $state: the rerun failed"
      continue
    fi
    new_reaches_done "AC-4 (a) $state" "$vol"
  done
}

# ------------------------------------------------------------- AC-5
ac5() {
  vol="${run_id}-ac5"
  archive="$(old_volume "$vol")" || { fail "AC-5: preparation"; return; }
  p="$(vol_port "$vol")"; name="${run_id}-ac5-live"
  start "$name" "$old" "$vol" "$p"
  ready_within "$p" 180 "$name" || { fail "AC-5: the v0.2.0 cell did not start"; return; }
  out="$(verb "$new" "$vol" upgrade-cache --backup "$archive" 2>&1 || true)"
  if printf '%s' "$out" | grep -q "a pre-043 node is live on this volume"; then
    pass "AC-5: with a live v0.2.0 cell the verb refuses naming a live node"
  else
    say "$out"; fail "AC-5: the live-cell refusal"
  fi
  docker kill "$name" >/dev/null; docker rm "$name" >/dev/null
  out="$(verb "$new" "$vol" upgrade-cache --backup "$archive" 2>&1 || true)"
  marker="$(docker run --rm -v "${vol}:/data" --entrypoint sh "$new" -c 'test -e /data/hiqlite/state_machine/lock && echo present')"
  if printf '%s' "$out" | grep -q "stopped uncleanly" && [ "$marker" = present ]; then
    pass "AC-5: after an unclean v0.2.0 stop the verb refuses naming it and leaves the marker"
  else
    say "$out"; fail "AC-5: the unclean-stop refusal (marker: $marker)"
  fi
  for var in HQL_CACHE_LEGACY_MOVE_ASIDE HQL_DANGER_RAFT_STATE_RESET HQL_BACKUP_RESTORE; do
    before="$(tree "$vol")"
    if docker run --rm -e RAHI_PUBLIC_URL="http://localhost:$(vol_port "$vol")" -e "$var=true" -v "${vol}:/data" \
        --entrypoint /usr/local/bin/rahi "$new" serve >/dev/null 2>&1; then
      fail "AC-5: serve with $var set did not refuse"
    elif [ "$(tree "$vol")" = "$before" ]; then
      pass "AC-5: serve with $var set refuses and changes nothing"
    else
      fail "AC-5: serve with $var set changed the volume"
    fi
  done
}

ac3
ac3a
ac4d
ac5
unexecuted "AC-4 (g): no seam in the pinned Rauthy build injects a crash between its two cache renames (hiqlite F-130); the outcome of that interruption is not recorded by this run"
unexecuted "AC-5: the real v0.2.0 stop and start races at offsets around T1 (D-P12, D-P13) need a harness that times a signal inside T1; FR-012's library interleavings cover the orders"
unexecuted "v0.1.0: not run by this script; v0.1.0's exclusion stays source-established (AC-9)"

say ""
say "spec 043 live upgrade legs against $new (old: $old):"
printf '%s' "$results" >&2
if [ "$failures" -gt 0 ]; then
  say "$failures leg(s) failed"
  exit 1
fi
say "every executed leg passed"
