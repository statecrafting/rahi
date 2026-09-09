//! One deployment in a temp directory: a key set, a single-voter node, and
//! a stub rauthy that answers the two backup routes (spec 030 FR-001).

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing
)]

use std::collections::BTreeMap;
use std::net::{SocketAddr, TcpListener};
use std::path::Path;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{Path as AxumPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use base64::Engine as _;
use rahi_kernel::Manifest;
use rahi_ledger::{Hash, Ledger};
use rahi_ops::{KeySet, rauthy_api::RauthyApi};
use rahi_store::{EncKey, EncKeys, Store, StoreConfig, StoreSecrets};
use rahi_types::Config;
use tokio::task::JoinHandle;

/// The fixture manifest every ledger here is rooted at.
pub const MANIFEST: &str = r#"
[app]
name = "ops-fixture"
org = "statecrafting"

[services.notes]
capabilities = []

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

/// The admin token the stub accepts.
pub const ADMIN_TOKEN: &str = "test-admin-token";

pub fn free_addr() -> SocketAddr {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}

pub fn manifest_hash() -> Hash {
    Manifest::parse(MANIFEST).unwrap().hash().unwrap()
}

/// Write a complete key set under `dir`, and return it.
pub fn write_keys(dir: &Path) -> KeySet {
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
    keys.write(
        rahi_ops::BACKUP_KEY_FILE,
        rahi_ops::generate_backup_identity().as_bytes(),
    )
    .unwrap();
    keys.write(rahi_ops::ADMIN_TOKEN_FILE, ADMIN_TOKEN.as_bytes())
        .unwrap();
    keys
}

/// A config whose volume is `data_dir` and whose rauthy is at `rauthy`.
pub fn config(data_dir: &Path, rauthy: SocketAddr) -> Config {
    let data = data_dir.display().to_string();
    let api = free_addr().to_string();
    let raft = free_addr().to_string();
    let rauthy = rauthy.to_string();
    let env = BTreeMap::from([
        ("RAHI_PUBLIC_URL", "http://localhost:8080"),
        ("RAHI_DATA_DIR", data.as_str()),
        ("RAHI_HIQLITE_API_ADDR", api.as_str()),
        ("RAHI_HIQLITE_RAFT_ADDR", raft.as_str()),
        ("RAHI_RAUTHY_ADDR", rauthy.as_str()),
    ]);
    Config::from_env(&env).unwrap()
}

/// Open the app node of `config` with the secrets in `keys`.
pub async fn open_store(config: &Config, keys: &KeySet) -> Store {
    let cfg = StoreConfig::from_config(config, keys.store_secrets().unwrap());
    Store::open(&cfg).await.expect("single-voter node opens")
}

/// Open the chain over `store` with the signer in `keys`.
pub async fn open_ledger(store: &Store, keys: &KeySet) -> Ledger {
    Ledger::open(
        store.handle(),
        keys.ledger_signer().unwrap(),
        manifest_hash(),
    )
    .await
    .expect("the chain opens")
}

/// What the stub was asked.
#[derive(Default)]
pub struct StubLog {
    pub triggers: usize,
    pub fetched: Vec<String>,
}

#[derive(Clone)]
struct StubState {
    log: Arc<Mutex<StubLog>>,
    body: Arc<Vec<u8>>,
    refuse: bool,
}

/// A stub rauthy: `POST /auth/v1/backup` records a trigger and makes a new
/// file appear, `GET /auth/v1/backup` lists it, and
/// `GET /auth/v1/backup/local/{name}` serves the bytes.
pub struct Stub {
    pub addr: SocketAddr,
    pub log: Arc<Mutex<StubLog>>,
    handle: JoinHandle<()>,
}

impl Drop for Stub {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

impl Stub {
    pub fn api(&self) -> RauthyApi {
        RauthyApi::new(format!("http://{}", self.addr), ADMIN_TOKEN).unwrap()
    }
}

/// Serve a stub whose backup body is `body`; `refuse` makes it answer 401.
pub async fn stub(body: &[u8], refuse: bool) -> Stub {
    let log = Arc::new(Mutex::new(StubLog::default()));
    let state = StubState {
        log: log.clone(),
        body: Arc::new(body.to_vec()),
        refuse,
    };
    let app = Router::new()
        .route("/auth/v1/health", get(|| async { StatusCode::OK }))
        .route("/auth/v1/backup", get(list).post(trigger))
        .route("/auth/v1/backup/local/{name}", get(fetch))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Stub { addr, log, handle }
}

fn authorised(state: &StubState, headers: &HeaderMap) -> bool {
    !state.refuse
        && headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v == format!("API-Key {ADMIN_TOKEN}"))
}

async fn trigger(State(state): State<StubState>, headers: HeaderMap) -> impl IntoResponse {
    if !authorised(&state, &headers) {
        return StatusCode::UNAUTHORIZED;
    }
    state.log.lock().unwrap().triggers += 1;
    StatusCode::OK
}

async fn list(State(state): State<StubState>, headers: HeaderMap) -> impl IntoResponse {
    if !authorised(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, String::new());
    }
    let n = state.log.lock().unwrap().triggers;
    let local: Vec<serde_json::Value> = (1..=n)
        .map(|i| {
            serde_json::json!({
                "name": format!("backup_node_1_{i}.sqlite"),
                "last_modified": 1_700_000_000 + i as i64,
                "size": state.body.len(),
            })
        })
        .collect();
    (
        StatusCode::OK,
        serde_json::json!({ "local": local, "s3": [] }).to_string(),
    )
}

async fn fetch(
    State(state): State<StubState>,
    headers: HeaderMap,
    AxumPath(name): AxumPath<String>,
) -> impl IntoResponse {
    if !authorised(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, Vec::new());
    }
    state.log.lock().unwrap().fetched.push(name);
    (StatusCode::OK, state.body.as_ref().clone())
}
