//! A cell's own preflight checks, driven over argv (spec 049 AC-1).
//!
//! As in the stop suite, the test binary is the fixture: a child started on
//! [`FIXTURE`] with [`CASE`] set composes [`CheckCell`] through
//! `rahi_cli::run_with` and exits with the verb's code. The case names the
//! declaration the cell returns; each check appends its name to the file
//! [`CALLS`] names, so a test can tell which checks ran.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::process::Command;
use std::time::{Duration, Instant};

use axum::Router;
use base64::Engine as _;
use rahi_cli::{AppCheck, AppVerdict, Cell, PreflightContext};
use rahi_edge::AppState;
use rahi_kernel::{CapabilityKind, ServiceName};
use rahi_ops::KeySet;
use rahi_store::{EncKey, EncKeys, Migration, StoreSecrets};
use rahi_types::EnvReader as _;

/// The libtest name of the fixture's process entry.
const FIXTURE: &str = "fixture_cell";
/// Set on a child to the verb the fixture runs.
const VERB: &str = "RAHI_TEST_FIXTURE_VERB";
/// Set on a child to the declaration it returns.
const CASE: &str = "RAHI_TEST_PREFLIGHT_CASE";
/// Set on a child to the file each check records its call in.
const CALLS: &str = "RAHI_TEST_PREFLIGHT_CALLS";
/// Read by the `model` check: the cell's own configuration.
const MODEL: &str = "CHECK_CELL_MODEL";

const MANIFEST: &str = r#"# The check cell: its `embed` service may reach one provider.

schema_version = "1.0.0"

[app]
name = "check-cell"
org = "rahi-tests"

[resources]
tables = ["models"]
egress = ["api.granted.example"]

[[capabilities]]
id = "models-read"
kind = "db.read"
resource = "models"

[[capabilities]]
id = "provider"
kind = "http.egress"
resource = "api.granted.example"

[services.embed]
capabilities = ["models-read", "provider"]

[ledger]
schema_version = "1.0.0"
max_record_bytes = 65536

[observability]
metrics_path = "/metrics"
otel = false

[auth]
operator_role = "check-cell-operator"

[contract]
version = "1.0.0"
"#;

static MIGRATIONS: std::sync::LazyLock<Vec<Migration>> = std::sync::LazyLock::new(|| {
    vec![
        Migration::new(
            1,
            "models",
            "CREATE TABLE models (revision TEXT PRIMARY KEY, active INTEGER NOT NULL)",
        ),
        Migration::new(
            2,
            "seed_model",
            "INSERT INTO models (revision, active) VALUES ('m-7', 1)",
        ),
    ]
});

fn record(name: &str) {
    if let Ok(path) = std::env::var(CALLS) {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        writeln!(file, "{name}").unwrap();
    }
}

fn model() -> AppCheck {
    AppCheck::new("model", |ctx: PreflightContext| async move {
        record("model");
        let rows: Vec<(String, i64)> = match ctx
            .store()
            .query(
                "SELECT revision, active FROM models ORDER BY revision",
                vec![],
            )
            .await
        {
            Ok(rows) => rows,
            Err(err) => return AppVerdict::fail(err.to_string()),
        };
        let configured = ctx.env().get(MODEL).unwrap_or_default();
        AppVerdict::pass(format!("{} model(s)", rows.len())).with_report([
            format!("configured {configured}"),
            format!("active {}", rows.first().map_or("none", |r| r.0.as_str())),
        ])
    })
}

fn backlog() -> AppCheck {
    AppCheck::new("backlog", |_| async {
        record("backlog");
        AppVerdict::warn("3 rows wait for embedding")
    })
}

fn provider() -> AppCheck {
    AppCheck::new("provider", |ctx: PreflightContext| async move {
        record("provider");
        let service = ServiceName::parse("embed").unwrap();
        let host = "api.ungranted.example";
        if ctx
            .manifest()
            .covers(&service, CapabilityKind::HttpEgress, host)
        {
            AppVerdict::pass(host)
        } else {
            AppVerdict::capability_missing(&service, CapabilityKind::HttpEgress, host)
        }
    })
}

