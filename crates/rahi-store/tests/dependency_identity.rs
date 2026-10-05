//! spec 043 FR-001: dependency identity and locked metadata.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]

use std::path::Path;
use std::process::Command;

const HIQLITE_PATCHED_CHECKSUM: &str =
    "9d3586f7db4e971836ffc5982086bcfa1de0c0293492a194efd7d4ac21ff5ebf";
const HIQLITE_WAL_PATCHED_CHECKSUM: &str =
    "df82af141317c61b135d600a93c0ec2283c40f2761ab956b20568b2c23bc2746";
const F130_CHILD_DIR: &str = "RAHI_TEST_F130_DIR";
const F130_CHILD_MODE: &str = "RAHI_TEST_F130_MODE";

#[derive(Default)]
struct LockPkg {
    name: String,
    version: String,
    source: String,
    checksum: String,
}

#[test]
fn dependency_identity_and_checksums() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("workspace root");

    let cargo_lock =
        std::fs::read_to_string(workspace_root.join("Cargo.lock")).expect("read Cargo.lock");

    let mut packages = Vec::new();
    for block in cargo_lock.split("[[package]]").skip(1) {
        let mut pkg = LockPkg::default();
        for line in block.lines() {
            let line = line.trim();
            let field = |prefix: &str| {
                line.strip_prefix(prefix)
                    .and_then(|rest| rest.strip_suffix('"'))
                    .map(str::to_owned)
            };
            if let Some(val) = field("name = \"") {
                pkg.name = val;
            } else if let Some(val) = field("version = \"") {
                pkg.version = val;
            } else if let Some(val) = field("source = \"") {
                pkg.source = val;
            } else if let Some(val) = field("checksum = \"") {
                pkg.checksum = val;
            }
        }
        packages.push(pkg);
    }

    let mut found_patched = false;
    let mut found_wal_patched = false;

    for pkg in packages {
        assert_ne!(
            pkg.name, "hiqlite",
            "Cargo.lock must not contain package 'hiqlite'; must use aliased 'hiqlite-patched'"
        );
        assert_ne!(
            pkg.name, "hiqlite-wal",
            "Cargo.lock must not contain package 'hiqlite-wal'; must use aliased 'hiqlite-wal-patched'"
        );

        if pkg.name.contains("hiqlite") {
            assert!(
                !pkg.source.contains("git+"),
                "hiqlite package '{}' must not have a git source: {}",
                pkg.name,
                pkg.source
            );
            assert!(
                pkg.source.starts_with("registry+"),
                "hiqlite package '{}' must have a registry source: {}",
                pkg.name,
                pkg.source
            );
            assert_eq!(
                pkg.version, "0.15.0-patched.3",
                "hiqlite package '{}' must be version 0.15.0-patched.3",
                pkg.name
            );
        }

        if pkg.name == "hiqlite-patched" {
            found_patched = true;
            assert_eq!(
                pkg.checksum, HIQLITE_PATCHED_CHECKSUM,
                "hiqlite-patched checksum mismatch"
            );
        } else if pkg.name == "hiqlite-wal-patched" {
            found_wal_patched = true;
            assert_eq!(
                pkg.checksum, HIQLITE_WAL_PATCHED_CHECKSUM,
                "hiqlite-wal-patched checksum mismatch"
            );
        }
    }

    assert!(found_patched, "hiqlite-patched not found in Cargo.lock");
    assert!(
        found_wal_patched,
        "hiqlite-wal-patched not found in Cargo.lock"
    );

    // Check cargo metadata --locked for both Linux targets
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        let output = Command::new("cargo")
            .current_dir(workspace_root)
            .args([
                "metadata",
                "--format-version",
                "1",
                "--locked",
                "--filter-platform",
                target,
            ])
            .output()
            .expect("cargo metadata failed to execute");

        assert!(
            output.status.success(),
            "cargo metadata for target {target} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let json: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("parse metadata JSON");
        let pkgs = json
            .get("packages")
            .and_then(serde_json::Value::as_array)
            .expect("packages array");

        let mut meta_patched = false;
        let mut meta_wal = false;

        for p in pkgs {
            let name = p
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let source = p
                .get("source")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let ver = p
                .get("version")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();

            assert_ne!(
                name, "hiqlite",
                "Target {target}: metadata must not contain package 'hiqlite'"
            );
            assert_ne!(
                name, "hiqlite-wal",
                "Target {target}: metadata must not contain package 'hiqlite-wal'"
            );

            if name.contains("hiqlite") {
                assert!(
                    !source.contains("git+"),
                    "Target {target}: package '{name}' must not have a git source"
                );
                assert_eq!(
                    ver, "0.15.0-patched.3",
                    "Target {target}: package '{name}' version mismatch"
                );
            }

            if name == "hiqlite-patched" {
                meta_patched = true;
            } else if name == "hiqlite-wal-patched" {
                meta_wal = true;
            }
        }

        assert!(
            meta_patched,
            "Target {target}: hiqlite-patched not in metadata"
        );
        assert!(
            meta_wal,
            "Target {target}: hiqlite-wal-patched not in metadata"
        );
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .expect("bind an ephemeral port")
        .local_addr()
        .expect("read the ephemeral port")
        .port()
}

