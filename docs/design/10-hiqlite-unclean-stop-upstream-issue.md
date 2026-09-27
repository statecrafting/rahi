# Proposed upstream issue: hiqlite's unclean-stop marker

Status: **draft, not filed.** Written for `sebadob/hiqlite` under spec 048
D-7. Filing it is the owner's act. The text below the rule is the issue as
proposed; everything above it is context for the reader in this repository.

rahi runs the downstream `hiqlite-patched 0.15.0-patched.3`, which already
returns a typed refusal instead of panicking (its `upgrade_exclusion.rs`,
step 3), takes an OS lock for ownership (its `storage_lock.rs`), and gates
health until the startup replay is applied (its `recovery.rs`). The issue
asks upstream for the same properties, so that a consumer on the upstream
crate (rahi up to 0.3.0 pinned `sebadob/hiqlite@8f3b9bd`, 0.14.0) gets
them. The evidence was gathered on 2026-09-27 against the crates.io sources
of 0.14.0 and 0.15.0 and against the probe in
`docs/design/evidence/048-probes/auto-heal/`.

---

## Title

Unclean-stop marker: return an error instead of panicking, make the
rebuild a runtime choice, and keep health false until the rebuild is applied

## Summary

`state_machine/lock` is created when a node starts and removed only by a
graceful `Client::shutdown()`. Any other exit leaves it: a `?` between
`start_node*` and `shutdown()`, a panic, a SIGTERM the embedding program
does not handle, SIGKILL after a grace period, power loss. What happens on
the next start depends on a compile-time feature:

- without `auto-heal`, `start_node*` **panics** with "Lock file already
  exists ... Node did not shut down gracefully - needs manual interaction";
- with `auto-heal` (a default feature), the SQLite state machine is deleted
  and rebuilt from the latest snapshot plus the Raft log, and the node
  reports healthy **before that replay has been applied**.

A downstream service hit the first: it used `?` between start and
shutdown and handled only Ctrl-C, so a container stop left the marker and
the next start panicked. Evaluating the second for a
single-node deployment found the transient partial state described below.

## Observed

1. **A library panic instead of an error.** 0.14.0
   `src/store/state_machine/sqlite/state_machine.rs:283` and 0.15.0 the same
   file, `check_set_lock_file`, `panic!` on a stale marker without
   `auto-heal`. 0.15.0 adds an `fs4` try-lock that tells a live owner apart
   from a stale marker (good), but also answers both cases, and a failure
   to open or lock the file, with `panic!`. The panic runs in the caller's
   task inside `start_node*`, so the embedding program cannot report a
   structured error, choose a recovery, or exit with its own code.

2. **The rebuild can only be chosen at compile time.** Cargo unifies
   features across the dependency graph: if any crate in a build enables
   `auto-heal`, every embedding of hiqlite in that build has it, and a
   crate that embeds hiqlite cannot offer the choice to its own users per
   deployment. Conversely, `auto-heal` is in `default`, so a consumer that
   forgets `default-features = false` gets automatic deletion of its
   state-machine database on every unclean stop without having chosen it.

3. **Health is reported before the rebuild is applied.** With 0.15.0 and
   `auto-heal`, a single voter with `default_raft_config(50)`: 30 writes,
   each one `txn` of an insert plus an aggregate update, then SIGKILL, then
   restart, `wait_until_healthy_db()`, and read. In 7 runs the first read
   held 25, 30, 23, 23, 30, 30 and 21 of the 30 acknowledged rows; every
   second restart that was read held 30. In 7 runs killed mid-write after
   425 acknowledged writes, the first read once held 419. The state
   converges, but a consumer that trusts health reads, and may act on,
   less than it was told was committed.

4. **The TODO in the rebuild is answerable.** "TODO is it enough to delete
   DB only, or do we need to do a full wipe?" (0.14.0 line 271, 0.15.0
   same function): on a single voter, after many snapshots and log purges
   (log 602, snapshot 599, purged 598), after a kill mid-interval (25
   entries past a snapshot), and before any snapshot, deleting the database
   alone and letting the node restore the latest snapshot and replay the
   log produced every acknowledged write, with no entry skipped or applied
   twice, once the replay had been applied (probe runs on the downstream
   fork and on 0.15.0 alike). We did not test a multi-voter cluster, a torn
   snapshot, or a full disk.

## Proposed

1. `start_node*` returns `Err(Error::...)` for a stale marker, a marker held
   by a live process, and a marker it cannot open or lock; no `panic!` in
   the start path. The message names the data directory and the recovery.
2. The rebuild on an unclean stop becomes a runtime `NodeConfig` option
   (for example `on_unclean_stop: Refuse | Rebuild`), defaulting to
   `Refuse`; the `auto-heal` feature, if kept, only changes the default.
3. Health (`is_healthy_db`, `wait_until_healthy_db`, `/health`) stays false
   until the state machine has applied the log the node held at start,
   whether or not a rebuild happened.
4. A public helper that tells the caller a directory was not stopped
   cleanly before it starts a node there, so an embedding program can
   decide (restore a backup, rebuild, refuse) without parsing a message.
5. The SIGKILL probe above as a regression test, and the TODO replaced by
   its answer.

The downstream fork carries (1) for the stale-marker case as a typed
`Error::Startup`, an exclusive OS lock at the data directory root for (4)'s
ownership half, and (3) as its `recovery.rs`; we are glad to send any of
them as pull requests if the direction is welcome.
