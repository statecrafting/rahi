//! An in-process OIDC stub, a real store, and the cell built over both
//! (spec 022 FR-001).
//!
//! The stub is a real axum router on a real loopback port that answers the
//! five endpoints this chassis calls: discovery, JWKS, token, userinfo, and
//! revocation. It signs id tokens with the RSA key under `testdata/oidc/` and
//! publishes that key's modulus and exponent as a JWK, so the verification
//! path under test is the real one: a real RS256 signature checked against a
//! real key set fetched over a real socket.
//!
//! Everything the tests need to steer is on [`Stub`] behind a mutex: which
//! roles userinfo reports, whether the next refresh is refused, and how many
//! times each endpoint has been called. Counting the calls is what lets
//! FR-001's "exactly one refresh" be an assertion rather than a hope.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    dead_code
)]

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use rahi_idp::{Discovery, IdpConfig, Jwks, SessionKey, Sessions};
use rahi_kernel::{Kernel, KernelOptions, Manifest};
use rahi_ledger::{Ledger, LedgerSigner};
use rahi_store::{EncKey, EncKeys, Store, StoreConfig, StoreSecrets};
use rahi_types::Config;
use ring::rand::SystemRandom;
use ring::signature::{RSA_PKCS1_SHA256, RsaKeyPair};
use serde_json::{Value, json};
use tokio::task::JoinHandle;
use tower::ServiceExt as _;

/// The throwaway RSA key the stub signs with, PKCS#8 in a PEM wrapper.
const SIGNING_KEY_PEM: &str = include_str!("../../testdata/oidc/signing-key.pkcs8.pem");
/// The published key set: that key's modulus and exponent as a JWK.
const JWKS: &str = include_str!("../../testdata/oidc/jwks.json");
/// The fixture cell's manifest, for the kernel the role gate ledgers into.
const MANIFEST: &str = include_str!("../../testdata/oidc/manifest.toml");

/// The key id both the JWKS and every id token name.
pub const KID: &str = "rahi-test-1";
/// The app's client id, which is also the id token's audience.
pub const CLIENT_ID: &str = "hello-cell";
/// The subject every fixture principal carries.
pub const SUB: &str = "rauthy-subject-1";
/// The moment the injected clock starts at.
pub const T0: u64 = 1_767_225_600;

/// What the stub will do next, and what it has been asked so far.
#[derive(Clone, Debug)]
pub struct Stub {
    /// The roles userinfo reports. Change it to remove a role at the IdP.
    pub roles: Vec<String>,
    /// The email userinfo reports, if any.
    pub email: Option<String>,
    /// `email_verified`, or `None` to omit the claim entirely (FR-003).
    pub email_verified: Option<bool>,
    /// The one authorization code the token endpoint accepts.
    pub code: String,
    /// The refresh token currently outstanding.
    pub refresh_token: String,
    /// The access token handed out with it.
    pub access_token: String,
    /// The nonce the next id token carries. The test reads it off the login
    /// redirect, which is where a browser would have carried it.
    pub nonce: Option<String>,
    /// When set, the next refresh grant is refused with 400.
    pub refuse_refresh: bool,
    /// How long the access token lives, in seconds.
    pub expires_in: u64,
    /// Code exchanges so far.
    pub code_exchanges: usize,
    /// Refresh grants so far, refused ones included.
    pub refreshes: usize,
    /// Userinfo reads so far.
    pub userinfo_reads: usize,
    /// Revocations so far.
    pub revocations: usize,
}

impl Default for Stub {
    fn default() -> Self {
        Self {
            roles: vec!["reader".to_owned()],
            email: Some("someone@example.com".to_owned()),
            email_verified: Some(true),
            code: "the-one-code".to_owned(),
            refresh_token: "refresh-0".to_owned(),
            access_token: "access-0".to_owned(),
            nonce: None,
            refuse_refresh: false,
            expires_in: 900,
            code_exchanges: 0,
            refreshes: 0,
            userinfo_reads: 0,
            revocations: 0,
        }
    }
}

