//! Spec 043 B-4, B-5a, FR-003 and AC-4 (a), (b), (e), (f): the cache
//! transition against real stores in temporary directories, interrupted at
//! every fault point and resumed.
//!
//! The legacy store is a real hiqlite store opened at the legacy path; the
//! workspace takes no dependency on hiqlite 0.14 (FR-010), so the old-format
//! caches are this build's own. What is under test here is the verb's
//! mechanics: the guard, quiescence, the identity plan, evidence, the floor,
//! and recovery by identity. The real v0.2.0 legs run in the live workflow.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]

use std::collections::BTreeMap;
use std::fs::File;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use rahi_ops::archive::{self, ArchiveManifest, Part};
use rahi_ops::cell_lock::{self, Legacy, SupervisorFence};
use rahi_ops::upgrade::{self, Faults, NoFaults, Outcome, Phase};
use rahi_ops::{KeySet, rauthy_env};
use rahi_store::Store;
use rahi_types::{Config, Error, Result};

struct Volume {
    _dir: tempfile::TempDir,
    env: BTreeMap<String, String>,
    archive: PathBuf,
}

impl Volume {
    fn config(&self) -> Config {
        Config::from_env(&self.env).unwrap()
    }
}

/// A pre-043 volume: keys, a store with one row at the legacy path, the
/// rendered Rauthy environment at the old path, and a verifying archive.
async fn pre043() -> Volume {
    let dir = tempfile::tempdir().unwrap();
    let env = BTreeMap::from([
        (
            "RAHI_PUBLIC_URL".to_owned(),
            "http://localhost:8080".to_owned(),
        ),
        ("RAHI_DATA_DIR".to_owned(), dir.path().display().to_string()),
        ("RAHI_HIQLITE_API_ADDR".to_owned(), free_addr().to_string()),
        ("RAHI_HIQLITE_RAFT_ADDR".to_owned(), free_addr().to_string()),
    ]);
    let config = Config::from_env(&env).unwrap();
    let keys = KeySet::of(&config);
    rahi_ops::keys::generate(&keys).unwrap();

    let mut legacy_cfg =
        rahi_ops::store_config(&config, &env, keys.store_secrets().unwrap()).unwrap();
    legacy_cfg.data_dir = config.legacy_hiqlite_dir();
    let store = Store::open(&legacy_cfg).await.unwrap();
    let handle = store.handle();
    handle
        .execute("CREATE TABLE app_rows (v TEXT NOT NULL)", vec![])
        .await
        .unwrap();
    handle
        .execute("INSERT INTO app_rows (v) VALUES ('kept')", vec![])
        .await
        .unwrap();
    store.shutdown().await.unwrap();
    drop(store);
    clean_stop(&config.legacy_hiqlite_dir());

    let old_env = rauthy_env::legacy_env_path(&config);
    std::fs::create_dir_all(old_env.parent().unwrap()).unwrap();
    std::fs::write(&old_env, b"RAUTHY_OLD=1\n").unwrap();

    let parts = vec![
        Part::new(archive::APP_DIR, "a.sqlite", b"app".to_vec()),
        Part::new(archive::RAUTHY_DIR, "r.sqlite", b"rauthy".to_vec()),
        Part::new(archive::KEYS_DIR, "ledger.key", b"k".to_vec()),
    ];
    let manifest = ArchiveManifest::over(&parts, 1, "sha256:test".to_owned(), None);
    let sealed = archive::seal(&parts, &manifest, &keys.backup_recipient().unwrap()).unwrap();
    let archive = dir.path().join("pre-upgrade.tar.age");
    std::fs::write(&archive, sealed).unwrap();
    Volume {
        _dir: dir,
        env,
        archive,
    }
}

/// What a pre-043 node's clean stop leaves (D-P12): SQLite closed, so no
/// `-wal` and no `-shm` beside the database. In this process hiqlite's
/// connections outlive `shutdown`, so the WAL is checkpointed into the
/// database and the two files are removed, which a closing last connection
/// would have done.
fn clean_stop(store_dir: &Path) {
    let db = store_dir
        .join("state_machine")
        .join("db")
        .join("hiqlite.db");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
        .unwrap();
    drop(conn);
    for suffix in ["-wal", "-shm"] {
        let path = PathBuf::from(format!("{}{suffix}", db.display()));
        if path.exists() {
            std::fs::remove_file(path).unwrap();
        }
    }
}

