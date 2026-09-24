# Probes behind spec 043 revisions 2 to 4

Owned by `specs/043-patched-dependency-adoption/spec.md` (draft), which cites
these as D-P6, D-P7 and D-P12 to D-P14, and the revision 4 models as
D-P17 to D-P19; D-P8 was a direct image run, recorded below. They are
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

## Revision 3 (2026-09-23)

`common.rs` now reads the raft and API ports from `PR` and `PA`, and
`race.py` allocates them per start. Build as above; building both probes
took 65 s.

### G3: T1 against a pre-043 node stopping and starting (`race.py`), D-P12, D-P13

`race.py tl <dir>` samples one clean 0.14 stop. `race.py s2 <dir> <offsets>`
runs T1 (whole-file guard by `link`, WAL-lock probe, SQLite `-wal`/`-shm`
check, identity re-check) at offsets in tenths of a millisecond after the
stop begins, and diffs the legacy tree after any accepting verdict.
`race.py s1 <dir> <offsets>` runs T1 at offsets after a 0.14 start and
records whether the old node served.

| probe | runs | result |
|---|---|---|
| timeline | 1 | cache WAL lock released 0.51 ms, raft WAL lock 1.27 ms, marker removed 1.57 ms, `-shm` 1.9 ms, `-wal` 2.18 ms, exit 3.06 ms |
| stop race | 2 x 10 | 6 `EEXIST`; 1 case with both locks free and SQLite still open (revision 2 accepts, revision 3 refuses); 13 accepted, none followed by a change under the legacy store, guard untouched |
| start race | 15 | 1 accepted then the old node panicked on the guard; 3 refused on the held WAL lock, old node panicked; 11 `EEXIST`; never accepted while the old node served |

### G4: published verbs on a fenced volume (`img.sh`), D-P14

`serve` and `ledger verify` panic on the marker, `restore` and `preflight`
refuse, the app store is unchanged; `serve` leaves non-empty WAL debris
under the fence path. Direct `supervise` spawns Rauthy within a second.
With a directory at `rauthy/rauthy.env`, `supervise` exits 3 before the
spawn; with an unparseable `restore.marker`, it exits 1 before the spawn
(twice). Those two runs were done by hand after `img.sh`, on the same
volume, as recorded in D-P14.

Total runtime probing in this pass: about eight minutes.

## Revision 4 models (2026-09-23)

Deterministic models, stdlib Python 3.14 on macOS arm64, each in a
temporary directory it removes. They run no rahi code and no hiqlite or
Rauthy binary; they check the rules revision 4 states, and each carries a
negative control that fails on the rule it replaces. None of the earlier
runtime probes was repeated.

| script | spec | result |
|---|---|---|
| `clock_model.py` | D-P17, FR-014 | revision 3's pruning admits the post-prune rollback counterexample (by `jti` and by subject); retention and the persisted watermark refuse it, the watermark across a restart; a volatile watermark admits it (control); grid of 74,880 cases per policy: revision 3 admits a revoked token in 5,031, all with the clock below its last prune, the other two in none; well under a second |
| `supervisor_race.py` | D-P18 | a command prepared before the fence still spawns after it and after a T4 stand-in; a read after the fence fails `EISDIR` (control); under 0.1 s, 60 s alarm |
| `evidence_names.py` | D-P19, FR-013 | no-replace refuses onto a file and onto empty and non-empty directories, a plain rename replaces the empty one (control); revision 3's fixed name stalls on a second occurrence without overwriting; per-occurrence names take three occurrences with interruptions to three names, recover by identity, and refuse a foreign identity |

Run each with `python3 <script>`; exit `0` means every expectation,
including each control's failure, held.
