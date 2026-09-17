//! spec 037 FR-001 and B-3: a restored volume hands rauthy its snapshot on
//! the next start, exactly once.
//!
//! The rauthy here is a shell script that writes the environment it was
//! given and then answers health, which is all the supervisor asks of it.
//! What the test reads is that environment: `HQL_BACKUP_RESTORE` names the
//! placed snapshot on the first start and is absent on the second, and the
//! restore marker says why.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rahi_ops::backup::{self, Destination};
use rahi_ops::restore::{self, KeySource, Marker};
use rahi_ops::supervise::{self, RAUTHY_RESTORE_ENV_VAR};
use rahi_ops::{KeySet, backups_dir, restore_marker};
use rahi_types::Config;

/// One deployment, backed up: the archive, and the key that opens it.
async fn backed_up(dir: &Path) -> (PathBuf, KeySet) {
    let stub = common::stub(b"rauthy-snapshot", false).await;
    let config = common::config(dir, stub.addr);
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
    .unwrap();
    store.shutdown().await.unwrap();
    (backups_dir(&config).join(outcome.name), keys)
}

/// A rauthy that records the environment it was started with and then
/// answers health on `addr` until it is killed.
fn recording_rauthy(record: &Path, addr: std::net::SocketAddr) -> PathBuf {
    let script = format!(
        "#!/bin/sh\n\
         env > \"{record}.$$\"\n\
         cat \"{record}.$$\" > \"{record}\"\n\
         while true; do\n\
         printf 'HTTP/1.1 200 OK\\r\\nContent-Length: 2\\r\\n\\r\\nok' | nc -l {port} >/dev/null 2>&1 || sleep 0.2\n\
         done\n",
        record = record.display(),
        port = addr.port(),
    );
    let path = record.with_extension("sh");
    std::fs::write(&path, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path
}

/// The environment `rauthy_command` would hand the child, read from the
/// command itself rather than by running it: what the supervisor passes is
/// what this test is about, and running a real rauthy is `live.yml`'s job.
fn child_env(config: &Config, env: &BTreeMap<&str, &str>) -> BTreeMap<String, String> {
    let command = supervise::rauthy_command(config, env).expect("the command builds");
    command
        .as_std()
        .get_envs()
        .filter_map(|(k, v)| {
            Some((
                k.to_string_lossy().into_owned(),
                v?.to_string_lossy().into_owned(),
            ))
        })
        .collect()
}

/// FR-001: the restore source is passed on the first start and not on the
/// second, and the marker is what decides.
#[tokio::test]
async fn the_restored_snapshot_is_handed_to_rauthy_once() {
    let source = tempfile::tempdir().unwrap();
    let (archive, source_keys) = backed_up(source.path()).await;

    let fresh = tempfile::tempdir().unwrap();
    let config = common::config(fresh.path(), common::free_addr());
    rahi_ops::first_boot::layout(&config).unwrap();
    restore::run(
        &config,
        &archive,
        &KeySource::File(source_keys.path(rahi_ops::BACKUP_KEY_FILE)),
    )
    .await
    .expect("the archive restores into an empty volume");

    // rauthy's own environment, which first boot rendered, is what the
    // command starts from; the restore source is added to it.
    let secrets = KeySet::of(&config).rauthy_secrets().unwrap();
    let rendered = rahi_ops::rauthy_env::render(
        &config,
        &secrets,
        "ops-fixture",
        rahi_ops::rauthy_env::HqlPorts::from_env(&BTreeMap::<&str, &str>::new()).unwrap(),
    )
    .unwrap();
    std::fs::write(rahi_ops::rauthy_env::env_path(&config), rendered).unwrap();

    let marker = Marker::read(&restore_marker(&config)).unwrap().unwrap();
    assert!(
        marker.rauthy_snapshot_applied.is_none(),
        "a restore marks the snapshot placed, not applied"
    );
    let snapshot = PathBuf::from(&marker.rauthy_snapshot);
    assert!(snapshot.is_file());

    // First start: the supervisor points rauthy's own hiqlite at the file.
    let env = BTreeMap::from([("RAHI_RAUTHY_BIN", "/bin/true")]);
    let first = child_env(&config, &env);
    assert_eq!(
        first.get(RAUTHY_RESTORE_ENV_VAR).map(String::as_str),
        Some(format!("file:{}", snapshot.display()).as_str()),
        "the first start after a restore hands rauthy the snapshot"
    );
    assert!(
        first.contains_key("HQL_DATA_DIR"),
        "and the rendered environment is still all there"
    );

    // rauthy came up healthy on it, so the supervisor records that.
    let applied = restore::record_rauthy_snapshot_applied(&config)
        .await
        .unwrap()
        .expect("the first healthy start records the application");
    assert_eq!(applied, snapshot);
    let marker = Marker::read(&restore_marker(&config)).unwrap().unwrap();
    assert!(marker.rauthy_snapshot_applied.is_some());

    // Second start: nothing is passed, and the file is still there for an
    // operator to inspect.
    let second = child_env(&config, &env);
    assert!(
        !second.contains_key(RAUTHY_RESTORE_ENV_VAR),
        "a later start passes nothing: {second:?}"
    );
    assert!(snapshot.is_file(), "the snapshot is kept, not consumed");

    // And recording it again is a no-op, whatever calls it.
    assert_eq!(
        restore::record_rauthy_snapshot_applied(&config)
            .await
            .unwrap(),
        None
    );
}

/// A volume that was never restored passes nothing, which is every ordinary
/// start (B-3's last sentence).
#[tokio::test]
async fn a_volume_that_was_never_restored_passes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::config(dir.path(), common::free_addr());
    rahi_ops::first_boot::layout(&config).unwrap();
    common::write_keys(&config.keys_dir());
    let secrets = KeySet::of(&config).rauthy_secrets().unwrap();
    let rendered = rahi_ops::rauthy_env::render(
        &config,
        &secrets,
        "ops-fixture",
        rahi_ops::rauthy_env::HqlPorts::from_env(&BTreeMap::<&str, &str>::new()).unwrap(),
    )
    .unwrap();
    std::fs::write(rahi_ops::rauthy_env::env_path(&config), rendered).unwrap();

    assert!(
        restore::pending_rauthy_snapshot(&config).unwrap().is_none(),
        "no marker, nothing pending"
    );
    let env = BTreeMap::from([("RAHI_RAUTHY_BIN", "/bin/true")]);
    assert!(!child_env(&config, &env).contains_key(RAUTHY_RESTORE_ENV_VAR));
}