fn sleeper() -> AppCheck {
    AppCheck::new("sleeper", |_| async {
        record("sleeper");
        tokio::time::sleep(Duration::from_secs(60)).await;
        AppVerdict::pass("woke")
    })
}

fn panics() -> AppCheck {
    AppCheck::new("panics", |_| async {
        record("panics");
        panic!("the check fell over");
    })
}

fn after() -> AppCheck {
    AppCheck::new("after", |_| async {
        record("after");
        AppVerdict::pass("still ran")
    })
}

fn noisy() -> AppCheck {
    AppCheck::new("noisy", |_| async {
        record("noisy");
        AppVerdict::pass("two\nlines")
            .with_report((0..40).map(|i| format!("{i}:{}", "x".repeat(300))))
    })
}

struct CheckCell;

impl Cell for CheckCell {
    fn manifest() -> &'static str {
        MANIFEST
    }

    fn migrations() -> &'static [Migration] {
        &MIGRATIONS
    }

    fn routes(_state: AppState) -> Router {
        Router::new()
    }

    fn preflight_checks() -> Vec<AppCheck> {
        match std::env::var(CASE).as_deref() {
            Ok("three") => vec![model(), backlog(), provider()],
            Ok("two") => vec![model(), backlog()],
            Ok("bounds") => vec![sleeper(), panics(), after()],
            Ok("duplicate") => vec![model(), backlog(), model()],
            Ok("invalid") => vec![
                model(),
                AppCheck::new("Bad-Name", |_| async {
                    record("bad");
                    AppVerdict::pass("ran")
                }),
            ],
            Ok("many") => (0..33)
                .map(|i| {
                    AppCheck::new(format!("c{i}"), move |_| async move {
                        record(&format!("c{i}"));
                        AppVerdict::pass("ran")
                    })
                })
                .collect(),
            Ok("noisy") => vec![noisy()],
            _ => Vec::new(),
        }
    }
}

/// A cell written before spec 049: it implements only the earlier methods.
struct PlainCell;

impl Cell for PlainCell {
    fn manifest() -> &'static str {
        MANIFEST
    }

    fn migrations() -> &'static [Migration] {
        &MIGRATIONS
    }

    fn routes(_state: AppState) -> Router {
        Router::new()
    }
}

#[test]
fn fixture_cell() {
    let Ok(verb) = std::env::var(VERB) else {
        return;
    };
    let args: Vec<String> = verb.split_whitespace().map(str::to_owned).collect();
    let env = rahi_cli::process_env();
    let code = if std::env::var(CASE).as_deref() == Ok("plain") {
        rahi_cli::run_with::<PlainCell>(&args, &env)
    } else {
        rahi_cli::run_with::<CheckCell>(&args, &env)
    };
    std::process::exit(code);
}

// ------------------------------------------------------------------ driver

fn free_port() -> u16 {
    use std::fs::{File, OpenOptions};
    use std::net::Ipv4Addr;
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
            return port;
        }
    }
}

/// A stand-in for rauthy that answers its health route and knows no client,
/// so the chassis's `rauthy` and `tokens` checks pass without one.
fn stub_rauthy() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap_or(0);
            let head = String::from_utf8_lossy(&buf[..n]);
            let status = if head.starts_with("GET /auth/v1/health ") {
                "200 OK"
            } else {
                "404 Not Found"
            };
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
            );
        }
    });
    port
}

