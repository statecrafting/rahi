//! spec 011 FR-001: migrate applied once, idempotent, refusing a lower
//! version; and spec 036 FR-004: a migration whose SQL changed under an
//! applied version is refused rather than skipped.

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

/// Spec 011's forward-only rule, and spec 036 B-7's refusal beside it.
///
/// This test used to be `recorded_versions_are_skipped_even_when_their_sql_changed`
/// and asserted that a rewritten version 1 was skipped without a word, which
/// is the third defect spec 036's Purpose reproduces. Skipping an *unchanged*
/// applied version is still the rule and is still asserted; skipping a
/// changed one is now `Error::Integrity` (B-7).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn applied_versions_are_skipped_unchanged_and_refused_when_rewritten() {
    let f = common::open().await;
    let store = f.store.handle();
    store.migrate(&list()).await.unwrap();

    let mut extended: Vec<Migration> = list();
    extended.push(Migration::new(
        3,
        "tags",
        "CREATE TABLE tags (id INTEGER PRIMARY KEY)",
    ));
    let report = store.migrate(&extended).await.unwrap();
    assert_eq!(report.previous, 2);
    assert_eq!(report.applied, [3], "1 and 2 are history; only 3 runs");
    assert_eq!(report.current, 3);

    let rewritten = [
        Migration::new(1, "items", "CREATE TABLE items (id INTEGER)"),
        Migration::new(2, "items_note", "CREATE TABLE items (id INTEGER)"),
        Migration::new(3, "tags", "CREATE TABLE tags (id INTEGER PRIMARY KEY)"),
    ];
    let err = store.migrate(&rewritten).await.unwrap_err();
    assert!(matches!(err, Error::Integrity(_)), "{err}");
    assert!(err.message().contains("migration 1"), "{err}");

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

/// Spec 036 FR-004 and B-7: the SQL of an applied version changed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_migration_edited_under_an_applied_version_is_refused_by_name() {
    let f = common::open().await;
    let store = f.store.handle();

    let original = vec![Migration::new(
        1,
        "notes",
        "CREATE TABLE notes (id TEXT PRIMARY KEY)",
    )];
    store.migrate(&original).await.unwrap();

    let recorded = store.recorded_migrations().await.unwrap();
    let one = recorded.iter().find(|r| r.version == 1).expect("recorded");
    assert_eq!(
        one.checksum.as_deref(),
        Some(original[0].checksum().as_str()),
        "B-7: the checksum of the SQL that ran is recorded with the version"
    );
    assert_eq!(one.additive, Some(false), "B-8: undeclared is not additive");

    // The same version, the same name, different SQL: before spec 036 this
    // was skipped as applied and the table never gained its column.
    let edited = vec![Migration::new(
        1,
        "notes",
        "CREATE TABLE notes (id TEXT PRIMARY KEY, body TEXT)",
    )];
    let err = store.migrate(&edited).await.unwrap_err();
    assert!(matches!(err, Error::Integrity(_)), "{err}");
    assert_eq!(err.exit_code(), 1, "{err}");
    for fragment in [
        "migration 1",
        "notes",
        &original[0].checksum(),
        &edited[0].checksum(),
    ] {
        assert!(err.message().contains(fragment), "{fragment}: {err}");
    }

    // And the same read `serve` makes says the same thing, without writing.
    let err = rahi_store::check_checksums(&store.recorded_migrations().await.unwrap(), &edited)
        .expect_err("serve refuses it too");
    assert!(matches!(err, Error::Integrity(_)), "{err}");

    // The original list still runs clean: the refusal is about the edit.
    store.migrate(&original).await.unwrap();

    f.store.shutdown().await.unwrap();
}

/// Spec 036 B-7: a row written before this spec carries no checksum, and the
/// first migrate under it records the binary's rather than refusing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_row_written_before_this_spec_is_given_the_binarys_checksum() {
    let f = common::open().await;
    let store = f.store.handle();
    let list = vec![Migration::new(1, "notes", "CREATE TABLE notes (id TEXT)").additive()];

    // A pre-036 store: the table and the row as the old binary wrote them.
    store
        .execute(
            "CREATE TABLE IF NOT EXISTS schema_version (version INTEGER PRIMARY KEY, \
             name TEXT NOT NULL)",
            vec![],
        )
        .await
        .unwrap();
    for (version, name) in [(0_i64, "baseline"), (1, "notes")] {
        store
            .execute(
                "INSERT INTO schema_version (version, name) VALUES ($1, $2)",
                vec![
                    rahi_store::Value::from(version),
                    rahi_store::Value::from(name),
                ],
            )
            .await
            .unwrap();
    }
    store
        .execute("CREATE TABLE notes (id TEXT)", vec![])
        .await
        .unwrap();

    let report = store
        .migrate(&list)
        .await
        .expect("the old row is not a mismatch");
    assert_eq!(
        report.applied,
        Vec::<u32>::new(),
        "version 1 is already applied"
    );

    let recorded = store.recorded_migrations().await.unwrap();
    let one = recorded.iter().find(|r| r.version == 1).expect("recorded");
    assert_eq!(
        one.checksum.as_deref(),
        Some(list[0].checksum().as_str()),
        "B-7: the first migrate under this spec records the binary's checksum"
    );
    assert!(one.is_additive(), "B-8: and the declaration beside it");

    // From here the check has something to compare against.
    let edited = vec![Migration::new(
        1,
        "notes",
        "CREATE TABLE notes (id TEXT, n INT)",
    )];
    let err = store.migrate(&edited).await.unwrap_err();
    assert!(matches!(err, Error::Integrity(_)), "{err}");

    f.store.shutdown().await.unwrap();
}

/// Spec 036 B-8: the declaration is recorded, and it is never inferred.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn additive_is_declared_and_recorded_never_guessed() {
    let f = common::open().await;
    let store = f.store.handle();
    store
        .migrate(&[
            Migration::new(1, "adds_a_table", "CREATE TABLE a (id TEXT)").additive(),
            Migration::new(2, "rewrites", "CREATE TABLE b (id TEXT NOT NULL)"),
        ])
        .await
        .unwrap();

    let recorded = store.recorded_migrations().await.unwrap();
    assert!(
        recorded
            .iter()
            .find(|r| r.version == 1)
            .unwrap()
            .is_additive()
    );
    assert!(
        !recorded
            .iter()
            .find(|r| r.version == 2)
            .unwrap()
            .is_additive(),
        "a migration that did not declare itself additive is not additive"
    );
    assert!(
        !recorded
            .iter()
            .find(|r| r.version == 0)
            .unwrap()
            .is_additive(),
        "and neither is the baseline, which declares nothing"
    );

    f.store.shutdown().await.unwrap();
}