async fn run(volume: &Volume, faults: &dyn Faults) -> Result<Outcome> {
    upgrade::run(&volume.config(), &volume.env, &volume.archive, faults).await
}

/// Fails the `n`th fault point it meets, as a crash there would.
struct FailAt {
    n: usize,
    seen: AtomicUsize,
    last: Mutex<String>,
}

impl FailAt {
    fn new(n: usize) -> Self {
        Self {
            n,
            seen: AtomicUsize::new(0),
            last: Mutex::new(String::new()),
        }
    }
}

impl Faults for FailAt {
    fn hit(&self, point: &str) -> Result<()> {
        if self.seen.fetch_add(1, Ordering::SeqCst) + 1 == self.n {
            point.clone_into(&mut self.last.lock().unwrap());
            return Err(Error::Io(format!("injected fault at {point}")));
        }
        Ok(())
    }
}

/// Fails once at one named fault point.
struct FailOn {
    point: &'static str,
    hit: AtomicBool,
}

impl FailOn {
    fn new(point: &'static str) -> Self {
        Self {
            point,
            hit: AtomicBool::new(false),
        }
    }
}

impl Faults for FailOn {
    fn hit(&self, point: &str) -> Result<()> {
        if point == self.point && !self.hit.swap(true, Ordering::SeqCst) {
            return Err(Error::Io(format!("injected fault at {point}")));
        }
        Ok(())
    }
}

enum Pre043Action {
    HoldLog,
    TruncateMarker,
    UnlinkMarker,
    LeaveDatabaseOpen,
}

/// Performs one pre-043 action after T1 publishes and syncs its guard.
struct Pre043Interleave {
    config: Config,
    action: Pre043Action,
    ran: AtomicBool,
    held: Mutex<Option<File>>,
}

impl Pre043Interleave {
    fn new(config: Config, action: Pre043Action) -> Self {
        Self {
            config,
            action,
            ran: AtomicBool::new(false),
            held: Mutex::new(None),
        }
    }
}

impl Faults for Pre043Interleave {
    fn hit(&self, point: &str) -> Result<()> {
        if point != "t1.dirsync" || self.ran.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let legacy = self.config.legacy_hiqlite_dir();
        let marker = rahi_ops::legacy_marker(&self.config);
        match self.action {
            Pre043Action::HoldLog => {
                let path = legacy.join("logs").join("lock.hql");
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                let file = std::fs::OpenOptions::new()
                    .create(true)
                    .truncate(false)
                    .write(true)
                    .open(path)
                    .unwrap();
                file.try_lock().unwrap();
                *self.held.lock().unwrap() = Some(file);
            }
            Pre043Action::TruncateMarker => {
                drop(File::create(marker).unwrap());
            }
            Pre043Action::UnlinkMarker => {
                std::fs::remove_file(marker).unwrap();
            }
            Pre043Action::LeaveDatabaseOpen => {
                let db = legacy.join("state_machine").join("db");
                std::fs::create_dir_all(&db).unwrap();
                std::fs::write(db.join("hiqlite.db-wal"), b"wal").unwrap();
                std::fs::write(db.join("hiqlite.db-shm"), b"shm").unwrap();
            }
        }
        Ok(())
    }
}

