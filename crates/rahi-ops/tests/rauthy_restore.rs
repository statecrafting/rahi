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
/// answers health on its configured port until it is killed.
fn recording_rauthy(record: &Path) -> PathBuf {
    let script = format!(
        "#!/bin/sh\nenv > '{record}'\nexport RAHI_RECORDING_STUB=1\nexec '{exe}' --exact recording_stub_process --nocapture\n",
        record = record.display(),
        exe = std::env::current_exe().unwrap().display(),
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
    if !common::disposable_process("the_restored_snapshot_is_handed_to_rauthy_once", 40) {
        return;
    }
    let source = tempfile::tempdir().unwrap();
    let (archive, source_keys) = backed_up(source.path()).await;

    let fresh = tempfile::tempdir().unwrap();
    let config = common::config(fresh.path(), common::free_addr());
    rahi_ops::first_boot::layout(&config).unwrap();
    restore::run(
        &config,
        &archive,
        &KeySource::File(source_keys.path(rahi_ops::BACKUP_KEY_FILE)),
        &common::compatibility(),
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

    // Execute the recording stub through both supervisor starts. Read only
    // what the actual child recorded, never Command's planned environment.
    std::fs::remove_file(KeySet::of(&config).path(rahi_ops::BACKUP_PASSKEY_FILE)).unwrap();
    let record = fresh.path().join("child.env");
    let bin = recording_rauthy(&record);
    let env = BTreeMap::from([("RAHI_RAUTHY_BIN", bin.to_str().unwrap())]);
    for first in [true, false] {
        run_supervisor(&config, &env).await;
        let actual = std::fs::read_to_string(&record).unwrap();
        let restore_line = format!("{RAUTHY_RESTORE_ENV_VAR}=file:{}", snapshot.display());
        assert_eq!(
            actual.lines().any(|line| line == restore_line),
            first,
            "{actual}"
        );
        assert!(actual.lines().any(|line| line.starts_with("HQL_DATA_DIR=")));
        assert!(
            Marker::read(&restore_marker(&config))
                .unwrap()
                .unwrap()
                .rauthy_snapshot_applied
                .is_some()
        );
        assert!(snapshot.is_file());
        std::fs::remove_file(&record).unwrap();
    }
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

/// Executed only as the disposable rauthy child, with a hard lifetime even
/// if the supervisor fails to terminate it. No shell descendants or nc.
#[test]
fn recording_stub_process() {
    if std::env::var("RAHI_RECORDING_STUB").as_deref() != Ok("1") {
        return;
    }
    use std::io::{Read, Write};
    use std::time::{Duration, Instant};
    let port = std::env::var("LISTEN_PORT_HTTP").unwrap();
    let listener = std::net::TcpListener::bind(format!("127.0.0.1:{port}")).unwrap();
    listener.set_nonblocking(true).unwrap();
    let stop = Instant::now() + Duration::from_secs(10);
    while Instant::now() < stop {
        if let Ok((mut stream, _)) = listener.accept() {
            stream
                .set_read_timeout(Some(Duration::from_millis(100)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_millis(100)))
                .unwrap();
            let _ = stream.read(&mut [0; 2048]);
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

async fn run_supervisor(config: &Config, env: &BTreeMap<&str, &str>) {
    use std::time::Duration;
    let (command, supplied) = supervise::prepare_rauthy(config, env).unwrap();
    let api = rahi_ops::rauthy_api::RauthyApi::new(config.rauthy_base_url(), "test").unwrap();
    let keys = KeySet::of(config);
    let ready = async {
        supervise::wait_healthy(&api, Duration::from_secs(3)).await?;
        supervise::ready_after_health(config, &keys, &api, supplied.as_ref()).await?;
        Ok(())
    };
    let result = tokio::time::timeout(
        Duration::from_secs(6),
        supervise::supervise_with(
            command,
            ready,
            |_| async { Ok(()) },
            std::future::pending(),
            supervise::Graces {
                term: Duration::from_millis(200),
                serve: Duration::from_millis(200),
                shutdown_term: Duration::from_millis(200),
            },
        ),
    )
    .await
    .expect("supervisor is bounded");
    assert_eq!(result.reason, supervise::Reason::ServeEnded);
    assert_eq!(result.code, 0);
}

#[tokio::test]
async fn missing_pending_source_fails_closed_and_can_be_retried() {
    if !common::disposable_process("missing_pending_source_fails_closed_and_can_be_retried", 40) {
        return;
    }
    let source = tempfile::tempdir().unwrap();
    let (archive, keys) = backed_up(source.path()).await;
    let fresh = tempfile::tempdir().unwrap();
    let config = common::config(fresh.path(), common::free_addr());
    rahi_ops::first_boot::layout(&config).unwrap();
    restore::run(
        &config,
        &archive,
        &KeySource::File(keys.path(rahi_ops::BACKUP_KEY_FILE)),
        &common::compatibility(),
    )
    .await
    .unwrap();
    let secrets = KeySet::of(&config).rauthy_secrets().unwrap();
    let rendered = rahi_ops::rauthy_env::render(
        &config,
        &secrets,
        "ops-fixture",
        rahi_ops::rauthy_env::HqlPorts::from_env(&BTreeMap::<&str, &str>::new()).unwrap(),
    )
    .unwrap();
    std::fs::write(rahi_ops::rauthy_env::env_path(&config), rendered).unwrap();
    std::fs::remove_file(KeySet::of(&config).path(rahi_ops::BACKUP_PASSKEY_FILE)).unwrap();
    let marker = Marker::read(&restore_marker(&config)).unwrap().unwrap();
    let snapshot = PathBuf::from(&marker.rauthy_snapshot);
    let saved = snapshot.with_extension("saved");
    std::fs::rename(&snapshot, &saved).unwrap();
    let record = fresh.path().join("retry.env");
    let bin = recording_rauthy(&record);
    let env = BTreeMap::from([("RAHI_RAUTHY_BIN", bin.to_str().unwrap())]);
    assert!(matches!(
        supervise::prepare_rauthy(&config, &env),
        Err(rahi_types::Error::Io(_))
    ));
    assert!(!record.exists(), "missing source must fail before spawning");
    // Ordinary health cannot certify a pending marker it did not supply.
    let api = rahi_ops::rauthy_api::RauthyApi::new(config.rauthy_base_url(), "test").unwrap();
    supervise::ready_after_health(&config, &KeySet::of(&config), &api, None)
        .await
        .unwrap();
    assert_eq!(
        Marker::read(&restore_marker(&config)).unwrap().unwrap(),
        marker
    );
    std::fs::create_dir(&snapshot).unwrap();
    assert!(matches!(
        supervise::prepare_rauthy(&config, &env),
        Err(rahi_types::Error::Validation(_))
    ));
    std::fs::remove_dir(&snapshot).unwrap();
    std::fs::rename(&saved, &snapshot).unwrap();
    // A failed actual child start leaves the source retryable.
    let failing = fresh.path().join("failing-rauthy");
    std::fs::write(&failing, "#!/bin/sh\nexit 9\n").unwrap();
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&failing, std::fs::Permissions::from_mode(0o755)).unwrap();
    let failure_env = BTreeMap::from([("RAHI_RAUTHY_BIN", failing.to_str().unwrap())]);
    let (failed, _) = supervise::prepare_rauthy(&config, &failure_env).unwrap();
    let exit = supervise::supervise(
        failed,
        std::future::pending(),
        |_| async { Ok(()) },
        std::future::pending(),
    )
    .await;
    assert_eq!(exit.reason, supervise::Reason::RauthyExited);
    assert_eq!(exit.code, 9);
    assert_eq!(
        Marker::read(&restore_marker(&config)).unwrap().unwrap(),
        marker
    );
    // Preparing one source cannot mark a replacement marker as applied.
    let (_, supplied) = supervise::prepare_rauthy(&config, &env).unwrap();
    let mut changed = marker.clone();
    changed.rauthy_snapshot = saved.display().to_string();
    std::fs::write(
        restore_marker(&config),
        serde_json::to_vec(&changed).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        supervise::ready_after_health(&config, &KeySet::of(&config), &api, supplied.as_ref()).await,
        Err(rahi_types::Error::Conflict(_))
    ));
    assert_eq!(
        Marker::read(&restore_marker(&config)).unwrap().unwrap(),
        changed
    );
    std::fs::write(
        restore_marker(&config),
        serde_json::to_vec(&marker).unwrap(),
    )
    .unwrap();
    run_supervisor(&config, &env).await;
    assert!(std::fs::read_to_string(record).unwrap().contains(&format!(
        "{RAUTHY_RESTORE_ENV_VAR}=file:{}",
        snapshot.display()
    )));
    assert!(
        Marker::read(&restore_marker(&config))
            .unwrap()
            .unwrap()
            .rauthy_snapshot_applied
            .is_some()
    );
}
