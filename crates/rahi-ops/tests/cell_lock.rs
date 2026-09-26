//! Spec 043 B-4a, B-5, FR-008 and FR-011: the cell's two locks, its two
//! fences, the no-replace rename, the whole-file publisher, and the one gate
//! every entry point passes.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};

use rahi_ops::cell_lock::{
    self, CELL_LOCK_FILE, Entry, Legacy, SupervisorFence, TRANSITION_LOCK_FILE,
};
use rahi_ops::upgrade::{self, Phase, Record};
use rahi_ops::{Published, first_boot, publish_whole, rauthy_env, rename_noreplace};
use rahi_types::{Config, Error};

fn env(data_dir: &Path) -> BTreeMap<String, String> {
    BTreeMap::from([
        (
            "RAHI_PUBLIC_URL".to_owned(),
            "http://localhost:8080".to_owned(),
        ),
        ("RAHI_DATA_DIR".to_owned(), data_dir.display().to_string()),
    ])
}

fn config(data_dir: &Path) -> Config {
    Config::from_env(&env(data_dir)).unwrap()
}

fn no_env(_: &str) -> Option<OsString> {
    None
}

fn gate(config: &Config, entry: Entry) -> rahi_types::Result<cell_lock::Gate> {
    cell_lock::gate_with_env(config, entry, &no_env)
}

/// Every path under `root`, relative, with its bytes (directories as `None`).
fn tree(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn walk(dir: &Path, root: &Path, into: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            let rel = path.strip_prefix(root).unwrap().to_path_buf();
            if path.is_dir() {
                into.insert(rel, None);
                walk(&path, root, into);
            } else {
                into.insert(rel, Some(std::fs::read(&path).unwrap()));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn write_record(config: &Config, phase: Phase) {
    let mut record = Record::begin("0".repeat(32));
    record.phase = phase;
    upgrade::write(config, &record).unwrap();
}

/// A pre-043 volume: a store at the legacy path.
fn legacy_store(config: &Config) {
    let db = config.legacy_hiqlite_dir().join("state_machine").join("db");
    std::fs::create_dir_all(&db).unwrap();
    std::fs::write(db.join("hiqlite.db"), b"sqlite").unwrap();
    std::fs::write(rahi_ops::legacy_marker(config), b"").unwrap();
}

#[test]
fn a_no_replace_rename_never_replaces_a_file_or_a_non_empty_directory() {
    let dir = tempfile::tempdir().unwrap();
    let from = dir.path().join("from");
    std::fs::write(&from, b"from").unwrap();

    let file = dir.path().join("file");
    std::fs::write(&file, b"file").unwrap();
    assert!(matches!(
        rename_noreplace(&from, &file),
        Err(Error::Conflict(_))
    ));
    assert_eq!(std::fs::read(&from).unwrap(), b"from");
    assert_eq!(std::fs::read(&file).unwrap(), b"file");

    let full = dir.path().join("full");
    std::fs::create_dir(&full).unwrap();
    std::fs::write(full.join("inside"), b"inside").unwrap();
    let from_dir = dir.path().join("from-dir");
    std::fs::create_dir(&from_dir).unwrap();
    assert!(rename_noreplace(&from_dir, &full).is_err());
    assert!(from_dir.is_dir());
    assert_eq!(std::fs::read(full.join("inside")).unwrap(), b"inside");

    let to = dir.path().join("to");
    rename_noreplace(&from, &to).unwrap();
    assert!(!from.exists());
    assert_eq!(std::fs::read(&to).unwrap(), b"from");
}

#[test]
fn the_publisher_writes_whole_or_leaves_the_destination_alone() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("lock");
    assert_eq!(
        publish_whole(&dest, ".tmp-a", b"first").unwrap(),
        Published::Created
    );
    assert_eq!(std::fs::read(&dest).unwrap(), b"first");
    assert_eq!(
        publish_whole(&dest, ".tmp-b", b"second").unwrap(),
        Published::Exists
    );
    assert_eq!(std::fs::read(&dest).unwrap(), b"first");
    let names: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, vec![OsString::from("lock")], "no temporary is left");
}

#[test]
fn locks_are_non_blocking_and_shared_only_with_shared() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("x.lock");
    let exclusive = cell_lock::try_lock(&path, true).unwrap().unwrap();
    assert!(cell_lock::try_lock(&path, true).unwrap().is_none());
    assert!(cell_lock::try_lock(&path, false).unwrap().is_none());
    drop(exclusive);
    let a = cell_lock::try_lock(&path, false).unwrap().unwrap();
    let b = cell_lock::try_lock(&path, false).unwrap().unwrap();
    assert!(cell_lock::try_lock(&path, true).unwrap().is_none());
    drop((a, b));
    assert!(cell_lock::try_lock(&path, true).unwrap().is_some());
}

