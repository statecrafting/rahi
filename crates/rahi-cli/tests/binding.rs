//! The binding document end to end (spec 040 AC-1, AC-4, AC-5).
//!
//! Every test drives the `rahi` binary, the empty cell, over a temp volume
//! with no rauthy, and reads `/binding` over a plain HTTP/1.1 socket.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::io::{Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use base64::Engine as _;
use rahi_ops::KeySet;
use rahi_ops::binding::{self, validate};
use rahi_store::{EncKey, EncKeys, StoreSecrets};
use serde_json::Value;

/// The packaged Rauthy image, as spec 040 B-5 fixes it.
const RAUTHY_IMAGE: &str = "ghcr.io/bartekus/rauthy-patched:0.36.2-patched.3@sha256:d75cac0f708f3e238c458b622fea2f0b7dda9b67e9435eeafa37698d88a2a3c8";
/// A digest-pinned artifact image a deployer might declare.
const ARTIFACT: &str = "ghcr.io/statecrafting/rahi@sha256:0a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f9";

/// A loopback port no concurrently running test process was handed: the
/// allocator every test binary in the workspace shares (spec 030 D-10).
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

fn free_port() -> String {
    format!("127.0.0.1:{}", loopback_port())
}

/// One volume: a data dir, a full key set, free ports, no rauthy.
struct Volume {
    dir: tempfile::TempDir,
    listen: String,
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
        let listen = free_port();
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
            ("RAHI_LISTEN_ADDR".to_owned(), listen.clone()),
        ];
        Self { dir, listen, env }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn command(&self, exe: &Path, args: &[&str], extra: &[(&str, &str)]) -> Command {
        let mut cmd = Command::new(exe);
        cmd.args(args);
        for (k, _) in std::env::vars() {
            if k.starts_with("RAHI_") {
                cmd.env_remove(k);
            }
        }
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        for (k, v) in extra {
            cmd.env(k, v);
        }
        cmd
    }

    fn migrate(&self) {
        let out = self
            .command(Path::new(env!("CARGO_BIN_EXE_rahi")), &["migrate"], &[])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Start `serve` from `exe` and wait until it answers `/healthz`.
    fn serve(&self, exe: &Path, extra: &[(&str, &str)], name: &str) -> Serving {
        let log = self.path().join(format!("{name}.stderr"));
        let child = self
            .command(exe, &["serve"], extra)
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&log).unwrap())
            .spawn()
            .unwrap();
        let serving = Serving {
            child,
            addr: self.listen.clone(),
            log,
        };
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if get(&serving.addr, "/healthz").is_some_and(|(status, _)| status == 200) {
                return serving;
            }
            assert!(
                Instant::now() < deadline,
                "serve did not answer: {}",
                serving.stderr()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// A running `serve`, stopped with SIGTERM so the store shuts down cleanly.
struct Serving {
    child: Child,
    addr: String,
    log: PathBuf,
}

impl Serving {
    fn binding(&self) -> (Vec<u8>, Value) {
        let (status, body) = get(&self.addr, "/binding").expect("/binding answers");
        assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
        let document = serde_json::from_slice(&body).unwrap();
        (body, document)
    }

    fn stderr(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    fn stop(mut self) -> String {
        let status = Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status()
            .unwrap();
        assert!(status.success());
        let deadline = Instant::now() + Duration::from_secs(30);
        while self.child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "serve did not stop");
            std::thread::sleep(Duration::from_millis(50));
        }
        self.stderr()
    }
}

impl Drop for Serving {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// One `GET`, no cookie and no CSRF token; the status and the body.
fn get(addr: &str, path: &str) -> Option<(u16, Vec<u8>)> {
    let mut stream = TcpStream::connect(addr).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .ok()?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )
    .ok()?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).ok()?;
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let status = head.split_whitespace().nth(1)?.parse().ok()?;
    let mut body = raw[split + 4..].to_vec();
    if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        body = dechunk(&body);
    }
    Some((status, body))
}

