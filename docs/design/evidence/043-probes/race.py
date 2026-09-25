#!/usr/bin/env python3
"""043 revision 3 probes: the T1 guard protocol against a real hiqlite 0.14 node
that is starting (S1) or stopping (S2). Disposable; dynamic ports; one scenario
per invocation; each run bounded at 60 s by the caller."""
import fcntl, hashlib, os, shutil, socket, subprocess, sys, time

SP = os.path.dirname(os.path.abspath(__file__))
OLD = f"{SP}/old/target/debug/probe-old"
GID = "rahi-upgrade-cache 7f3a"

def ports():
    out = []
    socks = []
    for _ in range(2):
        s = socket.socket(); s.bind(("127.0.0.1", 0)); socks.append(s); out.append(s.getsockname()[1])
    for s in socks: s.close()
    return out

def env():
    pr, pa = ports(); e = dict(os.environ); e["PR"], e["PA"] = str(pr), str(pa); return e

def base(root):
    shutil.rmtree(root, ignore_errors=True); os.makedirs(root)
    r = subprocess.run([OLD, f"{root}/hiqlite", "write"], env=env(), capture_output=True, timeout=60)
    assert b"shutdown=Ok" in r.stdout, r.stdout[-400:]

def held(path):
    try: fd = os.open(path, os.O_RDONLY)
    except FileNotFoundError: return None
    try:
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB); fcntl.flock(fd, fcntl.LOCK_UN); return False
    except BlockingIOError: return True
    finally: os.close(fd)

def fsync_dir(d):
    fd = os.open(d, os.O_RDONLY); os.fsync(fd); os.close(fd)

def t1(legacy, gid=GID):
    """Revision 3's T1: whole-content guard by link(2), then probe, then re-verify identity."""
    sm = f"{legacy}/state_machine"; marker = f"{sm}/lock"; tmp = f"{sm}/.rahi-guard.tmp"
    with open(tmp, "w") as f: f.write(gid); f.flush(); os.fsync(f.fileno())
    try: os.link(tmp, marker)
    except FileExistsError:
        os.unlink(tmp); return "refuse:marker-exists", None
    fsync_dir(sm); ino = os.stat(marker).st_ino; os.unlink(tmp)
    locks = {n: held(f"{legacy}/{n}/lock.hql") for n in ("logs", "logs_cache")}
    try:
        st = os.stat(marker); content = open(marker).read()
    except FileNotFoundError:
        return "refuse:guard-vanished", locks
    if st.st_ino != ino or content != gid:
        return f"refuse:guard-altered(ino_same={st.st_ino == ino},len={len(content)})", locks
    if any(locks.values()):
        return "refuse:wal-held", locks
    db = f"{legacy}/state_machine/db"
    sq = [n for n in (os.listdir(db) if os.path.isdir(db) else []) if n.endswith(("-wal", "-shm"))]
    if sq:
        return "rev2-proceed/rev3-refuse:sqlite-open", locks
    return "proceed", locks

def tree(root):
    h = {}
    for d, _, fs in os.walk(root):
        for n in fs:
            p = os.path.join(d, n)
            try: h[os.path.relpath(p, root)] = hashlib.sha256(open(p, "rb").read()).hexdigest()[:12]
            except FileNotFoundError: pass
    return h

def s1(work, offsets_ms):
    """Start race: 0.14 launched, T1 runs d ms later."""
    for d in offsets_ms:
        root = f"{work}/s1-{d}"; base(root); lg = f"{root}/hiqlite"
        p = subprocess.Popen([OLD, lg, "hang"], env=env(), stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        time.sleep(d / 10000); verdict, locks = t1(lg)
        try: out, _ = p.communicate(timeout=2)
        except subprocess.TimeoutExpired:
            p.kill(); out, _ = p.communicate()
        out = out.decode(errors="replace")
        started = "HANGING" in out
        why = "started" if started else ("marker-panic" if "Lock file already exists" in out else
              ("wal-lock-err" if "WAL lock" in out or "LockFile" in out else "other:" + out.strip().splitlines()[-1][:80] if out.strip() else "no-output"))
        m = f"{lg}/state_machine/lock"
        mc = open(m).read() if os.path.exists(m) else None
        bad = verdict == "proceed" and started
        print(f"d={d:>3}ms T1={verdict:<28} locks={locks} old={why:<14} marker={mc!r}{'  VIOLATION' if bad else ''}", flush=True)

def s2(work, offsets_ms):
    """Stop race: 0.14 starts, then shuts down cleanly; T1 runs d ms after the shutdown begins.
    A 'proceed' verdict followed by any change under the legacy store is a violation."""
    for d in offsets_ms:
        root = f"{work}/s2-{d}"; base(root); lg = f"{root}/hiqlite"
        p = subprocess.Popen([OLD, lg, "sleepstop"], env=env(), stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        # the default mode prints sql_rows then calls shutdown; wait for that line
        buf = b""
        while b"sql_rows" not in buf:
            c = p.stdout.read1(4096) if hasattr(p.stdout, "read1") else p.stdout.read(1)
            if not c: break
            buf += c
        time.sleep(d / 10000)
        verdict, locks = t1(lg)
        snap = tree(lg) if verdict.startswith(("proceed", "rev2-proceed")) else None
        out, _ = p.communicate(timeout=30); out = (buf + out).decode(errors="replace")
        changed = None
        if snap is not None:
            after = tree(lg); changed = sorted(k for k in set(snap) | set(after) if snap.get(k) != after.get(k) and k != "state_machine/lock")
        m = f"{lg}/state_machine/lock"; mc = open(m).read() if os.path.exists(m) else None
        sd = [l for l in out.splitlines() if l.startswith("shutdown=")]
        bad = snap is not None and (changed or mc != GID)
        print(f"d={d:>3}ms T1={verdict:<28} locks={locks} old={sd[0][:30] if sd else 'no-shutdown-line'} after_proceed_changed={changed} marker={mc!r}{'  VIOLATION' if bad else ''}", flush=True)

def timeline(work):
    """Sample one clean 0.14 stop: marker, both WAL locks, db-wal size, process."""
    root = f"{work}/tl"; base(root); lg = f"{root}/hiqlite"
    p = subprocess.Popen([OLD, lg, "sleepstop"], env=env(), stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    buf = b""
    while b"sql_rows" not in buf:
        c = p.stdout.read1(4096)
        if not c: break
        buf += c
    t0 = time.monotonic(); last = None; rows = []
    while True:
        alive = p.poll() is None
        dbw = [f for f in os.listdir(f"{lg}/state_machine/db")] if os.path.isdir(f"{lg}/state_machine/db") else []
        st = (os.path.exists(f"{lg}/state_machine/lock"), held(f"{lg}/logs/lock.hql"), held(f"{lg}/logs_cache/lock.hql"),
              tuple(sorted(f for f in dbw if f.endswith(("-wal", "-shm")))), alive)
        if st != last: rows.append((round((time.monotonic() - t0) * 1000, 2), st)); last = st
        if not alive: break
        if time.monotonic() - t0 > 30: p.kill(); break
    print("ms  (marker, logs_lock_held, logs_cache_lock_held, db_wal_shm, alive)")
    for r in rows: print(r)
    print(p.communicate()[0].decode()[-300:])

if __name__ == "__main__":
    work = sys.argv[2]; os.makedirs(work, exist_ok=True)
    {"s1": lambda: s1(work, [int(x) for x in sys.argv[3].split(",")]),
     "s2": lambda: s2(work, [int(x) for x in sys.argv[3].split(",")]),
     "tl": lambda: timeline(work)}[sys.argv[1]]()