/// The stub's shared state.
pub type Shared = Arc<Mutex<Stub>>;

/// A whole cell under test: the stub, the store, and the session context.
pub struct Cell {
    /// What the stub will do next.
    pub stub: Shared,
    /// The session context the router is built over.
    pub sessions: Sessions,
    /// The kernel a role gate ledgers into.
    pub kernel: Kernel,
    /// The injected clock, in unix seconds. Move it to expire an assertion.
    pub clock: Arc<AtomicU64>,
    node: Store,
    dir: tempfile::TempDir,
    stub_task: JoinHandle<()>,
}

impl Cell {
    /// Read the stub's state.
    pub fn stub(&self) -> Stub {
        self.stub.lock().expect("the stub is not poisoned").clone()
    }

    /// Change the stub's state.
    pub fn set(&self, change: impl FnOnce(&mut Stub)) {
        change(&mut self.stub.lock().expect("the stub is not poisoned"));
    }

    /// Move the injected clock forward by `seconds`.
    pub fn advance(&self, seconds: u64) {
        self.clock.fetch_add(seconds, Ordering::Relaxed);
    }
}

impl Drop for Cell {
    fn drop(&mut self) {
        self.stub_task.abort();
    }
}

/// Boot the stub, a single-voter store, and the session context over both.
///
/// The cell's public URL *is* the stub's address, so the endpoints rauthy
/// publishes and the loopback base the cell dials are the same host. That is
/// what a single deployment unit looks like from inside, and it means the
/// back-channel rewrite is exercised as a no-op rather than skipped.
pub async fn boot() -> Cell {
    let stub: Shared = Arc::new(Mutex::new(Stub::default()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a free port");
    let addr = listener.local_addr().expect("its address");
    let app = stub_router(Arc::clone(&stub), addr);
    let stub_task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let config = config(addr);
    let idp = IdpConfig::derive(&config, CLIENT_ID).expect("the fixture configuration derives");
    let discovery = Discovery::fetch_within(&idp, Duration::from_secs(5))
        .await
        .expect("the stub's discovery document arrives");
    let jwks = Jwks::load(&discovery)
        .await
        .expect("the stub's key set loads");

    let dir = tempfile::tempdir().expect("a temp dir");
    let node = Store::open(&store_config(&dir.path().join("hiqlite")))
        .await
        .expect("a single-voter node opens");
    let store = node.handle();

    let manifest = Manifest::parse(MANIFEST).expect("the fixture manifest parses");
    let ledger = Ledger::open(
        store.clone(),
        LedgerSigner::from_seed([11u8; 32]),
        manifest.hash().expect("the manifest hashes"),
    )
    .await
    .expect("the chain opens and verifies");
    let kernel = Kernel::boot_with(
        manifest,
        store.clone(),
        ledger,
        KernelOptions {
            queue_capacity: 16,
            clock: Some(Arc::new(|| T0)),
        },
    )
    .await
    .expect("the kernel boots against its own manifest");

    let clock = Arc::new(AtomicU64::new(T0));
    let ticking = Arc::clone(&clock);
    let sessions = Sessions::new(
        &idp,
        &config,
        discovery,
        jwks,
        store,
        SessionKey::from_bytes(&[13u8; 32]).expect("a key"),
        "the-client-secret".to_owned(),
    )
    .expect("the session context builds")
    .with_clock(Arc::new(move || ticking.load(Ordering::Relaxed)));

    Cell {
        stub,
        sessions,
        kernel,
        clock,
        node,
        dir,
        stub_task,
    }
}

/// The cell's configuration: one origin, and it is the stub's.
pub fn config(addr: SocketAddr) -> Config {
    let public = format!("http://{addr}");
    let rauthy = addr.to_string();
    let env = BTreeMap::from([
        ("RAHI_PUBLIC_URL", public.as_str()),
        ("RAHI_RAUTHY_ADDR", rauthy.as_str()),
    ]);
    Config::from_env(&env).expect("the fixture environment is well formed")
}

fn store_config(data_dir: &std::path::Path) -> StoreConfig {
    StoreConfig {
        node_id: 1,
        nodes: Vec::new(),
        data_dir: data_dir.to_path_buf(),
        raft_addr: free_addr(),
        api_addr: free_addr(),
        secrets: StoreSecrets {
            secret_raft: "raft-secret-for-tests-0000".to_owned(),
            secret_api: "api-secret-for-tests-00000".to_owned(),
            enc_keys: EncKeys {
                active: "test".to_owned(),
                keys: vec![EncKey {
                    id: "test".to_owned(),
                    key: vec![7u8; 32],
                }],
            },
        },
        backup_keep_days: 1,
        s3: None,
    }
}

fn free_addr() -> SocketAddr {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("a free port")
        .local_addr()
        .expect("its address")
}

// ---------------------------------------------------------------- the stub

#[derive(Clone)]
struct StubState {
    stub: Shared,
    issuer: String,
    base: String,
}

fn stub_router(stub: Shared, addr: SocketAddr) -> Router {
    let base = format!("http://{addr}");
    let state = StubState {
        stub,
        issuer: format!("{base}/auth/v1"),
        base,
    };
    Router::new()
        .route(
            "/auth/v1/.well-known/openid-configuration",
            get(discovery_document),
        )
        .route("/auth/v1/oidc/certs", get(certs))
        .route("/auth/v1/oidc/token", post(token))
        .route("/auth/v1/oidc/userinfo", get(userinfo))
        .route("/auth/v1/oidc/revoke", post(revoke))
        .with_state(state)
}

async fn discovery_document(State(state): State<StubState>) -> Json<Value> {
    let base = &state.base;
    Json(json!({
        "issuer": state.issuer,
        "authorization_endpoint": format!("{base}/auth/v1/oidc/authorize"),
        "token_endpoint": format!("{base}/auth/v1/oidc/token"),
        "userinfo_endpoint": format!("{base}/auth/v1/oidc/userinfo"),
        "end_session_endpoint": format!("{base}/auth/v1/oidc/logout"),
        "revocation_endpoint": format!("{base}/auth/v1/oidc/revoke"),
        "jwks_uri": format!("{base}/auth/v1/oidc/certs"),
    }))
}

async fn certs() -> Response {
    (
        [(header::CONTENT_TYPE, "application/json")],
        JWKS.to_owned(),
    )
        .into_response()
}

async fn token(State(state): State<StubState>, body: String) -> Response {
    let form = form(&body);
    let mut stub = state.stub.lock().expect("the stub is not poisoned");

    match form.get("grant_type").map(String::as_str) {
        Some("authorization_code") => {
            stub.code_exchanges += 1;
            if form.get("code") != Some(&stub.code) {
                return refusal("invalid_grant", "that is not the outstanding code");
            }
            if form.get("code_verifier").is_none_or(String::is_empty) {
                return refusal("invalid_request", "PKCE is required");
            }
            if form.get("redirect_uri").is_none_or(String::is_empty) {
                return refusal("invalid_request", "the redirect uri is required");
            }
            stub.code = "spent".to_owned();
            stub.refresh_token = "refresh-1".to_owned();
            stub.access_token = "access-1".to_owned();
            let id_token = sign_id_token(&state.issuer, &stub);
            grant(&stub, Some(id_token))
        }
        Some("refresh_token") => {
            stub.refreshes += 1;
            if stub.refuse_refresh {
                return refusal("invalid_grant", "this refresh token is no longer accepted");
            }
            if form.get("refresh_token") != Some(&stub.refresh_token) {
                return refusal("invalid_grant", "that is not the outstanding refresh token");
            }
            let next = stub.refreshes;
            stub.refresh_token = format!("refresh-{}", next + 1);
            stub.access_token = format!("access-{}", next + 1);
            grant(&stub, None)
        }
        _ => refusal("unsupported_grant_type", "the stub knows two grants"),
    }
}

async fn userinfo(State(state): State<StubState>, request: Request<Body>) -> Response {
    let presented = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or_default()
        .to_owned();
    let mut stub = state.stub.lock().expect("the stub is not poisoned");
    stub.userinfo_reads += 1;
    if presented != stub.access_token {
        return (StatusCode::UNAUTHORIZED, "not the outstanding access token").into_response();
    }
    Json(claims(&stub)).into_response()
}

async fn revoke(State(state): State<StubState>, _body: String) -> StatusCode {
    state
        .stub
        .lock()
        .expect("the stub is not poisoned")
        .revocations += 1;
    StatusCode::OK
}

/// The claims both the id token and userinfo carry.
fn claims(stub: &Stub) -> Value {
    let mut claims = json!({ "sub": SUB, "roles": stub.roles });
    if let Some(email) = &stub.email {
        claims["email"] = json!(email);
    }
    // `None` omits the claim entirely, which is FR-003's whole point.
    if let Some(verified) = stub.email_verified {
        claims["email_verified"] = json!(verified);
    }
    claims
}

fn grant(stub: &Stub, id_token: Option<String>) -> Response {
    let mut body = json!({
        "access_token": stub.access_token,
        "refresh_token": stub.refresh_token,
        "token_type": "Bearer",
        "expires_in": stub.expires_in,
    });
    if let Some(id_token) = id_token {
        body["id_token"] = json!(id_token);
    }
    Json(body).into_response()
}

fn refusal(error: &str, detail: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": error, "error_description": detail })),
    )
        .into_response()
}

/// Parse an `application/x-www-form-urlencoded` body.
fn form(body: &str) -> BTreeMap<String, String> {
    url::form_urlencoded::parse(body.as_bytes())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

/// Sign an RS256 id token with the fixture key.
pub fn sign_id_token(issuer: &str, stub: &Stub) -> String {
    let mut payload = claims(stub);
    payload["iss"] = json!(issuer);
    payload["aud"] = json!(CLIENT_ID);
    payload["exp"] = json!(T0 + 3600);
    payload["iat"] = json!(T0);
    if let Some(nonce) = &stub.nonce {
        payload["nonce"] = json!(nonce);
    }
    sign(
        &json!({ "alg": "RS256", "typ": "JWT", "kid": KID }),
        &payload,
    )
}

/// Sign an arbitrary header and payload, for the tests that forge one.
pub fn sign(header: &Value, payload: &Value) -> String {
    let signing_input = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(header).expect("the header serialises")),
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(payload).expect("the payload serialises")),
    );
    let key = signing_key();
    let mut signature = vec![0u8; key.public().modulus_len()];
    key.sign(
        &RSA_PKCS1_SHA256,
        &SystemRandom::new(),
        signing_input.as_bytes(),
        &mut signature,
    )
    .expect("the fixture key signs");
    format!("{signing_input}.{}", URL_SAFE_NO_PAD.encode(&signature))
}

