//! One deployment in a temp directory: a key set, a single-voter node, and
//! a stub rauthy that answers the login ceremony the backup admin drives
//! (spec 037 B-1) and the three backup routes (spec 030 FR-001).

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
use axum::routing::{get, post};
use base64::Engine as _;
use rahi_kernel::Manifest;
use rahi_ledger::{Hash, Ledger};
use rahi_ops::{KeySet, rauthy_api::RauthyApi};
use rahi_store::{EncKey, EncKeys, Store, StoreConfig, StoreSecrets};
use rahi_types::Config;
use tokio::task::JoinHandle;

/// The fixture manifest every ledger here is rooted at.
pub const MANIFEST: &str = r#"
schema_version = "1.0.0"

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

/// The fixture manifest's hash as text, for spec 036 B-9's restore check.
pub fn manifest_hash_text() -> &'static str {
    static TEXT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    TEXT.get_or_init(|| manifest_hash().to_string())
}

/// The running cell a restore is checked against (spec 036 B-9): this
/// fixture, with no migrations and no `--adopt`.
pub fn compatibility() -> rahi_ops::restore::Compatibility<'static> {
    rahi_ops::restore::Compatibility {
        manifest_hash: manifest_hash_text(),
        migrations: &[],
        adopt: false,
    }
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
    // rauthy's own secrets, which first boot mints and the supervisor
    // renders rauthy's environment from.
    keys.write(
        rahi_ops::RAUTHY_SECRETS_FILE,
        serde_json::to_string(&rahi_ops::rauthy_env::RauthySecrets {
            enc_key_id: "test".to_owned(),
            enc_key: base64::engine::general_purpose::STANDARD.encode([9u8; 32]),
            secret_raft: "rauthy-raft-secret-for-tests-000".to_owned(),
            secret_api: "rauthy-api-secret-for-tests-0000".to_owned(),
            admin_email: "admin@localhost".to_owned(),
            admin_password: "admin-password-for-tests".to_owned(),
            api_key_name: "rahi".to_owned(),
            api_key_secret: "x".repeat(64),
        })
        .unwrap()
        .as_bytes(),
    )
    .unwrap();
    // Spec 037 B-1: the credential `rahi backup` presents to rauthy.
    let passkey_config = Config::from_env(&BTreeMap::from([(
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
    pub logins: usize,
    /// The epoch second of each accepted trigger, which is what the
    /// snapshot it produced is named for (spec 037 B-2).
    pub taken: Vec<i64>,
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
    /// The client the verbs use: the admin token for health, and a backup
    /// passkey for the backup routes (spec 037 B-1).
    pub fn api(&self) -> RauthyApi {
        self.api_with(Some(test_passkey()))
    }

    /// The same client with a credential of your choosing, or none.
    pub fn api_with(&self, passkey: Option<rahi_ops::rauthy_session::Passkey>) -> RauthyApi {
        RauthyApi::new(format!("http://{}", self.addr), ADMIN_TOKEN)
            .unwrap()
            .with_passkey(passkey)
    }
}

/// A credential for the stub, which does not verify assertions.
pub fn test_passkey() -> rahi_ops::rauthy_session::Passkey {
    let config = Config::from_env(&BTreeMap::from([(
        "RAHI_PUBLIC_URL",
        "http://localhost:8080",
    )]))
    .unwrap();
    rahi_ops::rauthy_session::Passkey::generate(&config).unwrap()
}

/// Serve a stub whose backup body is `body`; `refuse` makes it answer 401.
pub async fn stub(body: &[u8], refuse: bool) -> Stub {
    delayed_stub(body, refuse, "", "", 0, std::time::Duration::ZERO).await
}

/// Delay a selected HTTP call, including response-body delivery, without changing its answer.
pub async fn delayed_stub(
    body: &[u8],
    refuse: bool,
    method: &str,
    path: &str,
    occurrence: usize,
    delay: std::time::Duration,
) -> Stub {
    let log = Arc::new(Mutex::new(StubLog::default()));
    let state = StubState {
        log: log.clone(),
        body: Arc::new(body.to_vec()),
        refuse,
    };
    let app = Router::new()
        .route("/auth/v1/health", get(|| async { StatusCode::OK }))
        .route("/auth/v1/oidc/session", post(session))
        // Difficulty zero: the verb's own solver returns at once.
        .route("/auth/v1/pow", post(|| async { "1:0:0:salt:hash:" }))
        .route("/auth/v1/oidc/authorize", post(authorize))
        .route("/auth/v1/users/webauthn_start", post(webauthn_start))
        .route("/auth/v1/users/webauthn_finish", post(webauthn_finish))
        .route("/auth/v1/backup", get(list).post(trigger))
        .route("/auth/v1/backup/local/{name}", get(fetch))
        .with_state(state);
    let method = method.to_owned();
    let path = path.to_owned();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let app = app.layer(axum::middleware::from_fn(
        move |request: axum::extract::Request, next: axum::middleware::Next| {
            let selected = request.method().as_str() == method
                && (request.uri().path() == path
                    || (path.ends_with("/local/") && request.uri().path().starts_with(&path)))
                && calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == occurrence;
            async move {
                let response = next.run(request).await;
                if selected {
                    tokio::time::sleep(delay).await;
                }
                response
            }
        },
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Stub { addr, log, handle }
}

/// The stub's session cookie, which is what a backup route looks for. A
/// refusing stub answers the ceremony and then refuses the routes, which is
/// what a real rauthy does to a session that is not MFA-satisfied.
const SESSION_COOKIE: &str = "RauthySession=stub";

fn authorised(state: &StubState, headers: &HeaderMap) -> bool {
    !state.refuse
        && headers
            .get("cookie")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.contains(SESSION_COOKIE))
}

async fn session(State(state): State<StubState>) -> impl IntoResponse {
    state.log.lock().unwrap().logins += 1;
    (
        StatusCode::CREATED,
        [(
            axum::http::header::SET_COOKIE,
            format!("{SESSION_COOKIE}; Path=/"),
        )],
        serde_json::json!({ "csrf_token": "stub-csrf" }).to_string(),
    )
}

async fn authorize() -> impl IntoResponse {
    (
        StatusCode::OK,
        serde_json::json!({ "code": "a".repeat(48), "exp": 60 }).to_string(),
    )
}

async fn webauthn_start() -> impl IntoResponse {
    (
        StatusCode::OK,
        serde_json::json!({
            "code": "a".repeat(48),
            "rcr": { "publicKey": { "challenge": "c3R1Yg" } },
        })
        .to_string(),
    )
}

async fn webauthn_finish() -> impl IntoResponse {
    (StatusCode::ACCEPTED, "{}")
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

async fn trigger(State(state): State<StubState>, headers: HeaderMap) -> impl IntoResponse {
    if !authorised(&state, &headers) {
        return StatusCode::UNAUTHORIZED;
    }
    let mut log = state.log.lock().unwrap();
    log.triggers += 1;
    // hiqlite names a snapshot for the second its request was issued at,
    // and ignores a request inside the window of the last one; the stub
    // does both, because that is what the verb's freshness rule reads.
    let now = now();
    if log.taken.last().is_none_or(|last| now - last >= 60) {
        log.taken.push(now);
    }
    StatusCode::OK
}

async fn list(State(state): State<StubState>, headers: HeaderMap) -> impl IntoResponse {
    if !authorised(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, String::new());
    }
    let taken = state.log.lock().unwrap().taken.clone();
    let local: Vec<serde_json::Value> = taken
        .iter()
        .map(|ts| {
            serde_json::json!({
                "name": format!("backup_node_1_{ts}.sqlite"),
                "last_modified": ts,
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

/// Run potentially stuck integration work in a disposable test process.
/// Parent waits only to the explicit bound, then kills and reaps the child.
pub fn disposable_process(test: &str, seconds: u64) -> bool {
    if std::env::var("RAHI_DISPOSABLE_TEST").as_deref() == Ok(test) {
        return true;
    }
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env("RAHI_DISPOSABLE_TEST", test)
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "{test}: {status}");
            return false;
        }
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("{test} exceeded {seconds}s; child killed and reaped");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}