fn dechunk(mut raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(line) = raw.windows(2).position(|w| w == b"\r\n") {
        let size = usize::from_str_radix(String::from_utf8_lossy(&raw[..line]).trim(), 16).unwrap();
        if size == 0 {
            break;
        }
        out.extend_from_slice(&raw[line + 2..line + 2 + size]);
        raw = &raw[line + 4 + size..];
    }
    out
}

fn sha256_of(path: &Path) -> String {
    use std::fmt::Write as _;
    let bytes = std::fs::read(path).unwrap();
    let mut out = "sha256:".to_owned();
    for byte in ring::digest::digest(&ring::digest::SHA256, &bytes).as_ref() {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn at<'a>(document: &'a Value, path: &str) -> &'a Value {
    path.split('.').fold(document, |node, step| &node[step])
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// The `http_requests_total` lines of a `/metrics` scrape.
fn request_counts(addr: &str) -> Vec<String> {
    let (_, body) = get(addr, "/metrics").unwrap();
    String::from_utf8(body)
        .unwrap()
        .lines()
        .filter(|line| line.starts_with("http_requests_total"))
        .map(str::to_owned)
        .collect()
}

/// AC-4: section 3.1's three examples are test input. Each is fed to the
/// validator FR-001 uses, and each validates.
#[test]
fn the_three_examples_of_section_3_1_validate() {
    let spec = std::fs::read_to_string(
        repo_root().join("specs/040-runtime-identity-and-binding-surface/spec.md"),
    )
    .unwrap();
    let section = &spec[spec.find("### 3.1").unwrap()..spec.find("## 4.").unwrap()];
    let examples: Vec<Value> = section
        .split("```json")
        .skip(1)
        .map(|block| block.split("```").next().unwrap())
        .filter(|block| block.contains("\"schema\""))
        .map(|block| serde_json::from_str(block).unwrap())
        .collect();
    assert_eq!(examples.len(), 3, "section 3.1 holds three examples");
    for (n, example) in examples.iter().enumerate() {
        validate(example).unwrap_or_else(|err| panic!("example {}: {err}", n + 1));
    }
    // Example 3 is the version output outside a cell: what this binary
    // prints has the same members at the same bases.
    let printed: Value = serde_json::from_slice(
        &Command::new(env!("CARGO_BIN_EXE_rahi"))
            .args(["version", "--binding"])
            .env_remove(binding::ENV_ARTIFACT_IMAGE)
            .env_remove(binding::ENV_POD_NAME)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    for path in binding::WRAPPER_PATHS {
        if path == "observation" || path.starts_with("build.") || path == "instance.id" {
            continue;
        }
        assert_eq!(
            at(&printed, path)["basis"],
            at(&examples[2], path)["basis"],
            "{path}"
        );
    }
}

/// FR-001 to FR-004, FR-006, FR-007a: the document a booted cell serves,
/// its stability for the life of the process, and a restart's new id.
#[test]
fn a_booted_cell_serves_its_binding_and_a_restart_mints_a_new_instance() {
    let volume = Volume::new();
    volume.migrate();
    let exe = Path::new(env!("CARGO_BIN_EXE_rahi"));
    let extra = [
        (binding::ENV_ARTIFACT_IMAGE, ARTIFACT),
        (binding::ENV_RAUTHY_IMAGE, RAUTHY_IMAGE),
        (binding::ENV_POD_NAME, "rahi-0"),
    ];
    let serving = volume.serve(exe, &extra, "first");
    let (bytes, document) = serving.binding();

    // FR-001: the shape.
    validate(&document).unwrap();
    // Two reads in one boot are byte-identical (B-6).
    assert_eq!(serving.binding().0, bytes);

    // FR-002: the measured values are the measured bytes.
    assert_eq!(
        at(&document, "build.binary.sha256"),
        &serde_json::json!({ "value": sha256_of(exe), "basis": "measured" })
    );
    let hash = rahi_kernel::Manifest::parse(rahi_cli::EmptyCell::MANIFEST)
        .unwrap()
        .hash()
        .unwrap()
        .to_string();
    assert_eq!(
        at(&document, "manifest.hash"),
        &serde_json::json!({ "value": hash, "basis": "measured" })
    );
    assert_eq!(at(&document, "store.schema_version")["value"], 1);
    assert_eq!(at(&document, "store.schema_version")["basis"], "measured");
    assert_eq!(
        at(&document, "store.migration_sets")["value"],
        serde_json::json!({})
    );
    assert_eq!(at(&document, "store.migration_sets")["basis"], "measured");
    assert_eq!(
        at(&document, "store.layout"),
        &serde_json::json!({ "value": "app-store", "basis": "declared" })
    );
    assert_eq!(
        at(&document, "artifact.image"),
        &serde_json::json!({ "value": ARTIFACT, "basis": "declared" })
    );
    for path in ["epoch.ref", "epoch.match"] {
        assert_eq!(at(&document, path)["reason"], "not_implemented", "{path}");
    }
    assert_eq!(at(&document, "instance.pod")["value"], "rahi-0");
    assert_eq!(
        at(&document, "build.rahi_version")["value"],
        env!("CARGO_PKG_VERSION")
    );
    assert_eq!(at(&document, "build.revision")["reason"], "not_declared");

    // FR-003: minted, and says so.
    let id = at(&document, "instance.id")["value"]
        .as_str()
        .unwrap()
        .to_owned();
    let (node, hex) = id.split_once('-').unwrap();
    assert!(
        node.chars().all(|c| c.is_ascii_digit()) && !node.is_empty(),
        "{id}"
    );
    assert!(
        hex.len() == 32 && hex.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')),
        "{id}"
    );
    assert_eq!(at(&document, "instance.id")["basis"], "minted");
    assert_eq!(at(&document, "instance.node")["value"].to_string(), node);

    // FR-007a: the packaged identities, declared, and nothing more.
    assert_eq!(
        at(&document, "components.rauthy.image"),
        &serde_json::json!({ "value": RAUTHY_IMAGE, "basis": "declared" })
    );
    assert_eq!(
        at(&document, "components.hiqlite"),
        &serde_json::json!({
            "value": {
                "hiqlite-patched": "0.15.0-patched.3",
                "hiqlite-wal-patched": "0.15.0-patched.3",
                "hiqlite-derive-patched": "0.15.0-patched.3"
            },
            "basis": "declared"
        })
    );
    let text = String::from_utf8_lossy(&bytes).to_ascii_lowercase();
    for claim in ["health", "qualif", "attest", "patched.4", "n3", "n=3"] {
        assert!(!text.contains(claim), "the document claims {claim:?}");
    }

    // The genesis record carries the same manifest hash.
    let metrics = get(&serving.addr, "/metrics").unwrap().1;
    let metrics = String::from_utf8(metrics).unwrap();
    let build_info: Vec<&str> = metrics
        .lines()
        .filter(|line| line.starts_with("rahi_build_info"))
        .collect();
    assert_eq!(
        build_info,
        vec![format!(
            "rahi_build_info{{contract_version=\"1.0.0\",rahi_version=\"{}\"}} 1",
            env!("CARGO_PKG_VERSION")
        )]
    );

    // FR-006: unguarded and unobserved. No cookie, no CSRF token, and a
    // scrape leaves the request counters where they were.
    let before = request_counts(&serving.addr);
    for _ in 0..3 {
        serving.binding();
    }
    assert_eq!(request_counts(&serving.addr), before);

    serving.stop();
    let export = volume.path().join("chain.jsonl");
    let out = volume
        .command(exe, &["ledger", "export", export.to_str().unwrap()], &[])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let genesis = std::fs::read_to_string(&export).unwrap();
    assert!(
        genesis.lines().next().unwrap().contains(&hash),
        "the genesis record names the booted manifest hash"
    );

    // A restart mints a different id on the same node; without the packaged
    // declaration, Rauthy is not applicable.
    let again = volume.serve(exe, &[], "second");
    let (_, second) = again.binding();
    validate(&second).unwrap();
    assert_ne!(at(&second, "instance.id"), at(&document, "instance.id"));
    assert_eq!(at(&second, "instance.node"), at(&document, "instance.node"));
    assert_eq!(
        at(&second, "components.rauthy.image")["reason"],
        "not_applicable"
    );
    assert_eq!(
        at(&second, "artifact.image"),
        &serde_json::json!({
            "basis": "absent",
            "reason": "not_declared",
            "source": binding::ENV_ARTIFACT_IMAGE
        })
    );
    assert_eq!(at(&second, "instance.pod")["source"], binding::ENV_POD_NAME);
    again.stop();
}

/// FR-004: a malformed artifact image refuses the start, naming it.
#[test]
fn a_malformed_artifact_image_refuses_the_start() {
    let volume = Volume::new();
    volume.migrate();
    let out = volume
        .command(
            Path::new(env!("CARGO_BIN_EXE_rahi")),
            &["serve"],
            &[(
                binding::ENV_ARTIFACT_IMAGE,
                "ghcr.io/statecrafting/rahi:latest",
            )],
        )
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains(binding::ENV_ARTIFACT_IMAGE), "{stderr}");
}

/// FR-004: an executable the process cannot read back is stated absent,
/// the boot completes, the log carries the error, and the document names no
/// path and no error string.
#[test]
fn an_unreadable_executable_is_absent_and_leaks_nothing() {
    use std::os::unix::fs::PermissionsExt as _;

    let volume = Volume::new();
    volume.migrate();
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("rahi-unreadable");
    std::fs::copy(env!("CARGO_BIN_EXE_rahi"), &exe).unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o111)).unwrap();
    assert!(
        std::fs::read(&exe).is_err(),
        "this test needs a user that file modes bind, not root"
    );

    let serving = volume.serve(&exe, &[], "unreadable");
    let (bytes, document) = serving.binding();
    validate(&document).unwrap();
    assert_eq!(
        at(&document, "build.binary.sha256"),
        &serde_json::json!({ "basis": "absent", "reason": "unreadable" })
    );
    let text = String::from_utf8_lossy(&bytes);
    let dir_text = dir.path().display().to_string();
    for leak in [dir_text.as_str(), "rahi-unreadable", "denied", "os error"] {
        assert!(!text.contains(leak), "the document carries {leak:?}");
    }
    let log = serving.stop();
    assert!(
        log.contains("binding: the executable could not be read")
            && log.contains("rahi-unreadable"),
        "{log}"
    );
}

