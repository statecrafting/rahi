//! Denials survive a graceful stop, and replicas mint ids no other replica
//! can (spec 035 FR-001 and FR-005).
//!
//! Both tests run real cells in their own processes and stop them the way an
//! orchestrator does, with SIGTERM. The cell is a fixture whose manifest
//! grants nothing its route asks for, so every request is a ledgered denial.
//! It is not a second binary of this crate: the test binary is the fixture.
//! A child started on [`FIXTURE`] with [`FIXTURE_VERB`] set composes
//! [`DenyCell`] through `rahi_cli::run_with` and exits with the verb's code,
//! so only the test build carries it.
//!
//! The fixture declares no migrations. Nothing here is about schema, and a
//! cell with none serves on a fresh volume without a `migrate` step, which
//! keeps a three-node start free of the follower's exit 2 that
//! `deploy/README.md` describes.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Barrier, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse as _, Response};
use axum::routing::get;
use base64::Engine as _;
use rahi_cli::Cell;
use rahi_edge::obs::metrics::{
    KERNEL_DECISIONS_ABANDONED, KERNEL_DECISIONS_DROPPED, KERNEL_LEDGER_FAILURES,
};
use rahi_edge::{AppState, READYZ_PATH, Route, RouteClass};
use rahi_kernel::{CapabilityKind, Governed};
use rahi_ledger::Ledger;
use rahi_ops::KeySet;
use rahi_store::{EncKey, EncKeys, Migration, StoreHandle, StoreSecrets};
use rahi_types::Sub;

/// The libtest name of the fixture's process entry.
const FIXTURE: &str = "fixture_cell";
/// Set on a child to the verb (and its arguments) the fixture runs.
const FIXTURE_VERB: &str = "RAHI_TEST_FIXTURE_VERB";

const DENY_PATH: &str = "/api/deny";
const CHAIN_PATH: &str = "/api/chain";

/// FR-001's burst.
const REQUESTS: usize = 200;
/// FR-005's denials per replica.
const PER_REPLICA: usize = 10;

const READY_BUDGET: Duration = Duration::from_secs(120);
const CLUSTER_READY_BUDGET: Duration = Duration::from_secs(180);
/// Far above serve's own stop (spec 031 D-4 and spec 035 B-1), so a stop
/// that hangs fails here rather than passing on a kill.
const STOP_BUDGET: Duration = Duration::from_secs(60);

const MANIFEST: &str = r#"# The deny cell: `items` may be read and nothing else, so the write its
# route attempts is always a ledgered denial (spec 035 FR-001, FR-005).

[app]
name = "deny-cell"
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
operator_role = "deny-cell-operator"

[contract]
version = "1.0.0"
"#;

struct DenyCell;

impl Cell for DenyCell {
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
        Router::new()
            .route(DENY_PATH, get(deny))
            .with_state(write)
            .merge(
                Router::new()
                    .route(CHAIN_PATH, get(chain))
                    .with_state(state.ledger().clone()),
            )
    }

    fn exposed() -> Vec<Route> {
        vec![
            Route::new(DENY_PATH, RouteClass::Public),
            Route::new(CHAIN_PATH, RouteClass::Public),
        ]
    }
}