/// AC-3's end state as far as the verb reaches it (`floored`).
async fn assert_floored(volume: &Volume) {
    let config = volume.config();
    let record = upgrade::read(&config).unwrap().unwrap();
    assert_eq!(record.phase, Phase::Floored);
    // The legacy path is the fence: the guard and nothing else.
    let Legacy::Fence { marker, debris } = cell_lock::inspect_legacy(&config).unwrap() else {
        panic!("the legacy path is the fence");
    };
    assert_eq!(marker, format!("{}{}", cell_lock::GUARD_PREFIX, record.id));
    assert!(debris.is_empty(), "{debris:?}");
    assert_eq!(
        cell_lock::inspect_supervisor_fence(&config).unwrap(),
        SupervisorFence::Fence
    );
    assert_eq!(
        std::fs::read(rauthy_env::env_path(&config)).unwrap(),
        b"RAUTHY_OLD=1\n"
    );
    // The old rendered file is in evidence, whole.
    let t1e = record.evidence.iter().find(|m| m.step == "t1e").unwrap();
    assert_eq!(std::fs::read(&t1e.destination).unwrap(), b"RAUTHY_OLD=1\n");
    // The app store's caches are aside, and nothing named pre-upgrade-*.
    let aside = upgrade::aside_dir(&config, &record.id);
    assert!(aside.join("logs_cache").exists());
    let app: Vec<String> = std::fs::read_dir(config.hiqlite_dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        !app.iter().any(|n| n.starts_with("pre-upgrade-")),
        "{app:?}"
    );
    // Both old caches are aside; T3's open made the app store's new ones.
    assert!(aside.join("state_machine_cache").exists());

    // The store opens at its new path with its rows and the floor.
    let keys = KeySet::of(&config);
    let cfg = rahi_ops::store_config(&config, &volume.env, keys.store_secrets().unwrap()).unwrap();
    let store = Store::open(&cfg).await.unwrap();
    #[derive(serde::Deserialize)]
    struct Row {
        v: String,
    }
    let rows: Vec<Row> = store
        .handle()
        .query_consistent("SELECT v FROM app_rows", vec![])
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].v, "kept");
    assert_eq!(
        store.handle().revocation_floor().await.unwrap(),
        record.instant
    );
    store.shutdown().await.unwrap();
}

#[tokio::test]
async fn the_transition_reaches_floored_and_a_second_run_changes_nothing() {
    let volume = pre043().await;
    let Outcome::Floored { instant, .. } = run(&volume, &NoFaults).await.unwrap() else {
        panic!("floored");
    };
    assert!(instant > 0);
    assert_floored(&volume).await;
    let before = upgrade::read(&volume.config()).unwrap();
    assert_eq!(
        run(&volume, &NoFaults).await.unwrap(),
        Outcome::Already {
            phase: Phase::Floored
        }
    );
    assert_eq!(upgrade::read(&volume.config()).unwrap(), before);
    // The gate now admits serve and supervise.
    assert!(cell_lock::gate(&volume.config(), cell_lock::Entry::Serve).is_ok());
}

/// FR-003, AC-4 (a): every fault point, then a rerun that reaches the end.
#[tokio::test]
async fn every_interruption_resumes_to_the_same_end_state() {
    let mut n = 1;
    loop {
        let volume = pre043().await;
        let faults = FailAt::new(n);
        let first = run(&volume, &faults).await;
        let point = faults.last.lock().unwrap().clone();
        if point.is_empty() {
            assert!(first.is_ok(), "the uninterrupted run completes: {first:?}");
            break;
        }
        assert!(first.is_err(), "the fault at {point} stopped the run");
        let resumed = run(&volume, &NoFaults).await;
        assert!(
            matches!(
                resumed,
                Ok(Outcome::Floored { .. }
                    | Outcome::Already {
                        phase: Phase::Floored
                    })
            ),
            "after a fault at {point} (#{n}): {resumed:?}"
        );
        assert_floored(&volume).await;
        n += 1;
    }
    assert!(n > 15, "the verb exposes its fault points: {n}");
}

/// AC-3 and D-23: without a verifying archive the verb refuses before it
/// records `begin`, so it writes no guard, no fence and no record.
#[tokio::test]
async fn the_verb_without_a_verifying_archive_refuses_and_changes_nothing() {
    let volume = pre043().await;
    let config = volume.config();
    std::fs::write(&volume.archive, b"not an archive").unwrap();
    let marker = rahi_ops::legacy_marker(&config);
    let before = std::fs::read(&marker).ok();
    assert!(run(&volume, &NoFaults).await.is_err());
    assert!(upgrade::read(&config).unwrap().is_none(), "no record");
    assert_eq!(std::fs::read(&marker).ok(), before, "no guard");
    assert!(
        config.data_dir.join("rauthy").join("rauthy.env").is_file(),
        "no supervisor fence"
    );
    assert!(!config.hiqlite_dir().join("state_machine").exists());
}

