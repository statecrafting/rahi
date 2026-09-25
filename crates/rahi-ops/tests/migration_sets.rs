//! spec 046 FR-003, FR-004, FR-006: the serve check, the additive rule, and
//! backup and restore, per named migration set.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod common;

use std::collections::BTreeMap;

use rahi_ops::archive::{self, ArchiveManifest, ArchiveSchema, Part};
use rahi_ops::migrate;
use rahi_ops::restore::{self, Compatibility, KeySource};
use rahi_store::{Migration, MigrationSet, RecordedMigration, SetName};
use rahi_types::Error;

fn library(versions: &[(u32, bool)]) -> MigrationSet {
    let migrations = versions
        .iter()
        .map(|(v, additive)| {
            let m = Migration::new(
                *v,
                format!("aicortex-{v}"),
                format!("CREATE TABLE IF NOT EXISTS aicortex_t{v} (id TEXT PRIMARY KEY)"),
            );
            if *additive { m.additive() } else { m }
        })
        .collect();
    MigrationSet::new("aicortex", migrations).unwrap()
}

fn app() -> Vec<Migration> {
    vec![Migration::new(1, "notes", "CREATE TABLE notes (id TEXT PRIMARY KEY)").additive()]
}

/// FR-003 (the serve half): an edited library migration is an integrity
/// failure from the serve check, even with `app` behind.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serve_refuses_an_edited_library_migration_before_reporting_app_behind() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::config(dir.path(), common::free_addr());
    let keys = common::write_keys(&config.keys_dir());
    let store = common::open_store(&config, &keys).await;
    let lib = library(&[(1, true), (2, true)]);
    migrate::run_sets(&store, &[], std::slice::from_ref(&lib))
        .await
        .unwrap();

    let mut edited = lib.clone();
    edited.migrations[1].sql = "CREATE TABLE aicortex_t2 (id TEXT)".to_owned();
    let err = migrate::check_current_sets(&store, &app(), &[edited])
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Integrity(_)), "{err}");
    assert!(err.message().contains("set aicortex"), "{err}");
    assert!(err.message().contains("migration 2"), "{err}");

    // Unedited, app is behind and that is what is reported, naming migrate.
    let err = migrate::check_current_sets(&store, &app(), std::slice::from_ref(&lib))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Stale(_)), "{err}");
    store.shutdown().await.unwrap();
}

/// FR-004: a store ahead of the binary in `aicortex` by one additive version
/// serves; by one non-additive version it refuses with exit 2 naming the set
/// and the version. A set the binary does not carry is judged from 1 (D-8).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_set_ahead_serves_only_across_additive_versions() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::config(dir.path(), common::free_addr());
    let keys = common::write_keys(&config.keys_dir());
    let store = common::open_store(&config, &keys).await;

    let ahead_additive = library(&[(1, true), (2, true)]);
    migrate::run_sets(&store, &app(), &[ahead_additive])
        .await
        .unwrap();
    migrate::check_current_sets(&store, &app(), &[library(&[(1, true)])])
        .await
        .expect("ahead by one additive version serves");
    migrate::check_current_sets(&store, &app(), &[])
        .await
        .expect("an unlinked, additive-only library still serves (D-8)");
    store.shutdown().await.unwrap();

    let dir = tempfile::tempdir().unwrap();
    let config = common::config(dir.path(), common::free_addr());
    let keys = common::write_keys(&config.keys_dir());
    let store = common::open_store(&config, &keys).await;
    migrate::run_sets(&store, &app(), &[library(&[(1, true), (2, false)])])
        .await
        .unwrap();
    let err = migrate::check_current_sets(&store, &app(), &[library(&[(1, true)])])
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Stale(_)), "{err}");
    assert_eq!(err.exit_code(), 2);
    assert!(err.message().contains("migration set aicortex"), "{err}");
    assert!(err.message().contains("version 2"), "{err}");
    let err = migrate::check_current_sets(&store, &app(), &[])
        .await
        .unwrap_err();
    assert!(err.message().contains("aicortex"), "{err}");
    store.shutdown().await.unwrap();
}