fn signing_key() -> RsaKeyPair {
    let der: String = SIGNING_KEY_PEM
        .lines()
        .filter(|line| !line.starts_with("-----"))
        .collect();
    let der = STANDARD.decode(der).expect("the fixture key is base64");
    RsaKeyPair::from_pkcs8(&der).expect("the fixture key is PKCS#8")
}

// -------------------------------------------------------------- the client

/// A whole response, read into memory.
pub struct Answer {
    /// The status.
    pub status: StatusCode,
    /// Every header, `Set-Cookie` included.
    pub headers: axum::http::HeaderMap,
    /// The body as text.
    pub body: String,
}

impl Answer {
    /// The body as JSON.
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.body).expect("the body is JSON")
    }

    /// The `Location` header.
    pub fn location(&self) -> &str {
        self.headers
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .expect("the answer redirects")
    }

    /// Every `Set-Cookie` value.
    pub fn set_cookies(&self) -> Vec<&str> {
        self.headers
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .collect()
    }

    /// The value the answer set `name` to, if it set it.
    pub fn cookie(&self, name: &str) -> Option<String> {
        self.set_cookies().into_iter().find_map(|cookie| {
            let (pair, _) = cookie.split_once(';')?;
            let (key, value) = pair.split_once('=')?;
            (key == name).then(|| value.to_owned())
        })
    }

    /// Whether the answer cleared `name`.
    pub fn cleared(&self, name: &str) -> bool {
        self.set_cookies()
            .iter()
            .any(|cookie| cookie.starts_with(&format!("{name}=;")) && cookie.contains("Max-Age=0"))
    }
}