/// AC-5: a live pre-043 node (its log lock held) refuses T1 and leaves the
/// guard; an unclean stop's marker refuses and is left alone.
#[tokio::test]
async fn a_live_or_uncleanly_stopped_pre043_node_is_refused() {
    let volume = pre043().await;
    let config = volume.config();
    let lock = config.legacy_hiqlite_dir().join("logs").join("lock.hql");
    std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
    let held = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock)
        .unwrap();
    held.try_lock().unwrap();
    let err = run(&volume, &NoFaults).await.unwrap_err();
    assert!(err.message().contains("live"), "{err}");
    let marker = rahi_ops::legacy_marker(&config);
    let record = upgrade::read(&config).unwrap().unwrap();
    assert_eq!(
        std::fs::read_to_string(&marker).unwrap(),
        format!("{}{}", cell_lock::GUARD_PREFIX, record.id),
        "the guard stays"
    );
    drop(held);

    let unclean = pre043().await;
    let marker = rahi_ops::legacy_marker(&unclean.config());
    std::fs::write(&marker, b"").unwrap();
    let err = run(&unclean, &NoFaults).await.unwrap_err();
    assert!(err.message().contains("uncleanly"), "{err}");
    assert_eq!(std::fs::read(&marker).unwrap(), b"", "the marker is left");

    let open_db = pre043().await;
    let db = open_db
        .config()
        .legacy_hiqlite_dir()
        .join("state_machine")
        .join("db");
    std::fs::create_dir_all(&db).unwrap();
    std::fs::write(db.join("hiqlite.db-wal"), b"").unwrap();
    let err = run(&open_db, &NoFaults).await.unwrap_err();
    assert!(err.message().contains("-wal"), "{err}");
}

/// FR-012 and AC-5: reproduce each pre-043 operation at a deterministic
/// point after the guard is durable and before T1 accepts it.
#[tokio::test]
async fn every_pre043_t1_interleaving_refuses_and_never_reaches_guarded() {
    for action in [
        Pre043Action::HoldLog,
        Pre043Action::TruncateMarker,
        Pre043Action::UnlinkMarker,
        Pre043Action::LeaveDatabaseOpen,
    ] {
        let volume = pre043().await;
        let faults = Pre043Interleave::new(volume.config(), action);
        let err = run(&volume, &faults).await.unwrap_err();
        assert!(
            err.message().contains("pre-043 node")
                || err.message().contains("database open")
                || err.message().contains("no longer this transition's guard"),
            "{err}"
        );
        let record = upgrade::read(&volume.config()).unwrap().unwrap();
        assert_eq!(record.phase, Phase::Begin, "T1 was not accepted");
    }
}

fn recreate_directory(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::write(path.join("occurrence"), bytes).unwrap();
}

fn t2_evidence(record: &upgrade::Record) -> Vec<&upgrade::EvidenceMove> {
    record.evidence.iter().filter(|m| m.step == "t2").collect()
}

