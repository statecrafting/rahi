#!/bin/bash
SP=$(cd "$(dirname "$0")" && pwd); OLDB=$SP/old/target/debug/probe-old; NEWB=$SP/new/target/debug/probe-new; R=$SP/runs
run(){ timeout --foreground 60 "$@" > $R/last.out 2>&1; rc=$?; grep -E 'START_ERR|started in|sql_rows|shutdown=|panicked at|HOLD|RELEASED|Lock file|LockFile|StorageInUse' $R/last.out | sed "s#$R#<R>#g" | cut -c1-240 | head -6; echo "  exit=$rc"; }
echo "=== transition with consent (0.15), clean"; HQL_CACHE_LEGACY_MOVE_ASIDE=true run $NEWB $R/g1/s
for x in b c d; do rm -rf $R/g1$x; cp -cR $R/g1/s $R/g1$x; done
echo "=== G1-a: own fd holds hiqlite-owner.lock, then in-process start"; run $NEWB $R/g1/s selfhold-owner
echo "=== G1-b: own fd holds logs/lock.hql, then in-process start"; run $NEWB $R/g1b selfhold-wal
echo "=== G1-c: hold both, release, then start"; run $NEWB $R/g1c hold-release
echo "=== G1-d: another process holds the owner lock"; python3 -c "
import fcntl,os,time; fd=os.open('$R/g1d/hiqlite-owner.lock',os.O_RDWR); fcntl.flock(fd,fcntl.LOCK_EX|fcntl.LOCK_NB); time.sleep(6)" & sleep 1; run $NEWB $R/g1d; wait
