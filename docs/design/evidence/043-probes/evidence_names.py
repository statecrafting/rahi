#!/usr/bin/env python3
"""Spec 043 revision 4, D-P19: evidence names unique per occurrence, under
no-replace renames, with interrupted-move recovery by identity.

Real filesystem calls in a disposable directory: `renamex_np(RENAME_EXCL)`
on macOS or `renameat2(RENAME_NOREPLACE)` on Linux through ctypes, and
plain `rename(2)` only as the negative control. No rahi code runs; the
naming and recovery rules are 043 B-4's, implemented here in miniature.

Checks:
  1. no-replace onto an existing file and onto an empty and a non-empty
     directory fails with EEXIST and changes neither side; plain rename
     onto an empty directory silently replaces it (why FR-011 forbids it);
  2. revision 3's fixed name, `evidence/<id>/logs`, meets its own earlier
     occurrence on a second refused old start: the no-replace rename
     refuses, nothing is overwritten, and the verb cannot proceed;
  3. revision 4's names, `evidence/<id>/<seq>-<step>/<relative path>` with
     `seq` allocated in the recorded intent, never collide across three
     occurrences, and recovery after an interruption between the intent
     and the rename, and after the rename, decides by identity;
  4. a destination holding an identity the intent does not name refuses.
Exit 0 means every expectation held.
"""

import ctypes
import ctypes.util
import errno
import json
import os
import platform
import sys
import tempfile

libc = ctypes.CDLL(ctypes.util.find_library("c"), use_errno=True)


def rename_noreplace(src, dst):
    if platform.system() == "Darwin":
        fn, args = libc.renamex_np, (src.encode(), dst.encode(), 0x00000004)  # RENAME_EXCL
    else:
        fn = libc.renameat2
        args = (-100, src.encode(), -100, dst.encode(), 1)  # AT_FDCWD, RENAME_NOREPLACE
    if fn(*args) != 0:
        e = ctypes.get_errno()
        raise OSError(e, os.strerror(e), dst)


def ident(path):
    st = os.lstat(path)
    return [st.st_dev, st.st_ino]


def tree(path):
    out = {}
    for base, dirs, files in os.walk(path):
        for name in dirs + files:
            p = os.path.join(base, name)
            rel = os.path.relpath(p, path)
            out[rel] = open(p, "rb").read() if os.path.isfile(p) else "dir"
    return out


FAILURES = []


def expect(label, got, want):
    ok = got == want
    print(f"  {'ok  ' if ok else 'FAIL'} {label}: {got}")
    if not ok:
        FAILURES.append(label)


def debris(legacy, n):
    """What a refused pre-043 start leaves (hiqlite H-7 Q4): logs/ with a
    WAL, meta.hql and lock.hql; state_machine/db and backups."""
    os.makedirs(os.path.join(legacy, "logs"), exist_ok=True)
    for name, data in (("0000000000000001.wal", b"wal %d" % n), ("meta.hql", b"meta %d" % n), ("lock.hql", b"")):
        with open(os.path.join(legacy, "logs", name), "wb") as f:
            f.write(data)
    os.makedirs(os.path.join(legacy, "state_machine", "db"), exist_ok=True)
    os.makedirs(os.path.join(legacy, "state_machine", "backups"), exist_ok=True)


def check_noreplace(root):
    print("1. no-replace semantics")
    d = os.path.join(root, "nr")
    os.makedirs(os.path.join(d, "full"))
    os.makedirs(os.path.join(d, "empty"))
    open(os.path.join(d, "full", "x"), "w").write("x")
    open(os.path.join(d, "file"), "w").write("file")
    open(os.path.join(d, "src"), "w").write("src")
    os.makedirs(os.path.join(d, "srcdir"))
    open(os.path.join(d, "srcdir", "y"), "w").write("y")
    before = tree(d)
    for dst, src in (("file", "src"), ("empty", "srcdir"), ("full", "srcdir")):
        try:
            rename_noreplace(os.path.join(d, src), os.path.join(d, dst))
            code = 0
        except OSError as err:
            code = errno.errorcode[err.errno]
        expect(f"no-replace {src} onto {dst}", code, "EEXIST")
    expect("tree unchanged after the refusals", tree(d) == before, True)
    os.rename(os.path.join(d, "srcdir"), os.path.join(d, "empty"))
    expect("negative control: plain rename replaced the empty directory", os.path.exists(os.path.join(d, "empty", "y")), True)


def check_fixed_name(root):
    print("2. revision 3's fixed evidence name across two refused old starts")
    legacy = os.path.join(root, "r3", "hiqlite")
    ev = os.path.join(root, "r3", "upgrade-cache", "evidence", "id")
    os.makedirs(ev)
    debris(legacy, 1)
    rename_noreplace(os.path.join(legacy, "logs"), os.path.join(ev, "logs"))
    debris(legacy, 2)  # a second refused start recreates logs/
    first = tree(os.path.join(ev, "logs"))
    try:
        rename_noreplace(os.path.join(legacy, "logs"), os.path.join(ev, "logs"))
        code = 0
    except OSError as err:
        code = errno.errorcode[err.errno]
    expect("second occurrence to the same name", code, "EEXIST")
    expect("first occurrence's bytes intact (no overwrite)", tree(os.path.join(ev, "logs")) == first, True)
    expect("second occurrence still at the source (the verb is stuck)", os.path.exists(os.path.join(legacy, "logs", "meta.hql")), True)