#[test]
fn a_fresh_volume_gets_both_fences_before_any_app_store() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    let gate = gate(&config, Entry::Serve).unwrap();
    assert!(gate.fenced());
    assert!(gate.owns_cell());
    let Legacy::Fence { marker, debris } = gate.legacy() else {
        panic!("the legacy path is the fence: {:?}", gate.legacy());
    };
    assert!(marker.starts_with(cell_lock::FENCE_PREFIX));
    assert!(debris.is_empty());
    assert_eq!(gate.supervisor_fence(), SupervisorFence::Fence);
    assert!(
        rauthy_env::legacy_env_path(&config)
            .join(cell_lock::SUPERVISOR_FENCE_FILE)
            .is_file()
    );
    assert!(
        !config.hiqlite_dir().exists(),
        "the gate creates no app store"
    );
    // The legacy path is the marker and nothing else.
    let legacy = tree(&config.legacy_hiqlite_dir());
    assert_eq!(
        legacy.keys().cloned().collect::<Vec<_>>(),
        vec![
            PathBuf::from("state_machine"),
            PathBuf::from("state_machine/lock")
        ]
    );
}

#[test]
fn each_refused_variable_refuses_before_any_lock() {
    for (name, _) in cell_lock::REFUSED_ENV {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        for entry in [
            Entry::UpgradeCache,
            Entry::Abort,
            Entry::Supervise,
            Entry::Serve,
            Entry::FirstBoot,
            Entry::Store { may_attach: true },
            Entry::Restore,
        ] {
            let set = |key: &str| (key == name).then(|| OsString::from("true"));
            let err = cell_lock::gate_with_env(&config, entry, &set).unwrap_err();
            assert!(matches!(err, Error::Config(_)), "{name}: {err}");
            assert!(err.message().contains(name));
        }
        assert!(
            tree(dir.path()).is_empty(),
            "{name}: nothing on the volume changed"
        );
    }
}

#[test]
fn a_legacy_store_is_refused_by_every_entry_point_but_the_verb_and_first_boot() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    legacy_store(&config);
    for entry in [
        Entry::Supervise,
        Entry::Serve,
        Entry::Store { may_attach: false },
        Entry::Restore,
    ] {
        let err = gate(&config, entry).unwrap_err();
        assert!(matches!(err, Error::Conflict(_)), "{entry:?}: {err}");
        assert!(err.message().contains("rahi upgrade-cache"), "{err}");
    }
    let before = tree(&config.legacy_hiqlite_dir());
    for entry in [Entry::UpgradeCache, Entry::Abort, Entry::FirstBoot] {
        let gate = gate(&config, entry).unwrap();
        assert!(!gate.fenced());
        assert!(matches!(gate.legacy(), Legacy::Other { .. }));
    }
    assert_eq!(tree(&config.legacy_hiqlite_dir()), before);
    assert!(!rauthy_env::legacy_env_path(&config).exists());
}

#[test]
fn a_foreign_or_absent_marker_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    std::fs::create_dir_all(config.legacy_hiqlite_dir().join("state_machine")).unwrap();
    let err = gate(&config, Entry::Serve).unwrap_err();
    assert!(err.message().contains("is absent"), "{err}");
    std::fs::write(rahi_ops::legacy_marker(&config), b"").unwrap();
    let err = gate(&config, Entry::Serve).unwrap_err();
    assert!(err.message().contains("not a fence"), "{err}");
}

#[test]
fn debris_beside_the_fence_is_reported_and_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    drop(gate(&config, Entry::Serve).unwrap());
    let legacy = config.legacy_hiqlite_dir();
    std::fs::create_dir_all(legacy.join("logs")).unwrap();
    std::fs::write(legacy.join("logs").join("lock.hql"), b"").unwrap();
    std::fs::write(legacy.join("state_machine").join("meta.hql"), b"x").unwrap();
    let before = tree(&legacy);
    let gate = gate(&config, Entry::Serve).unwrap();
    assert!(!gate.fenced());
    assert_eq!(
        gate.debris(),
        &[
            PathBuf::from("logs"),
            PathBuf::from("state_machine/meta.hql")
        ]
    );
    assert_eq!(tree(&legacy), before);
}

#[test]
fn a_legacy_database_file_beside_the_fence_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    drop(gate(&config, Entry::Serve).unwrap());
    let db = config.legacy_hiqlite_dir().join("state_machine").join("db");
    std::fs::create_dir_all(&db).unwrap();
    std::fs::write(db.join("hiqlite.db"), b"sqlite").unwrap();
    let err = gate(&config, Entry::Serve).unwrap_err();
    assert!(
        err.message().contains("a store is at the legacy path"),
        "{err}"
    );
}

