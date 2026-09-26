//! The cell and the process driver the spec 043 stop tests share (B-7, B-9,
//! B-10).
//!
//! As in spec 035's shutdown suite, the test binary is the fixture: a child
//! started on [`FIXTURE`] with [`FIXTURE_VERB`] set composes [`StopCell`]
//! through `rahi_cli::run_with` and exits with the verb's code. Each test
//! file that uses this module declares the entry with [`fixture_entry!`].
//!
//! [`StopCell`] carries the declared workload of AC-7: a route whose every
//! request is a ledgered denial, and a streaming route whose streams stay
//! open until the cell shuts them down.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    dead_code
)]

use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Barrier, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse as _, Response};
use axum::routing::get;
use base64::Engine as _;
use rahi_cli::Cell;
use rahi_edge::stream::{StreamRoutes as _, stream};
use rahi_edge::{AppState, READYZ_PATH, Route, RouteClass, StreamHub};
use rahi_kernel::{CapabilityKind, Governed};
use rahi_ops::KeySet;
use rahi_store::{EncKey, EncKeys, Migration, StoreHandle, StoreSecrets};
use rahi_types::Sub;

/// The libtest name of the fixture's process entry.
pub const FIXTURE: &str = "fixture_cell";
/// Set on a child to the verb (and its arguments) the fixture runs.
pub const FIXTURE_VERB: &str = "RAHI_TEST_FIXTURE_VERB";

pub const DENY_PATH: &str = "/api/deny";
pub const EVENTS_PATH: &str = "/api/events";
/// A request that takes longer than the connection drain (`DRAIN_BUDGET`).
pub const SLOW_PATH: &str = "/api/slow";

pub const READY_BUDGET: Duration = Duration::from_secs(120);

const MANIFEST: &str = r#"# The stop cell: `items` may be read and nothing else, so the write its
# deny route attempts is always a ledgered denial (spec 043 AC-7).

schema_version = "1.0.0"

[app]
name = "stop-cell"
org = "rahi-tests"

[resources]
tables = ["items"]

[[capabilities]]
id = "items-read"
kind = "db.read"
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
operator_role = "stop-cell-operator"

[contract]
version = "1.0.0"
"#;

pub struct StopCell;

impl Cell for StopCell {
    fn manifest() -> &'static str {
        MANIFEST
    }

    fn migrations() -> &'static [Migration] {
        &[]
    }

    fn routes(state: AppState) -> Router {
        let write = Governed::new(
            state.kernel(),
            "items",
            CapabilityKind::DbWrite,
            "items",
            state.store().clone(),
        )
        .expect("the manifest declares the items service");
        let hub = state.extension::<StreamHub>().unwrap_or_default();
        Router::new()
            .route(
                SLOW_PATH,
                get(|| async {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    "slow"
                }),
            )
            .route(DENY_PATH, get(deny))
            .with_state(write)
            .merge(Router::new().stream_route(
                EVENTS_PATH,
                get(move || {
                    let hub = hub.clone();
                    async move { events(&hub) }
                }),
            ))
    }

    fn exposed() -> Vec<Route> {
        vec![
            Route::new(DENY_PATH, RouteClass::Public),
            Route::new(EVENTS_PATH, RouteClass::Public),
            Route::new(SLOW_PATH, RouteClass::Public),
        ]
    }
}

async fn deny(State(write): State<Governed<StoreHandle>>) -> Response {
    match write
        .execute(
            &Sub::new("spec-043-caller"),
            "INSERT INTO items (body) VALUES ('x')",
            vec![],
        )
        .await
    {
        Ok(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "the manifest grants no write, and one was allowed",
        )
            .into_response(),
        Err(err) => rahi_edge::error::response(&err),
    }
}

/// A stream that says hello and then stays open until the cell cancels it.
fn events(hub: &StreamHub) -> Response {
    let (tx, rx) = hub.channel();
    tokio::spawn(async move {
        let _ = tx.emit(rahi_edge::stream::Event::new("hello", "open"));
        tx.cancelled().await;
    });
    stream(rx)
}

/// The fixture's process entry. Without [`FIXTURE_VERB`] it does nothing.
pub fn fixture_main() {
    let Ok(verb) = std::env::var(FIXTURE_VERB) else {
        return;
    };
    let args: Vec<String> = verb.split_whitespace().map(str::to_owned).collect();
    let code = rahi_cli::run_with::<StopCell>(&args, &rahi_cli::process_env());
    std::process::exit(code);
}

/// Declare the fixture's libtest entry in a test file.
#[macro_export]
macro_rules! fixture_entry {
    () => {
        #[test]
        fn fixture_cell() {
            stop_fixture::fixture_main();
        }
    };
}