/// FR-006: a backup of a store with named sets is format 2 and restores
/// under this binary; a backup with none is format 1 and carries no `sets`
/// key.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_backup_with_named_sets_is_format_two_and_restores() {
    let env: BTreeMap<&str, &str> = BTreeMap::new();
    let lib = library(&[(1, true)]);

    for with_sets in [false, true] {
        let source = tempfile::tempdir().unwrap();
        let stub = common::stub(b"rauthy-snapshot", false).await;
        let config = common::config(source.path(), stub.addr);
        let keys = common::write_keys(&config.keys_dir());
        let store = common::open_store(&config, &keys).await;
        let ledger = common::open_ledger(&store, &keys).await;
        let sets: Vec<MigrationSet> = if with_sets {
            vec![lib.clone()]
        } else {
            Vec::new()
        };
        migrate::run_sets(&store, &app(), &sets).await.unwrap();
        let hash = ledger.current_manifest().await.unwrap().to_string();
        let outcome = rahi_ops::backup::run(
            &store,
            &stub.api(),
            &keys,
            &hash,
            &rahi_ops::backup::Destination::default_for(&config),
            &env,
        )
        .await
        .unwrap();
        store.shutdown().await.unwrap();
        let path = rahi_ops::backups_dir(&config).join(outcome.name);
        let sealed = std::fs::read(&path).unwrap();
        let (manifest, _) = archive::open(&sealed, &keys.backup_identity().unwrap()).unwrap();
        let schema = manifest.schema.as_ref().unwrap();
        if with_sets {
            assert_eq!(manifest.format, archive::FORMAT_SETS);
            assert_eq!(
                schema.sets[&SetName::reference("aicortex").unwrap()].len(),
                1
            );
        } else {
            assert_eq!(
                manifest.format,
                archive::FORMAT,
                "byte-compatible with 0.2.0"
            );
            assert!(schema.sets.is_empty());
            let json = serde_json::to_string(&manifest).unwrap();
            assert!(!json.contains("\"sets\""), "no sets key: {json}");
        }

        let fresh = tempfile::tempdir().unwrap();
        let into = common::config(fresh.path(), common::free_addr());
        let list = app();
        restore::run_sets(
            &into,
            &path,
            &KeySource::File(keys.path(rahi_ops::BACKUP_KEY_FILE)),
            &Compatibility {
                manifest_hash: &hash,
                migrations: &list,
                adopt: false,
            },
            &sets,
        )
        .await
        .expect("the archive restores under this binary");
        assert!(rahi_ops::restore_marker(&into).exists());
    }
}

/// FR-006: a format-2 archive whose `aicortex` history is ahead across a
/// non-additive version is refused before anything is replaced; so is one
/// whose library checksum differs from the binary's.
#[test]
fn a_format_two_archive_ahead_across_a_non_additive_version_is_refused() {
    let ours = common::manifest_hash_text();
    let lib = library(&[(1, true)]);
    let recorded = |v: u32, checksum: String, additive: bool| RecordedMigration {
        version: v,
        name: format!("aicortex-{v}"),
        checksum: Some(checksum),
        additive: Some(additive),
    };
    let archive_with = |rows: Vec<RecordedMigration>| {
        let parts = [
            Part::new("app-hiqlite", "db", vec![1]),
            Part::new("rauthy", "snapshot", vec![2]),
            Part::new("keys", "ledger.key", vec![3]),
        ];
        let schema = ArchiveSchema {
            version: 0,
            migrations: Vec::new(),
            sets: BTreeMap::from([(SetName::reference("aicortex").unwrap(), rows)]),
        };
        (
            ArchiveManifest::over(&parts, 0, ours.to_owned(), Some(schema)),
            parts,
        )
    };
    let cell = Compatibility {
        manifest_hash: ours,
        migrations: &[],
        adopt: false,
    };
    let sets = [lib.clone()];

    let (manifest, parts) = archive_with(vec![
        recorded(1, lib.migrations[0].checksum(), true),
        recorded(2, format!("sha256:{}", "cd".repeat(32)), false),
    ]);
    assert_eq!(manifest.format, archive::FORMAT_SETS);
    let err = restore::check_compatible_sets(&manifest, &parts, &cell, &sets).unwrap_err();
    assert!(matches!(err, Error::Stale(_)), "{err}");
    assert!(err.message().contains("aicortex"), "{err}");

    let (manifest, parts) = archive_with(vec![
        recorded(1, lib.migrations[0].checksum(), true),
        recorded(2, format!("sha256:{}", "cd".repeat(32)), true),
    ]);
    restore::check_compatible_sets(&manifest, &parts, &cell, &sets)
        .expect("ahead across an additive version restores");

    let (manifest, parts) = archive_with(vec![recorded(
        1,
        format!("sha256:{}", "ab".repeat(32)),
        true,
    )]);
    let err = restore::check_compatible_sets(&manifest, &parts, &cell, &sets).unwrap_err();
    assert!(matches!(err, Error::Integrity(_)), "{err}");
}