/// FR-013 and AC-4 (f): three occurrences of the same debris receive three
/// names, including recovery after an intent and after a completed rename.
#[tokio::test]
async fn repeated_debris_is_recovered_by_identity_without_replacement() {
    let volume = pre043().await;
    assert!(run(&volume, &FailOn::new("t2.move.0")).await.is_err());
    let config = volume.config();
    let record = upgrade::read(&config).unwrap().unwrap();
    assert_eq!(record.phase, Phase::Relocating);
    let source = record.plan[0].source.clone();
    assert_eq!(
        source.file_name().unwrap(),
        "logs",
        "the pre-043 WAL log moves before an old start can mutate it"
    );

    recreate_directory(&source, b"one");
    assert!(
        run(&volume, &FailOn::new("evidence.t2.intent"))
            .await
            .is_err()
    );
    let record = upgrade::read(&config).unwrap().unwrap();
    let first = t2_evidence(&record)[0];
    assert!(!first.done);
    assert_eq!(std::fs::read(source.join("occurrence")).unwrap(), b"one");

    assert!(
        run(&volume, &FailOn::new("evidence.recovered"))
            .await
            .is_err()
    );
    let record = upgrade::read(&config).unwrap().unwrap();
    let first = t2_evidence(&record)[0];
    assert!(first.done);
    assert_eq!(
        std::fs::read(first.destination.join("occurrence")).unwrap(),
        b"one"
    );

    recreate_directory(&source, b"two");
    assert!(
        run(&volume, &FailOn::new("evidence.t2.rename"))
            .await
            .is_err()
    );
    let record = upgrade::read(&config).unwrap().unwrap();
    let moves = t2_evidence(&record);
    assert_eq!(moves.len(), 2);
    assert!(moves[1].done);
    assert_eq!(
        std::fs::read(moves[1].destination.join("occurrence")).unwrap(),
        b"two"
    );

    recreate_directory(&source, b"three");
    run(&volume, &NoFaults).await.unwrap();
    let record = upgrade::read(&config).unwrap().unwrap();
    let moves = t2_evidence(&record);
    assert_eq!(moves.len(), 3);
    let mut destinations = moves.iter().map(|m| &m.destination).collect::<Vec<_>>();
    destinations.sort();
    destinations.dedup();
    assert_eq!(destinations.len(), 3);
    for (moved, bytes) in
        moves
            .iter()
            .zip([b"one".as_slice(), b"two".as_slice(), b"three".as_slice()])
    {
        assert!(moved.done);
        assert_eq!(
            std::fs::read(moved.destination.join("occurrence")).unwrap(),
            bytes
        );
    }
}

/// FR-013: an evidence name occupied by another identity refuses recovery
/// and preserves both the intended source and the foreign destination.
#[tokio::test]
async fn a_foreign_evidence_identity_refuses_without_replacing_anything() {
    let volume = pre043().await;
    assert!(run(&volume, &FailOn::new("t2.move.0")).await.is_err());
    let config = volume.config();
    let record = upgrade::read(&config).unwrap().unwrap();
    let source = record.plan[0].source.clone();
    recreate_directory(&source, b"source");
    assert!(
        run(&volume, &FailOn::new("evidence.t2.intent"))
            .await
            .is_err()
    );
    let record = upgrade::read(&config).unwrap().unwrap();
    let destination = t2_evidence(&record)[0].destination.clone();
    recreate_directory(&destination, b"foreign");

    let err = run(&volume, &NoFaults).await.unwrap_err();
    assert!(err.message().contains("identity"), "{err}");
    assert_eq!(std::fs::read(source.join("occurrence")).unwrap(), b"source");
    assert_eq!(
        std::fs::read(destination.join("occurrence")).unwrap(),
        b"foreign"
    );
}

/// AC-4 (f): a non-empty app store refuses before T2's intent.
#[tokio::test]
async fn a_non_empty_app_store_refuses_t2_before_its_intent() {
    let volume = pre043().await;
    let app = volume.config().hiqlite_dir();
    std::fs::create_dir_all(&app).unwrap();
    std::fs::write(app.join("stray"), b"x").unwrap();
    let err = run(&volume, &NoFaults).await.unwrap_err();
    assert!(err.message().contains("not empty"), "{err}");
    let record = upgrade::read(&volume.config()).unwrap().unwrap();
    assert_eq!(record.phase, Phase::Verified);
    assert!(record.plan.is_empty());
}