/// Drive one request through `router` and read the whole answer.
pub async fn send(router: &Router, request: Request<Body>) -> Answer {
    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("the router is infallible");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("the body is readable");
    Answer {
        status,
        headers,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

/// A GET with an optional `Cookie` header.
pub fn get_with(uri: &str, cookies: &[String]) -> Request<Body> {
    request("GET", uri, cookies)
}

/// A request with an optional `Cookie` header.
pub fn request(method: &str, uri: &str, cookies: &[String]) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if !cookies.is_empty() {
        builder = builder.header(header::COOKIE, cookies.join("; "));
    }
    builder.body(Body::empty()).expect("a request")
}

/// The value of one query parameter of `url`.
pub fn query_param(url: &str, name: &str) -> Option<String> {
    url::Url::parse(url)
        .ok()?
        .query_pairs()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.into_owned())
}

// ------------------------------------------------------------ the cell's app

/// The router both test binaries drive: the session routes, one authenticated
/// route, and one behind a role gate.
///
/// The session layer is outside the role gate, which is the only order that
/// works: the gate reads the principal the layer resolved.
pub fn app(cell: &Cell) -> Router {
    let guarded = Router::new()
        .route("/me", get(whoami))
        .merge(rahi_idp::with_role(
            rahi_idp::RequireRole::new(rahi_types::Role::new("rahi-operator"), cell.kernel.clone()),
            Router::new().route("/ops", get(|| async { "operator" })),
        ));
    Router::new()
        .nest(
            rahi_idp::SESSION_PREFIX,
            rahi_idp::session_router(cell.sessions.clone()),
        )
        .nest(
            "/api",
            rahi_idp::with_sessions(cell.sessions.clone(), guarded),
        )
}

