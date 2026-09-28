//! Spec 048 D-3: after an unclean stop, `auto-heal` deletes the SQLite state
//! machine and rebuilds it from the latest snapshot plus the log tail. These
//! tests SIGKILL a writer after the log has been snapshotted and purged many
//! times, restart with `auto-heal`, and compare the rebuilt state with what
//! the writer had acknowledged.
//!
//! Every write is one transaction that inserts row `i` and bumps two
//! aggregates (`n += 1`, `s += i`). A replay that skipped an entry, or
//! applied one twice, breaks `count == n` or `s == sum(ids)`; a lost
//! acknowledged write breaks `count >= acked`.
//!
//! The test binary is its own child: a child is started on the `child` test
//! with `PROBE_PHASE` set.

use std::io::{BufRead as _, BufReader};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use hiqlite::Param;
use probe_auto_heal::{Cache, node_config};

const WRITES_QUIESCED: i64 = 600;
/// Fewer writes than one snapshot interval: the rebuild is the log alone.
const WRITES_BEFORE_SNAPSHOT: i64 = 30;
/// Mid-interval (snapshots land every 50 entries), so the rebuild replays a
/// log tail on top of the snapshot rather than a snapshot alone.
const KILL_AFTER: i64 = 425;

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn spawn(phase: &str, dir: &Path, raft: u16, api: u16) -> Child {
    Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child", "--nocapture", "--test-threads=1"])
        .env("PROBE_PHASE", phase)
        .env("PROBE_DIR", dir)
        .env("PROBE_RAFT", raft.to_string())
        .env("PROBE_API", api.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

/// Read the child's `KEY value` lines until `until` is seen; answer the last
/// value per key.
fn read_until(child: &mut Child, until: &str) -> std::collections::BTreeMap<String, String> {
    let out = child.stdout.as_mut().unwrap();
    let mut seen = std::collections::BTreeMap::new();
    for line in BufReader::new(out).lines() {
        let line = line.unwrap();
        // libtest's own output can share a line with the child's, so a
        // record is found by its prefix anywhere in the line.
        let Some((_, record)) = line.split_once("@@ ") else {
            continue;
        };
        let (k, v) = record.split_once(' ').unwrap_or((record, ""));
        seen.insert(k.to_owned(), v.to_owned());
        if k == until {
            return seen;
        }
    }
    panic!("the child ended before {until}: {seen:?}");
}

fn marker(dir: &Path) -> std::path::PathBuf {
    dir.join("state_machine").join("lock")
}

/// Kill the writer at `until`, then rebuild and read. Answers the writer's
/// last lines and the reader's.
fn crash_then_heal(
    phase: &str,
    until: &str,
) -> (
    std::collections::BTreeMap<String, String>,
    std::collections::BTreeMap<String, String>,
    std::collections::BTreeMap<String, String>,
) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("hiqlite");
    let (raft, api) = (free_port(), free_port());

    let mut writer = spawn(phase, &dir, raft, api);
    let wrote = read_until(&mut writer, until);
    writer.kill().unwrap(); // SIGKILL: no shutdown runs
    writer.wait().unwrap();
    assert!(
        marker(&dir).exists(),
        "the SIGKILL must leave hiqlite's unclean-stop marker"
    );

    // The rebuild, then a second clean start of the healed directory.
    let mut first = spawn("read", &dir, raft, api);
    let healed = read_until(&mut first, "DONE");
    assert!(first.wait().unwrap().success());
    assert!(!marker(&dir).exists(), "a clean stop removes the marker");
    let mut second = spawn("read", &dir, raft, api);
    let again = read_until(&mut second, "DONE");
    assert!(second.wait().unwrap().success());
    (wrote, healed, again)
}

fn num(map: &std::collections::BTreeMap<String, String>, key: &str) -> i64 {
    map.get(key)
        .unwrap_or_else(|| panic!("{key} missing in {map:?}"))
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap()
}

fn assert_consistent(read: &std::collections::BTreeMap<String, String>) {
    let (count, n, s, max) = (
        num(read, "COUNT"),
        num(read, "N"),
        num(read, "S"),
        num(read, "MAX"),
    );
    assert_eq!(count, n, "every insert bumped n once: {read:?}");
    assert_eq!(max, count, "ids are contiguous from 1: {read:?}");
    assert_eq!(s, count * (count + 1) / 2, "s is the sum of the ids: {read:?}");
}

#[test]
fn rebuild_after_snapshots_and_purges_is_complete() {
    let (wrote, healed, again) = crash_then_heal("write-quiesced", "DONE");
    eprintln!("@@ writer: {wrote:?}\nhealed: {healed:?}\nagain: {again:?}");
    assert!(num(&wrote, "PURGED") > 0, "the log was purged: {wrote:?}");
    assert!(num(&wrote, "SNAPSHOT") > 0, "a snapshot was taken: {wrote:?}");
    assert_eq!(num(&healed, "COUNT"), WRITES_QUIESCED);
    assert_consistent(&healed);
    assert_eq!(healed, again, "the healed store reopens unchanged");
}

#[test]
fn rebuild_after_a_kill_mid_write_keeps_every_acknowledged_write() {
    let (wrote, healed, again) = crash_then_heal("write-flowing", "KILL");
    eprintln!("@@ writer: {wrote:?}\nhealed: {healed:?}\nagain: {again:?}");
    let acked = num(&wrote, "ACKED");
    assert!(acked >= KILL_AFTER);
    // The writer kept writing after it printed KILL, so the store may hold
    // more than the parent saw acknowledged; never less.
    assert!(num(&healed, "COUNT") >= acked, "{healed:?} lost acked {acked}");
    assert_consistent(&healed);
    assert_eq!(healed, again, "the healed store reopens unchanged");
}

#[test]
fn rebuild_before_the_first_snapshot_replays_the_whole_log() {
    let (wrote, healed, again) = crash_then_heal("write-small", "DONE");
    eprintln!("writer: {wrote:?}\nhealed: {healed:?}\nagain: {again:?}");
    assert_eq!(num(&wrote, "SNAPSHOT"), 0, "no snapshot yet: {wrote:?}");
    assert_eq!(num(&healed, "COUNT"), WRITES_BEFORE_SNAPSHOT);
    assert_consistent(&healed);
    assert_eq!(healed, again, "the healed store reopens unchanged");
}

/// The child process. A no-op unless `PROBE_PHASE` is set.
#[tokio::test(flavor = "multi_thread")]
async fn child() {
    let Ok(phase) = std::env::var("PROBE_PHASE") else {
        return;
    };
    let dir = std::env::var("PROBE_DIR").unwrap();
    let raft: u16 = std::env::var("PROBE_RAFT").unwrap().parse().unwrap();
    let api: u16 = std::env::var("PROBE_API").unwrap().parse().unwrap();
    let client = hiqlite::start_node_with_cache::<Cache>(node_config(&dir, raft, api))
        .await
        .unwrap();
    client.wait_until_healthy_db().await;

    match phase.as_str() {
        "write-quiesced" | "write-flowing" | "write-small" => {
            client
                .batch(
                    "CREATE TABLE t (id INTEGER PRIMARY KEY);
                     CREATE TABLE agg (k INTEGER PRIMARY KEY, n INTEGER NOT NULL, s INTEGER NOT NULL);
                     INSERT INTO agg (k, n, s) VALUES (1, 0, 0);",
                )
                .await
                .unwrap();
            let mut i: i64 = 0;
            loop {
                i += 1;
                let res = client
                    .txn([
                        ("INSERT INTO t (id) VALUES ($1)", vec![Param::from(i)]),
                        ("UPDATE agg SET n = n + 1, s = s + $1 WHERE k = 1", vec![Param::from(i)]),
                    ])
                    .await
                    .unwrap();
                for r in res {
                    assert_eq!(r.unwrap(), 1);
                }
                println!("@@ ACKED {i}");
                if phase == "write-flowing" && i == KILL_AFTER {
                    println!("@@ KILL {i}");
                }
                if (phase == "write-quiesced" && i == WRITES_QUIESCED)
                    || (phase == "write-small" && i == WRITES_BEFORE_SNAPSHOT)
                {
                    break;
                }
            }
            // Let the snapshot policy and the purge catch up with the log.
            for _ in 0..100 {
                let m = client.metrics_db().await.unwrap();
                let last = m.last_log_index.unwrap_or(0);
                let snap = m.snapshot.map_or(0, |l| l.index);
                if (m.purged.is_some() || phase == "write-small") && last.saturating_sub(snap) < probe_auto_heal::LOGS_UNTIL_SNAPSHOT {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            let m = client.metrics_db().await.unwrap();
            println!("@@ LAST_LOG {}", m.last_log_index.unwrap_or(0));
            println!("@@ SNAPSHOT {}", m.snapshot.map_or(0, |l| l.index));
            println!("@@ PURGED {}", m.purged.map_or(0, |l| l.index));
            println!("@@ DONE");
            // Wait for the SIGKILL.
            tokio::time::sleep(Duration::from_secs(600)).await;
        }
        "read" => {
            let mut rows = client
                .query_raw(
                    "SELECT (SELECT COUNT(*) FROM t) AS c, (SELECT COALESCE(MAX(id), 0) FROM t) AS m, \
                     n, s FROM agg WHERE k = 1",
                    vec![],
                )
                .await
                .unwrap();
            let mut row = rows.pop().expect("the agg row survives the rebuild");
            println!("@@ COUNT {}", row.get::<i64>("c"));
            println!("@@ MAX {}", row.get::<i64>("m"));
            println!("@@ N {}", row.get::<i64>("n"));
            println!("@@ S {}", row.get::<i64>("s"));
            client.shutdown().await.unwrap();
            println!("@@ DONE");
        }
        other => panic!("unknown phase {other}"),
    }
}
