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

    let no_parts: [Part; 0] = [];

    // Ahead across a non-additive migration: refused, whatever --adopt says.
    let err = restore::check_compatible(
        &archive_manifest(ours, schema(false)),
        &no_parts,
        &cell(false),
    )
    .expect_err("a schema this binary cannot read is refused");
    assert!(matches!(err, Error::Stale(_)), "{err}");
    assert_eq!(err.exit_code(), 2, "{err}");
    assert!(err.message().contains("version 2"), "{err}");
    restore::check_compatible(
        &archive_manifest(ours, schema(false)),
        &no_parts,
        &cell(true),
    )
    .expect_err("--adopt is about the manifest, never about the schema");

    // Ahead across an additive one: served.
    let evidence = restore::check_compatible(
        &archive_manifest(ours, schema(true)),
        &no_parts,
        &cell(false),
    )
    .expect("an additive version above the binary restores");
    assert_eq!(evidence, restore::SchemaEvidence::Recorded);

    // A manifest the binary does not name: refused without --adopt.
    let err = restore::check_compatible(
        &archive_manifest(&theirs, schema(true)),
        &no_parts,
        &cell(false),
    )
    .expect_err("an unadopted manifest is refused");
    assert!(matches!(err, Error::Stale(_)), "{err}");
    assert_eq!(err.exit_code(), 2, "{err}");
    assert!(err.message().contains("--adopt"), "{err}");
    assert!(err.message().contains(&theirs), "{err}");
    restore::check_compatible(
        &archive_manifest(&theirs, schema(true)),
        &no_parts,
        &cell(true),
    )
    .expect("--adopt restores it, and the next deploy step ledgers the change");
}

/// An app part that is an ordinary SQLite database, the way a hiqlite
/// snapshot is: `restore` writes it to the destination byte for byte.
///
/// `rows` are `schema_version` rows; `None` means the table is never created,
/// which is what a store no migration has touched looks like.
fn archived_db(rows: Option<&[(u32, &str, Option<bool>)]>) -> Vec<u8> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hiqlite.db");
    {
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch("CREATE TABLE notes (id TEXT PRIMARY KEY)")
            .unwrap();
        if let Some(rows) = rows {
            db.execute_batch(
                "CREATE TABLE schema_version (version INTEGER PRIMARY KEY, name TEXT NOT NULL, \
                 checksum TEXT NULL, additive INTEGER NULL)",
            )
            .unwrap();
            for (version, name, additive) in rows {
                db.execute(
                    "INSERT INTO schema_version (version, name, checksum, additive) \
                     VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![
                        i64::from(*version),
                        *name,
                        format!("sha256:{}", "ee".repeat(32)),
                        additive.map(i64::from),
                    ],
                )
                .unwrap();
            }
        }
    }
    std::fs::read(&path).unwrap()
}

/// An app part whose `schema_version` table has the **pre-036 shape**: the
/// two columns spec 011 created, with neither `checksum` nor `additive`
/// (spec 036 B-7, B-8 added those).
///
/// This is what a database backed up before spec 036, by a cell that had
/// applied a migration, actually looks like. It is a separate fixture from
/// [`archived_db`] because that one writes the four-column table a store
/// running this binary has, which is not evidence about a legacy archive.
fn pre_036_archived_db(rows: &[(u32, &str)]) -> Vec<u8> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hiqlite.db");
    {
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch("CREATE TABLE notes (id TEXT PRIMARY KEY)")
            .unwrap();
        db.execute_batch(
            "CREATE TABLE schema_version (version INTEGER PRIMARY KEY, name TEXT NOT NULL)",
        )
        .unwrap();
        for (version, name) in rows {
            db.execute(
                "INSERT INTO schema_version (version, name) VALUES (?1, ?2)",
                rusqlite::params![i64::from(*version), *name],
            )
            .unwrap();
        }
    }
    std::fs::read(&path).unwrap()
}

/// A pre-036 archive: its manifest records no schema, and its app part is
/// the database the evidence has to come out of.
fn legacy_archive(manifest_hash: &str, app: Vec<u8>) -> (ArchiveManifest, Vec<Part>) {
    let parts = vec![
        Part::new("app-hiqlite", "db", app),
        Part::new("rauthy", "snapshot", vec![2]),
        Part::new("keys", "ledger.key", vec![3]),
    ];
    let manifest = ArchiveManifest::over(&parts, 0, manifest_hash.to_owned(), None);
    (manifest, parts)
}

