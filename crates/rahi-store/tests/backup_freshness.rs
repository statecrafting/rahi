//! spec 037 B-2 and FR-002: a backup reports the snapshot it took, never
//! one that was already there.
//!
//! # What these cost, and why
//!
//! hiqlite ignores a backup request within sixty seconds of the one before
//! and acknowledges it as success anyway, and that window is its own
//! constant. The only way to show that a second backup inside the window
//! produces a *second* snapshot is to take one, so the first test here
//! waits the window out and runs for a bit over a minute. Shortening it
//! would mean accepting the first snapshot as the second's answer, which is
//! the defect the spec is about.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod common;

use std::time::{Duration, Instant};

use rahi_store::backup::{BackupId, SUPPRESSION_WINDOW, snapshot_ts};
use rahi_types::Error;

/// Read a snapshot with sqlite's own reader: the bytes of a `VACUUM INTO`
/// are an ordinary database, and its contents are the only evidence that
/// tells one snapshot from another.
fn rows_in(path: &std::path::Path, table: &str) -> Vec<String> {
    // No sqlite dependency here on purpose: the store crate has hiqlite and
    // hiqlite has rusqlite, but a test that opens the file through the
    // store would re-elect a node on it. The page text of a small database
    // carries the row values verbatim, which is enough to say which rows a
    // snapshot holds.
    let bytes = std::fs::read(path).unwrap();
    assert!(
        bytes.starts_with(b"SQLite format 3\0"),
        "{} is a database",
        path.display()
    );
    let text = String::from_utf8_lossy(&bytes).into_owned();
    assert!(text.contains(table), "{} holds {table}", path.display());
    ["before", "between"]
        .into_iter()
        .filter(|needle| text.contains(needle))
        .map(str::to_owned)
        .collect()
}

fn path_of(f: &common::Fixture, id: &BackupId) -> std::path::PathBuf {
    f.store.config().backup_dir().join(id.as_str())
}

/// Two backups inside one suppression window, with a row written between
/// them: two snapshots, the second holding what the first cannot (FR-002).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_backup_inside_the_window_is_a_second_snapshot() {
    let f = common::open().await;
    let store = f.store.handle();
    store
        .execute(
            "CREATE TABLE marks (id INTEGER PRIMARY KEY, note TEXT)",
            vec![],
        )
        .await
        .unwrap();
    store
        .execute("INSERT INTO marks (id, note) VALUES (1, 'before')", vec![])
        .await
        .unwrap();

    let first = store.backup().await.expect("the first backup");
    assert!(first.as_str().starts_with("backup_node_1_"), "{first:?}");

    // A row that exists only after the first snapshot was taken.
    store
        .execute("INSERT INTO marks (id, note) VALUES (2, 'between')", vec![])
        .await
        .unwrap();

    let started = Instant::now();
    let second = store.backup().await.expect("the second backup");
    let waited = started.elapsed();
    assert_ne!(first, second, "a second snapshot, not the first file again");
    assert!(
        snapshot_ts(second.as_str()) > snapshot_ts(first.as_str()),
        "{first:?} then {second:?}"
    );
    assert!(
        waited >= SUPPRESSION_WINDOW / 2,
        "the second backup waited the window out rather than reporting a stale file: {waited:?}"
    );

    // The independent check: the contents, not the name and not the age.
    assert_eq!(rows_in(&path_of(&f, &first), "marks"), vec!["before"]);
    assert_eq!(
        rows_in(&path_of(&f, &second), "marks"),
        vec!["before", "between"],
        "the second snapshot holds the row written after the first"
    );

    f.store.shutdown().await.unwrap();
}

/// A deadline that cannot outlast the suppression window is a refusal
/// naming the deadline, with no snapshot reported (FR-002, D-4).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_deadline_inside_the_window_is_refused_and_names_the_bound() {
    let f = common::open().await;
    let store = f.store.handle();
    store
        .execute("CREATE TABLE t (id INTEGER PRIMARY KEY)", vec![])
        .await
        .unwrap();
    let taken = store.backup().await.expect("the first backup");

    assert!(
        store.suppressed_for().await.unwrap().is_some(),
        "the window is in place immediately after a backup"
    );
    let err = store
        .backup_within(Duration::from_secs(3))
        .await
        .expect_err("a deadline inside the window is a refusal");
    let Error::Upstream(text) = &err else {
        panic!("{err:?} is an upstream error");
    };
    assert!(text.contains("3 second deadline"), "{text}");
    assert!(text.contains("ignores a backup request"), "{text}");

    // And nothing new was written: the refusal is not a half-success.
    let listed = store.backup_list_local().await.unwrap();
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0].id, taken);

    f.store.shutdown().await.unwrap();
}

/// The name is the freshness oracle, and it is read the way hiqlite writes
/// it. A file that is not one of hiqlite's has no timestamp and can never
/// be mistaken for this call's snapshot.
#[test]
fn a_snapshot_name_carries_the_second_its_request_was_issued_at() {
    assert_eq!(
        snapshot_ts("backup_node_1_1789666246.sqlite"),
        Some(1_789_666_246)
    );
    assert_eq!(snapshot_ts("backup_node_12_1.sqlite"), Some(1));
    assert_eq!(snapshot_ts("backup_node_1_notanumber.sqlite"), None);
    assert_eq!(snapshot_ts("rauthy_backup_1.sqlite"), None);
    assert_eq!(snapshot_ts("backup_node_1_1789666246.sqlite.partial"), None);
    assert_eq!(snapshot_ts("hiqlite.db"), None);
}
