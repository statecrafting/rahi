//! spec 046 FR-001, FR-002, FR-003, FR-005, FR-007: named migration sets
//! against a real single-node store.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod common;

use rahi_store::{
    Migration, MigrationSet, SetName, StoreHandle, coordination_migration, coordination_set,
    receipt_set,
};
use rahi_types::Error;
use serde::Deserialize;

fn aicortex(versions: u32) -> MigrationSet {
    let migrations = (1..=versions)
        .map(|v| {
            Migration::new(
                v,
                format!("aicortex-{v}"),
                format!("CREATE TABLE IF NOT EXISTS aicortex_t{v} (id TEXT PRIMARY KEY)"),
            )
            .additive()
        })
        .collect();
    MigrationSet::new("aicortex", migrations)
        .unwrap()
        .requires("rahi.receipts", 1)
        .unwrap()
}

fn app() -> Vec<Migration> {
    vec![
        Migration::new(1, "notes", "CREATE TABLE notes (id TEXT PRIMARY KEY)"),
        Migration::new(2, "tags", "CREATE TABLE tags (id TEXT PRIMARY KEY)"),
    ]
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct VersionRow {
    version: i64,
    name: String,
    checksum: Option<String>,
    additive: Option<i64>,
}

async fn schema_version_rows(store: &StoreHandle) -> Vec<VersionRow> {
    store
        .query_consistent(
            "SELECT version, name, checksum, additive FROM schema_version ORDER BY version",
            vec![],
        )
        .await
        .unwrap()
}

/// FR-001: `app`, `aicortex` (1 to 3) and `rahi.receipts` migrate a fresh
/// store; each set reads back with its own versions and checksums; a second
/// run applies nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_set_migrates_and_reads_back_its_own_history() {
    let f = common::open().await;
    let store = f.store.handle();
    let sets = vec![coordination_set(), receipt_set(), aicortex(3)];

    let report = store.migrate_sets(&app(), &sets).await.unwrap();
    let applied: Vec<(&str, u32)> = report
        .applied
        .iter()
        .map(|(s, v)| (s.as_str(), *v))
        .collect();
    assert_eq!(
        applied,
        vec![
            ("rahi.coordination", 1),
            ("rahi.receipts", 1),
            ("aicortex", 1),
            ("aicortex", 2),
            ("aicortex", 3),
            ("app", 1),
            ("app", 2),
        ],
        "chassis sets, then libraries, then app (B-7)"
    );

    let histories = store.recorded_set_migrations().await.unwrap();
    let lib = &histories[&SetName::reference("aicortex").unwrap()];
    assert_eq!(
        lib.iter().map(|r| r.version).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    for (row, m) in lib.iter().zip(aicortex(3).migrations.iter()) {
        assert_eq!(row.checksum.as_deref(), Some(m.checksum().as_str()));
        assert_eq!(row.additive, Some(true));
    }
    let app_history = &histories[&SetName::app()];
    assert_eq!(
        app_history.iter().map(|r| r.version).collect::<Vec<_>>(),
        vec![0, 1, 2],
        "app stays in schema_version, baseline included"
    );
    assert_eq!(
        histories[&SetName::reference("rahi.receipts").unwrap()].len(),
        1
    );

    let again = store.migrate_sets(&app(), &sets).await.unwrap();
    assert!(again.applied.is_empty(), "a second run applies nothing");

    f.store.shutdown().await.unwrap();
}

/// FR-002: a requirement above the required set's last version, and a cycle,
/// are refused with the store unchanged.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unsatisfiable_or_cyclic_plan_applies_nothing() {
    let f = common::open().await;
    let store = f.store.handle();

    let needy = MigrationSet::new(
        "aicortex",
        vec![
            Migration::new(1, "a1", "CREATE TABLE a1 (x)"),
            Migration::new(2, "a2", "CREATE TABLE a2 (x)")
                .requires("rahi.receipts", 2)
                .unwrap(),
        ],
    )
    .unwrap();
    let err = store
        .migrate_sets(&app(), &[coordination_set(), receipt_set(), needy])
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Validation(_)), "{err}");
    assert!(err.message().contains("rahi.receipts"), "{err}");
    let histories = store.recorded_set_migrations().await.unwrap();
    assert!(
        histories.values().all(|rows| rows.is_empty()),
        "nothing was applied or recorded: {histories:?}"
    );

    let a = MigrationSet::new("a", vec![Migration::new(1, "a", "CREATE TABLE a (x)")])
        .unwrap()
        .requires("b", 1)
        .unwrap();
    let b = MigrationSet::new("b", vec![Migration::new(1, "b", "CREATE TABLE b (x)")])
        .unwrap()
        .requires("a", 1)
        .unwrap();
    let err = store.migrate_sets(&app(), &[a, b]).await.unwrap_err();
    assert!(matches!(err, Error::Validation(_)), "{err}");
    assert!(err.message().contains("cycle"), "{err}");
    let histories = store.recorded_set_migrations().await.unwrap();
    assert!(histories.values().all(|rows| rows.is_empty()));

    f.store.shutdown().await.unwrap();
}