/// Spec 036 FR-008, D-12: an archive whose manifest records no history is
/// judged on the history in its own payload, or it is refused. There is no
/// unchecked restore, and `--adopt` never buys one.
#[test]
fn a_legacy_archive_is_judged_on_the_database_it_carries_or_refused() {
    let ours = common::manifest_hash_text();
    let list = migrations();
    let cell = |adopt: bool| Compatibility {
        manifest_hash: ours,
        migrations: &list,
        adopt,
    };

    for adopt in [false, true] {
        // A database that has applied nothing has provably applied nothing:
        // the baseline, read out of the file's own catalogue.
        let (manifest, parts) = legacy_archive(ours, archived_db(None));
        let evidence = restore::check_compatible(&manifest, &parts, &cell(adopt))
            .expect("a legacy archive at the baseline is compatible");
        assert_eq!(
            evidence,
            restore::SchemaEvidence::ArchivedDatabase,
            "and it says where the evidence came from"
        );

        // Ahead across a version the archived database records additive.
        let (manifest, parts) = legacy_archive(
            ours,
            archived_db(Some(&[(1, "notes", Some(true)), (2, "widen", Some(true))])),
        );
        restore::check_compatible(&manifest, &parts, &cell(adopt))
            .expect("every version above this binary's last is recorded additive");

        // Ahead across one it does not: refused, with or without --adopt.
        let (manifest, parts) = legacy_archive(
            ours,
            archived_db(Some(&[
                (1, "notes", Some(true)),
                (2, "rewrite", Some(false)),
            ])),
        );
        let err = restore::check_compatible(&manifest, &parts, &cell(adopt))
            .expect_err("a schema this binary cannot read is refused");
        assert!(matches!(err, Error::Stale(_)), "{err}");
        assert_eq!(err.exit_code(), 2, "{err}");
        assert!(err.message().contains("version 2"), "{err}");

        // A version that declared nothing is not additive (D-8), so the same
        // refusal stands for a row written before spec 036.
        let (manifest, parts) = legacy_archive(
            ours,
            archived_db(Some(&[(1, "notes", Some(true)), (2, "undeclared", None)])),
        );
        restore::check_compatible(&manifest, &parts, &cell(adopt))
            .expect_err("an undeclared version above the binary is not permission");

        // A payload that yields no evidence at all: refused, never restored
        // on a warning.
        let (manifest, parts) = legacy_archive(ours, b"this is not a database".to_vec());
        let err = restore::check_compatible(&manifest, &parts, &cell(adopt))
            .expect_err("no evidence, no restore");
        assert!(matches!(err, Error::Stale(_)), "{err}");
        assert_eq!(err.exit_code(), 2, "{err}");
        assert!(
            err.message().contains("cannot be established"),
            "the message names the missing evidence: {err}"
        );

        // And an archive with no app part at all.
        let (manifest, _) = legacy_archive(ours, archived_db(None));
        let err = restore::check_compatible(&manifest, &[], &cell(adopt))
            .expect_err("nothing to read the history out of");
        assert!(matches!(err, Error::Stale(_)), "{err}");
    }
}

