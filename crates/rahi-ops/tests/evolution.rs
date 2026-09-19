//! Adoption as a deploy step, and the restore that refuses what it cannot
//! serve (spec 036 B-3, B-8, B-9, FR-005, FR-006).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod common;

use std::collections::BTreeMap;

use rahi_kernel::Manifest;
use rahi_ops::archive::{ArchiveManifest, ArchiveSchema, Part};
use rahi_ops::migrate;
use rahi_ops::restore::{self, Compatibility, KeySource};
use rahi_store::{Migration, RecordedMigration};
use rahi_types::Error;

/// The fixture manifest with one more grant: a different ceiling, and so a
/// different hash.
fn widened() -> String {
    common::MANIFEST.replace(
        "[services.notes]\ncapabilities = []",
        "[resources]\ntables = [\"notes\"]\n\n[[capabilities]]\nid = \"notes-read\"\nkind = \
         \"db.read\"\nresource = \"notes\"\n\n[services.notes]\ncapabilities = [\"notes-read\"]",
    )
}

/// A manifest whose `ledger.max_record_bytes` cannot hold its own transition.
fn cramped() -> String {
    widened().replace("max_record_bytes = 65536", "max_record_bytes = 512")
}

fn migrations() -> Vec<Migration> {
    vec![Migration::new(1, "notes", "CREATE TABLE notes (id TEXT PRIMARY KEY)").additive()]
}

/// Spec 036 B-3: one deploy step moves the schema and the ceiling, and a
/// second run of the same step appends nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn adopt_appends_once_and_says_so_when_there_is_nothing_to_adopt() {
    let dir = tempfile::tempdir().unwrap();
    let stub = common::stub(b"rauthy-snapshot", false).await;
    let config = common::config(dir.path(), stub.addr);
    let keys = common::write_keys(&config.keys_dir());
    let store = common::open_store(&config, &keys).await;
    let ledger = common::open_ledger(&store, &keys).await;

    let manifest = Manifest::parse(&widened()).expect("the widened manifest parses");
    let before = ledger.current_manifest().await.unwrap();
    assert_eq!(
        before,
        common::manifest_hash(),
        "the chain starts at genesis"
    );

    let (report, adoption) = migrate::adopt(&store, &ledger, &manifest, &migrations())
        .await
        .expect("the deploy step runs");
    assert_eq!(report.applied, vec![1], "the migration ran");
    assert!(adoption.adopted(), "and the manifest was adopted");
    assert_eq!(adoption.from, before);
    assert_eq!(adoption.to, manifest.hash().unwrap());
    assert_eq!(
        ledger.current_manifest().await.unwrap(),
        manifest.hash().unwrap(),
        "the chain now names the adopted manifest"
    );
    assert!(
        !adoption.diffed,
        "the chain held no earlier manifest text to diff against"
    );
    let said = migrate::render_adoption(&adoption);
    assert!(said.contains("not shown"), "{said}");

    // A second run of the same step: nothing to apply, nothing to append.
    let (report, adoption) = migrate::adopt(&store, &ledger, &manifest, &migrations())
        .await
        .expect("the step is idempotent");
    assert_eq!(report.applied, Vec::<u32>::new());
    assert!(!adoption.adopted(), "nothing was appended");
    let said = migrate::render_adoption(&adoption);
    assert!(said.contains("nothing appended"), "{said}");

    // A third manifest: now there is a previous ceiling in the chain, so the
    // grants added and removed are a real diff.
    let narrowed = Manifest::parse(common::MANIFEST).expect("parses");
    let (_, adoption) = migrate::adopt(&store, &ledger, &narrowed, &migrations())
        .await
        .expect("the rollback adopts");
    assert!(adoption.diffed, "the previous manifest was recoverable");
    assert_eq!(adoption.removed, vec!["notes:notes-read".to_owned()]);
    assert!(adoption.added.is_empty());
    let said = migrate::render_adoption(&adoption);
    assert!(said.contains("grants removed: notes:notes-read"), "{said}");

    store.shutdown().await.unwrap();
}

