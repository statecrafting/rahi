//! The composer driven over argv (spec 030 FR-003, FR-004, FR-005).
//!
//! Every test spawns the `rahi` binary, the chassis with no app, against a
//! temp volume and asserts on its exit code and what it printed.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use base64::Engine as _;
use rahi_cli::VERBS;
use rahi_ops::KeySet;
use rahi_store::{EncKey, EncKeys, StoreSecrets};

/// A loopback port no concurrently running test process was handed (spec
/// 030 D-10).
///
/// Binding port 0 and dropping the listener raced: hiqlite binds the address
/// later, and a parallel test binary could take the port in between. A port
/// now comes from 20000..32000, below every OS ephemeral range, starting at an
/// offset derived from the process id; it is claimed by an exclusive lock on
/// `<temp>/rahi-test-ports/<port>.lock`, held until this process exits, and
/// probed with a bind before it is handed out. Every copy of this allocator
/// in the workspace uses the same range and lock directory.
fn free_port() -> String {
    format!("127.0.0.1:{}", loopback_port())
}

fn loopback_port() -> u16 {
    use std::fs::{File, OpenOptions};
    use std::net::{Ipv4Addr, TcpListener};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Mutex, PoisonError};

    const FLOOR: u32 = 20_000;
    const SPAN: u32 = 12_000;
    static NEXT: AtomicU32 = AtomicU32::new(0);
    static HELD: Mutex<Vec<File>> = Mutex::new(Vec::new());

    let dir = std::env::temp_dir().join("rahi-test-ports");
    std::fs::create_dir_all(&dir).expect("the port lock directory");
    let offset = std::process::id().wrapping_mul(7919) % SPAN;
    loop {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        assert!(
            n < SPAN,
            "every test port in {FLOOR}..{} is taken",
            FLOOR + SPAN
        );
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
            return port;
        }
    }
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
        // Spec 037 B-1: the key set carries the backup admin's passkey,
        // because rauthy's backup routes take an admin session and the API
        // key above is refused on them.
        let passkey_config = rahi_types::Config::from_env(&std::collections::BTreeMap::from([(
            "RAHI_PUBLIC_URL",
            "http://localhost:8080",
        )]))
        .unwrap();
        keys.write(
            rahi_ops::BACKUP_PASSKEY_FILE,
            rahi_ops::rauthy_session::Passkey::generate(&passkey_config)
                .unwrap()
                .to_json()
                .unwrap()
                .as_bytes(),
        )
        .unwrap();
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

/// The repository root, from this crate's own directory.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

/// The `implementation:` of the spec with this ordinal, read from the spec
/// document's own front matter.
///
/// The lifecycle half of AC-2's required set. Read from `specs/`, which is
/// the source the derived registry is compiled from, so this test never
/// parses a derived artefact by hand (the governed-artefact-reads rule) and
/// never asks the binary under test what it thinks the corpus says.
fn implementation_of(ordinal: &str) -> String {
    let specs = repo_root().join("specs");
    let dir = std::fs::read_dir(&specs)
        .unwrap_or_else(|e| panic!("{} is readable: {e}", specs.display()))
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(&format!("{ordinal}-")))
        })
        .unwrap_or_else(|| panic!("B-1 annotates ({ordinal}) and specs/{ordinal}-* exists"));
    let text = std::fs::read_to_string(dir.join("spec.md"))
        .unwrap_or_else(|e| panic!("{} is readable: {e}", dir.display()));
    text.lines()
        .find_map(|l| l.strip_prefix("implementation:"))
        .map(|v| v.trim().trim_matches('"').to_owned())
        .unwrap_or_else(|| panic!("specs/{ordinal}-* names an implementation state"))
}

/// The bare verb of one B-1 argv entry: the words before its first
/// placeholder, flag group or flag. `restore <archive>` is `restore`;
/// `ledger verify [--full]` is `ledger verify`; `upgrade-cache --backup
/// <archive>` is `upgrade-cache` (030 D-11).
fn verb_of(entry: &str) -> String {
    let end = entry
        .find(" <")
        .into_iter()
        .chain(entry.find(" ["))
        .chain(entry.find(" --"))
        .min()
        .unwrap_or(entry.len());
    entry[..end].trim().to_owned()
}

