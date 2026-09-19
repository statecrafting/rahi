//! The worked upgrade, rollback, and restore example (spec 036 FR-003).
//!
//! Two fixture cells that differ by one grant and one additive migration,
//! driven over argv through `rahi_cli::run_with`, which is what `main`
//! calls and what returns the exit codes the example names. The cells are
//! types rather than two binaries, so one volume can be handed from v1 to v2
//! and back the way an image upgrade and a rollback hand it over.
//!
//! The example, from the spec:
//!
//! ```text
//! rahi migrate --adopt-manifest   # applies 2 (additive), appends H1 -> H2, exit 0
//! rahi supervise                  # boots: current manifest H2 == booted H2
//! ```
//!
//! then the rollback to v1 through the same step, then a v1 archive restored
//! into a v2 deployment, refused until `--adopt`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::collections::BTreeMap;
use std::net::TcpListener;
use std::path::{Path, PathBuf};

use axum::Router;
use base64::Engine as _;
use rahi_cli::{Cell, EmptyCell};
use rahi_edge::AppState;
use rahi_kernel::Manifest;
use rahi_ops::KeySet;
use rahi_store::{EncKey, EncKeys, Migration, StoreSecrets};

/// v1's manifest: the chassis manifest with one service and no grants.
const V1_MANIFEST: &str = r#"schema_version = "1.0.0"

[app]
name = "evolution"
org = "statecrafting"

[resources]
tables = ["items"]

[[capabilities]]
id = "items-read"
kind = "db.read"
resource = "items"

[[capabilities]]
id = "items-migrate"
kind = "db.migrate"
resource = "items"

[services.items]
capabilities = ["items-read"]

[ledger]
schema_version = "1.0.0"
max_record_bytes = 65536

[observability]
metrics_path = "/metrics"
otel = false

[auth]
operator_role = "rahi-operator"

[contract]
version = "1.0.0"
"#;

/// v2's manifest: one grant more (`items-migrate`), and nothing else.
fn v2_manifest() -> &'static str {
    static TEXT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    TEXT.get_or_init(|| {
        V1_MANIFEST.replace(
            r#"capabilities = ["items-read"]"#,
            r#"capabilities = ["items-read", "items-migrate"]"#,
        )
    })
}

fn migration_one() -> Migration {
    Migration::new(1, "items", "CREATE TABLE items (id TEXT PRIMARY KEY)").additive()
}

/// v2's extra migration: additive, so v1 may serve a store that has it.
fn migration_two() -> Migration {
    Migration::new(2, "items_note", "ALTER TABLE items ADD COLUMN note TEXT").additive()
}

static V1_MIGRATIONS: std::sync::LazyLock<Vec<Migration>> =
    std::sync::LazyLock::new(|| vec![migration_one()]);
static V2_MIGRATIONS: std::sync::LazyLock<Vec<Migration>> =
    std::sync::LazyLock::new(|| vec![migration_one(), migration_two()]);

/// The cell as v1 ships it.
struct V1;

/// The cell as v2 ships it: one grant and one additive migration more.
struct V2;

impl Cell for V1 {
    fn manifest() -> &'static str {
        V1_MANIFEST
    }
    fn migrations() -> &'static [Migration] {
        &V1_MIGRATIONS
    }
    fn routes(_state: AppState) -> Router {
        Router::new()
    }
}

impl Cell for V2 {
    fn manifest() -> &'static str {
        v2_manifest()
    }
    fn migrations() -> &'static [Migration] {
        &V2_MIGRATIONS
    }
    fn routes(_state: AppState) -> Router {
        Router::new()
    }
}

fn free_port() -> String {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .to_string()
}

/// One volume, as the binary's environment describes it.
struct Volume {
    dir: tempfile::TempDir,
    env: BTreeMap<String, String>,
}

impl Volume {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        write_keys(&dir.path().join("keys"));
        let env = BTreeMap::from([
            (
                "RAHI_PUBLIC_URL".to_owned(),
                "http://localhost:8080".to_owned(),
            ),
            ("RAHI_DATA_DIR".to_owned(), dir.path().display().to_string()),
            ("RAHI_HIQLITE_API_ADDR".to_owned(), free_port()),
            ("RAHI_HIQLITE_RAFT_ADDR".to_owned(), free_port()),
            ("RAHI_RAUTHY_ADDR".to_owned(), free_port()),
            ("RAHI_RAUTHY_MODE".to_owned(), "none".to_owned()),
            ("RAHI_LISTEN_ADDR".to_owned(), "127.0.0.1:0".to_owned()),
        ]);
        Self { dir, env }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn keys(&self) -> KeySet {
        KeySet::at(self.path().join("keys"))
    }
}

fn backup_identity() -> &'static str {
    static IDENTITY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    IDENTITY.get_or_init(rahi_ops::generate_backup_identity)
}