/// FR-008, AC-5: `rahi version` is byte for byte what it was, and
/// `--binding` is the document a binary can state outside a cell.
#[test]
fn the_version_verb_is_unchanged_and_binding_is_explicit() {
    let out = Command::new(env!("CARGO_BIN_EXE_rahi"))
        .arg("version")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        out.stdout,
        format!("rahi {}\n", env!("CARGO_PKG_VERSION")).into_bytes()
    );

    let out = Command::new(env!("CARGO_BIN_EXE_rahi"))
        .args(["version", "--binding"])
        .env_remove(binding::ENV_ARTIFACT_IMAGE)
        .env_remove(binding::ENV_RAUTHY_IMAGE)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let document: Value = serde_json::from_slice(&out.stdout).unwrap();
    validate(&document).unwrap();
    assert_eq!(document["schema"], binding::SCHEMA);
    for path in [
        "manifest.hash",
        "manifest.app.name",
        "manifest.app.org",
        "manifest.contract_version",
        "store.layout",
        "store.schema_version",
        "store.migration_sets",
        "instance.node",
        "components.rauthy.image",
    ] {
        assert_eq!(at(&document, path)["reason"], "not_applicable", "{path}");
    }
    for path in ["epoch.ref", "epoch.match"] {
        assert_eq!(at(&document, path)["reason"], "not_implemented", "{path}");
    }
    assert!(
        at(&document, "instance.id")["value"]
            .as_str()
            .unwrap()
            .starts_with("0-")
    );
    assert_eq!(
        at(&document, "build.binary.sha256")["value"],
        sha256_of(Path::new(env!("CARGO_BIN_EXE_rahi")))
    );
}