/// Spec 030 B-1's argv list, as the spec document writes it: each entry and
/// the ordinal annotation it carries, if any.
///
/// The parse is deliberately literal. Everything between `parses argv:` and
/// the sentence that follows it is a comma-separated list of backticked
/// entries, each optionally followed by `(NNN)`, and that is all this reads.
fn b1_argv_entries() -> Vec<(String, Option<String>)> {
    let path = repo_root()
        .join("specs")
        .join("030-operational-verbs")
        .join("spec.md");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} is readable: {e}", path.display()));
    // The document wraps B-1 over several lines; the list is one sentence.
    let flat = text.replace('\n', " ");
    let start = flat.find("parses argv:").expect("B-1 names the argv list") + "parses argv:".len();
    let rest = &flat[start..];
    let end = rest
        .find("Exit codes")
        .expect("B-1's argv list ends at the exit codes");
    let list = &rest[..end];

    let mut out = Vec::new();
    let bytes: Vec<char> = list.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != '`' {
            i += 1;
            continue;
        }
        let open = i + 1;
        let Some(close) = (open..bytes.len()).find(|&j| bytes[j] == '`') else {
            break;
        };
        let entry: String = bytes[open..close].iter().collect();
        // An annotation is `(NNN)` after optional whitespace.
        let mut k = close + 1;
        while k < bytes.len() && bytes[k].is_whitespace() {
            k += 1;
        }
        let mut ordinal = None;
        if k < bytes.len() && bytes[k] == '(' {
            let digits: String = bytes[k + 1..]
                .iter()
                .copied()
                .take_while(char::is_ascii_digit)
                .collect();
            if digits.len() == 3 && bytes.get(k + 1 + digits.len()) == Some(&')') {
                ordinal = Some(digits);
            }
        }
        out.push((
            entry.split_whitespace().collect::<Vec<_>>().join(" "),
            ordinal,
        ));
        i = close + 1;
    }
    out
}

/// AC-2's **required verb set**, derived from spec 030 B-1's argv list and
/// the corpus lifecycle, and from nothing else.
///
/// Every unannotated entry, plus every entry annotated with the ordinal of a
/// spec that is `complete`, plus every entry annotated with the ordinal of
/// the spec the change under test is implementing, which is the spec the
/// corpus has at `in-progress`. Neither `rahi_cli::VERBS` nor the parser is
/// consulted: they are what this set is the expectation for.
fn required_verb_set() -> BTreeSet<String> {
    let entries = b1_argv_entries();
    assert!(
        entries.len() >= 9,
        "B-1's argv list parsed to {} entries, which is not a list: {entries:?}",
        entries.len()
    );
    entries
        .into_iter()
        .filter(|(_, ordinal)| match ordinal {
            None => true,
            Some(o) => matches!(implementation_of(o).as_str(), "complete" | "in-progress"),
        })
        .map(|(entry, _)| verb_of(&entry))
        .collect()
}

/// The verbs `--help` lists, by name, in the order it lists them.
fn listed_verbs(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .skip_while(|l| !l.starts_with("verbs:"))
        .skip(1)
        .take_while(|l| l.starts_with("  "))
        .map(|l| {
            let line = l.trim();
            let column = line.find("  ").unwrap_or(line.len());
            verb_of(&line[..column])
        })
        .collect()
}

