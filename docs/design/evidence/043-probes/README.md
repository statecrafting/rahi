# Probes behind spec 043 revision 2

Owned by `specs/043-patched-dependency-adoption/spec.md` (draft), which cites
these as D-P6 and D-P7; D-P8 was a direct image run, recorded below. They are
disposable probes, not tests: they authorize nothing, qualify nothing, and
AC-4 and AC-5 re-run the same questions on Linux against the real images.

## Setup (2026-09-23)

- Host: macOS 26 arm64, rustc 1.96.0, Docker 29.6.2.
- `probe-new`: `probe-new.Cargo.toml` as `new/Cargo.toml`, `main.rs` as
  `new/src/main.rs`; resolves `hiqlite-patched 0.15.0-patched.1` and
  `hiqlite-wal-patched 0.15.0-patched.1` from crates.io, rahi's feature set.
- `probe-old`: `probe-old.Cargo.toml` as `old/Cargo.toml`, the same
  `main.rs`; resolves `hiqlite 0.14.0` from git `sebadob/hiqlite@8f3b9bd`,
  the revision rahi v0.2.0 ships.
- `common.rs` sits beside both directories. `cargo build` in each.
- Ports 28471 and 28472 (a concurrent session's probe held 18411).

## G1: self-contention (`g1.sh`), spec D-P6

A 0.14-written store, moved with consent by 0.15, then:

| case | result |
|---|---|
| own descriptor holds `hiqlite-owner.lock`, then in-process start | `StorageInUse` after 0.26 ms, exit 3 |
| own descriptor holds `logs/lock.hql`, then in-process start | hiqlite-wal panic at `log_store.rs:49`; start returns `WAL: Generic: task 14 panicked`, 15 ms, exit 3 |
| both held, dropped, then start | started, `sql_rows=1`, shutdown `Ok`, exit 0 |
| another process holds `hiqlite-owner.lock` | `StorageInUse`, exit 3 |

## G2: the old version at every intermediate state (`g2.sh`), spec D-P7

Each state is built from a fresh 0.14-written volume. `snap.sh` records
every path and a content hash before and after the 0.14 start.

| state | 0.14 start | tree |
|---|---|---|
| O1 revision 1 `verified` (empty lock files left by T0) | **served** | lock files removed, db written |
| O2 revision 1 `moved`, no guard | **served**, fresh 0.14 cache, revoked `jti` gone | new `logs_cache`, db written |
| O3 revision 1 `flooring`, 0.15 SIGKILLed while open | refused: `Lock file already exists` | `logs/lock.hql` removed |
| O4 revision 1 `floored`, clean, no guard | **served** over the 0.15 store, twice | db written each time |
| O5 revision 1 `armed` | refused | `logs/lock.hql` **created in the live store** |
| N1 revision 2 guard in the intact store | refused | unchanged |
| N2 revision 2 partly relocated | refused | empty `logs/` WAL files and dirs created under the fence path only |
| N3 revision 2 fence only | refused | as N2; the guard is kept |
| N4 0.15 opens the relocated store at its new path | started, rows intact, shutdown `Ok` | |
| N5 0.15 SIGKILLed while open there, marker moved to evidence, reopen | started, rows intact, shutdown `Ok` | |
| N6 0.14 on the fence after N4 and N5 | refused | unchanged |
| N7 0.14 on a fresh-volume fence | refused | as N2, under the fence path only |

One run per state. The stray files in N2, N3 and N7 come from 0.14's WAL
task racing its state machine's panic, which is also why revision 1's "no
file changed" (D-P5) held in some runs only.

## D-P8: the v0.2.0 image on a fenced volume

```sh
V=<disposable>; mkdir -p $V/hiqlite/state_machine $V/rauthy
printf 'rahi-upgrade-cache 7f3a' > $V/hiqlite/state_machine/lock
docker run --rm --name rahi-fence-probe -v $V:/data \
  -e RAHI_PUBLIC_URL=http://localhost:8080 \
  ghcr.io/statecrafting/rahi-hello-cell:0.2.0   # index sha256:494a566d...b66f06b
```

`first-boot` generated keys; `rahi migrate --adopt-manifest` was still
blocked in attach after three minutes; the process list held only
`entrypoint.sh` and `migrate`; no Rauthy. Stopped with `docker stop -t 5`.
The diff showed new `keys/`, `backups/`, `rauthy/config.toml` and
`rauthy/rauthy.env` (first-boot's absent-only writes) and nothing under
`hiqlite/`. Limitation: a fresh volume, the default entrypoint, arm64 only.