class Verb:
    """B-4's evidence move in miniature: intent (with seq and identity)
    recorded durably, then the rename, then completion."""

    def __init__(self, data, tid):
        self.data, self.tid = data, tid
        self.state_path = os.path.join(data, "upgrade-cache.json")
        self.state = {"seq": 0, "intent": None}
        if os.path.exists(self.state_path):
            self.state = json.load(open(self.state_path))

    def save(self):
        tmp = self.state_path + ".tmp"
        with open(tmp, "w") as f:
            json.dump(self.state, f)
            f.flush()
            os.fsync(f.fileno())
        os.rename(tmp, self.state_path)

    def move_to_evidence(self, src, step, crash=None):
        self.state["seq"] += 1
        rel = os.path.relpath(src, os.path.join(self.data, "hiqlite"))
        dst = os.path.join(self.data, "upgrade-cache", "evidence", self.tid, f"{self.state['seq']:06d}-{step}", rel)
        self.state["intent"] = {"src": src, "dst": dst, "ident": ident(src)}
        self.save()
        if crash == "after-intent":
            return "crashed"
        os.makedirs(os.path.dirname(dst), exist_ok=False)
        rename_noreplace(src, dst)
        if crash == "after-rename":
            return "crashed"
        self.state["intent"] = None
        self.save()
        return dst

    def recover(self):
        it = self.state["intent"]
        if it is None:
            return "nothing to recover"
        src, dst, want = it["src"], it["dst"], it["ident"]
        at_dst = os.path.lexists(dst) and ident(dst) == want
        at_src = os.path.lexists(src) and ident(src) == want
        if at_dst:
            outcome = "moved"
        elif at_src:
            if os.path.lexists(dst):
                return "refuse: destination holds another identity"
            os.makedirs(os.path.dirname(dst), exist_ok=True)
            rename_noreplace(src, dst)
            outcome = "redone"
        else:
            return "refuse: planned identity at neither path"
        self.state["intent"] = None
        self.save()
        return outcome


def check_unique_names(root):
    print("3. revision 4's per-occurrence names, with interruption and recovery")
    data = os.path.join(root, "r4")
    legacy = os.path.join(data, "hiqlite")
    os.makedirs(legacy)
    verb = Verb(data, "c" * 32)
    dests = []
    debris(legacy, 1)
    dests.append(verb.move_to_evidence(os.path.join(legacy, "logs"), "t2"))
    debris(legacy, 2)
    expect("interrupted after the intent", verb.move_to_evidence(os.path.join(legacy, "logs"), "t2", crash="after-intent"), "crashed")
    verb = Verb(data, "c" * 32)  # a new process reads the recorded intent
    expect("recovery at the source identity", verb.recover(), "redone")
    dests.append(os.path.join(data, "upgrade-cache", "evidence", "c" * 32, "000002-t2", "logs"))
    debris(legacy, 3)
    expect("interrupted after the rename", verb.move_to_evidence(os.path.join(legacy, "logs"), "t2", crash="after-rename"), "crashed")
    verb = Verb(data, "c" * 32)
    expect("recovery at the destination identity", verb.recover(), "moved")
    dests.append(os.path.join(data, "upgrade-cache", "evidence", "c" * 32, "000003-t2", "logs"))
    walls = [open(os.path.join(d, "0000000000000001.wal"), "rb").read() for d in dests]
    expect("three occurrences, three names, each with its own bytes", walls, [b"wal 1", b"wal 2", b"wal 3"])
    expect("seq never reused", verb.state["seq"], 3)


def check_foreign(root):
    print("4. a destination holding an identity the intent does not name")
    data = os.path.join(root, "r4f")
    legacy = os.path.join(data, "hiqlite")
    os.makedirs(legacy)
    debris(legacy, 9)
    verb = Verb(data, "d" * 32)
    verb.move_to_evidence(os.path.join(legacy, "logs"), "t2", crash="after-intent")
    it = verb.state["intent"]
    os.makedirs(it["dst"])  # someone else created the destination
    before = tree(data)
    verb = Verb(data, "d" * 32)
    expect("recovery refuses", verb.recover(), "refuse: destination holds another identity")
    expect("nothing changed", tree(data) == before, True)


def main():
    print(f"platform: {platform.system()} {platform.machine()}")
    with tempfile.TemporaryDirectory(prefix="rahi-043-evidence-") as root:
        check_noreplace(root)
        check_fixed_name(root)
        check_unique_names(root)
        check_foreign(root)
    if FAILURES:
        print(f"{len(FAILURES)} expectation(s) failed: {FAILURES}")
        return 1
    print("all expectations held")
    return 0


if __name__ == "__main__":
    sys.exit(main())