/// Spec 030 AC-2, in the order the criterion states it.
///
/// First `VERBS` is judged against the required set, which is derived from
/// B-1's text and the corpus lifecycle rather than taken as given: a verb the
/// required set carries and `VERBS` omits is an AC-2 failure, not a
/// redefinition of the criterion. Only then is the help compared against that
/// same independent set.
#[test]
fn help_lists_exactly_the_required_verb_set() {
    let required = required_verb_set();
    let declared: BTreeSet<String> = VERBS.iter().map(|v| (*v).to_owned()).collect();
    assert_eq!(
        declared, required,
        "rahi_cli::VERBS is the declaration of AC-2's required set and is itself under test"
    );
    assert_eq!(VERBS.len(), declared.len(), "VERBS names each verb once");

    let run = bare(&["--help"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    let listed = listed_verbs(&run.stdout);
    assert_eq!(
        listed.iter().cloned().collect::<BTreeSet<String>>(),
        required,
        "--help lists exactly the required verb set:\n{}",
        run.stdout
    );
    assert_eq!(
        listed.len(),
        required.len(),
        "no verb is listed twice:\n{}",
        run.stdout
    );
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
fn serve_with_a_static_directory_that_is_absent_is_exit_3_before_the_store_opens() {
    // Spec 039 B-5 and FR-003: the slot would answer 404 for every page, so
    // the cell refuses to start and says which directory is missing. The
    // store is never opened, which is why this is not the stale-schema exit.
    let volume = Volume::new();
    let missing = volume.path().join("no-such-web-dir");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rahi"));
    cmd.arg("serve");
    for (k, v) in &volume.env {
        cmd.env(k, v);
    }
    cmd.env(rahi_cli::serve::ENV_STATIC_DIR, &missing);
    let run = Run::of(cmd.output().unwrap());
    assert_eq!(run.code, 3, "{}\n{}", run.stdout, run.stderr);
    assert!(run.stderr.contains("error: config:"), "{}", run.stderr);
    assert!(
        run.stderr.contains(&missing.display().to_string()),
        "the missing directory is named: {}",
        run.stderr
    );
    assert!(run.stderr.contains("404"), "{}", run.stderr);
    assert!(
        !volume
            .path()
            .join("app-store")
            .join("state_machine")
            .exists(),
        "no node was opened"
    );
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
        !volume
            .path()
            .join("app-store")
            .join("state_machine")
            .exists(),
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
        !volume
            .path()
            .join("app-store")
            .join("state_machine")
            .exists(),
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

/// A stub rauthy on its own runtime thread: health, the login ceremony the
/// backup admin drives (spec 037 B-1), and the three backup routes.
///
/// It answers the ceremony without verifying the assertion: what is under
/// test here is the verb, and the credential itself is verified against the
/// pinned release by `rahi-ops`'s live test (037 FR-006). Its snapshots are
/// named the way hiqlite names them, `backup_node_<id>_<ts>.sqlite`, because
/// that name is what tells the verb which snapshot is its own.
struct StubRauthy {
    addr: String,
    _runtime: tokio::runtime::Runtime,
}

fn stub_rauthy() -> StubRauthy {
    use axum::routing::{get, post};
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    // Every accepted trigger records the second it arrived in, which is
    // what the file name carries.
    let taken: std::sync::Arc<std::sync::Mutex<Vec<i64>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let list = taken.clone();
    let code = "a".repeat(48);
    let login_code = code.clone();
    let app = axum::Router::new()
        .route("/auth/v1/health", get(|| async { "ok" }))
        .route(
            "/auth/v1/oidc/session",
            post(|| async {
                (
                    [(axum::http::header::SET_COOKIE, "RauthySession=stub; Path=/")],
                    serde_json::json!({"csrf_token": "stub-csrf"}).to_string(),
                )
            }),
        )
        // Difficulty zero: any counter solves it, so the verb's own solver
        // returns at once and the stub stays a stub.
        .route("/auth/v1/pow", post(|| async { "1:0:0:salt:hash:" }))
        .route(
            "/auth/v1/oidc/authorize",
            post(move || {
                let code = code.clone();
                async move { serde_json::json!({"code": code, "exp": 60}).to_string() }
            }),
        )
        .route(
            "/auth/v1/users/webauthn_start",
            post(move || {
                let code = login_code.clone();
                async move {
                    serde_json::json!({
                        "code": code,
                        "rcr": {"publicKey": {"challenge": "c3R1Yg"}},
                    })
                    .to_string()
                }
            }),
        )
        .route("/auth/v1/users/webauthn_finish", post(|| async { "{}" }))
        .route(
            "/auth/v1/backup",
            get(move || {
                let taken = list.lock().unwrap().clone();
                async move {
                    let local: Vec<serde_json::Value> = taken
                        .iter()
                        .map(|ts| {
                            serde_json::json!({
                                "name": format!("backup_node_1_{ts}.sqlite"),
                                "last_modified": ts,
                                "size": 6,
                            })
                        })
                        .collect();
                    serde_json::json!({"local": local, "s3": []}).to_string()
                }
            })
            .post(move || {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs() as i64;
                taken.lock().unwrap().push(now);
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

    // A --to directory outside the volume works the same way. This second
    // backup falls inside hiqlite's sixty second suppression window, on
    // both stores, so the verb waits it out and produces a snapshot of its
    // own rather than sealing the one already on disk (spec 037 B-1, B-2).
    // That wait is why this test takes a minute; shortening it would mean
    // accepting a stale archive as a fresh one.
    let second = std::time::Instant::now();
    let elsewhere = tempfile::tempdir().unwrap();
    let run = source.run(&["backup", "--to", elsewhere.path().to_str().unwrap()]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 1);
    assert!(
        second.elapsed() >= std::time::Duration::from_secs(30),
        "the second backup waited the window out"
    );
    let snapshots: Vec<String> = std::fs::read_dir(
        source
            .path()
            .join("app-store")
            .join("state_machine")
            .join("backups"),
    )
    .map(|entries| {
        entries
            .filter_map(|e| Some(e.ok()?.file_name().to_string_lossy().into_owned()))
            .collect()
    })
    .unwrap_or_default();
    assert_eq!(
        snapshots.len(),
        2,
        "two backups, two app snapshots: {snapshots:?}"
    );

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
fn backup_inside_a_running_replica_attaches_to_its_node_instead_of_opening_a_second() {
    let rauthy = stub_rauthy();
    let mut volume = Volume::new();
    volume.env.retain(|(k, _)| k != "RAHI_RAUTHY_ADDR");
    volume
        .env
        .push(("RAHI_RAUTHY_ADDR".to_owned(), rauthy.addr.clone()));
    assert_eq!(volume.run(&["migrate"]).code, 0);

    // The node runs here, the way `serve` holds it inside a pod; the verb
    // runs in another process against the same volume (spec 032 B-5).
    let env: BTreeMap<String, String> = volume.env.iter().cloned().collect();
    let config = rahi_types::Config::from_env(&env).unwrap();
    // Spec 043 B-4a: the process that owns the node holds `cell.lock`, as
    // `serve` does; that lock, not hiqlite's own, is what a verb attaches to.
    let _owner = rahi_ops::cell_lock::gate(&config, rahi_ops::cell_lock::Entry::Serve).unwrap();
    let secrets = KeySet::of(&config).store_secrets().unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let store = runtime
        .block_on(rahi_store::Store::open(
            &rahi_ops::store_config(&config, &env, secrets).unwrap(),
        ))
        .unwrap();
    assert!(
        rahi_ops::app_lock_file(&config).exists(),
        "the node holds its lock"
    );

    let elsewhere = tempfile::tempdir().unwrap();
    let run = volume.run(&["backup", "--to", elsewhere.path().to_str().unwrap()]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(
        run.stdout.starts_with("backup: rahi-backup-"),
        "{}",
        run.stdout
    );
    assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 1);
    assert!(
        rahi_ops::app_lock_file(&config).exists(),
        "the running node's lock survives the verb"
    );

    // The migration Job (spec 032 B-6): no volume of its own, a pure client
    // of the peers, the same verb.
    let job = tempfile::tempdir().unwrap();
    let job_keys = job.path().join("keys");
    std::fs::create_dir_all(&job_keys).unwrap();
    for entry in std::fs::read_dir(volume.path().join("keys")).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), job_keys.join(entry.file_name())).unwrap();
    }
    std::fs::set_permissions(&job_keys, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut client = Volume::new();
    client.env.retain(|(k, _)| {
        k != "RAHI_DATA_DIR" && k != "RAHI_HIQLITE_API_ADDR" && k != "RAHI_HIQLITE_RAFT_ADDR"
    });
    client
        .env
        .push(("RAHI_DATA_DIR".to_owned(), job.path().display().to_string()));
    client
        .env
        .push(("RAHI_STORE_CLIENT".to_owned(), "true".to_owned()));
    client.env.push((
        "RAHI_HIQ_NODES".to_owned(),
        format!("1 {} {}", config.hiqlite.raft_addr, config.hiqlite.api_addr),
    ));
    let run = client.run(&["migrate"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(
        run.stdout.contains("applied: none"),
        "the store is current: {}",
        run.stdout
    );
    assert!(
        !job.path().join("app-store").join("state_machine").exists(),
        "a client opens no node of its own"
    );
    runtime.block_on(store.shutdown()).unwrap();
}

#[test]
fn first_boot_export_renders_a_secret_with_every_key_and_touches_no_volume() {
    let dir = tempfile::tempdir().unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rahi"));
    cmd.args(["first-boot", "--export"])
        .env("RAHI_PUBLIC_URL", "https://cell.example.com")
        .env("RAHI_DATA_DIR", dir.path());
    let run = Run::of(cmd.output().unwrap());
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(run.stdout.contains("kind: Secret"), "{}", run.stdout);
    assert!(run.stdout.contains("name: rahi-keys"));
    for name in [
        "ledger.key",
        "session.key",
        "hiqlite.json",
        "backup.key",
        "rauthy.json",
        "rauthy_admin_token",
    ] {
        assert!(
            run.stdout.contains(&format!("  {name}: ")),
            "{name} in the Secret"
        );
    }
    assert!(
        std::fs::read_dir(dir.path()).unwrap().next().is_none(),
        "the volume was not touched"
    );
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

// ---------------------------------------------------------------------------
// Spec 042: the boot gate, the repair verb, and the verb boundary
// ---------------------------------------------------------------------------

/// The committed fixture of spec 042 FR-014, written by the published 0.1.0
/// crates. Its absence is a broken checkout, never a skip.
fn v010_dir() -> PathBuf {
    let dir = repo_root()
        .join("crates")
        .join("rahi-ledger")
        .join("testdata")
        .join("chains")
        .join("v0.1.0-sealed");
    assert!(
        dir.is_dir(),
        "the committed fixture {} is missing: this is a broken checkout, not a missing optional \
         tool. Rebuild it with its write.sh, which pulls rahi-ledger = \"=0.1.0\" from crates.io.",
        dir.display()
    );
    dir
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().filter_map(Result::ok) {
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// Seed the 0.1.0 fixture into a volume's own store: its resident rows, its
/// pre-036 segment headers, its signing key, and a copy of its archive.
///
/// Nothing here creates an identity row, because 0.1.0 had no such table.
/// That is the state spec 042 B-10 refuses to serve.
fn seed_v010(volume: &Volume) -> PathBuf {
    let dir = v010_dir();
    // The chain is verified against the key that signed it, so the volume
    // has to carry the fixture's key rather than its own.
    KeySet::at(volume.path().join("keys"))
        .write(
            rahi_ops::LEDGER_KEY_FILE,
            std::fs::read_to_string(dir.join("signing-key.b64"))
                .unwrap()
                .trim()
                .as_bytes(),
        )
        .unwrap();

    // The default `RAHI_LEDGER_ARCHIVE_DIR`, so `ledger verify --full`
    // reaches the same bodies `ledger reindex <archive>` is pointed at.
    let archive = volume
        .path()
        .join(rahi_cli::serve::DEFAULT_LEDGER_ARCHIVE_DIR);
    copy_tree(&dir.join("archive"), &archive);

    let env: BTreeMap<String, String> = volume.env.iter().cloned().collect();
    let config = rahi_types::Config::from_env(&env).unwrap();
    let keys = KeySet::at(volume.path().join("keys"));
    let cfg = rahi_ops::store_config(&config, &env, keys.store_secrets().unwrap()).unwrap();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async move {
        let store = rahi_store::Store::open(&cfg).await.unwrap();
        let handle = store.handle();
        handle
            .execute(rahi_ledger::DECISIONS_TABLE_SQL, vec![])
            .await
            .unwrap();
        for line in std::fs::read_to_string(dir.join("resident.jsonl"))
            .unwrap()
            .lines()
            .filter(|l| !l.trim().is_empty())
        {
            let record = rahi_ledger::SignedRecord::from_bytes(line.as_bytes()).unwrap();
            handle
                .execute(
                    "INSERT INTO kernel_decisions (id, prev_hash, hash, record) \
                     VALUES ($1, $2, $3, $4)",
                    vec![
                        rahi_store::Value::from(record.record.id.as_str()),
                        rahi_store::Value::from(record.record.previous_record_hash.as_str()),
                        rahi_store::Value::from(record.record.record_hash.as_str()),
                        rahi_store::Value::Blob(record.to_canonical_bytes().unwrap()),
                    ],
                )
                .await
                .unwrap();
        }
        // The pre-036 shape: no `current_manifest` column, which `open` adds.
        handle
            .execute(
                "CREATE TABLE IF NOT EXISTS kernel_segments (\
                 segment_hash TEXT PRIMARY KEY, prev_segment_hash TEXT NOT NULL, \
                 last_hash TEXT NOT NULL, first_id TEXT NOT NULL, last_id TEXT NOT NULL, \
                 count INTEGER NOT NULL)",
                vec![],
            )
            .await
            .unwrap();
        for line in std::fs::read_to_string(dir.join("segments.jsonl"))
            .unwrap()
            .lines()
            .filter(|l| !l.trim().is_empty())
        {
            let header: rahi_ledger::SegmentHeader = serde_json::from_str(line).unwrap();
            handle
                .execute(
                    "INSERT INTO kernel_segments \
                     (segment_hash, prev_segment_hash, last_hash, first_id, last_id, count) \
                     VALUES ($1, $2, $3, $4, $5, $6)",
                    vec![
                        rahi_store::Value::from(header.segment_hash.as_str()),
                        rahi_store::Value::from(header.prev_segment_hash.as_str()),
                        rahi_store::Value::from(header.last_hash.as_str()),
                        rahi_store::Value::from(header.first_id.as_str()),
                        rahi_store::Value::from(header.last_id.as_str()),
                        rahi_store::Value::from(header.count),
                    ],
                )
                .await
                .unwrap();
        }
        store.shutdown().await.unwrap();
    });
    archive
}

/// Spec 042 AC-2, AC-4, FR-009. The whole operator-facing migration, driven
/// over argv against a chain the published 0.1.0 crates wrote.
///
/// `serve` refuses and names the command; `ledger verify` and `ledger export`
/// work on the same chain and print the uncovered count; `preflight` fails
/// with coverage as the named check; `ledger reindex` exits 0 and reports
/// complete coverage; and then all of them pass.
#[test]
fn an_unreindexed_chain_refuses_service_until_ledger_reindex_has_run() {
    let volume = Volume::new();
    // The cell's own migrations first, so the refusal under test is spec
    // 042's coverage gate and not spec 030 B-2's schema check.
    assert_eq!(volume.run(&["migrate"]).code, 0);
    let archive = seed_v010(&volume);
    let archive = archive.to_str().unwrap().to_owned();

    // `serve` refuses, exit 2, naming the one command that clears it.
    let run = volume.run(&["serve"]);
    assert_eq!(
        run.code, 2,
        "stdout:\n{}\nstderr:\n{}",
        run.stdout, run.stderr
    );
    assert!(
        run.stderr.contains("rahi ledger reindex"),
        "the refusal names the command: {}",
        run.stderr
    );

    // Diagnosis, export and repair stay available, and each says the chain
    // is not accounted for so no reader mistakes the output for a proof.
    let run = volume.run(&["ledger", "verify"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(
        run.stdout.contains("ok at resident depth"),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout.contains("3 uncovered segment(s)"),
        "ledger verify prints the uncovered count: {}",
        run.stdout
    );
    let full = volume.run(&["ledger", "verify", "--full"]);
    assert_eq!(full.code, 0, "{}", full.stderr);

    let out = volume.path().join("chain.jsonl");
    let run = volume.run(&["ledger", "export", out.to_str().unwrap()]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(
        run.stdout.contains("are not accounted for"),
        "ledger export names the uncovered count: {}",
        run.stdout
    );

    let run = volume.run(&["preflight"]);
    assert_eq!(run.code, 1, "{}", run.stdout);
    assert!(
        run.stdout.contains("FAIL coverage:"),
        "coverage is the named failing check: {}",
        run.stdout
    );
    assert!(
        run.stdout.contains("rahi ledger reindex"),
        "and it names the command: {}",
        run.stdout
    );
    assert!(
        run.stdout.contains("PASS ledger:"),
        "the chain itself verified: a missing upgrade step is not damage: {}",
        run.stdout
    );

    // The repair.
    let run = volume.run(&["ledger", "reindex", &archive]);
    assert_eq!(
        run.code, 0,
        "stdout:\n{}\nstderr:\n{}",
        run.stdout, run.stderr
    );
    assert!(run.stdout.contains("3 segment(s) walked"), "{}", run.stdout);
    assert!(
        run.stdout
            .contains("0 sealed segment(s) and 0 resident record(s) remain unaccounted for"),
        "{}",
        run.stdout
    );
    for forbidden in [
        "repaired",
        "resolved",
        "corrected",
        "deduplicated",
        "reconciled",
    ] {
        assert!(
            !run.stdout.contains(forbidden),
            "{forbidden:?} in {}",
            run.stdout
        );
    }

    // And the four numbers, at both depths, none of them from the archive.
    for depth in [vec!["ledger", "verify"], vec!["ledger", "verify", "--full"]] {
        let run = volume.run(&depth);
        assert_eq!(run.code, 0, "{}", run.stderr);
        assert!(
            run.stdout.contains(
                "identity coverage: 0 uncovered segment(s), 0 unstamped resident record(s), \
                 9 identity row(s), 0 collision(s)"
            ),
            "{:?}: {}",
            depth,
            run.stdout
        );
    }

    let run = volume.run(&["preflight"]);
    assert!(run.stdout.contains("PASS coverage:"), "{}", run.stdout);

    // A second reindex is a no-op that still exits 0.
    let run = volume.run(&["ledger", "reindex", &archive]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(run.stdout.contains("0 segment(s) walked"), "{}", run.stdout);
}

/// Spec 042 AC-4, D-3. The refusal has no override.
///
/// No argument, flag, or environment variable starts `serve` on a chain with
/// incomplete coverage; `serve` offers none in the usage; and
/// `open_for_repair` is unreachable from `serve`'s code path.
#[test]
fn nothing_starts_serve_on_a_chain_with_incomplete_coverage() {
    let volume = Volume::new();
    assert_eq!(volume.run(&["migrate"]).code, 0);
    seed_v010(&volume);

    for extra in [
        vec!["serve", "--allow-uncovered"],
        vec!["serve", "--force"],
        vec!["serve", "--repair"],
        vec!["serve", "--skip-coverage"],
    ] {
        let run = volume.run(&extra);
        assert_ne!(
            run.code, 0,
            "{extra:?} must not start serve: {}",
            run.stdout
        );
        assert!(
            run.stderr.contains("serve does not take"),
            "{extra:?}: {}",
            run.stderr
        );
    }

    for (key, value) in [
        ("RAHI_ALLOW_UNCOVERED", "1"),
        ("RAHI_LEDGER_ALLOW_UNCOVERED", "true"),
        ("RAHI_SKIP_COVERAGE", "1"),
    ] {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_rahi"));
        cmd.arg("serve");
        for (k, v) in &volume.env {
            cmd.env(k, v);
        }
        cmd.env_remove(rahi_ops::RESTORE_ENV_VAR);
        cmd.env(key, value);
        let run = Run::of(cmd.output().unwrap());
        assert_eq!(run.code, 2, "{key} must not start serve: {}", run.stdout);
        assert!(run.stderr.contains("rahi ledger reindex"), "{}", run.stderr);
    }

    let usage = bare(&["--help"]).stdout;
    for forbidden in ["allow-uncovered", "skip-coverage", "--force"] {
        assert!(
            !usage.contains(forbidden),
            "the usage offers {forbidden:?}:\n{usage}"
        );
    }

    // The repair open is unreachable from `serve`'s code path: it is
    // constructed only where the three diagnostic and repair verbs are
    // dispatched, which is not this file.
    let serve = std::fs::read_to_string(
        repo_root()
            .join("crates")
            .join("rahi-cli")
            .join("src")
            .join("serve.rs"),
    )
    .unwrap();
    assert!(
        !serve.contains("open_for_repair"),
        "serve.rs must not be able to reach the repair open"
    );
}

/// Spec 042 D-1, spec 030 B-1 and AC-2. `ledger reindex` is a verb of its
/// own and is announced as mutating, so an operator reaching for the
/// read-only diagnostic cannot get the repair by mistyping a flag.
#[test]
fn ledger_reindex_is_a_verb_of_its_own_and_says_it_mutates() {
    let usage = bare(&["--help"]).stdout;
    let line = usage
        .lines()
        .find(|l| l.trim_start().starts_with("ledger reindex"))
        .unwrap_or_else(|| panic!("--help lists ledger reindex:\n{usage}"));
    assert!(line.contains("<archive>"), "it takes the archive: {line}");
    assert!(
        line.contains("MUTATES"),
        "and says so, beside the read-only `ledger verify`: {line}"
    );
    assert!(
        !usage.contains("verify [--full] [--reindex]") && !usage.contains("--reindex"),
        "the repair is not a flag on the diagnostic (D-1):\n{usage}"
    );

    // Its argument is required.
    let run = bare(&["ledger", "reindex"]);
    assert_eq!(run.code, 1);
    assert!(run.stderr.contains("ledger reindex needs an archive"));
}

/// The argv that runs `verb` of [`VERBS`] against `volume`. A verb this does
/// not know fails the test, so a new verb cannot skip spec 043 FR-008.
fn argv_of(verb: &str, volume: &Path) -> Vec<String> {
    let archive = volume
        .join("backups")
        .join("none.age")
        .display()
        .to_string();
    let words: Vec<String> = match verb {
        "serve" | "preflight" | "migrate" | "backup" | "supervise" | "first-boot" => {
            vec![verb.to_owned()]
        }
        "restore" => vec!["restore".to_owned(), archive],
        "ledger verify" => vec!["ledger".to_owned(), "verify".to_owned()],
        "ledger export" => vec![
            "ledger".to_owned(),
            "export".to_owned(),
            volume.join("export.jsonl").display().to_string(),
        ],
        "ledger reindex" => vec![
            "ledger".to_owned(),
            "reindex".to_owned(),
            volume.join("ledger-archive").display().to_string(),
        ],
        "upgrade-cache" => vec!["upgrade-cache".to_owned(), "--backup".to_owned(), archive],
        other => panic!("spec 043 FR-008: {other:?} has no argv here; add it"),
    };
    words
}

/// Spec 043 FR-008: every verb of [`VERBS`] passes B-4a's gate before it
/// reads the transition record, opens the store or spawns Rauthy. With both
/// locks held elsewhere and a record no build can read, each one refuses at
/// a lock (a verb that read the record first would name the record instead),
/// creates no fence, opens no node and spawns no Rauthy.
#[test]
fn every_verb_locks_before_it_reads_opens_or_spawns() {
    let volume = Volume::new();
    let data = volume.path();
    let record = data.join(rahi_ops::upgrade::STATE_FILE);
    std::fs::write(&record, b"{ not a transition record").unwrap();
    let spawned = data.join("rauthy-spawned");
    let fake_rauthy = data.join("fake-rauthy.sh");
    std::fs::write(
        &fake_rauthy,
        format!("#!/bin/sh\ntouch {}\nsleep 30\n", spawned.display()),
    )
    .unwrap();
    std::fs::set_permissions(&fake_rauthy, std::fs::Permissions::from_mode(0o755)).unwrap();
    let cell = rahi_ops::cell_lock::try_lock(&data.join(rahi_ops::cell_lock::CELL_LOCK_FILE), true)
        .unwrap()
        .unwrap();
    let layout =
        rahi_ops::cell_lock::try_lock(&data.join(rahi_ops::cell_lock::TRANSITION_LOCK_FILE), true)
            .unwrap()
            .unwrap();
    let before = std::fs::read_dir(data)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect::<BTreeSet<_>>();
    for verb in VERBS {
        let argv = argv_of(verb, data);
        let args: Vec<&str> = argv.iter().map(String::as_str).collect();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_rahi"));
        cmd.args(&args);
        for (k, v) in &volume.env {
            cmd.env(k, v);
        }
        cmd.env_remove(rahi_ops::RESTORE_ENV_VAR);
        cmd.env("RAHI_RAUTHY_BIN", &fake_rauthy);
        let run = Run::of(cmd.output().unwrap());
        assert_ne!(run.code, 0, "{verb}: refused\n{}{}", run.stdout, run.stderr);
        let said = format!("{}{}", run.stdout, run.stderr);
        assert!(
            said.contains(rahi_ops::cell_lock::CELL_LOCK_FILE)
                || said.contains(rahi_ops::cell_lock::TRANSITION_LOCK_FILE),
            "{verb}: refused at a lock, before reading the record:\n{}{}",
            run.stdout,
            run.stderr
        );
        assert!(!spawned.exists(), "{verb}: no Rauthy was spawned");
        let after = std::fs::read_dir(data)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect::<BTreeSet<_>>();
        assert_eq!(after, before, "{verb}: nothing was created on the volume");
        assert_eq!(
            std::fs::read(&record).unwrap(),
            b"{ not a transition record",
            "{verb}: the record is untouched"
        );
    }
    drop((cell, layout));
}

/// Spec 043 AC-3's shape through the binary: a volume in the pre-043 layout
/// is refused by `serve` before anything opens; `upgrade-cache` with a
/// verifying archive reaches `floored`; `serve` then answers ready and
/// records `done`; a second verb run changes nothing.
#[test]
fn a_pre043_volume_is_refused_then_transitioned_then_served_to_done() {
    let volume = Volume::new();
    let data = volume.path();
    assert_eq!(volume.run(&["migrate"]).code, 0);

    // The layout every pre-043 binary wrote: the store at <data>/hiqlite and
    // the rendered environment at <data>/rauthy/rauthy.env.
    std::fs::remove_dir_all(data.join("hiqlite")).unwrap();
    std::fs::remove_dir_all(data.join("rauthy").join("rauthy.env")).unwrap();
    std::fs::rename(data.join("app-store"), data.join("hiqlite")).unwrap();
    std::fs::write(data.join("rauthy").join("rauthy.env"), b"OLD=1\n").unwrap();
    let db = data.join("hiqlite").join("state_machine").join("db");
    let leftovers: Vec<String> = std::fs::read_dir(&db)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with("-wal") || n.ends_with("-shm"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "a stopped process closed SQLite: {leftovers:?}"
    );

    let refused = volume.run(&["serve"]);
    assert_ne!(refused.code, 0);
    assert!(
        refused.stderr.contains("rahi upgrade-cache"),
        "{}",
        refused.stderr
    );
    assert!(!data.join("app-store").exists(), "nothing was opened");

    let keys = KeySet::at(data.join("keys"));
    let parts = vec![
        rahi_ops::archive::Part::new(rahi_ops::archive::APP_DIR, "a.sqlite", b"app".to_vec()),
        rahi_ops::archive::Part::new(rahi_ops::archive::RAUTHY_DIR, "r.sqlite", b"r".to_vec()),
        rahi_ops::archive::Part::new(rahi_ops::archive::KEYS_DIR, "ledger.key", b"k".to_vec()),
    ];
    let manifest =
        rahi_ops::archive::ArchiveManifest::over(&parts, 1, "sha256:test".to_owned(), None);
    let sealed =
        rahi_ops::archive::seal(&parts, &manifest, &keys.backup_recipient().unwrap()).unwrap();
    let archive = data.join("pre-upgrade.tar.age");
    std::fs::write(&archive, sealed).unwrap();

    let run = volume.run(&["upgrade-cache", "--backup", archive.to_str().unwrap()]);
    assert_eq!(run.code, 0, "{}{}", run.stdout, run.stderr);
    assert!(
        run.stdout.contains("Every old process is stopped"),
        "{}",
        run.stdout
    );
    assert!(run.stdout.contains("is floored at"), "{}", run.stdout);

    let record = || {
        let text = std::fs::read_to_string(data.join(rahi_ops::upgrade::STATE_FILE)).unwrap();
        serde_json::from_str::<serde_json::Value>(&text).unwrap()["phase"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    assert_eq!(record(), "floored");

    let mut serve = Command::new(env!("CARGO_BIN_EXE_rahi"));
    serve.arg("serve");
    for (k, v) in &volume.env {
        serve.env(k, v);
    }
    serve.env_remove(rahi_ops::RESTORE_ENV_VAR);
    let mut child = serve
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
    while record() != "done" && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(record(), "done", "serve's first ready answer completes it");

    let again = volume.run(&["upgrade-cache", "--backup", archive.to_str().unwrap()]);
    assert_eq!(again.code, 0, "{}", again.stderr);
    assert!(
        again.stdout.contains("`done`; nothing to do"),
        "{}",
        again.stdout
    );
}

/// Spec 043 D-21 (c): `RAHI_TEST_UPGRADE_CRASH_AT` stops the real binary at
/// a persistent state as a crash would (exit 137, nothing after it), and a
/// rerun of the verb resumes from there to `floored`.
#[test]
fn the_verb_stopped_at_a_persistent_state_resumes_to_floored() {
    let volume = Volume::new();
    let data = volume.path();
    assert_eq!(volume.run(&["migrate"]).code, 0);
    std::fs::remove_dir_all(data.join("hiqlite")).unwrap();
    std::fs::remove_dir_all(data.join("rauthy").join("rauthy.env")).unwrap();
    std::fs::rename(data.join("app-store"), data.join("hiqlite")).unwrap();
    std::fs::write(data.join("rauthy").join("rauthy.env"), b"OLD=1\n").unwrap();

    let keys = KeySet::at(data.join("keys"));
    let parts = vec![
        rahi_ops::archive::Part::new(rahi_ops::archive::APP_DIR, "a.sqlite", b"app".to_vec()),
        rahi_ops::archive::Part::new(rahi_ops::archive::RAUTHY_DIR, "r.sqlite", b"r".to_vec()),
        rahi_ops::archive::Part::new(rahi_ops::archive::KEYS_DIR, "ledger.key", b"k".to_vec()),
    ];
    let manifest =
        rahi_ops::archive::ArchiveManifest::over(&parts, 1, "sha256:test".to_owned(), None);
    let sealed =
        rahi_ops::archive::seal(&parts, &manifest, &keys.backup_recipient().unwrap()).unwrap();
    let archive = data.join("pre-upgrade.tar.age");
    std::fs::write(&archive, sealed).unwrap();
    let phase = || {
        let text = std::fs::read_to_string(data.join(rahi_ops::upgrade::STATE_FILE)).unwrap();
        serde_json::from_str::<serde_json::Value>(&text).unwrap()["phase"]
            .as_str()
            .unwrap()
            .to_owned()
    };

    let mut crashed = Command::new(env!("CARGO_BIN_EXE_rahi"));
    crashed.args(["upgrade-cache", "--backup", archive.to_str().unwrap()]);
    for (k, v) in &volume.env {
        crashed.env(k, v);
    }
    crashed.env(rahi_cli::ENV_UPGRADE_CRASH_AT, "guarded");
    let out = crashed.output().unwrap();
    assert_eq!(out.status.code(), Some(137), "{out:?}");
    assert_eq!(phase(), "guarded");

    let run = volume.run(&["upgrade-cache", "--backup", archive.to_str().unwrap()]);
    assert_eq!(run.code, 0, "{}{}", run.stdout, run.stderr);
    assert_eq!(phase(), "floored");
}