/// FR-003 (the migrate half): an edited SQL under `(aicortex, 2)` is
/// `Error::Integrity` naming the set and version, even with `app` behind.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_edited_library_migration_is_an_integrity_failure_even_when_app_is_behind() {
    let f = common::open().await;
    let store = f.store.handle();
    let sets = vec![coordination_set(), receipt_set(), aicortex(2)];
    store.migrate_sets(&app()[..1], &sets).await.unwrap();

    let mut edited = aicortex(2);
    edited.migrations[1].sql = "CREATE TABLE IF NOT EXISTS aicortex_t2 (id TEXT)".to_owned();
    let err = store
        .migrate_sets(&app(), &[coordination_set(), receipt_set(), edited])
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Integrity(_)), "{err}");
    assert!(err.message().contains("set aicortex"), "{err}");
    assert!(err.message().contains("migration 2"), "{err}");
    let rows = schema_version_rows(&store).await;
    assert_eq!(rows.len(), 2, "app's pending version 2 was not applied");

    f.store.shutdown().await.unwrap();
}

/// FR-005: a store written by the 0.2.0 path, `coordination_migration(1)`
/// and two app migrations in the cell's list, upgrades with its
/// `schema_version` rows byte for byte, and adding `rahi.coordination`
/// records version 1 without changing a table.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_single_sequence_store_upgrades_with_its_history_untouched() {
    let f = common::open().await;
    let store = f.store.handle();
    let legacy = vec![
        coordination_migration(1),
        Migration::new(2, "notes", "CREATE TABLE notes (id TEXT PRIMARY KEY)"),
        Migration::new(3, "tags", "CREATE TABLE tags (id TEXT PRIMARY KEY)"),
    ];
    store.migrate(&legacy).await.unwrap();
    let before = schema_version_rows(&store).await;

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    struct Table {
        name: String,
        sql: String,
    }
    let tables = |store: StoreHandle| async move {
        let rows: Vec<Table> = store
            .query_consistent(
                "SELECT name, sql FROM sqlite_master WHERE type = 'table' \
                 AND name NOT IN ('schema_version', 'schema_set_version') ORDER BY name",
                vec![],
            )
            .await
            .unwrap();
        rows
    };
    let tables_before = tables(store.clone()).await;

    // The same list, now with the coordination set beside it.
    let report = store
        .migrate_sets(&legacy, &[coordination_set()])
        .await
        .unwrap();
    assert_eq!(
        report.applied,
        vec![("rahi.coordination".to_owned(), 1)],
        "only the set's version 1 is recorded"
    );
    assert_eq!(
        schema_version_rows(&store).await,
        before,
        "every schema_version row is byte for byte what it was (I-3)"
    );
    assert_eq!(
        tables(store.clone()).await,
        tables_before,
        "recording rahi.coordination 1 changed no table"
    );

    f.store.shutdown().await.unwrap();
}

/// FR-007: the name grammar and the reservations, through the public API.
#[test]
fn a_library_cannot_take_a_reserved_or_malformed_name() {
    for bad in [
        "",
        "Aicortex",
        "9lives",
        "app",
        "rahi.receipts",
        "rahi.mine",
        &"x".repeat(65),
    ] {
        let err = MigrationSet::new(bad, vec![]).unwrap_err();
        assert!(matches!(err, Error::Validation(_)), "{bad:?}: {err}");
    }
    assert!(MigrationSet::new("aicortex", vec![]).is_ok());
    assert_eq!(coordination_set().name.as_str(), "rahi.coordination");
    assert_eq!(receipt_set().name.as_str(), "rahi.receipts");
    assert_eq!(
        coordination_set().migrations[0].sql,
        coordination_migration(1).sql,
        "B-12: version 1 is byte for byte coordination_migration's SQL"
    );
}
