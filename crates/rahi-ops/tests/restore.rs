//! The restore verb (spec 030 FR-002): a cluster reset, once.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod common;

use std::collections::BTreeMap;
use std::io::Write as _;

use rahi_ops::archive::{self, KEYS_DIR};
use rahi_ops::backup::{self, Destination};
use rahi_ops::restore::{self, KeySource, Marker, Outcome};
use rahi_ops::{KeySet, backups_dir, restore_marker};
use rahi_types::Error;

/// Take one backup of a live deployment and return the archive path.
async fn backed_up(
    dir: &std::path::Path,
) -> (rahi_types::Config, KeySet, std::path::PathBuf, String) {
    let stub = common::stub(b"rauthy-snapshot", false).await;
    let config = common::config(dir, stub.addr);
    let keys = common::write_keys(&config.keys_dir());
    let store = common::open_store(&config, &keys).await;
    let ledger = common::open_ledger(&store, &keys).await;
    let head = ledger.head().await.unwrap().to_string();
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
    .unwrap();
    store.shutdown().await.unwrap();
    let path = backups_dir(&config).join(outcome.name);
    (config, keys, path, head)
}

#[tokio::test]
async fn restore_yields_a_verifying_store_with_the_same_keys_and_is_single_shot() {
    let source = tempfile::tempdir().unwrap();
    let (_, source_keys, archive_path, head) = backed_up(source.path()).await;

    let fresh = tempfile::tempdir().unwrap();
    let config = common::config(fresh.path(), common::free_addr());
    let key = KeySource::File(source_keys.path(rahi_ops::BACKUP_KEY_FILE));
    let outcome = restore::run(&config, &archive_path, &key)
        .await
        .expect("the archive restores into an empty volume");
    let Outcome::Restored(marker) = outcome else {
        panic!("first restore applies");
    };
    assert_eq!(
        marker.archive,
        archive_path.file_name().unwrap().to_string_lossy()
    );
    assert_eq!(
        Marker::read(&restore_marker(&config)).unwrap().unwrap(),
        marker
    );
    assert!(std::path::Path::new(&marker.rauthy_snapshot).is_file());
    assert_eq!(
        std::fs::read(&marker.rauthy_snapshot).unwrap(),
        b"rauthy-snapshot"
    );

    let keys = KeySet::of(&config);
    keys.check().expect("restored keys carry mode 0600");
    for (name, bytes) in source_keys.export().unwrap() {
        assert_eq!(
            std::fs::read(keys.path(&name)).unwrap(),
            bytes,
            "{name} matches"
        );
    }

    let store = common::open_store(&config, &keys).await;
    let ledger = common::open_ledger(&store, &keys).await;
    ledger.verify().await.expect("the restored chain verifies");
    assert_eq!(
        ledger.head().await.unwrap().to_string(),
        head,
        "the same head"
    );
    store.shutdown().await.unwrap();

    let again = restore::run(&config, &archive_path, &key).await.unwrap();
    assert!(matches!(again, Outcome::AlreadyRestored(m) if m == marker));
}

#[tokio::test]
async fn a_running_node_refuses_restore() {
    let source = tempfile::tempdir().unwrap();
    let (_, source_keys, archive_path, _) = backed_up(source.path()).await;
    let fresh = tempfile::tempdir().unwrap();
    let config = common::config(fresh.path(), common::free_addr());
    let lock = rahi_ops::app_lock_file(&config);
    std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
    std::fs::write(&lock, b"").unwrap();
    let key = KeySource::File(source_keys.path(rahi_ops::BACKUP_KEY_FILE));
    let err = restore::run(&config, &archive_path, &key)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Conflict(_)), "{err}");
    assert!(!config.keys_dir().exists(), "nothing was written");
}

#[tokio::test]
async fn a_tampered_part_is_refused_before_anything_is_written() {
    let identity = age::x25519::Identity::generate();
    let parts = vec![
        archive::Part::new(archive::APP_DIR, "a.sqlite", b"app".to_vec()),
        archive::Part::new(archive::RAUTHY_DIR, "r.sqlite", b"rauthy".to_vec()),
        archive::Part::new(KEYS_DIR, "ledger.key", b"k".to_vec()),
    ];
    let manifest = archive::ArchiveManifest::over(&parts, 0, "h".to_owned());
    // Build the same tar by hand, with one part altered after the manifest
    // named its hash, and seal it to the identity.
    let mut tar = tar::Builder::new(Vec::new());
    let mut add = |path: &str, bytes: &[u8]| {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o600);
        header.set_cksum();
        tar.append_data(&mut header, path, bytes).unwrap();
    };
    add(
        archive::MANIFEST_PATH,
        &serde_json::to_vec(&manifest).unwrap(),
    );
    add("app-hiqlite/a.sqlite", b"tampered");
    add("rauthy/r.sqlite", b"rauthy");
    add("keys/ledger.key", b"k");
    let plain = tar.into_inner().unwrap();
    let encryptor = age::Encryptor::with_recipients(std::iter::once(
        &identity.to_public() as &dyn age::Recipient
    ))
    .unwrap();
    let mut sealed = Vec::new();
    let mut w = encryptor.wrap_output(&mut sealed).unwrap();
    w.write_all(&plain).unwrap();
    w.finish().unwrap();

    let fresh = tempfile::tempdir().unwrap();
    let archive_path = fresh.path().join("rahi-backup-tampered.tar.age");
    std::fs::write(&archive_path, &sealed).unwrap();
    let key_path = fresh.path().join("backup.key");
    std::fs::write(
        &key_path,
        age::secrecy::ExposeSecret::expose_secret(&identity.to_string()),
    )
    .unwrap();
    let volume = fresh.path().join("volume");
    std::fs::create_dir_all(&volume).unwrap();
    let config = common::config(&volume, common::free_addr());

    let err = restore::run(&config, &archive_path, &KeySource::File(key_path))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Integrity(_)), "{err}");
    assert!(!config.keys_dir().exists(), "no key was written");
    assert!(!config.hiqlite_dir().exists(), "no node state was touched");
    assert!(!restore_marker(&config).exists(), "no marker was written");
}

#[tokio::test]
async fn the_wrong_key_does_not_open_the_archive() {
    let source = tempfile::tempdir().unwrap();
    let (_, _, archive_path, _) = backed_up(source.path()).await;
    let fresh = tempfile::tempdir().unwrap();
    let key_path = fresh.path().join("other.key");
    std::fs::write(&key_path, rahi_ops::generate_backup_identity()).unwrap();
    let volume = fresh.path().join("volume");
    std::fs::create_dir_all(&volume).unwrap();
    let config = common::config(&volume, common::free_addr());
    let err = restore::run(&config, &archive_path, &KeySource::File(key_path))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Unauthorized(_)), "{err}");
    assert!(!config.keys_dir().exists());
}
