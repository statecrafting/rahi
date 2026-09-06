//! spec 011 FR-003: backup produces a listed file; S3 only with RAHI_TEST_S3.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod common;

use rahi_store::{S3Backup, Store};
use rahi_types::Error;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backup_writes_a_file_under_the_backup_dir_and_lists_it() {
    let f = common::open().await;
    let store = f.store.handle();
    store
        .execute("CREATE TABLE t (id INTEGER PRIMARY KEY)", vec![])
        .await
        .unwrap();
    store
        .execute("INSERT INTO t (id) VALUES (1)", vec![])
        .await
        .unwrap();

    let id = store.backup().await.expect("leader takes a backup");
    assert!(id.as_str().starts_with("backup_node_1_"), "{id:?}");
    assert!(id.as_str().ends_with(".sqlite"), "{id:?}");

    let path = f.store.config().backup_dir().join(id.as_str());
    assert!(path.is_file(), "{} exists", path.display());

    let listed = store.backup_list_local().await.unwrap();
    assert!(listed.iter().any(|l| l.id == id), "{listed:?}");

    f.store.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s3_listing_needs_a_target() {
    let f = common::open().await;
    let err = f.store.backup_list_s3().await.unwrap_err();
    assert!(matches!(err, Error::Config(_)), "{err}");
    f.store.shutdown().await.unwrap();
}

/// Exercised only when `RAHI_TEST_S3` names a bucket, as
/// `endpoint|bucket|region|access_key|secret_key`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s3_listing_when_a_bucket_is_named() {
    let Some(spec) = std::env::var_os("RAHI_TEST_S3") else {
        eprintln!("skipped: RAHI_TEST_S3 is not set");
        return;
    };
    let spec = spec.to_string_lossy().into_owned();
    let parts: Vec<&str> = spec.split('|').collect();
    assert_eq!(
        parts.len(),
        5,
        "RAHI_TEST_S3=endpoint|bucket|region|access_key|secret_key"
    );
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = common::config(&dir.path().join("hiqlite"));
    cfg.s3 = Some(S3Backup {
        endpoint: parts[0].to_owned(),
        bucket: parts[1].to_owned(),
        region: parts[2].to_owned(),
        access_key: parts[3].to_owned(),
        secret_key: parts[4].to_owned(),
        path_style: true,
    });
    let store = Store::open(&cfg).await.unwrap();
    let id = store.backup().await.unwrap();
    let listed = store.backup_list_s3().await.unwrap();
    assert!(
        listed.iter().any(|l| l.id.as_str().contains(id.as_str())),
        "{listed:?}"
    );
    store.shutdown().await.unwrap();
}