/// Spec 036 FR-006 and D-6: an oversized manifest leaves the store and the
/// chain exactly as they were.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_oversized_manifest_is_refused_before_a_migration_is_applied() {
    let dir = tempfile::tempdir().unwrap();
    let stub = common::stub(b"rauthy-snapshot", false).await;
    let config = common::config(dir.path(), stub.addr);
    let keys = common::write_keys(&config.keys_dir());
    let store = common::open_store(&config, &keys).await;
    let ledger = common::open_ledger(&store, &keys).await;

    let manifest = Manifest::parse(&cramped()).expect("the cramped manifest parses");
    let before_version = migrate::schema_version(&store).await.unwrap();
    let before_count = ledger.count().await.unwrap();
    let before_manifest = ledger.current_manifest().await.unwrap();

    let err = migrate::adopt(&store, &ledger, &manifest, &migrations())
        .await
        .expect_err("the transition does not fit ledger.max_record_bytes");
    assert!(matches!(err, Error::Validation(_)), "{err}");
    assert_eq!(err.exit_code(), 1, "{err}");
    assert!(err.message().contains("ledger.max_record_bytes"), "{err}");
    assert!(err.message().contains("512"), "{err}");

    assert_eq!(
        migrate::schema_version(&store).await.unwrap(),
        before_version,
        "no migration was applied"
    );
    assert_eq!(
        ledger.count().await.unwrap(),
        before_count,
        "no record was appended"
    );
    assert_eq!(
        ledger.current_manifest().await.unwrap(),
        before_manifest,
        "and the chain names what it named before"
    );

    store.shutdown().await.unwrap();
}

/// Spec 036 B-8: a store ahead of the binary is served only across
/// migrations recorded additive.
#[test]
fn a_store_ahead_is_served_only_across_additive_migrations() {
    let recorded = |version: u32, additive: Option<bool>| RecordedMigration {
        version,
        name: format!("v{version}"),
        checksum: Some(format!("sha256:{}", "aa".repeat(32))),
        additive,
    };

    let additive = [recorded(1, Some(true)), recorded(2, Some(true))];
    migrate::check_ahead(&additive, 1).expect("an additive version above the binary is served");

    let mixed = [recorded(1, Some(true)), recorded(2, Some(false))];
    let err = migrate::check_ahead(&mixed, 1).expect_err("a non-additive one is not");
    assert!(matches!(err, Error::Stale(_)), "{err}");
    assert_eq!(err.exit_code(), 2, "{err}");
    assert!(err.message().contains("version 2"), "{err}");
    assert!(err.message().contains("v2"), "{err}");

    let undeclared = [recorded(1, Some(true)), recorded(2, None)];
    let err = migrate::check_ahead(&undeclared, 1)
        .expect_err("a version that declared nothing is not additive");
    assert!(matches!(err, Error::Stale(_)), "{err}");

    // Below or at the binary's last: not this check's business.
    migrate::check_ahead(&mixed, 2).expect("nothing is above the binary");
}

fn archive_manifest(manifest_hash: &str, schema: Option<ArchiveSchema>) -> ArchiveManifest {
    let parts = [
        Part::new("app-hiqlite", "db", vec![1]),
        Part::new("rauthy", "snapshot", vec![2]),
        Part::new("keys", "ledger.key", vec![3]),
    ];
    ArchiveManifest::over(&parts, 0, manifest_hash.to_owned(), schema)
}

