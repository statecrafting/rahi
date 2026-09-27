# Probes behind spec 048

Owned by `specs/048-store-clean-shutdown/spec.md`, which cites them as
D-3's evidence. They are probes, not rahi tests: the feature they exercise
is one rahi deliberately does not enable, and no workspace build may see
it (Cargo features are additive across a dependency graph).

## `auto-heal/`: is the rebuild after an unclean stop complete?

A standalone package (`[workspace]` of its own, `Cargo.lock` ignored) on
rahi's hiqlite pin, `hiqlite-patched =0.15.0-patched.3`, with rahi's feature
set plus `auto-heal`. `tests/rebuild.rs` starts a single voter with a
snapshot every 50 log entries (`LOGS_UNTIL_SNAPSHOT`; rahi runs hiqlite's
default of 10 000), SIGKILLs it, restarts it twice, and compares.

Each write is one `txn` inserting row `i` and applying `n += 1, s += i` to
an aggregate row, so a skipped entry breaks `count == n`, a doubly applied
one breaks `s == sum(ids)`, and a lost acknowledged write breaks
`count >= acked`.

```sh
cd docs/design/evidence/048-probes/auto-heal
cargo test -- --test-threads=1 --nocapture
```

### Results, 2026-09-27, macOS 26 arm64, rustc 1.96.0

| scenario | writer at the kill | first start (the rebuild) | second start |
|---|---|---|---|
| quiesced after 600 writes | log 602, snapshot 599, purged 598 | 600 rows, `n`/`s` consistent | identical |
| killed mid-write at a snapshot boundary (400), 10 runs | acked 400 | 400 or 401 rows, consistent, 10/10 | identical |
| killed mid-write mid-interval (425), 5 runs | acked 425 | 426 rows, consistent, 5/5 | identical |
| 30 writes, before any snapshot | snapshot 0, purged 0 | 30 rows, consistent | identical |

Every run left `state_machine/lock` behind the SIGKILL, every rebuild
removed it on its clean stop, and every first read already held every
acknowledged write: the fork's startup-recovery gate (its `recovery.rs`)
keeps health and reads refusing until the log held at start is applied.

### The same probe on upstream `hiqlite =0.15.0`

The package with its dependency line changed to `hiqlite = { version =
"=0.15.0", default-features = false, features = ["sqlite", "cache",
"counters", "dlock", "backup", "auto-heal"] }` (upstream has no
`listen_notify_local`, and the probe needs no `s3`), and the scenario
assertions kept (3 runs) and then reduced to printing so the second start
is always read (4 runs):

| scenario | first start (the rebuild), 7 runs | second start |
|---|---|---|
| quiesced after 600 writes | 600, 7/7 | 600 |
| killed mid-write at 425 | 426, 419, 426, 426, 425, 426, 426: **419 is below the 425 acked** | as the first start, where read |
| 30 writes, before any snapshot | **25, 30, 23, 23, 30, 30, 21** | 30, 4/4 read |

Upstream reports the node healthy (`wait_until_healthy_db`) before the
replay of the log it held at start is applied, so a reader that trusts
health sees fewer rows than were acknowledged, and a writer could act on
that. The state converges: every second start read everything. This is the
defect the fork records as F-134 and fixes with the recovery gate; it is
part of the proposed upstream issue in
`docs/design/10-hiqlite-unclean-stop-upstream-issue.md`.

### What the probe does not cover

A multi-voter cluster (a rebuilding follower), a torn or corrupt snapshot
file, a full disk during the rebuild, and hiqlite 0.14. It measures
completeness, not the rebuild's duration on a large store.