fn write_keys(dir: &Path) {
    let keys = KeySet::at(dir);
    let seed = base64::engine::general_purpose::STANDARD.encode([5u8; 32]);
    keys.write(rahi_ops::LEDGER_KEY_FILE, seed.as_bytes())
        .unwrap();
    keys.write(rahi_ops::SESSION_KEY_FILE, &[3u8; 32]).unwrap();
    let secrets = StoreSecrets {
        secret_raft: "raft-secret-for-tests-0000".to_owned(),
        secret_api: "api-secret-for-tests-00000".to_owned(),
        enc_keys: EncKeys {
            active: "test".to_owned(),
            keys: vec![EncKey {
                id: "test".to_owned(),
                key: vec![7u8; 32],
            }],
        },
    };
    keys.write(
        rahi_ops::STORE_SECRETS_FILE,
        serde_json::to_string(&secrets).unwrap().as_bytes(),
    )
    .unwrap();
    // One backup identity across every volume in this file: an upgrade, a
    // rollback, and a restore are one deployment's key set over time, and an
    // archive that the destination's own key cannot open would be testing
    // custody rather than the compatibility checks this file is about.
    keys.write(rahi_ops::BACKUP_KEY_FILE, backup_identity().as_bytes())
        .unwrap();
    keys.write(rahi_ops::ADMIN_TOKEN_FILE, b"token").unwrap();
}

/// Run one verb of cell `C` against `volume`, and return its exit code.
fn verb<C: Cell>(volume: &Volume, args: &[&str]) -> i32 {
    let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
    rahi_cli::run_with::<C>(&args, &volume.env)
}

/// `serve`'s exit code, without the listener outliving the check.
///
/// `serve` succeeds by never returning, so the example's "boots" steps are
/// run through the same boot sequence with a stop that has already resolved:
/// config, store, the schema check, the chain, the kernel with the manifest,
/// the edge, bind, stop. Every refusal the example names happens before the
/// bind, so this reports the exit code the verb would have.
fn serve_code<C: Cell>(volume: &Volume) -> i32 {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    match runtime.block_on(rahi_cli::serve::serve_until::<C>(&volume.env, async {})) {
        Ok(()) => 0,
        Err(err) => err.exit_code(),
    }
}

fn manifest_hash(text: &str) -> String {
    Manifest::parse(text).unwrap().hash().unwrap().to_string()
}

/// The chain's current manifest, read the way a reader of the volume would.
fn current_manifest<C: Cell>(volume: &Volume) -> String {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let booted = rahi_cli::Booted::open::<C>(&volume.env).await.unwrap();
        let ledger = booted.ledger().await.unwrap();
        let current = ledger.current_manifest().await.unwrap().to_string();
        booted.shutdown().await;
        current
    })
}

/// FR-003: the whole worked example, exit code by exit code.
#[test]
fn the_worked_example_runs_end_to_end() {
    let volume = Volume::new();
    let h1 = manifest_hash(V1_MANIFEST);
    let h2 = manifest_hash(v2_manifest());
    assert_ne!(h1, h2, "one grant is one different ceiling");

    // v1 ships: the deploy step applies migration 1 and writes the chain.
    assert_eq!(verb::<V1>(&volume, &["migrate", "--adopt-manifest"]), 0);
    assert_eq!(current_manifest::<V1>(&volume), h1);
    assert_eq!(serve_code::<V1>(&volume), 0, "v1 boots on its own ceiling");

    // The upgrade to v2 without the deploy step: the manifest is not the
    // chain's, and that is exit 2 naming the step, never an integrity error.
    assert_eq!(
        serve_code::<V2>(&volume),
        2,
        "B-4: an unadopted manifest is stale, not damage"
    );

    // The upgrade, as the example writes it.
    assert_eq!(verb::<V2>(&volume, &["migrate", "--adopt-manifest"]), 0);
    assert_eq!(current_manifest::<V2>(&volume), h2, "H1 -> H2 was appended");
    assert_eq!(serve_code::<V2>(&volume), 0, "v2 boots: H2 == H2");

    // The rollback to v1: migration 2 is additive, so v1 may serve a store
    // at schema 2, and the ceiling returns through the same deploy step.
    assert_eq!(
        serve_code::<V1>(&volume),
        2,
        "the chain still names H2, so v1 is stale until it adopts"
    );
    assert_eq!(verb::<V1>(&volume, &["migrate", "--adopt-manifest"]), 0);
    assert_eq!(current_manifest::<V1>(&volume), h1, "H2 -> H1 was appended");
    assert_eq!(
        serve_code::<V1>(&volume),
        0,
        "and v1 boots on H1 over a store at schema 2"
    );

    // The chain verifies at both depths after three transitions.
    assert_eq!(verb::<V1>(&volume, &["ledger", "verify"]), 0);
    assert_eq!(verb::<V1>(&volume, &["ledger", "verify", "--full"]), 0);
}