/// Spec 036 FR-012, D-16: what legacy support actually reaches.
///
/// D-12 says a legacy archive is judged on the history in its own payload.
/// That holds for an archive whose database has applied nothing, which is
/// the baseline read out of the file's catalogue. It does **not** reach an
/// archive whose database has applied migrations under the pre-036 table
/// shape: the history read selects `checksum` and `additive`, which that
/// table does not have, so the read fails and the archive is refused with
/// `Error::Stale`, exit 2, destination untouched.
///
/// The boundary is the table's shape, not its contents: an empty pre-036
/// table is refused too, because the read that fails is the `SELECT`. The
/// only legacy shape that restores is a database with no `schema_version`
/// table, which has provably applied nothing.
///
/// That refusal is the authorized direction (constitution: absence is never
/// permission) and this test fixes it as the behavior rather than leaving
/// the wider claim standing untested.
#[test]
fn a_pre_036_archive_that_applied_migrations_is_refused_not_restored() {
    let ours = common::manifest_hash_text();
    let list = migrations();
    let cell = |adopt: bool| Compatibility {
        manifest_hash: ours,
        migrations: &list,
        adopt,
    };

    for adopt in [false, true] {
        // Applied migrations, recorded the way spec 011 recorded them.
        let (manifest, parts) = legacy_archive(ours, pre_036_archived_db(&[(1, "notes")]));
        let err = restore::check_compatible(&manifest, &parts, &cell(adopt))
            .expect_err("D-16: this evidence cannot be read, so it is not a restore");
        assert!(matches!(err, Error::Stale(_)), "{err}");
        assert_eq!(err.exit_code(), 2, "{err}");
        assert!(
            err.message().contains("schema_version"),
            "the message names the evidence it could not read: {err}"
        );

        // And the boundary is the table's shape, not its contents: a
        // pre-036 table with no rows in it is refused for the same reason.
        // The one legacy shape that restores is a database with no
        // `schema_version` table at all, which is the baseline read out of
        // the file's own catalogue and is covered above.
        let (manifest, parts) = legacy_archive(ours, pre_036_archived_db(&[]));
        let err = restore::check_compatible(&manifest, &parts, &cell(adopt))
            .expect_err("the column the read needs is absent whether or not rows exist");
        assert!(matches!(err, Error::Stale(_)), "{err}");
    }
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

/// Spec 036 FR-009, D-13 and B-7: an applied migration whose SQL changed is
/// an integrity failure even when the store is also behind the binary.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_altered_applied_migration_outranks_the_behind_version_refusal() {
    let dir = tempfile::tempdir().unwrap();
    let stub = common::stub(b"rauthy-snapshot", false).await;
    let config = common::config(dir.path(), stub.addr);
    let keys = common::write_keys(&config.keys_dir());
    let store = common::open_store(&config, &keys).await;

    // Version 1 applied, as it was declared.
    migrate::run(&store, &migrations())
        .await
        .expect("1 applies");
    migrate::check_current(&store, &migrations())
        .await
        .expect("the store is exactly what this binary declares");

    // The binary now declares a different version 1 and a pending 2. Before
    // D-13 this reported only that the store was behind, and running the
    // named command would have applied 2 on top of a history this binary
    // cannot vouch for.
    let altered = vec![
        Migration::new(
            1,
            "notes",
            "CREATE TABLE notes (id TEXT PRIMARY KEY, body TEXT)",
        )
        .additive(),
        Migration::new(2, "widen", "CREATE TABLE widen (id TEXT PRIMARY KEY)").additive(),
    ];
    let err = migrate::check_current(&store, &altered)
        .await
        .expect_err("what ran and what this build declares are not the same migration");
    assert!(matches!(err, Error::Integrity(_)), "{err}");
    assert_eq!(err.exit_code(), 1, "{err}");
    assert!(
        err.message().contains("migration 1"),
        "it names the altered version: {err}"
    );

    // B-7 on the migrate path too: the verb refuses before it applies the
    // pending version, which is where this order already held.
    let err = migrate::run(&store, &altered)
        .await
        .expect_err("migrate refuses the altered history");
    assert!(matches!(err, Error::Integrity(_)), "{err}");
    assert_eq!(
        migrate::schema_version(&store).await.unwrap(),
        1,
        "and nothing was applied"
    );

    // The ordinary behind-version refusal is unchanged when every recorded
    // checksum agrees.
    let mut honest = migrations();
    honest.push(Migration::new(2, "widen", "CREATE TABLE widen (id TEXT PRIMARY KEY)").additive());
    let err = migrate::check_current(&store, &honest)
        .await
        .expect_err("the store is behind");
    assert!(matches!(err, Error::Stale(_)), "{err}");
    assert_eq!(err.exit_code(), 2, "{err}");
    assert!(err.message().contains("rahi migrate"), "{err}");

    store.shutdown().await.unwrap();
}

/// Spec 036 FR-010, D-14: a read that does not answer is an error, never an
/// unavailable grant diff.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_recovery_read_that_does_not_answer_is_never_an_unavailable_diff() {
    let dir = tempfile::tempdir().unwrap();
    let stub = common::stub(b"rauthy-snapshot", false).await;
    let config = common::config(dir.path(), stub.addr);
    let keys = common::write_keys(&config.keys_dir());
    let store = common::open_store(&config, &keys).await;
    let ledger = common::open_ledger(&store, &keys).await;

    let manifest = Manifest::parse(&widened()).expect("the widened manifest parses");
    migrate::adopt(&store, &ledger, &manifest, &migrations())
        .await
        .expect("the deploy step runs");
    assert!(
        ledger.current_manifest_model().await.unwrap().is_some(),
        "the chain holds the adopted manifest's text"
    );

    // The resident read stops answering, the rows all still there.
    store
        .handle()
        .execute(
            "ALTER TABLE kernel_decisions RENAME TO kernel_decisions_kept",
            vec![],
        )
        .await
        .unwrap();
    store
        .handle()
        .execute(
            "CREATE VIEW kernel_decisions AS SELECT id, prev_hash, hash, record FROM \
             kernel_decisions_kept WHERE 0",
            vec![],
        )
        .await
        .unwrap();

    let err = ledger
        .current_manifest_model()
        .await
        .expect_err("a read that did not answer is not an absent manifest");
    assert!(matches!(err, Error::Integrity(_)), "{err}");

    // And the deploy step stops rather than printing an unavailable diff.
    let err = migrate::adopt(&store, &ledger, &manifest, &migrations())
        .await
        .expect_err("the adoption stops");
    assert!(matches!(err, Error::Integrity(_)), "{err}");

    store.shutdown().await.unwrap();
}
