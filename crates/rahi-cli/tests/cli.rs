//! The composer driven over argv (spec 030 FR-003, FR-004, FR-005).
//!
//! Every test spawns the `rahi` binary, the chassis with no app, against a
//! temp volume and asserts on its exit code and what it printed.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use base64::Engine as _;
use rahi_cli::VERBS;
use rahi_ops::KeySet;
use rahi_store::{EncKey, EncKeys, StoreSecrets};

fn free_port() -> String {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .to_string()
}

/// One volume: a data dir, a full key set, free ports, no rauthy.
struct Volume {
    dir: tempfile::TempDir,
    env: Vec<(String, String)>,
}

impl Volume {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let keys = KeySet::at(dir.path().join("keys"));
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
        keys.write(
            rahi_ops::BACKUP_KEY_FILE,
            rahi_ops::generate_backup_identity().as_bytes(),
        )
        .unwrap();
        keys.write(rahi_ops::ADMIN_TOKEN_FILE, b"token").unwrap();
        let env = vec![
            (
                "RAHI_PUBLIC_URL".to_owned(),
                "http://localhost:8080".to_owned(),
            ),
            ("RAHI_DATA_DIR".to_owned(), dir.path().display().to_string()),
            ("RAHI_HIQLITE_API_ADDR".to_owned(), free_port()),
            ("RAHI_HIQLITE_RAFT_ADDR".to_owned(), free_port()),
            ("RAHI_RAUTHY_ADDR".to_owned(), free_port()),
            ("RAHI_RAUTHY_MODE".to_owned(), "none".to_owned()),
            (
                "RAHI_LISTEN_ADDR".to_owned(),
                format!("127.0.0.1:{}", free_port().rsplit(':').next().unwrap()),
            ),
        ];
        Self { dir, env }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn run(&self, args: &[&str]) -> Run {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_rahi"));
        cmd.args(args);
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        cmd.env_remove(rahi_ops::RESTORE_ENV_VAR);
        Run::of(cmd.output().unwrap())
    }
}