/// What an authenticated route answers: the principal, as the IdP describes it
/// on this request.
async fn whoami(rahi_idp::Authenticated(principal): rahi_idp::Authenticated) -> Json<Value> {
    Json(json!({
        "sub": principal.sub.as_str(),
        "email": principal.verified_email().map(rahi_types::Email::as_str),
        "email_verified": principal.email_verified,
        "roles": principal.roles.iter().map(rahi_types::Role::as_str).collect::<Vec<_>>(),
    }))
}

/// Drive a whole login and return the session cookie it established.
pub async fn login(router: &Router, cell: &Cell) -> String {
    let started = send(router, get_with("/session/login", &[])).await;
    assert_eq!(started.status, StatusCode::SEE_OTHER, "{}", started.body);
    let authorize = started.location().to_owned();
    let state = query_param(&authorize, "state").expect("the redirect carries state");
    let nonce = query_param(&authorize, "nonce").expect("the redirect carries a nonce");
    let login_cookie = started.cookie("login").expect("the login cookie is set");

    let code = {
        let mut stub = cell.stub.lock().expect("the stub is not poisoned");
        stub.nonce = Some(nonce);
        stub.code.clone()
    };
    let established = send(
        router,
        get_with(
            &format!("/session/callback?code={code}&state={state}"),
            &[format!("login={login_cookie}")],
        ),
    )
    .await;
    assert_eq!(
        established.status,
        StatusCode::SEE_OTHER,
        "{}",
        established.body
    );
    established
        .cookie("session")
        .expect("the session cookie is set")
}