/// FR-003: the rollback the example refuses, when migration 2 is not
/// additive.
///
/// "Had migration 2 not been additive, v1's `serve` would refuse with exit 2
/// naming version 2: the rollback is then a forward fix, never a down
/// migration."
#[test]
fn a_rollback_across_a_non_additive_migration_is_refused_naming_the_version() {
    /// v2 with its second migration undeclared, which is not additive.
    struct V2Hard;
    static HARD: std::sync::LazyLock<Vec<Migration>> = std::sync::LazyLock::new(|| {
        vec![
            migration_one(),
            Migration::new(2, "items_note", "ALTER TABLE items ADD COLUMN note TEXT"),
        ]
    });
    impl Cell for V2Hard {
        fn manifest() -> &'static str {
            v2_manifest()
        }
        fn migrations() -> &'static [Migration] {
            &HARD
        }
        fn routes(_state: AppState) -> Router {
            Router::new()
        }
    }

    let volume = Volume::new();
    assert_eq!(verb::<V1>(&volume, &["migrate", "--adopt-manifest"]), 0);
    assert_eq!(verb::<V2Hard>(&volume, &["migrate", "--adopt-manifest"]), 0);

    // v1 adopts its ceiling back, and then still cannot serve the schema.
    assert_eq!(verb::<V1>(&volume, &["migrate", "--adopt-manifest"]), 0);
    assert_eq!(
        serve_code::<V1>(&volume),
        2,
        "B-8: a store ahead across a migration nobody declared additive"
    );
}

/// FR-003: the restore half of the example.
///
/// A v1 archive into a v2 deployment is refused; `--adopt` restores it; the
/// next deploy step applies migration 2 and appends H1 -> H2.
#[test]
fn a_v1_archive_into_a_v2_deployment_is_refused_until_adopt() {
    let source = Volume::new();
    assert_eq!(verb::<V1>(&source, &["migrate", "--adopt-manifest"]), 0);

    // A backup needs rauthy, which this fixture has none of, so the archive
    // is assembled from the same parts `backup` would gather. What the
    // example turns on is the archive's recorded manifest and schema, and
    // both are the live volume's.
    let archive = archive_of(&source);

    let into = Volume::new();
    assert_eq!(verb::<V2>(&into, &["migrate", "--adopt-manifest"]), 0);
    let h1 = manifest_hash(V1_MANIFEST);
    let h2 = manifest_hash(v2_manifest());
    assert_eq!(current_manifest::<V2>(&into), h2);

    let path = archive.display().to_string();
    assert_eq!(
        verb::<V2>(&into, &["restore", &path]),
        2,
        "the archive's chain names H1 and this binary's manifest is H2"
    );
    assert!(
        !rahi_ops::restore_marker(&config_of(&into)).exists(),
        "and nothing was written"
    );

    assert_eq!(verb::<V2>(&into, &["restore", &path, "--adopt"]), 0);
    assert_eq!(
        current_manifest::<V2>(&into),
        h1,
        "the restored chain is v1's, and still names H1"
    );

    assert_eq!(verb::<V2>(&into, &["migrate", "--adopt-manifest"]), 0);
    assert_eq!(
        current_manifest::<V2>(&into),
        h2,
        "the next deploy step applies migration 2 and appends H1 -> H2"
    );
    assert_eq!(serve_code::<V2>(&into), 0);
}

fn config_of(volume: &Volume) -> rahi_types::Config {
    rahi_types::Config::from_env(&volume.env).unwrap()
}

/// Seal an archive of `volume`, with the parts `backup` would gather and a
/// rauthy snapshot stood in for, since this fixture runs no rauthy.
fn archive_of(volume: &Volume) -> PathBuf {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let booted = rahi_cli::Booted::open::<V1>(&volume.env).await.unwrap();
        let ledger = booted.ledger().await.unwrap();
        let current = ledger.current_manifest().await.unwrap().to_string();
        let history = booted.store.handle().recorded_migrations().await.unwrap();
        let id = booted.store.backup().await.unwrap();
        let snapshot = booted.store.config().backup_dir().join(id.as_str());
        let bytes = tokio::fs::read(&snapshot).await.unwrap();
        booted.shutdown().await;

        let keys = volume.keys();
        let mut parts = vec![
            rahi_ops::archive::Part::new(rahi_ops::archive::APP_DIR, id.as_str(), bytes),
            rahi_ops::archive::Part::new(
                rahi_ops::archive::RAUTHY_DIR,
                "rauthy.sqlite",
                b"rauthy-snapshot".to_vec(),
            ),
        ];
        for (name, bytes) in keys.export().unwrap() {
            parts.push(rahi_ops::archive::Part::new(
                rahi_ops::archive::KEYS_DIR,
                &name,
                bytes,
            ));
        }
        let schema = Some(rahi_ops::archive::ArchiveSchema {
            version: history.last().map_or(0, |row| row.version),
            migrations: history,
        });
        let manifest = rahi_ops::archive::ArchiveManifest::over(&parts, 0, current, schema);
        manifest.check_complete().unwrap();
        let sealed =
            rahi_ops::archive::seal(&parts, &manifest, &keys.backup_recipient().unwrap()).unwrap();
        let path = volume.path().join("evolution.tar.age");
        tokio::fs::write(&path, sealed).await.unwrap();
        path
    })
}

/// The chassis cell is unaffected: its manifest is its own and the fixtures
/// above never touch it.
#[test]
fn the_empty_cell_still_names_its_own_manifest() {
    assert_ne!(
        manifest_hash(EmptyCell::MANIFEST),
        manifest_hash(V1_MANIFEST)
    );
}