/// AC-4 (e), B-5a: `--abort` from `relocated` returns every planned entry
/// with its identity, the original rendered file, and removes the guard;
/// the store opens at the legacy path with its rows. Refused from
/// `flooring` on.
#[tokio::test]
async fn abort_before_flooring_restores_the_operational_data() {
    let volume = pre043().await;
    let config = volume.config();
    // Stop just after `relocated` is recorded.
    struct UntilRelocated;
    impl Faults for UntilRelocated {
        fn hit(&self, point: &str) -> Result<()> {
            if point == "relocated" {
                return Err(Error::Io("stop".to_owned()));
            }
            Ok(())
        }
    }
    assert!(run(&volume, &UntilRelocated).await.is_err());
    let record = upgrade::read(&config).unwrap().unwrap();
    assert_eq!(record.phase, Phase::Relocated);
    let plan = record.plan.clone();
    assert!(!plan.is_empty());

    let outcome = upgrade::abort(&config, &NoFaults).unwrap();
    assert_eq!(
        outcome,
        Outcome::Aborted {
            id: record.id.clone()
        }
    );
    for planned in &plan {
        assert_eq!(
            upgrade::Identity::of(&planned.source).unwrap(),
            Some(planned.identity),
            "{} is back with its identity",
            planned.source.display()
        );
    }
    assert!(
        !rahi_ops::legacy_marker(&config).exists(),
        "the guard is removed"
    );
    assert_eq!(
        std::fs::read(rauthy_env::legacy_env_path(&config)).unwrap(),
        b"RAUTHY_OLD=1\n"
    );
    assert!(
        !config.hiqlite_dir().exists(),
        "the empty app store is removed"
    );
    assert_eq!(
        upgrade::read(&config).unwrap().unwrap().phase,
        Phase::Aborted
    );

    // The old layout serves its rows.
    let keys = KeySet::of(&config);
    let mut cfg =
        rahi_ops::store_config(&config, &volume.env, keys.store_secrets().unwrap()).unwrap();
    cfg.data_dir = config.legacy_hiqlite_dir();
    let store = Store::open(&cfg).await.unwrap();
    let rows: Vec<serde_json::Value> = store
        .handle()
        .query_consistent("SELECT v FROM app_rows", vec![])
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    store.shutdown().await.unwrap();

    // From flooring on, abort is refused.
    let floored = pre043().await;
    run(&floored, &NoFaults).await.unwrap();
    let err = upgrade::abort(&floored.config(), &NoFaults).unwrap_err();
    assert!(err.message().contains("archive"), "{err}");
}

/// A fresh volume needs no transition, and the fence says so.
#[tokio::test]
async fn a_fresh_or_fenced_volume_needs_no_transition() {
    let dir = tempfile::tempdir().unwrap();
    let env = BTreeMap::from([
        (
            "RAHI_PUBLIC_URL".to_owned(),
            "http://localhost:8080".to_owned(),
        ),
        ("RAHI_DATA_DIR".to_owned(), dir.path().display().to_string()),
    ]);
    let config = Config::from_env(&env).unwrap();
    let archive = dir.path().join("none");
    assert!(matches!(
        upgrade::run(&config, &env, &archive, &NoFaults)
            .await
            .unwrap(),
        Outcome::Nothing(_)
    ));
    drop(cell_lock::gate(&config, cell_lock::Entry::Serve).unwrap());
    assert!(matches!(
        upgrade::run(&config, &env, &archive, &NoFaults)
            .await
            .unwrap(),
        Outcome::Nothing(_)
    ));
}

/// The workspace's shared loopback allocator (spec 022 D-10).
fn free_addr() -> SocketAddr {
    use std::fs::{File, OpenOptions};
    use std::net::{Ipv4Addr, TcpListener};
    use std::sync::PoisonError;
    use std::sync::atomic::AtomicU32;

    const FLOOR: u32 = 20_000;
    const SPAN: u32 = 12_000;
    static NEXT: AtomicU32 = AtomicU32::new(0);
    static HELD: Mutex<Vec<File>> = Mutex::new(Vec::new());

    let dir = std::env::temp_dir().join("rahi-test-ports");
    std::fs::create_dir_all(&dir).expect("the port lock directory");
    let offset = std::process::id().wrapping_mul(7919) % SPAN;
    loop {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        assert!(n < SPAN, "every test port is taken");
        let port = u16::try_from(FLOOR + (offset + n) % SPAN).expect("a u16 port");
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(format!("{port}.lock")))
            .expect("a port lock file");
        if lock.try_lock().is_ok() && TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok() {
            HELD.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(lock);
            return SocketAddr::from(([127, 0, 0, 1], port));
        }
    }
}