struct Volume {
    dir: tempfile::TempDir,
    env: Vec<(String, String)>,
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
    took: Duration,
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
        let passkey_config = rahi_types::Config::from_env(&BTreeMap::from([(
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
            (
                "RAHI_HIQLITE_API_ADDR".to_owned(),
                format!("127.0.0.1:{}", free_port()),
            ),
            (
                "RAHI_HIQLITE_RAFT_ADDR".to_owned(),
                format!("127.0.0.1:{}", free_port()),
            ),
            (
                "RAHI_RAUTHY_ADDR".to_owned(),
                format!("127.0.0.1:{}", stub_rauthy()),
            ),
            ("RAHI_RAUTHY_MODE".to_owned(), "none".to_owned()),
            (
                "RAHI_LISTEN_ADDR".to_owned(),
                format!("127.0.0.1:{}", free_port()),
            ),
            (MODEL.to_owned(), "m-7".to_owned()),
        ];
        let volume = Self { dir, env };
        let migrate = volume.run("migrate", "");
        assert_eq!(
            migrate.code, 0,
            "migrate: {}{}",
            migrate.stdout, migrate.stderr
        );
        volume
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.path().join("calls"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn run(&self, verb: &str, case: &str) -> Run {
        let _ = std::fs::remove_file(self.dir.path().join("calls"));
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args(["--exact", FIXTURE, "--nocapture", "--test-threads=1"]);
        for (k, _) in std::env::vars() {
            if k.starts_with("RAHI_") {
                cmd.env_remove(k);
            }
        }
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        cmd.env(VERB, verb)
            .env(CASE, case)
            .env(CALLS, self.dir.path().join("calls"));
        let started = Instant::now();
        let out = cmd.output().unwrap();
        Run {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            took: started.elapsed(),
        }
    }
}

/// The report's lines: the twelve chassis lines, the app lines, and the
/// verdict, without what libtest prints around the child.
fn report(run: &Run) -> Vec<&str> {
    let start = run
        .stdout
        .find("PASS config:")
        .or_else(|| run.stdout.find("FAIL config:"))
        .unwrap_or_else(|| panic!("no report:\n{}\n{}", run.stdout, run.stderr));
    let body = &run.stdout[start..];
    let end = body.find("preflight: ").expect("the verdict line");
    let tail = &body[end..];
    let verdict_end = tail.find('\n').unwrap_or(tail.len());
    body[..end + verdict_end].lines().collect()
}

fn chassis_names(lines: &[&str]) -> Vec<String> {
    lines
        .iter()
        .take(rahi_ops::preflight::CHECKS.len())
        .map(|l| {
            l.split_once(':')
                .unwrap()
                .0
                .split_once(' ')
                .unwrap()
                .1
                .to_owned()
        })
        .collect()
}

fn assert_chassis_first(lines: &[&str]) {
    assert_eq!(
        chassis_names(lines),
        rahi_ops::preflight::CHECKS.map(str::to_owned).to_vec(),
        "{lines:#?}"
    );
}

#[test]
fn three_checks_report_in_order_and_the_capability_failure_fails_preflight() {
    // FR-001.
    let volume = Volume::new();
    let run = volume.run("preflight", "three");
    let lines = report(&run);
    assert_chassis_first(&lines);
    for line in &lines[..12] {
        assert!(
            line.starts_with("PASS "),
            "every chassis check passes: {lines:#?}"
        );
    }
    assert_eq!(
        &lines[12..],
        &[
            "PASS app.model: 1 model(s)",
            "  | configured m-7",
            "  | active m-7",
            "WARN app.backlog: 3 rows wait for embedding",
            "FAIL app.provider: capability http.egress on api.ungranted.example for service \
             embed is not in the manifest ceiling",
            "preflight: failed",
        ],
        "{}",
        run.stdout
    );
    assert_eq!(run.code, 1);
    assert_eq!(volume.calls(), ["model", "backlog", "provider"]);
}

#[test]
fn a_warning_alone_does_not_fail_preflight() {
    // FR-002.
    let volume = Volume::new();
    let run = volume.run("preflight", "two");
    let lines = report(&run);
    assert!(
        lines.contains(&"WARN app.backlog: 3 rows wait for embedding"),
        "{lines:#?}"
    );
    assert_eq!(lines.last(), Some(&"preflight: ok"), "{lines:#?}");
    assert_eq!(run.code, 0, "{}{}", run.stdout, run.stderr);
}

#[test]
fn a_slow_check_times_out_a_panicking_one_is_named_and_the_next_still_runs() {
    // FR-003, FR-007.
    let volume = Volume::new();
    let run = volume.run("preflight", "bounds");
    let lines = report(&run);
    assert_eq!(
        &lines[12..],
        &[
            "FAIL app.sleeper: timed out after 5s",
            "FAIL app.panics: panicked",
            "PASS app.after: still ran",
            "preflight: failed",
        ],
        "{}",
        run.stdout
    );
    assert_eq!(run.code, 1);
    assert_eq!(volume.calls(), ["sleeper", "panics", "after"]);
    assert!(
        run.took < Duration::from_secs(30 + 30),
        "the phase bound holds: {:?}",
        run.took
    );

    // The node was shut down after the aborted check: the owner lock is
    // free, a second preflight opens the store, and the seeded rows read
    // back as the migration wrote them.
    let again = volume.run("preflight", "two");
    assert_eq!(again.code, 0, "{}{}", again.stdout, again.stderr);
    let lines = report(&again);
    assert_eq!(
        &lines[12..15],
        &[
            "PASS app.model: 1 model(s)",
            "  | configured m-7",
            "  | active m-7"
        ],
        "{}",
        again.stdout
    );
}

#[test]
fn a_refused_declaration_is_one_line_and_runs_nothing() {
    // FR-004.
    let volume = Volume::new();
    for (case, says) in [
        ("duplicate", "check name \"model\" is declared twice"),
        (
            "invalid",
            "check name \"Bad-Name\" is not [a-z][a-z0-9_]{0,47}",
        ),
        ("many", "33 checks are declared; at most 32 are allowed"),
    ] {
        let run = volume.run("preflight", case);
        let lines = report(&run);
        assert_eq!(
            &lines[12..],
            &[format!("FAIL app: {says}").as_str(), "preflight: failed"],
            "{case}: {}",
            run.stdout
        );
        assert_eq!(run.code, 1, "{case}");
        assert!(volume.calls().is_empty(), "{case}: {:?}", volume.calls());
    }
}

#[test]
fn a_store_that_does_not_open_skips_every_check_by_name() {
    // FR-005: a key set that does not read.
    let volume = Volume::new();
    let keys = volume.dir.path().join("keys");
    std::fs::write(keys.join(rahi_ops::STORE_SECRETS_FILE), b"not json").unwrap();
    let run = volume.run("preflight", "three");
    let lines = report(&run);
    assert_eq!(
        &lines[12..],
        &[
            "SKIP app.model: skipped: the store did not open",
            "SKIP app.backlog: skipped: the store did not open",
            "SKIP app.provider: skipped: the store did not open",
            "preflight: failed",
        ],
        "{}",
        run.stdout
    );
    assert!(volume.calls().is_empty(), "{:?}", volume.calls());
}

#[test]
fn details_and_report_lines_are_sanitized_cut_and_capped() {
    // FR-006.
    let volume = Volume::new();
    let run = volume.run("preflight", "noisy");
    let lines = report(&run);
    let app = &lines[12..lines.len() - 1];
    assert_eq!(app[0], "PASS app.noisy: two?lines");
    assert_eq!(app.len(), 1 + 32 + 1, "{app:#?}");
    for line in &app[1..33] {
        let text = line.strip_prefix("  | ").expect("a report line");
        assert_eq!(text.chars().count(), 240, "{line}");
    }
    assert_eq!(app[33], "  | (8 more lines omitted)");
}

#[test]
fn a_cell_that_declares_nothing_prints_what_it_printed_before() {
    // FR-008: the same cell with no declaration prints the twelve chassis
    // lines and the verdict, and nothing between them.
    let volume = Volume::new();
    let run = volume.run("preflight", "none");
    let lines = report(&run);
    assert_chassis_first(&lines);
    assert_eq!(lines.len(), 13, "{lines:#?}");
    assert_eq!(lines[12], "preflight: ok");
    assert_eq!(run.code, 0);

    // A cell that never heard of the method prints the same report.
    let plain = volume.run("preflight", "plain");
    let plain_lines = report(&plain);
    assert_eq!(plain.code, 0, "{}{}", plain.stdout, plain.stderr);
    assert_eq!(chassis_names(&plain_lines), chassis_names(&lines));
    assert_eq!(plain_lines.len(), lines.len());
    assert_eq!(plain_lines.last(), lines.last());
}