#[test]
fn each_entry_point_accepts_exactly_its_transition_states() {
    let serving = [Phase::Floored, Phase::RauthyDone, Phase::Done];
    let all = [
        Phase::Begin,
        Phase::Guarded,
        Phase::Verifying,
        Phase::Verified,
        Phase::Relocating,
        Phase::Relocated,
        Phase::Flooring,
        Phase::Floored,
        Phase::RauthyDone,
        Phase::Done,
        Phase::Aborted,
    ];
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    drop(gate(&config, Entry::Serve).unwrap());
    for phase in all {
        write_record(&config, phase);
        // Spec 043 D-22: the store verbs accept what supervise and serve do.
        for entry in [
            Entry::Supervise,
            Entry::Serve,
            Entry::Store { may_attach: false },
        ] {
            assert_eq!(
                gate(&config, entry).is_ok(),
                serving.contains(&phase),
                "{entry:?} at {phase:?}"
            );
        }
        assert_eq!(
            gate(&config, Entry::Restore).is_ok(),
            phase == Phase::Done,
            "restore at {phase:?}"
        );
        for entry in [Entry::UpgradeCache, Entry::Abort, Entry::FirstBoot] {
            assert!(gate(&config, entry).is_ok(), "{entry:?} at {phase:?}");
        }
    }
}

#[test]
fn a_second_owner_is_refused_and_an_attaching_verb_attaches() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    let serving = gate(&config, Entry::Serve).unwrap();
    for entry in [
        Entry::Serve,
        Entry::Supervise,
        Entry::FirstBoot,
        Entry::Store { may_attach: false },
        Entry::UpgradeCache,
        Entry::Restore,
    ] {
        let err = gate(&config, entry).unwrap_err();
        assert!(err.message().contains(CELL_LOCK_FILE), "{entry:?}: {err}");
    }
    let attached = gate(&config, Entry::Store { may_attach: true }).unwrap();
    assert!(!attached.owns_cell());
    assert!(!attached.transition_lock().exclusive());
    drop((serving, attached));
}

#[test]
fn a_layout_change_excludes_every_other_entry_point_and_waits_for_none() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    drop(gate(&config, Entry::Serve).unwrap());
    // An attached verb holds the layout shared: no restore begins.
    let holder = cell_lock::try_lock(&dir.path().join(CELL_LOCK_FILE), true)
        .unwrap()
        .unwrap();
    let attached = gate(&config, Entry::Store { may_attach: true }).unwrap();
    drop(holder);
    let err = gate(&config, Entry::Restore).unwrap_err();
    assert!(err.message().contains(TRANSITION_LOCK_FILE), "{err}");
    drop(attached);
    // A restore holds it exclusively: nothing attaches or starts.
    let restoring = gate(&config, Entry::Restore).unwrap();
    let holder_path = dir.path().join(TRANSITION_LOCK_FILE);
    assert!(cell_lock::try_lock(&holder_path, false).unwrap().is_none());
    drop(restoring);
    assert!(gate(&config, Entry::Store { may_attach: true }).is_ok());
}

#[test]
fn concurrent_entry_points_have_exactly_one_admitted_lock_owner() {
    for (left, right) in [
        (Entry::FirstBoot, Entry::UpgradeCache),
        (Entry::Restore, Entry::FirstBoot),
        (Entry::Store { may_attach: true }, Entry::UpgradeCache),
        (Entry::Store { may_attach: true }, Entry::Restore),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        drop(gate(&config, Entry::Serve).unwrap());
        let barrier = Arc::new(Barrier::new(3));
        let results = std::thread::scope(|scope| {
            let start = Arc::clone(&barrier);
            let left_config = config.clone();
            let a = scope.spawn(move || {
                start.wait();
                let result = gate(&left_config, left);
                start.wait();
                result
            });
            let start = Arc::clone(&barrier);
            let right_config = config.clone();
            let b = scope.spawn(move || {
                start.wait();
                let result = gate(&right_config, right);
                start.wait();
                result
            });
            barrier.wait();
            barrier.wait();
            [a.join().unwrap(), b.join().unwrap()]
        });
        assert_eq!(
            results.iter().filter(|result| result.is_ok()).count(),
            1,
            "{left:?} racing {right:?}: {results:?}"
        );
    }
}

#[tokio::test]
async fn first_boot_fences_a_fresh_volume_and_opens_neither_store() {
    let dir = tempfile::tempdir().unwrap();
    let env = env(dir.path());
    let config = Config::from_env(&env).unwrap();
    first_boot::run(&env, "hello-cell").await.unwrap();
    assert!(matches!(
        cell_lock::inspect_legacy(&config).unwrap(),
        Legacy::Fence { .. }
    ));
    assert_eq!(
        cell_lock::inspect_supervisor_fence(&config).unwrap(),
        SupervisorFence::Fence
    );
    assert!(rauthy_env::env_path(&config).is_file());
    // The app store is an empty directory, and Rauthy's directory holds only
    // what rahi renders there.
    assert!(tree(&config.hiqlite_dir()).is_empty());
    let rauthy: Vec<_> = tree(&rahi_ops::rauthy_dir(&config)).into_keys().collect();
    assert_eq!(
        rauthy,
        vec![
            PathBuf::from("config.toml"),
            PathBuf::from("rauthy.env"),
            PathBuf::from("rauthy.env/FENCE"),
        ]
    );
    // Its locks are released at exit.
    assert!(gate(&config, Entry::Serve).is_ok());
}