/// Spec 036 FR-005 and B-9: what a restore refuses, and on what terms it
/// accepts.
#[test]
fn restore_refuses_a_newer_non_additive_archive_and_an_unadopted_manifest() {
    let ours = common::manifest_hash_text();
    let theirs = format!("sha256:{}", "cd".repeat(32));
    let list = migrations();

    let schema = |additive: bool| {
        Some(ArchiveSchema {
            version: 2,
            migrations: vec![
                RecordedMigration {
                    version: 1,
                    name: "notes".to_owned(),
                    checksum: Some(list[0].checksum()),
                    additive: Some(true),
                },
                RecordedMigration {
                    version: 2,
                    name: "rewrite".to_owned(),
                    checksum: Some(format!("sha256:{}", "bb".repeat(32))),
                    additive: Some(additive),
                },
            ],
        })
    };
    let cell = |adopt: bool| Compatibility {
        manifest_hash: ours,
        migrations: &list,
        adopt,
    };

    // Ahead across a non-additive migration: refused, whatever --adopt says.
    let err = restore::check_compatible(&archive_manifest(ours, schema(false)), &cell(false))
        .expect_err("a schema this binary cannot read is refused");
    assert!(matches!(err, Error::Stale(_)), "{err}");
    assert_eq!(err.exit_code(), 2, "{err}");
    assert!(err.message().contains("version 2"), "{err}");
    restore::check_compatible(&archive_manifest(ours, schema(false)), &cell(true))
        .expect_err("--adopt is about the manifest, never about the schema");

    // Ahead across an additive one: served.
    restore::check_compatible(&archive_manifest(ours, schema(true)), &cell(false))
        .expect("an additive version above the binary restores");

    // A manifest the binary does not name: refused without --adopt.
    let err = restore::check_compatible(&archive_manifest(&theirs, schema(true)), &cell(false))
        .expect_err("an unadopted manifest is refused");
    assert!(matches!(err, Error::Stale(_)), "{err}");
    assert_eq!(err.exit_code(), 2, "{err}");
    assert!(err.message().contains("--adopt"), "{err}");
    assert!(err.message().contains(&theirs), "{err}");
    restore::check_compatible(&archive_manifest(&theirs, schema(true)), &cell(true))
        .expect("--adopt restores it, and the next deploy step ledgers the change");

    // An archive written before this spec records no history at all; that is
    // reported rather than passed off as a check that ran.
    let old = archive_manifest(ours, None);
    restore::check_compatible(&old, &cell(false)).expect("the manifest still matches");
    assert!(
        !restore::schema_checked(&old),
        "and the verb says the schema could not be checked"
    );
    assert!(restore::schema_checked(&archive_manifest(
        ours,
        schema(true)
    )));
}

/// Spec 036 B-9: the refusal happens before a byte of the volume is written.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_restore_writes_nothing() {
    let source = tempfile::tempdir().unwrap();
    let stub = common::stub(b"rauthy-snapshot", false).await;
    let config = common::config(source.path(), stub.addr);
    let keys = common::write_keys(&config.keys_dir());
    let store = common::open_store(&config, &keys).await;
    let ledger = common::open_ledger(&store, &keys).await;
    let hash = ledger.current_manifest().await.unwrap().to_string();
    let env: BTreeMap<&str, &str> = BTreeMap::new();
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
    let archive = rahi_ops::backups_dir(&config).join(outcome.name);

    let fresh = tempfile::tempdir().unwrap();
    let into = common::config(fresh.path(), common::free_addr());
    let stranger = format!("sha256:{}", "ef".repeat(32));
    let list = migrations();
    let err = restore::run(
        &into,
        &archive,
        &KeySource::File(keys.path(rahi_ops::BACKUP_KEY_FILE)),
        &Compatibility {
            manifest_hash: &stranger,
            migrations: &list,
            adopt: false,
        },
    )
    .await
    .expect_err("the archive's chain names a manifest this binary does not");
    assert!(matches!(err, Error::Stale(_)), "{err}");
    assert_eq!(err.exit_code(), 2, "{err}");
    assert!(
        !rahi_ops::restore_marker(&into).exists(),
        "no marker was written"
    );
    assert!(
        !into.hiqlite_dir().join("state_machine").exists(),
        "and the volume was not touched"
    );

    // With --adopt the same archive restores, and the manifest difference is
    // the next deploy step's to ledger.
    restore::run(
        &into,
        &archive,
        &KeySource::File(keys.path(rahi_ops::BACKUP_KEY_FILE)),
        &Compatibility {
            manifest_hash: &stranger,
            migrations: &list,
            adopt: true,
        },
    )
    .await
    .expect("--adopt restores it");
    assert!(rahi_ops::restore_marker(&into).exists());
}
