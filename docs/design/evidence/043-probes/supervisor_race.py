#!/usr/bin/env python3
"""Spec 043 revision 4, D-P18: a deterministic model of a pre-043
supervisor that prepared its Rauthy command before the supervisor fence was
installed and spawns it afterwards.

A model of the published source path, not a run of the published binary.
v0.2.0 `crates/rahi-cli/src/lib.rs` `supervise` calls
`sup::prepare_rauthy(&config, env)?` (v0.1.0: `sup::rauthy_command`), which
reads `<data>/rauthy/rauthy.env` with `read_to_string` into a
`tokio::process::Command` (`env_clear`, `envs(parse(rendered))`); it then
builds the admin client and calls `sup::supervise`, whose `supervise_with`
calls `rauthy.spawn()`. Nothing between the read and the spawn reads the
volume again. This model performs the same three operations in a fixed
order in a disposable directory, with a real child process standing in for
Rauthy, and installs the fence exactly as 043 B-5 describes between them.

Scenario (one), with its control:
  A  prepare, then install the fence (T1 (e)), then write a stand-in for
     T4's 0.15 marker, then spawn: the prepared command still spawns.
  B  install the fence, then prepare: the read fails (EISDIR) before spawn.

Bounded: 60 s alarm, a temporary directory removed at exit, no volume, no
network. Exit 0 means A spawned and B refused, as the revision 4 text says.
"""

import errno
import os
import signal
import subprocess
import sys
import tempfile

signal.signal(signal.SIGALRM, lambda *_: sys.exit("timed out after 60 s"))
signal.alarm(60)


def render(data):
    os.makedirs(os.path.join(data, "rauthy"), exist_ok=True)
    with open(os.path.join(data, "rauthy", "rauthy.env"), "w") as f:
        f.write("HQL_NODE_ID=1\nHQL_DATA_DIR=%s\n" % os.path.join(data, "rauthy"))


def prepare(data):
    """prepare_rauthy: read_to_string, then build the command in memory."""
    with open(os.path.join(data, "rauthy", "rauthy.env")) as f:
        rendered = f.read()
    env = dict(line.split("=", 1) for line in rendered.splitlines() if "=" in line)
    env["PATH"] = os.environ.get("PATH", "")
    marker = os.path.join(data, "rauthy", "SPAWNED-BY-OLD-SUPERVISOR")
    argv = [sys.executable, "-c", f"open({marker!r}, 'w').write('old rauthy would open its storage here')"]
    return argv, env, marker


def install_fence(data, fence_id):
    """043 B-5, T1 (e): new env beside, FENCE in a temporary directory, the
    old file to evidence, then a no-replace rename of the directory into
    place (modelled with os.rename onto a path that is absent at that point)."""
    new_dir = os.path.join(data, "rauthy-env")
    os.makedirs(new_dir, exist_ok=True)
    old = os.path.join(data, "rauthy", "rauthy.env")
    with open(old) as f:
        content = f.read()
    tmp = os.path.join(new_dir, ".rauthy.env.tmp")
    with open(tmp, "w") as f:
        f.write(content)
        f.flush()
        os.fsync(f.fileno())
    os.rename(tmp, os.path.join(new_dir, "rauthy.env"))
    staging = os.path.join(data, "rauthy", f".rahi-fence-{fence_id}")
    os.mkdir(staging)
    with open(os.path.join(staging, "FENCE"), "w") as f:
        f.write(f"rahi-fence {fence_id}\n")
    evidence = os.path.join(data, "upgrade-cache", "evidence", fence_id)
    os.makedirs(evidence)
    os.rename(old, os.path.join(evidence, "rauthy.env"))
    assert not os.path.exists(old)
    os.rename(staging, old)


def t4_stand_in(data):
    os.makedirs(os.path.join(data, "rauthy", "logs_cache"), exist_ok=True)
    with open(os.path.join(data, "rauthy", "logs_cache", "hiqlite-0.15-format"), "w") as f:
        f.write("stand-in for the 0.15 cache Rauthy wrote at T4\n")


def spawn(argv, env):
    return subprocess.run(argv, env=env, timeout=20).returncode


def main():
    failures = []
    with tempfile.TemporaryDirectory(prefix="rahi-043-supervisor-") as root:
        a = os.path.join(root, "a")
        render(a)
        argv, env, marker = prepare(a)  # the old supervisor has read the file
        install_fence(a, "a" * 32)  # T1 (e) runs while it is between read and spawn
        t4_stand_in(a)  # and the transition reaches T4
        code = spawn(argv, env)  # the old supervisor resumes
        spawned = os.path.exists(marker)
        print(f"A prepare -> fence -> T4 -> spawn: child exit {code}, spawned={spawned}")
        print(f"  fence in place: {os.path.isdir(os.path.join(a, 'rauthy', 'rauthy.env'))}")
        if not (code == 0 and spawned):
            failures.append("A: the prepared command did not spawn")

        b = os.path.join(root, "b")
        render(b)
        install_fence(b, "b" * 32)
        try:
            prepare(b)
            refused = None
        except OSError as err:
            refused = err.errno
        name = errno.errorcode.get(refused, refused)
        print(f"B fence -> prepare: read failed with {name}, nothing spawned")
        if refused != errno.EISDIR:
            failures.append(f"B: expected EISDIR, got {name}")
    if failures:
        print("FAIL:", failures)
        return 1
    print("the fence stops a later read; it cannot invalidate a command already prepared")
    return 0


if __name__ == "__main__":
    sys.exit(main())