fn f130_config(dir: &str) -> hiqlite::NodeConfig {
    let mut config = hiqlite::NodeConfig {
        node_id: 1,
        nodes: vec![hiqlite::Node {
            id: 1,
            addr_raft: format!("127.0.0.1:{}", free_port()),
            addr_api: format!("127.0.0.1:{}", free_port()),
        }],
        data_dir: dir.to_owned().into(),
        secret_raft: "SuperSecureSecret1337".to_owned(),
        secret_api: "SuperSecureSecret1337".to_owned(),
        cache_storage_disk: true,
        ..Default::default()
    };
    config.enc_keys.enc_key_active = "k1".into();
    config.enc_keys.enc_keys = vec![("k1".into(), vec![7_u8; 32])];
    config
}

/// Child half of the F-130 qualification. The parent supplies the environment
/// before process start so the Rust 2024 test never mutates process-wide state.
#[tokio::test]
async fn f130_child_starts_the_pinned_dependency() {
    let Ok(dir) = std::env::var(F130_CHILD_DIR) else {
        return;
    };
    let mode = std::env::var(F130_CHILD_MODE).expect("the parent supplies a child mode");
    let result = hiqlite::start_node(f130_config(&dir)).await;
    match mode.as_str() {
        "refuse" => {
            let error = match result {
                Ok(client) => {
                    let _ = client.shutdown().await;
                    panic!("an interrupted move must refuse without consent")
                }
                Err(error) => error,
            };
            assert!(error.to_string().contains("interrupted between its two renames"));
        }
        "resume" => {
            let client = result.expect("consent completes the interrupted move");
            client.shutdown().await.expect("the recovered node stops cleanly");
        }
        other => panic!("unknown F-130 child mode: {other}"),
    }
}

/// Spec 043 AC-4(g): reconstruct the exact state left by a crash between the
/// published build's two cache renames, then exercise the pinned dependency
/// through its public start API. It must refuse without consent, resume into
/// the same evidence directory with consent, and start normally afterwards.
#[test]
fn interrupted_rauthy_cache_move_is_refused_then_resumed() {
    let dir = tempfile::tempdir().expect("temporary cache volume");
    let root = dir.path();
    std::fs::create_dir_all(root.join("logs_cache")).unwrap();
    std::fs::write(root.join("logs_cache/00000000000000000001.wal"), b"legacy wal").unwrap();
    std::fs::create_dir_all(root.join("state_machine_cache/snapshots")).unwrap();
    std::fs::write(root.join("state_machine_cache/snapshots/s1"), b"legacy snapshot").unwrap();
    std::fs::create_dir_all(root.join("pre-upgrade-100")).unwrap();
    std::fs::rename(root.join("logs_cache"), root.join("pre-upgrade-100/logs_cache")).unwrap();

    let run = |mode: &str, consent: bool| {
        let mut command = Command::new(std::env::current_exe().expect("current test executable"));
        command
            .args([
                "--exact",
                "f130_child_starts_the_pinned_dependency",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(F130_CHILD_DIR, root)
            .env(F130_CHILD_MODE, mode);
        if consent {
            command.env("HQL_CACHE_LEGACY_MOVE_ASIDE", "true");
        } else {
            command.env_remove("HQL_CACHE_LEGACY_MOVE_ASIDE");
        }
        let output = command.output().expect("run the F-130 child");
        assert!(
            output.status.success(),
            "F-130 child {mode} failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    };

    run("refuse", false);
    assert!(root.join("state_machine_cache/snapshots/s1").is_file());
    run("resume", true);
    assert_eq!(
        std::fs::read(root.join("pre-upgrade-100/state_machine_cache/snapshots/s1")).unwrap(),
        b"legacy snapshot"
    );
    assert!(
        !root.join("state_machine_cache/snapshots/s1").exists(),
        "the legacy snapshot is not left where the new cache can restore it"
    );
    run("resume", false);
}