/// A governed write the manifest never granted: a 403 carrying the id of
/// the decision the chain will hold.
async fn deny(State(write): State<Governed<StoreHandle>>) -> Response {
    match write
        .execute(
            &Sub::new("spec-035-caller"),
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

/// Every resident record id, read through this replica's own ledger handle.
async fn chain(State(ledger): State<Ledger>) -> Response {
    match ledger.records().await {
        Ok(records) => {
            let ids: Vec<String> = records.into_iter().map(|r| r.record.id).collect();
            (
                [(header::CONTENT_TYPE, "application/json")],
                serde_json::json!({ "ids": ids }).to_string(),
            )
                .into_response()
        }
        Err(err) => rahi_edge::error::response(&err),
    }
}

/// The fixture's process entry. Without [`FIXTURE_VERB`] it does nothing,
/// which is what it does in every ordinary run of this suite.
#[test]
fn fixture_cell() {
    let Ok(verb) = std::env::var(FIXTURE_VERB) else {
        return;
    };
    let args: Vec<String> = verb.split_whitespace().map(str::to_owned).collect();
    let code = rahi_cli::run_with::<DenyCell>(&args, &rahi_cli::process_env());
    std::process::exit(code);
}

// ------------------------------------------------------------------ nodes

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Every key a cell checks at boot. One cell's nodes share one set, as the
/// replicas of spec 032 share one Secret.
fn write_keys(data_dir: &Path, backup_identity: &str) {
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
    keys.write(rahi_ops::BACKUP_KEY_FILE, backup_identity.as_bytes())
        .unwrap();
    keys.write(rahi_ops::ADMIN_TOKEN_FILE, b"token").unwrap();
}

/// One cell of `size` nodes on loopback, each with its own volume, no rauthy.
struct Cluster {
    root: tempfile::TempDir,
    nodes: Vec<Node>,
}

#[derive(Clone)]
struct Node {
    data_dir: PathBuf,
    listen: String,
    env: Vec<(String, String)>,
}

impl Cluster {
    fn new(size: usize) -> Self {
        let root = tempfile::tempdir().unwrap();
        let backup_identity = rahi_ops::generate_backup_identity();
        let ports: Vec<(u16, u16, u16, u16)> = (0..size)
            .map(|_| (free_port(), free_port(), free_port(), free_port()))
            .collect();
        let peers = ports
            .iter()
            .enumerate()
            .map(|(i, (_, raft, api, _))| format!("{} 127.0.0.1:{raft} 127.0.0.1:{api}", i + 1))
            .collect::<Vec<_>>()
            .join(";");
        let nodes = ports
            .iter()
            .enumerate()
            .map(|(i, (listen, raft, api, rauthy))| {
                let data_dir = root.path().join(format!("node{}", i + 1));
                std::fs::create_dir_all(&data_dir).unwrap();
                write_keys(&data_dir, &backup_identity);
                let listen = format!("127.0.0.1:{listen}");
                let mut env = vec![
                    (
                        "RAHI_PUBLIC_URL".to_owned(),
                        "http://localhost:8080".to_owned(),
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
                    ("RAHI_RAUTHY_ADDR".to_owned(), format!("127.0.0.1:{rauthy}")),
                    ("RAHI_RAUTHY_MODE".to_owned(), "none".to_owned()),
                    ("RAHI_LISTEN_ADDR".to_owned(), listen.clone()),
                ];
                if size > 1 {
                    env.push(("RAHI_HIQ_NODE_ID".to_owned(), (i + 1).to_string()));
                    env.push(("RAHI_HIQ_NODES".to_owned(), peers.clone()));
                }
                Node {
                    data_dir,
                    listen,
                    env,
                }
            })
            .collect();
        Self { root, nodes }
    }
}

impl Node {
    /// This test binary, started as the fixture running `verb`.
    fn command(&self, verb: &str) -> Command {
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
    fn run(&self, verb: &str) -> Finished {
        let output = self.command(verb).output().unwrap();
        Finished {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    /// Start `verb` in the background, collecting what it prints.
    fn spawn(&self, verb: &str) -> Running {
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

    /// Wait for `/readyz` to answer 200 while `serve` is still running.
    fn wait_ready(&self, serve: &mut Running, within: Duration) {
        let deadline = Instant::now() + within;
        loop {
            if let Some(status) = serve.child.try_wait().unwrap() {
                panic!(
                    "serve exited {status} before it was ready\n{}",
                    serve.logs()
                );
            }
            if matches!(http_get(&self.listen, READYZ_PATH), Ok((200, _))) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "serve on {} was not ready within {within:?}\n{}",
                self.listen,
                serve.logs()
            );
            std::thread::sleep(Duration::from_millis(250));
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

struct Finished {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl Finished {
    fn logs(&self) -> String {
        format!(
            "exit {:?}\n--- stdout\n{}\n--- stderr\n{}",
            self.code, self.stdout, self.stderr
        )
    }
}

struct Running {
    child: Child,
    stdout: Arc<Mutex<String>>,
    stderr: Arc<Mutex<String>>,
    readers: Vec<JoinHandle<()>>,
}

impl Running {
    fn logs(&self) -> String {
        format!(
            "--- stdout\n{}\n--- stderr\n{}",
            self.stdout.lock().unwrap(),
            self.stderr.lock().unwrap()
        )
    }

    /// The graceful stop an orchestrator sends.
    fn sigterm(&self) {
        let status = Command::new("kill")
            .arg("-TERM")
            .arg(self.child.id().to_string())
            .status()
            .unwrap();
        assert!(status.success(), "kill -TERM exited {status}");
    }

    /// Wait for the process to exit on its own within `within`.
    fn wait(&mut self, within: Duration) -> Finished {
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
            std::thread::sleep(Duration::from_millis(50));
        };
        for reader in self.readers.drain(..) {
            reader.join().unwrap();
        }
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

/// One `GET` over a fresh connection, answered in full. Only a refused
/// connect is retried: a request that was sent may have been denied, and a
/// second send would mint a second decision.
fn http_get(addr: &str, path: &str) -> std::io::Result<(u16, String)> {
    let mut attempt = 0;
    let mut stream = loop {
        match TcpStream::connect(addr) {
            Ok(stream) => break stream,
            Err(err) if attempt < 20 => {
                attempt += 1;
                let _ = err;
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(err) => return Err(err),
        }
    };
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
    let chunked = head.lines().any(|l| {
        l.to_ascii_lowercase()
            .starts_with("transfer-encoding: chunked")
    });
    Ok((
        status,
        if chunked {
            dechunk(body)
        } else {
            body.to_owned()
        },
    ))
}

fn dechunk(mut body: &str) -> String {
    let mut out = String::new();
    while let Some((size, rest)) = body.split_once("\r\n") {
        let size = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
        if size == 0 || rest.len() < size {
            break;
        }
        out.push_str(&rest[..size]);
        body = rest[size..].trim_start_matches("\r\n");
    }
    out
}

/// `count` denials at `addr`, released together.
fn deny_concurrently(addr: &str, count: usize) -> Vec<(u16, Option<String>)> {
    let barrier = Arc::new(Barrier::new(count));
    let handles: Vec<JoinHandle<(u16, Option<String>)>> = (0..count)
        .map(|_| {
            let addr = addr.to_owned();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                let (status, body) = http_get(&addr, DENY_PATH).expect("the cell answers");
                (status, decision_of(&body))
            })
        })
        .collect();
    handles.into_iter().map(|h| h.join().unwrap()).collect()
}

fn decision_of(body: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(body.trim()).ok()?;
    json.get("decision")?.as_str().map(str::to_owned)
}

fn statuses(answers: &[(u16, Option<String>)]) -> BTreeMap<u16, usize> {
    let mut tally = BTreeMap::new();
    for (status, _) in answers {
        *tally.entry(*status).or_default() += 1;
    }
    tally
}

/// `kernel:<nonce>:<node>:<counter>` split into its nonce and node.
fn nonce_and_node(id: &str) -> (String, String) {
    let parts: Vec<&str> = id.split(':').collect();
    assert_eq!(parts.len(), 4, "{id} has spec 035 D-2's four segments");
    assert_eq!(parts[0], "kernel", "{id}");
    assert_eq!(parts[3].len(), 12, "{id}'s counter is twelve digits");
    (parts[1].to_owned(), parts[2].to_owned())
}

/// B-7's line for a boot on `nonce` as `node`.
fn boot_line(nonce: &str, node: &str) -> String {
    format!(
        "INFO rahi.decision: decision ids of this boot are kernel:{nonce}:{node}:<counter> \
         (nonce {nonce}, node {node})"
    )
}

// ------------------------------------------------------------------ tests

/// FR-001: 200 concurrent denials, SIGTERM the moment the last is answered,
/// and every answered id is in the chain the stopped volume holds.
#[test]
fn every_denial_answered_before_a_graceful_stop_is_in_the_chain() {
    let cell = Cluster::new(1);
    let node = &cell.nodes[0];
    let mut serve = node.spawn("serve");
    node.wait_ready(&mut serve, READY_BUDGET);

    let answers = deny_concurrently(&node.listen, REQUESTS);
    assert_eq!(
        statuses(&answers),
        BTreeMap::from([(403, REQUESTS)]),
        "every request is a denial"
    );
    let ids: Vec<String> = answers
        .iter()
        .map(|(_, id)| id.clone().expect("a 403 carries its decision id"))
        .collect();
    assert_eq!(ids.iter().collect::<BTreeSet<_>>().len(), REQUESTS);

    serve.sigterm();
    let stopped = serve.wait(STOP_BUDGET);
    assert_eq!(stopped.code, Some(0), "{}", stopped.logs());

    // B-7: the boot said which nonce and node it mints under.
    let (nonce, node_id) = nonce_and_node(&ids[0]);
    assert_eq!(node_id, "1", "a single node is node 1");
    assert!(
        ids.iter()
            .all(|id| nonce_and_node(id) == (nonce.clone(), node_id.clone())),
        "one boot, one nonce, one node: {ids:?}"
    );
    assert!(
        stopped.stdout.contains(&boot_line(&nonce, &node_id)),
        "B-7's boot line\n{}",
        stopped.logs()
    );

    let verify = node.run("ledger verify");
    assert_eq!(verify.code, Some(0), "{}", verify.logs());
    assert!(
        verify.stdout.contains(&format!(
            "ok at resident depth; {} resident record(s)",
            REQUESTS + 1
        )),
        "genesis and every denial:\n{}",
        verify.logs()
    );
    let export = cell.root.path().join("chain.jsonl");
    let exported = node.run(&format!("ledger export {}", export.display()));
    assert_eq!(exported.code, Some(0), "{}", exported.logs());
    let chain = std::fs::read_to_string(&export).unwrap();

    // D-4: no loss is unexplained; a missing id must be named by a loss
    // line with its cause, and under the default bound none is missing.
    let missing: Vec<&String> = ids
        .iter()
        .filter(|id| !chain.contains(&format!("\"{id}\"")))
        .collect();
    let unexplained: Vec<&&String> = missing
        .iter()
        .filter(|id| {
            !stopped.stderr.lines().any(|line| {
                line.starts_with("ERROR rahi.decision: decision ") && line.contains(id.as_str())
            })
        })
        .collect();
    assert!(
        unexplained.is_empty(),
        "denials absent from the chain with no counted cause: {unexplained:?}\n{}",
        stopped.logs()
    );
    assert!(
        missing.is_empty(),
        "under the default bound no answered denial is lost at a graceful stop: \
         {missing:?}\n{}",
        stopped.logs()
    );
    assert!(
        !stopped.stderr.contains("WARN rahi.decision"),
        "the drain bound did not expire\n{}",
        stopped.logs()
    );
}

/// FR-005: three processes, one cluster, ten denials at each replica at
/// once, and thirty distinct ids that all land.
#[test]
fn three_replicas_of_one_chain_mint_distinct_ids_and_lose_none() {
    let cell = Cluster::new(3);
    let mut serves: Vec<Running> = cell.nodes.iter().map(|n| n.spawn("serve")).collect();
    for (node, serve) in cell.nodes.iter().zip(serves.iter_mut()) {
        node.wait_ready(serve, CLUSTER_READY_BUDGET);
    }

    let per_replica: Vec<Vec<(u16, Option<String>)>> = std::thread::scope(|scope| {
        let handles: Vec<_> = cell
            .nodes
            .iter()
            .map(|node| scope.spawn(|| deny_concurrently(&node.listen, PER_REPLICA)))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    let mut answered: Vec<String> = Vec::new();
    for (i, answers) in per_replica.iter().enumerate() {
        assert_eq!(
            statuses(answers),
            BTreeMap::from([(403, PER_REPLICA)]),
            "replica {} denied every request",
            i + 1
        );
        for (_, id) in answers {
            let id = id.clone().expect("a 403 carries its decision id");
            assert_eq!(
                nonce_and_node(&id).1,
                (i + 1).to_string(),
                "replica {}'s ids name its node: {id}",
                i + 1
            );
            answered.push(id);
        }
    }
    let distinct: BTreeSet<String> = answered.iter().cloned().collect();
    assert_eq!(answered.len(), 3 * PER_REPLICA);
    assert_eq!(
        distinct.len(),
        3 * PER_REPLICA,
        "no two replicas minted one id: {answered:?}"
    );

    // The chain, read through replica 1, holds every one of them.
    let deadline = Instant::now() + Duration::from_secs(60);
    let resident = loop {
        let (status, body) = http_get(&cell.nodes[0].listen, CHAIN_PATH).unwrap();
        assert_eq!(status, 200, "{body}");
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        let denials: BTreeSet<String> = json["ids"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|id| id.as_str())
            .filter(|id| id.starts_with("kernel:"))
            .map(str::to_owned)
            .collect();
        if denials.len() >= distinct.len() || Instant::now() >= deadline {
            break denials;
        }
        std::thread::sleep(Duration::from_millis(250));
    };
    if resident != distinct {
        let missing: Vec<&String> = distinct.difference(&resident).collect();
        let logs: Vec<String> = serves.iter().map(Running::logs).collect();
        panic!(
            "thirty denial records, one per answered id; missing {missing:?}\n{}",
            logs.join("\n")
        );
    }

    for (i, node) in cell.nodes.iter().enumerate() {
        let (status, text) = http_get(&node.listen, "/metrics").unwrap();
        assert_eq!(status, 200);
        for family in [
            KERNEL_DECISIONS_DROPPED,
            KERNEL_DECISIONS_ABANDONED,
            KERNEL_LEDGER_FAILURES,
        ] {
            assert!(
                text.lines().any(|line| line == format!("{family} 0")),
                "replica {} reports {family} 0:\n{text}",
                i + 1
            );
        }
    }

    for serve in &serves {
        serve.sigterm();
    }
    for (i, serve) in serves.iter_mut().enumerate() {
        let stopped = serve.wait(STOP_BUDGET);
        assert_eq!(
            stopped.code,
            Some(0),
            "replica {}\n{}",
            i + 1,
            stopped.logs()
        );
        assert!(
            !stopped.stderr.contains("rahi.decision"),
            "replica {} lost nothing at its stop\n{}",
            i + 1,
            stopped.logs()
        );
        let node = (i + 1).to_string();
        let nonce = nonce_and_node(&per_replica[i][0].1.clone().unwrap()).0;
        assert!(
            stopped.stdout.contains(&boot_line(&nonce, &node)),
            "replica {node}'s boot line\n{}",
            stopped.logs()
        );
    }
}

/// The bound is read before anything opens: a value that is not seconds is
/// a configuration error, and the volume is never touched.
#[test]
fn a_denial_drain_timeout_that_is_not_seconds_is_refused_before_the_store_opens() {
    let cell = Cluster::new(1);
    let mut node = cell.nodes[0].clone();
    node.env.push((
        rahi_cli::serve::ENV_DENIAL_DRAIN_TIMEOUT.to_owned(),
        "soon".to_owned(),
    ));
    let run = node.run("serve");
    assert_eq!(run.code, Some(3), "{}", run.logs());
    assert!(
        run.stderr.contains(
            "error: config: RAHI_DENIAL_DRAIN_TIMEOUT_SECS \"soon\" is not a number of seconds"
        ),
        "{}",
        run.logs()
    );
    assert!(
        !node.data_dir.join("hiqlite").exists(),
        "the node was never opened"
    );
}