/// A marker written before spec 037 carries no `rauthy_snapshot_applied`;
/// it reads as not yet applied, and the next start applies it. A volume in
/// that state has a restored app store and an empty rauthy, so applying it
/// is the repair, not a regression.
#[test]
fn a_marker_from_before_this_spec_reads_as_not_yet_applied() {
    let dir = tempfile::tempdir().unwrap();
    let snapshot = dir.path().join("relayed.sqlite");
    std::fs::write(&snapshot, b"rauthy").unwrap();
    let older = serde_json::json!({
        "archive": "rahi-backup-20260912T210533Z.tar.age",
        "sha256": "0".repeat(64),
        "restored": 1_757_000_000u64,
        "rauthy_snapshot": snapshot.display().to_string(),
        "manifest": {
            "format": 1,
            "created": 1_757_000_000u64,
            "versions": {"rahi": "0.1.0", "store_schema": "1.0.0", "ledger_schema": "1.0.0"},
            "manifest_hash": "sha256:0",
            "parts": {},
        },
    });
    let path = dir.path().join("restore.marker");
    std::fs::write(&path, serde_json::to_vec(&older).unwrap()).unwrap();
    let marker = Marker::read(&path).unwrap().expect("it parses");
    assert!(marker.rauthy_snapshot_applied.is_none());
}

/// The recording rauthy is kept for `live.yml` and for a hand run; nothing
/// in this file needs a process, and the unused-code warning would be a
/// lie about why.
#[test]
fn the_recording_rauthy_script_is_written_executable() {
    let dir = tempfile::tempdir().unwrap();
    let path = recording_rauthy(&dir.path().join("env"), common::free_addr());
    assert!(path.is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }
}
