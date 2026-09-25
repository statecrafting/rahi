#!/bin/bash
# Old-version (hiqlite 0.14 @ 8f3b9bd, v0.2.0's) start attempted at every persistent intermediate state.
SP=$(cd "$(dirname "$0")" && pwd); OLDB=$SP/old/target/debug/probe-old; NEWB=$SP/new/target/debug/probe-new; R=$SP/runs2
rm -rf $R; mkdir -p $R
run(){ timeout --foreground 60 "$@" > $R/last.out 2>&1; rc=$?; grep -E 'START_ERR|started in|sql_rows|shutdown=|panicked at|Lock file|LockFile|StorageInUse|already exists|FileCorrupted' $R/last.out | sed "s#$R#<R>#g" | cut -c1-200 | head -5; echo "  exit=$rc"; }
old_attempt(){ # $1 = dir the old binary is pointed at, $2 = tree root to hash
  $SP/snap.sh "$2" > $R/before; run $OLDB "$1"; $SP/snap.sh "$2" > $R/after
  if cmp -s $R/before $R/after; then echo "  TREE UNCHANGED"; else echo "  TREE CHANGED:"; diff $R/before $R/after | sed 's/^/    /' | head -12; fi; }
fresh(){ rm -rf "$1"; mkdir -p "$1"; timeout --foreground 60 $OLDB "$1/hiqlite" write >/dev/null 2>&1 || echo "base write failed"; }
G='rahi-upgrade-cache 7f3a'
kill_during(){ # start 0.15 in hang mode at $1, SIGKILL once it serves
  $NEWB "$1" hang > $R/hang.out 2>&1 & p=$!; for i in $(seq 1 100); do grep -q HANGING $R/hang.out && break; sleep 0.1; done; kill -9 $p; wait $p 2>/dev/null; echo "  0.15 SIGKILLed while running: $(grep -c HANGING $R/hang.out) serving line(s)"; }

echo "##### 5707f60 layout (in place)"
echo "=== O1 verified: nothing moved, T0 crash left empty legacy lock files"; fresh $R/o1; touch $R/o1/hiqlite/logs/lock.hql $R/o1/hiqlite/logs_cache/lock.hql; old_attempt $R/o1/hiqlite $R/o1
echo "=== O2 moved: caches in pre-upgrade-*, no guard (crash after T2)"; fresh $R/o2; mkdir $R/o2/hiqlite/pre-upgrade-1; mv $R/o2/hiqlite/logs_cache $R/o2/hiqlite/state_machine_cache $R/o2/hiqlite/pre-upgrade-1/; old_attempt $R/o2/hiqlite $R/o2
echo "=== O3 flooring: 0.15 open over moved caches, SIGKILL mid-T3"; fresh $R/o3; mkdir $R/o3/hiqlite/pre-upgrade-1; mv $R/o3/hiqlite/logs_cache $R/o3/hiqlite/state_machine_cache $R/o3/hiqlite/pre-upgrade-1/; kill_during $R/o3/hiqlite; echo "  marker: $(ls -la $R/o3/hiqlite/state_machine/lock 2>&1 | awk '{print $5, $NF}')"; old_attempt $R/o3/hiqlite $R/o3
echo "=== O4 floored: 0.15 opened and stopped cleanly, no guard yet (crash before T4)"; fresh $R/o4; mkdir $R/o4/hiqlite/pre-upgrade-1; mv $R/o4/hiqlite/logs_cache $R/o4/hiqlite/state_machine_cache $R/o4/hiqlite/pre-upgrade-1/; run $NEWB $R/o4/hiqlite floor; old_attempt $R/o4/hiqlite $R/o4
echo "=== O4b same, then a second 0.14 start (after the first's effects)"; old_attempt $R/o4/hiqlite $R/o4
echo "=== O5 armed: guard written"; fresh $R/o5; mkdir $R/o5/hiqlite/pre-upgrade-1; mv $R/o5/hiqlite/logs_cache $R/o5/hiqlite/state_machine_cache $R/o5/hiqlite/pre-upgrade-1/; run $NEWB $R/o5/hiqlite floor >/dev/null; printf '%s' "$G" > $R/o5/hiqlite/state_machine/lock; old_attempt $R/o5/hiqlite $R/o5

echo "##### proposed fence layout"
echo "=== N1 guard written in place into the intact 0.14 store"; fresh $R/n1; printf '%s' "$G" > $R/n1/hiqlite/state_machine/lock; old_attempt $R/n1/hiqlite $R/n1
echo "=== N2 partial relocation: logs and db moved to app-store, guard in place"; fresh $R/n2; printf '%s' "$G" > $R/n2/hiqlite/state_machine/lock; mkdir -p $R/n2/app-store/state_machine; mv $R/n2/hiqlite/logs $R/n2/app-store/; mv $R/n2/hiqlite/state_machine/db $R/n2/app-store/state_machine/; old_attempt $R/n2/hiqlite $R/n2
echo "=== N3 fence only: hiqlite/ holds just state_machine/lock"; fresh $R/n3; printf '%s' "$G" > $R/n3/hiqlite/state_machine/lock; mkdir -p $R/n3/app-store/state_machine $R/n3/app-store/pre-upgrade-1
  mv $R/n3/hiqlite/logs $R/n3/app-store/; for d in db snapshots backups; do mv $R/n3/hiqlite/state_machine/$d $R/n3/app-store/state_machine/; done; mv $R/n3/hiqlite/logs_cache $R/n3/hiqlite/state_machine_cache $R/n3/app-store/pre-upgrade-1/; $SP/snap.sh $R/n3/hiqlite | sed 's/^/    fence: /'; old_attempt $R/n3/hiqlite $R/n3
echo "=== N4 0.15 opens the relocated store at the new path, no consent needed"; run $NEWB $R/n3/app-store floor
echo "=== N5 T3 crash at the new path, marker preserved as evidence, reopen"; kill_during $R/n3/app-store; mkdir -p $R/n3/evidence; mv $R/n3/app-store/state_machine/lock $R/n3/evidence/t3-marker-1; run $NEWB $R/n3/app-store
echo "=== N6 old start on the fence after 0.15 ran at the new path"; old_attempt $R/n3/hiqlite $R/n3
echo "=== N7 old start pointed at a fresh-volume fence (new binary created it, no legacy store ever)"; mkdir -p $R/n7/hiqlite/state_machine; printf 'rahi-fence 0.3.0' > $R/n7/hiqlite/state_machine/lock; old_attempt $R/n7/hiqlite $R/n7