/// A bare invocation with only the arguments and no `RAHI_*` at all.
fn bare(args: &[&str]) -> Run {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rahi"));
    cmd.args(args);
    for (k, _) in std::env::vars() {
        if k.starts_with("RAHI_") {
            cmd.env_remove(k);
        }
    }
    Run::of(cmd.output().unwrap())
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Run {
    fn of(output: Output) -> Self {
        Self {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }
}

#[test]
fn help_lists_exactly_the_verbs_of_b1() {
    let run = bare(&["--help"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    for verb in VERBS {
        assert!(
            run.stdout.lines().any(|l| l.trim_start().starts_with(verb)),
            "help lists {verb:?}:\n{}",
            run.stdout
        );
    }
    let listed = run
        .stdout
        .lines()
        .skip_while(|l| !l.starts_with("verbs:"))
        .skip(1)
        .take_while(|l| l.starts_with("  "))
        .count();
    assert_eq!(listed, VERBS.len(), "no verb beyond B-1's list");
}

#[test]
fn a_missing_or_unknown_verb_is_exit_1_with_the_usage() {
    let run = bare(&[]);
    assert_eq!(run.code, 1);
    assert!(run.stderr.contains("a verb is required"));
    assert!(run.stderr.contains("usage: rahi"));
    let run = bare(&["frobnicate"]);
    assert_eq!(run.code, 1);
    assert!(run.stderr.contains("unknown verb"));
    let run = bare(&["restore"]);
    assert_eq!(run.code, 1);
    assert!(run.stderr.contains("needs an archive"));
}

#[test]
fn preflight_without_config_fails_on_config_and_skips_the_rest() {
    let run = bare(&["preflight"]);
    assert_eq!(run.code, 1, "{}", run.stdout);
    assert!(
        run.stdout
            .contains("FAIL config: config: RAHI_PUBLIC_URL is required")
    );
    for name in rahi_ops::preflight::CHECKS.iter().skip(1) {
        assert!(
            run.stdout.contains(&format!("SKIP {name}:")),
            "{name} is reported:\n{}",
            run.stdout
        );
    }
    assert!(run.stdout.trim_end().ends_with("preflight: failed"));
}

#[test]
fn preflight_names_every_check_and_fails_on_a_key_file_mode() {
    let volume = Volume::new();
    let ledger_key = volume.path().join("keys").join(rahi_ops::LEDGER_KEY_FILE);
    std::fs::set_permissions(&ledger_key, std::fs::Permissions::from_mode(0o644)).unwrap();

    let run = volume.run(&["preflight"]);
    assert_eq!(run.code, 1, "{}\n{}", run.stdout, run.stderr);
    for name in rahi_ops::preflight::CHECKS {
        assert!(
            run.stdout
                .lines()
                .any(|l| l.contains(&format!(" {name}: "))),
            "{name} is reported by name:\n{}",
            run.stdout
        );
    }
    assert!(run.stdout.contains("PASS config:"));
    assert!(run.stdout.contains("PASS data_dir:"));
    assert!(run.stdout.contains("FAIL keys:"), "{}", run.stdout);
    assert!(run.stdout.contains("has mode 0644, expected 0600"));
    assert!(run.stdout.contains("SKIP hiqlite:"));
    assert!(run.stdout.contains("PASS restore_env:"));
    assert!(
        run.stdout.contains("FAIL rauthy:"),
        "no rauthy is listening"
    );
    assert!(run.stdout.contains("PASS disk:"));
}

#[test]
fn preflight_prints_the_engine_report_when_the_store_opens() {
    let volume = Volume::new();
    let run = volume.run(&["preflight"]);
    assert_eq!(
        run.code, 1,
        "rauthy is absent, so the run fails:\n{}",
        run.stdout
    );
    assert!(run.stdout.contains("PASS hiqlite:"), "{}", run.stdout);
    assert!(
        run.stdout
            .contains("PASS engine: extensions: none, max_value_bytes: 1048576"),
        "spec 016 AC-2:\n{}",
        run.stdout
    );
    assert!(
        run.stdout.contains("PASS ledger: no chain yet"),
        "{}",
        run.stdout
    );
    assert!(run.stdout.contains("FAIL rauthy:"));
}

#[test]
fn serve_behind_on_migrations_is_exit_2_and_names_the_command() {
    let volume = Volume::new();
    let run = volume.run(&["serve"]);
    assert_eq!(
        run.code, 2,
        "stdout:\n{}\nstderr:\n{}",
        run.stdout, run.stderr
    );
    assert!(run.stderr.contains("error: stale:"), "{}", run.stderr);
    assert!(
        run.stderr
            .contains("schema_version is 0, the cell expects 1")
    );
    assert!(run.stderr.contains("run: rahi migrate"), "{}", run.stderr);
}

#[test]
fn migrate_then_ledger_verify_and_export_are_exit_0() {
    let volume = Volume::new();
    let run = volume.run(&["migrate"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(
        run.stdout
            .contains("migrate: schema_version 0 -> 1, applied: 1"),
        "{}",
        run.stdout
    );

    let run = volume.run(&["migrate"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(run.stdout.contains("schema_version 1 -> 1, applied: none"));

    let run = volume.run(&["ledger", "verify"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(
        run.stdout.contains(
            "ledger verify: ok at resident depth; 1 resident record(s), 0 sealed segment(s)"
        ),
        "{}",
        run.stdout
    );

    let run = volume.run(&["ledger", "verify", "--full"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(run.stdout.contains("ok at full depth"));

    let out: PathBuf = volume.path().join("chain.jsonl");
    let run = volume.run(&["ledger", "export", out.to_str().unwrap()]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    let text = std::fs::read_to_string(&out).unwrap();
    assert_eq!(text.lines().count(), 1, "genesis only:\n{text}");
    let record: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    assert!(record.get("record_hash").is_some());
    assert!(record.get("signature").is_some());
}

#[test]
fn migrate_on_a_bad_restore_env_is_refused_before_the_node_opens() {
    let volume = Volume::new();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rahi"));
    cmd.arg("migrate");
    for (k, v) in &volume.env {
        cmd.env(k, v);
    }
    cmd.env(rahi_ops::RESTORE_ENV_VAR, "file:/nowhere");
    let run = Run::of(cmd.output().unwrap());
    assert_eq!(run.code, 3, "{}", run.stderr);
    assert!(run.stderr.contains("HQL_BACKUP_RESTORE is set"));
    assert!(
        !volume.path().join("hiqlite").exists(),
        "the node was never opened"
    );
}

#[test]
fn preflight_with_the_restore_env_set_refuses_to_open_the_node() {
    let volume = Volume::new();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rahi"));
    cmd.arg("preflight");
    for (k, v) in &volume.env {
        cmd.env(k, v);
    }
    cmd.env(rahi_ops::RESTORE_ENV_VAR, "file:/nowhere");
    let run = Run::of(cmd.output().unwrap());
    assert_eq!(run.code, 1, "{}", run.stdout);
    assert!(
        run.stdout
            .contains("FAIL restore_env: config: HQL_BACKUP_RESTORE is set"),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout
            .contains("SKIP hiqlite: skipped: HQL_BACKUP_RESTORE is set"),
        "{}",
        run.stdout
    );
    assert!(run.stdout.contains("SKIP engine:"));
    assert!(run.stdout.contains("SKIP ledger:"));
    assert!(
        !volume.path().join("hiqlite").exists(),
        "the node was never opened"
    );
}

#[test]
fn preflight_fails_on_a_key_directory_mode() {
    let volume = Volume::new();
    let keys = volume.path().join("keys");
    std::fs::set_permissions(&keys, std::fs::Permissions::from_mode(0o755)).unwrap();
    let run = volume.run(&["preflight"]);
    assert_eq!(run.code, 1, "{}", run.stdout);
    assert!(run.stdout.contains("FAIL keys:"), "{}", run.stdout);
    assert!(
        run.stdout.contains("has mode 0755, expected 0700"),
        "{}",
        run.stdout
    );
}

#[test]
fn backup_without_rauthy_is_exit_3_and_writes_nothing() {
    let volume = Volume::new();
    let run = volume.run(&["backup"]);
    assert_eq!(run.code, 3, "{}", run.stderr);
    assert!(run.stderr.contains("error: upstream:"), "{}", run.stderr);
    assert!(!volume.path().join("backups").exists());
}

/// A stub rauthy on its own runtime thread: the two backup routes and
/// health, accepting the volume's admin token.
struct StubRauthy {
    addr: String,
    _runtime: tokio::runtime::Runtime,
}

fn stub_rauthy() -> StubRauthy {
    use axum::routing::get;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let taken = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let list = taken.clone();
    let app = axum::Router::new()
        .route("/auth/v1/health", get(|| async { "ok" }))
        .route(
            "/auth/v1/backup",
            get(move || {
                let n = list.load(std::sync::atomic::Ordering::SeqCst);
                async move {
                    let local: Vec<serde_json::Value> = (1..=n)
                        .map(|i| serde_json::json!({"name": format!("rauthy_backup_{i}.sqlite"), "last_modified": i, "size": 6}))
                        .collect();
                    serde_json::json!({"local": local, "s3": []}).to_string()
                }
            })
            .post(move || {
                taken.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async { "" }
            }),
        )
        .route("/auth/v1/backup/local/{name}", get(|| async { "rauthy" }));
    runtime.spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    StubRauthy {
        addr,
        _runtime: runtime,
    }
}

#[test]
fn backup_and_restore_round_trip_through_the_binary() {
    let rauthy = stub_rauthy();
    let mut source = Volume::new();
    source.env.retain(|(k, _)| k != "RAHI_RAUTHY_ADDR");
    source
        .env
        .push(("RAHI_RAUTHY_ADDR".to_owned(), rauthy.addr.clone()));
    assert_eq!(source.run(&["migrate"]).code, 0);

    let run = source.run(&["backup"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(
        run.stdout.starts_with("backup: rahi-backup-"),
        "{}",
        run.stdout
    );
    let archive = std::fs::read_dir(source.path().join("backups"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|e| e == "age"))
        .expect("one archive landed");

    // A --to directory outside the volume works the same way. hiqlite names
    // its snapshot by the second, so two backups need a second between them.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let elsewhere = tempfile::tempdir().unwrap();
    let run = source.run(&["backup", "--to", elsewhere.path().to_str().unwrap()]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 1);

    let fresh = Volume::new();
    std::fs::remove_dir_all(fresh.path().join("keys")).unwrap();
    let key = source.path().join("keys").join(rahi_ops::BACKUP_KEY_FILE);
    let run = fresh.run(&[
        "restore",
        archive.to_str().unwrap(),
        "--key",
        key.to_str().unwrap(),
    ]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(
        run.stdout.starts_with("restore: applied rahi-backup-"),
        "{}",
        run.stdout
    );

    let run = fresh.run(&["ledger", "verify"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(run.stdout.contains("1 resident record(s)"));
    let run = fresh.run(&["migrate"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(
        run.stdout.contains("schema_version 1 -> 1, applied: none"),
        "the restored store carries its migrations: {}",
        run.stdout
    );

    let run = fresh.run(&[
        "restore",
        archive.to_str().unwrap(),
        "--key",
        key.to_str().unwrap(),
    ]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(run.stdout.contains("was already applied"), "{}", run.stdout);
}

#[test]
fn restore_of_a_missing_archive_is_exit_3() {
    let volume = Volume::new();
    let run = volume.run(&["restore", "/nowhere/rahi-backup-x.tar.age"]);
    assert_eq!(run.code, 3, "{}", run.stderr);
    assert!(run.stderr.contains("cannot be read"));
}

#[test]
fn first_boot_then_supervise_without_a_rauthy_binary_is_exit_3() {
    let dir = tempfile::tempdir().unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rahi"));
    cmd.arg("first-boot")
        .env("RAHI_PUBLIC_URL", "http://localhost:8080")
        .env("RAHI_DATA_DIR", dir.path());
    let run = Run::of(cmd.output().unwrap());
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(
        run.stdout.contains("first-boot: keys generated"),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout.contains("rauthy api token:  rahi$"),
        "{}",
        run.stdout
    );
    assert!(
        dir.path()
            .join("keys")
            .join(rahi_ops::ADMIN_TOKEN_FILE)
            .is_file()
    );

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rahi"));
    cmd.arg("first-boot")
        .env("RAHI_PUBLIC_URL", "http://localhost:8080")
        .env("RAHI_DATA_DIR", dir.path());
    let run = Run::of(cmd.output().unwrap());
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(
        run.stdout.contains("keys present and verified"),
        "{}",
        run.stdout
    );

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rahi"));
    cmd.arg("supervise")
        .env("RAHI_PUBLIC_URL", "http://localhost:8080")
        .env("RAHI_DATA_DIR", dir.path())
        .env("RAHI_RAUTHY_BIN", "/nonexistent/rauthy");
    let run = Run::of(cmd.output().unwrap());
    assert_eq!(run.code, 3, "{}\n{}", run.stdout, run.stderr);
    assert!(
        run.stderr.contains("rauthy cannot be spawned"),
        "{}",
        run.stderr
    );
}
