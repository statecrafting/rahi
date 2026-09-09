//! The backup verb (spec 030 FR-001): all four parts, or no archive.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod common;

use std::collections::BTreeMap;

use rahi_ops::archive::{self, APP_DIR, KEYS_DIR, RAUTHY_DIR};
use rahi_ops::backup::{self, Destination};
use rahi_ops::{KeySet, backups_dir};
use rahi_types::Error;

#[tokio::test]
async fn one_archive_holds_every_part() {
    let dir = tempfile::tempdir().unwrap();
    let stub = common::stub(b"rauthy-snapshot-bytes", false).await;
    let config = common::config(dir.path(), stub.addr);
    let keys = common::write_keys(&config.keys_dir());
    let store = common::open_store(&config, &keys).await;
    let ledger = common::open_ledger(&store, &keys).await;
    let hash = ledger.genesis_parent().to_string();

    let env: BTreeMap<&str, &str> = BTreeMap::new();
    let outcome = backup::run(
        &store,
        &stub.api(),
        &keys,
        &hash,
        &Destination::default_for(&config),
        &env,
    )
    .await
    .expect("a backup with every part present");

    assert!(outcome.name.starts_with(archive::NAME_PREFIX));
    assert!(outcome.name.ends_with(archive::NAME_SUFFIX));
    let path = backups_dir(&config).join(&outcome.name);
    assert!(path.is_file(), "the archive landed at {}", path.display());
    assert_eq!(outcome.location, path.display().to_string());
    assert!(
        std::fs::read_dir(backups_dir(&config)).unwrap().all(|e| !e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".partial")),
        "no partial file is left behind"
    );

    let sealed = std::fs::read(&path).unwrap();
    let (manifest, parts) = archive::open(&sealed, &keys.backup_identity().unwrap())
        .expect("the deployment's key opens its own archive");
    assert_eq!(manifest, outcome.manifest);
    assert_eq!(manifest.manifest_hash, hash);
    let dirs = |d: &str| parts.iter().filter(|p| p.dir() == d).count();
    assert_eq!(dirs(APP_DIR), 1, "one app snapshot");
    assert_eq!(dirs(RAUTHY_DIR), 1, "one rauthy snapshot");
    assert_eq!(dirs(KEYS_DIR), KeySet::REQUIRED.len(), "every key file");
    let rauthy = parts.iter().find(|p| p.dir() == RAUTHY_DIR).unwrap();
    assert_eq!(rauthy.bytes, b"rauthy-snapshot-bytes");
    assert_eq!(rauthy.name(), "backup_node_1_1.sqlite");
    let (triggers, fetched) = {
        let log = stub.log.lock().unwrap();
        (log.triggers, log.fetched.clone())
    };
    assert_eq!(triggers, 1, "rauthy was asked for exactly one snapshot");
    assert_eq!(fetched, vec!["backup_node_1_1.sqlite".to_owned()]);

    store.shutdown().await.unwrap();
}

#[tokio::test]
async fn no_rauthy_means_no_archive() {
    let dir = tempfile::tempdir().unwrap();
    let stub = common::stub(b"x", false).await;
    let config = common::config(dir.path(), stub.addr);
    let api = stub.api();
    drop(stub);
    let keys = common::write_keys(&config.keys_dir());
    let store = common::open_store(&config, &keys).await;

    let env: BTreeMap<&str, &str> = BTreeMap::new();
    let err = backup::run(
        &store,
        &api,
        &keys,
        "hash",
        &Destination::default_for(&config),
        &env,
    )
    .await
    .expect_err("no rauthy, no backup");
    assert!(matches!(err, Error::Upstream(_)), "{err}");
    assert!(
        !backups_dir(&config).exists(),
        "nothing was written to the destination"
    );
    store.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_refused_token_is_unauthorized_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let stub = common::stub(b"x", true).await;
    let config = common::config(dir.path(), stub.addr);
    let keys = common::write_keys(&config.keys_dir());
    let store = common::open_store(&config, &keys).await;

    let env: BTreeMap<&str, &str> = BTreeMap::new();
    let err = backup::run(
        &store,
        &stub.api(),
        &keys,
        "hash",
        &Destination::default_for(&config),
        &env,
    )
    .await
    .expect_err("a refused token is not a backup");
    assert!(matches!(err, Error::Unauthorized(_)), "{err}");
    assert!(!backups_dir(&config).exists());
    store.shutdown().await.unwrap();
}

#[test]
fn a_destination_parses_dirs_and_buckets() {
    assert_eq!(
        Destination::parse("/data/backups").unwrap(),
        Destination::Dir("/data/backups".into())
    );
    assert_eq!(
        Destination::parse("s3://cell-backups/prod/").unwrap(),
        Destination::S3 {
            bucket: "cell-backups".to_owned(),
            prefix: "prod".to_owned()
        }
    );
    assert!(Destination::parse("s3://").is_err());
}

#[test]
fn seal_refuses_a_manifest_that_is_incomplete_or_stale() {
    let identity = age::x25519::Identity::generate();
    let parts = vec![
        archive::Part::new(APP_DIR, "a.sqlite", b"a".to_vec()),
        archive::Part::new(RAUTHY_DIR, "r.sqlite", b"r".to_vec()),
    ];
    let manifest = archive::ArchiveManifest::over(&parts, 0, "h".to_owned());
    let err = archive::seal(&parts, &manifest, &identity.to_public()).unwrap_err();
    assert!(matches!(err, Error::Validation(_)), "no keys/ part: {err}");

    let mut parts = parts;
    parts.push(archive::Part::new(KEYS_DIR, "k", b"k".to_vec()));
    let manifest = archive::ArchiveManifest::over(&parts, 0, "h".to_owned());
    parts[0].bytes = b"changed".to_vec();
    let err = archive::seal(&parts, &manifest, &identity.to_public()).unwrap_err();
    assert!(
        matches!(err, Error::Validation(_)),
        "a part changed after hashing: {err}"
    );
}

#[test]
fn utc_stamp_is_the_expected_calendar() {
    assert_eq!(rahi_ops::utc_stamp(0), "19700101T000000Z");
    assert_eq!(rahi_ops::utc_stamp(1_767_225_600), "20260101T000000Z");
    assert_eq!(rahi_ops::utc_stamp(951_782_400), "20000229T000000Z");
}
