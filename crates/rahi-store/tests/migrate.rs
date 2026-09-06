//! spec 011 FR-001: migrate applied once, idempotent, refusing a lower version.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod common;

use rahi_store::Migration;
use rahi_types::Error;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Version {
    version: i64,
    name: String,
}

fn list() -> Vec<Migration> {
    vec![
        Migration::new(
            1,
            "items",
            "CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
             CREATE INDEX items_name ON items (name);",
        ),
        Migration::new(2, "items_note", "ALTER TABLE items ADD COLUMN note TEXT"),
    ]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn applies_once_and_is_idempotent() {
    let f = common::open().await;
    let store = f.store.handle();

    let first = store.migrate(&list()).await.unwrap();
    assert_eq!(first.previous, 0);
    assert_eq!(first.current, 2);
    assert_eq!(first.applied, [1, 2]);

    let second = store.migrate(&list()).await.unwrap();
    assert_eq!(second.previous, 2);
    assert_eq!(second.current, 2);
    assert!(
        second.applied.is_empty(),
        "nothing to apply the second time"
    );

    let recorded: Vec<Version> = store
        .query_consistent(
            "SELECT version, name FROM schema_version ORDER BY version",
            vec![],
        )
        .await
        .unwrap();
    let names: Vec<(i64, &str)> = recorded
        .iter()
        .map(|v| (v.version, v.name.as_str()))
        .collect();
    assert_eq!(names, [(0, "baseline"), (1, "items"), (2, "items_note")]);

    store
        .execute(
            "INSERT INTO items (id, name, note) VALUES (1, 'a', 'n')",
            vec![],
        )
        .await
        .expect("both migrations took effect");

    f.store.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recorded_versions_are_skipped_even_when_their_sql_changed() {
    let f = common::open().await;
    let store = f.store.handle();
    store.migrate(&list()).await.unwrap();

    let rewritten = [
        Migration::new(1, "items", "CREATE TABLE items (id INTEGER)"),
        Migration::new(2, "items_note", "CREATE TABLE items (id INTEGER)"),
        Migration::new(3, "tags", "CREATE TABLE tags (id INTEGER PRIMARY KEY)"),
    ];
    let report = store.migrate(&rewritten).await.unwrap();
    assert_eq!(report.previous, 2);
    assert_eq!(report.applied, [3], "1 and 2 are history; only 3 runs");
    assert_eq!(report.current, 3);

    f.store.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refuses_an_unapplied_version_below_the_recorded_one() {
    let f = common::open().await;
    let store = f.store.handle();
    store
        .migrate(&[Migration::new(5, "five", "CREATE TABLE five (id INTEGER)")])
        .await
        .unwrap();

    let err = store
        .migrate(&[Migration::new(
            3,
            "three",
            "CREATE TABLE three (id INTEGER)",
        )])
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Conflict(_)), "{err}");
    assert!(
        err.message()
            .contains("below the recorded schema version 5"),
        "{err}"
    );

    f.store.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failing_migration_is_rolled_back_whole() {
    let f = common::open().await;
    let store = f.store.handle();

    let bad = [Migration::new(
        1,
        "half",
        "CREATE TABLE half (id INTEGER PRIMARY KEY); CREATE TABLE half (id INTEGER)",
    )];
    let err = store.migrate(&bad).await.unwrap_err();
    assert!(matches!(err, Error::Validation(_)), "{err}");
    assert!(err.message().contains("rolled back"), "{err}");

    let tables: Vec<Name> = store
        .query(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'half'",
            vec![],
        )
        .await
        .unwrap();
    assert!(tables.is_empty(), "the first statement did not survive");
    let report = store.migrate(&[]).await.unwrap();
    assert_eq!(report.current, 0, "nothing was recorded");

    f.store.shutdown().await.unwrap();
}

#[derive(Debug, Deserialize)]
struct Name {
    #[allow(dead_code)]
    name: String,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_malformed_list_is_refused_before_anything_runs() {
    let f = common::open().await;
    let store = f.store.handle();
    for bad in [
        vec![Migration::new(0, "zero", "SELECT 1")],
        vec![
            Migration::new(2, "b", "SELECT 1"),
            Migration::new(1, "a", "SELECT 1"),
        ],
        vec![
            Migration::new(1, "a", "SELECT 1"),
            Migration::new(1, "a", "SELECT 1"),
        ],
        vec![Migration::new(1, "empty", "  ;  ")],
    ] {
        let err = store.migrate(&bad).await.unwrap_err();
        assert!(matches!(err, Error::Validation(_)), "{bad:?}: {err}");
    }
    f.store.shutdown().await.unwrap();
}