// ------------------------------------------------------------------ nodes

/// A loopback port no concurrently running test process was handed: the
/// allocator every rahi test shares (spec 035 D-12).
pub fn free_port() -> u16 {
    use std::fs::{File, OpenOptions};
    use std::net::{Ipv4Addr, TcpListener};
    use std::sync::PoisonError;
    use std::sync::atomic::{AtomicU32, Ordering};

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

/// Every key `serve` checks at boot, for a cell that runs without Rauthy.
pub fn write_keys(data_dir: &Path) {
    let keys = KeySet::at(data_dir.join("keys"));
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
}

/// The Rauthy binary the live workflow extracts from the pinned image, when
/// this run has one; a run that requires it and lacks it fails.
pub fn test_rauthy() -> Option<PathBuf> {
    let found = std::env::var("RAHI_TEST_RAUTHY")
        .ok()
        .filter(|p| !p.trim().is_empty())
        .map(PathBuf::from);
    if found.is_none() && std::env::var("RAHI_REQUIRE_RAUTHY").as_deref() == Ok("1") {
        panic!("RAHI_REQUIRE_RAUTHY=1 and RAHI_TEST_RAUTHY names no Rauthy binary");
    }
    found
}

/// One cell on loopback, on its own volume.
pub struct Node {
    pub root: tempfile::TempDir,
    pub data_dir: PathBuf,
    pub listen: String,
    pub env: Vec<(String, String)>,
}

impl Node {
    /// A cell with no Rauthy: `serve` alone.
    pub fn new() -> Self {
        let node = Self::bare(None);
        write_keys(&node.data_dir);
        node
    }

    /// A cell under `supervise` with the Rauthy at `rauthy`; its keys come
    /// from `first-boot`.
    pub fn with_rauthy(rauthy: &Path) -> Self {
        let node = Self::bare(Some(rauthy));
        let first = node.run("first-boot");
        assert_eq!(first.code, Some(0), "first-boot\n{}", first.logs());
        node
    }

    /// `size` cells of one cluster, no Rauthy, each on its own volume.
    pub fn cluster(size: usize) -> Vec<Self> {
        let mut nodes: Vec<Self> = (0..size).map(|_| Self::bare(None)).collect();
        let peers = nodes
            .iter()
            .enumerate()
            .map(|(i, n)| {
                format!(
                    "{} {} {}",
                    i + 1,
                    n.var("RAHI_HIQLITE_RAFT_ADDR"),
                    n.var("RAHI_HIQLITE_API_ADDR")
                )
            })
            .collect::<Vec<_>>()
            .join(";");
        for (i, node) in nodes.iter_mut().enumerate() {
            write_keys(&node.data_dir);
            node.set_env("RAHI_HIQ_NODE_ID", &(i + 1).to_string());
            node.set_env("RAHI_HIQ_NODES", &peers);
        }
        nodes
    }

    /// The value this node's children get for `key`.
    pub fn var(&self, key: &str) -> String {
        self.env
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    }

    fn bare(rauthy: Option<&Path>) -> Self {
        let root = tempfile::tempdir().unwrap();
        let data_dir = root.path().join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        let (listen, raft, api, rauthy_port, rauthy_raft, rauthy_api) = (
            free_port(),
            free_port(),
            free_port(),
            free_port(),
            free_port(),
            free_port(),
        );
        let listen = format!("127.0.0.1:{listen}");
        let mut env = vec![
            (
                "RAHI_PUBLIC_URL".to_owned(),
                format!("http://localhost:{}", listen.rsplit(':').next().unwrap()),
            ),
            ("RAHI_DATA_DIR".to_owned(), data_dir.display().to_string()),
            (
                "RAHI_HIQLITE_RAFT_ADDR".to_owned(),
                format!("127.0.0.1:{raft}"),
            ),
            (
                "RAHI_HIQLITE_API_ADDR".to_owned(),
                format!("127.0.0.1:{api}"),
            ),
            (
                "RAHI_RAUTHY_ADDR".to_owned(),
                format!("127.0.0.1:{rauthy_port}"),
            ),
            (
                "RAHI_RAUTHY_HQL_RAFT_PORT".to_owned(),
                rauthy_raft.to_string(),
            ),
            (
                "RAHI_RAUTHY_HQL_API_PORT".to_owned(),
                rauthy_api.to_string(),
            ),
            ("RAHI_LISTEN_ADDR".to_owned(), listen.clone()),
        ];
        match rauthy {
            Some(bin) => {
                env.push(("RAHI_RAUTHY_MODE".to_owned(), "required".to_owned()));
                env.push(("RAHI_RAUTHY_BIN".to_owned(), bin.display().to_string()));
            }
            None => env.push(("RAHI_RAUTHY_MODE".to_owned(), "none".to_owned())),
        }
        Self {
            root,
            data_dir,
            listen,
            env,
        }
    }

    /// Set one more variable on every later child.
    pub fn set_env(&mut self, key: &str, value: &str) {
        self.env.retain(|(k, _)| k != key);
        self.env.push((key.to_owned(), value.to_owned()));
    }

    /// This test binary, started as the fixture running `verb`.
    pub fn command(&self, verb: &str) -> Command {
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args([FIXTURE, "--exact", "--nocapture", "--test-threads=1"]);
        for (key, _) in std::env::vars() {
            if key.starts_with("RAHI_") || key.starts_with("HQL_") {
                cmd.env_remove(key);
            }
        }
        cmd.envs(self.env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        cmd.env(FIXTURE_VERB, verb);
        cmd
    }

    /// Run `verb` to completion.
    pub fn run(&self, verb: &str) -> Finished {
        let output = self.command(verb).output().unwrap();
        Finished {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    /// Start `verb` in the background, collecting what it prints.
    pub fn spawn(&self, verb: &str) -> Running {
        let mut child = self
            .command(verb)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = Arc::new(Mutex::new(String::new()));
        let stderr = Arc::new(Mutex::new(String::new()));
        let readers = vec![
            collect(child.stdout.take().unwrap(), Arc::clone(&stdout)),
            collect(child.stderr.take().unwrap(), Arc::clone(&stderr)),
        ];
        Running {
            child,
            stdout,
            stderr,
            readers,
        }
    }

    /// Wait for `/readyz` to answer 200 while the process is still running.
    pub fn wait_ready(&self, running: &mut Running, within: Duration) {
        let deadline = Instant::now() + within;
        loop {
            if let Some(status) = running.child.try_wait().unwrap() {
                panic!(
                    "the cell exited {status} before it was ready\n{}",
                    running.logs()
                );
            }
            if matches!(http_get(&self.listen, READYZ_PATH), Ok((200, _))) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the cell on {} was not ready within {within:?}\n{}",
                self.listen,
                running.logs()
            );
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    /// The stop record the process left.
    pub fn stop_record(&self) -> Option<rahi_ops::stop::StopRecord> {
        std::fs::read(self.data_dir.join(rahi_ops::stop::STOP_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
    }

    /// Wait until the stop record says SIGTERM arrived.
    pub fn wait_received(&self, within: Duration) -> rahi_ops::stop::StopRecord {
        let deadline = Instant::now() + within;
        loop {
            if let Some(record) = self.stop_record()
                && record.received_at.is_some()
            {
                return record;
            }
            assert!(Instant::now() < deadline, "no SIGTERM was recorded");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn collect(
    mut pipe: impl std::io::Read + Send + 'static,
    into: Arc<Mutex<String>>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match pipe.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => into
                    .lock()
                    .unwrap()
                    .push_str(&String::from_utf8_lossy(&buf[..n])),
            }
        }
    })
}

pub struct Finished {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl Finished {
    pub fn logs(&self) -> String {
        format!(
            "exit {:?}\n--- stdout\n{}\n--- stderr\n{}",
            self.code, self.stdout, self.stderr
        )
    }
}

pub struct Running {
    pub child: Child,
    stdout: Arc<Mutex<String>>,
    stderr: Arc<Mutex<String>>,
    readers: Vec<JoinHandle<()>>,
}

impl Running {
    pub fn logs(&self) -> String {
        format!(
            "--- stdout\n{}\n--- stderr\n{}",
            self.stdout.lock().unwrap(),
            self.stderr.lock().unwrap()
        )
    }

    fn signal(&self, name: &str) {
        let status = Command::new("kill")
            .arg(format!("-{name}"))
            .arg(self.child.id().to_string())
            .status()
            .unwrap();
        assert!(status.success(), "kill -{name} exited {status}");
    }

    /// The graceful stop an orchestrator sends.
    pub fn sigterm(&self) {
        self.signal("TERM");
    }

    /// Freeze or thaw the process.
    pub fn sigstop(&self) {
        self.signal("STOP");
    }

    /// Thaw the process.
    pub fn sigcont(&self) {
        self.signal("CONT");
    }

    /// The forced stop an orchestrator sends when the grace ran out.
    pub fn sigkill(&self) {
        self.signal("KILL");
    }

    /// Wait for the process to exit on its own within `within`.
    pub fn wait(&mut self, within: Duration) -> Finished {
        let deadline = Instant::now() + within;
        let code = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status.code();
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                panic!(
                    "the process did not stop within {within:?}\n{}",
                    self.logs()
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        // A child the process left behind (a Rauthy that ignored SIGTERM)
        // can hold the pipes open, so the readers get a bounded moment to
        // drain rather than a join.
        let deadline = Instant::now() + Duration::from_secs(2);
        while self.readers.iter().any(|r| !r.is_finished()) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        self.readers.clear();
        Finished {
            code,
            stdout: self.stdout.lock().unwrap().clone(),
            stderr: self.stderr.lock().unwrap().clone(),
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

// ------------------------------------------------------------------- http

/// One `GET` over a fresh connection, answered in full.
pub fn http_get(addr: &str, path: &str) -> std::io::Result<(u16, String)> {
    let mut stream = TcpStream::connect(addr)?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"
    )?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw)?;
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| std::io::Error::other(format!("no complete response: {text:?}")))?;
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| std::io::Error::other(format!("no status line: {head:?}")))?;
    Ok((status, body.to_owned()))
}

/// An open stream: the connection, held until dropped, after its first
/// event arrived.
pub struct OpenStream {
    stream: TcpStream,
}

impl OpenStream {
    /// Open [`EVENTS_PATH`] on `addr` and wait for its first event.
    pub fn open(addr: &str) -> Self {
        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        write!(
            stream,
            "GET {EVENTS_PATH} HTTP/1.1\r\nHost: {addr}\r\nAccept: text/event-stream\r\n\r\n"
        )
        .unwrap();
        let mut seen = String::new();
        let mut buf = [0u8; 1024];
        while !seen.contains("event: hello") {
            let n = stream.read(&mut buf).unwrap();
            assert!(n > 0, "the stream closed before its first event: {seen}");
            seen.push_str(&String::from_utf8_lossy(&buf[..n]));
        }
        assert!(seen.starts_with("HTTP/1.1 200"), "{seen}");
        Self { stream }
    }

    /// Read until the server closes the stream, as a client that heard the
    /// shutdown event does.
    pub fn read_to_close(mut self) {
        let mut sink = Vec::new();
        let _ = self.stream.read_to_end(&mut sink);
    }
}

/// `count` denials at `addr`, released together, on their own threads; the
/// handles answer each status.
pub fn deny_in_flight(addr: &str, count: usize) -> Vec<JoinHandle<Option<u16>>> {
    let barrier = Arc::new(Barrier::new(count + 1));
    let handles = (0..count)
        .map(|_| {
            let addr = addr.to_owned();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                http_get(&addr, DENY_PATH).ok().map(|(status, _)| status)
            })
        })
        .collect();
    barrier.wait();
    handles
}

/// A request to [`SLOW_PATH`] on its own thread, sent before this returns.
pub fn slow_in_flight(addr: &str) -> JoinHandle<Option<u16>> {
    let mut stream = TcpStream::connect(addr).unwrap();
    write!(
        stream,
        "GET {SLOW_PATH} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    std::thread::spawn(move || {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
        let mut raw = Vec::new();
        let _ = stream.read_to_end(&mut raw);
        String::from_utf8_lossy(&raw)
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
    })
}

/// `count` denials at `addr`, released together; each answer's status and
/// the decision id its body names.
pub fn deny_batch(addr: &str, count: usize) -> Vec<(Option<u16>, Option<String>)> {
    let barrier = Arc::new(Barrier::new(count));
    let handles: Vec<JoinHandle<(Option<u16>, Option<String>)>> = (0..count)
        .map(|_| {
            let addr = addr.to_owned();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                match http_get(&addr, DENY_PATH) {
                    Ok((status, body)) => (Some(status), decision_of(&body)),
                    Err(_) => (None, None),
                }
            })
        })
        .collect();
    handles.into_iter().map(|h| h.join().unwrap()).collect()
}

/// The decision id an error body names.
pub fn decision_of(body: &str) -> Option<String> {
    let start = body.find('{')?;
    let json: serde_json::Value = serde_json::from_str(body[start..].trim()).ok()?;
    json.get("decision")?.as_str().map(str::to_owned)
}

/// Tally statuses.
pub fn tally(answers: &[Option<u16>]) -> BTreeMap<Option<u16>, usize> {
    let mut tally = BTreeMap::new();
    for status in answers {
        *tally.entry(*status).or_default() += 1;
    }
    tally
}

/// Every phase the record holds, by name, in milliseconds.
pub fn phases(record: &rahi_ops::stop::StopRecord) -> BTreeMap<String, u64> {
    record
        .phases
        .iter()
        .map(|p| (p.phase.clone(), p.millis))
        .collect()
}
