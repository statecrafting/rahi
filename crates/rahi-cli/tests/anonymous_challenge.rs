//! An anonymous unsafe request on a declared bearer route meets the
//! discovery challenge, not the CSRF refusal (spec 051 FR-004, B-1 to B-3).
//!
//! The router is composed by `serve`'s own two seams: the edge takes
//! `serve::csrf_exemption`, and `serve::with_identity_layers` puts the
//! session layer outside the edge and the bearer layer outside both. So the
//! order the three layers meet a request in is the order a running cell
//! uses, not a copy of it.
//!
//! The IdP is a loopback stub that answers the two documents the identity
//! layers load at construction: discovery and an empty key set. None of the
//! three requests here presents a token for validation, so no key is ever
//! looked up; what is under test is which layer answers, and with what.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Request, StatusCode, header};
use axum::routing::{get, post};
use rahi_cli::serve::{csrf_exemption, with_identity_layers};
use rahi_edge::{AppState, Edge};
use rahi_idp::{
    Authenticated, BearerRoutes, Discovery, Envelope, IdpClaims, IdpConfig, Jwks, METADATA_PATH,
    RequireBearer, Resource, ResourceServer, SessionId, SessionKey, Sessions, seal,
};
use rahi_kernel::{Kernel, KernelOptions, Manifest};
use rahi_ledger::{Ledger, LedgerSigner};
use rahi_store::{EncKey, EncKeys, Store, StoreConfig, StoreSecrets};
use rahi_types::{Config, Sub};
use serde_json::{Value, json};
use tower::ServiceExt as _;

/// The app's client id at the stub.
const CLIENT_ID: &str = "hello-cell";
/// The subtree the bearer credential is declared on.
const API_PREFIX: &str = "/api";
/// An authenticated route on it, reached by `POST`: the shape of an MCP
/// client's first `initialize` request.
const ENDPOINT: &str = "/api/mcp";
/// The session cookie's name over plain `http`, which the stub origin is.
const SESSION_COOKIE: &str = "session";
/// The smallest manifest the kernel boots against.
const MANIFEST: &str = r#"
schema_version = "1.0.0"

[app]
name = "hello-cell"
org = "statecrafting"

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

/// FR-004: an anonymous `POST` is challenged; a session cookie without a
/// pair is refused `csrf`; a session cookie with a token is refused `400`.
#[tokio::test]
async fn an_anonymous_write_on_a_bearer_route_is_challenged_not_refused_csrf() {
    let cell = boot().await;

    // B-2: no credential at all.
    let anonymous = send(&cell.router, post_request(ENDPOINT, &[])).await;
    assert_eq!(
        anonymous.status,
        StatusCode::UNAUTHORIZED,
        "an anonymous write reaches the authentication layers: {}",
        anonymous.body
    );
    assert_ne!(anonymous.json().get("error"), Some(&json!("csrf")));
    let challenge = anonymous
        .headers
        .get(header::WWW_AUTHENTICATE)
        .and_then(|value| value.to_str().ok())
        .expect("the 401 carries the discovery challenge");
    let metadata = format!("{}{METADATA_PATH}", cell.origin);
    assert!(
        challenge.starts_with("Bearer ")
            && challenge.contains(&format!("resource_metadata=\"{metadata}\"")),
        "the challenge names this cell's protected resource metadata: {challenge}"
    );
    // B-4: an exempt request is minted no CSRF cookie.
    assert!(
        anonymous.headers.get(header::SET_COOKIE).is_none(),
        "{:?}",
        anonymous.headers.get_all(header::SET_COOKIE)
    );

    // B-3: a live session cookie is an ambient credential, so the session
    // layer admits it and the edge demands the pair.
    let cookie = format!("{SESSION_COOKIE}={}", cell.live_session().await);
    let cookied = send(
        &cell.router,
        post_request(ENDPOINT, &[(header::COOKIE, &cookie)]),
    )
    .await;
    assert_eq!(
        cookied.status,
        StatusCode::FORBIDDEN,
        "a session cookie without a pair is still checked: {}",
        cookied.body
    );
    assert_eq!(cookied.json()["error"], json!("csrf"));

    // B-3, 025 B-10: both credentials are refused before the check.
    let both = send(
        &cell.router,
        post_request(
            ENDPOINT,
            &[
                (header::COOKIE, &cookie),
                (header::AUTHORIZATION, "Bearer a.b.c"),
            ],
        ),
    )
    .await;
    assert_eq!(both.status, StatusCode::BAD_REQUEST, "{}", both.body);
    assert_eq!(both.json()["error"], json!("validation"));

    cell.stop().await;
}

// ------------------------------------------------------------- the fixture

struct Cell {
    router: Router,
    sessions: Sessions,
    key: SessionKey,
    origin: String,
    node: Store,
    _dir: tempfile::TempDir,
    stub: tokio::task::JoinHandle<()>,
}

impl Cell {
    /// A sealed envelope over a minted assertion: the cookie a login leaves.
    async fn live_session(&self) -> String {
        let sid = SessionId::fresh().expect("a session id");
        let sub = Sub::new("rauthy-subject-1");
        // An email, as rauthy sends for a person. A principal without one
        // does not survive the cache's round trip today (`Principal.email`
        // is skipped when absent, which a positional codec cannot read
        // back); that is outside this spec and reported separately.
        let claims = IdpClaims {
            sub: Some(sub.as_str().to_owned()),
            email: Some("person@example.com".to_owned()),
            email_verified: Some(true),
            ..IdpClaims::default()
        };
        self.sessions
            .mint(&sid, sub.clone(), &claims, Some(3600))
            .await
            .expect("the assertion is stored");
        let envelope = Envelope::new(sid, sub, "a-refresh-token".to_owned(), self.sessions.now());
        seal(&envelope, &self.key).expect("the envelope seals")
    }

    async fn stop(self) {
        self.node.shutdown().await.expect("a clean shutdown");
        self.stub.abort();
    }
}

/// The stub, a single-voter store, the kernel, and the router `serve` builds.
async fn boot() -> Cell {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a free port");
    let addr = listener.local_addr().expect("its address");
    let stub = tokio::spawn(async move {
        let _ = axum::serve(listener, stub_router(addr)).await;
    });

    let origin = format!("http://{addr}");
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
        LedgerSigner::from_seed([17u8; 32]),
        manifest.hash().expect("the manifest hashes"),
    )
    .await
    .expect("the chain opens and verifies");
    let kernel = Kernel::boot_with(
        manifest,
        store.clone(),
        ledger.clone(),
        KernelOptions {
            queue_capacity: 16,
            ..KernelOptions::default()
        },
    )
    .await
    .expect("the kernel boots against its own manifest");

    let key = SessionKey::from_bytes(&[19u8; 32]).expect("a key");
    let sessions = Sessions::new(
        &idp,
        &config,
        discovery,
        jwks.clone(),
        store.clone(),
        key.clone(),
        "the-client-secret".to_owned(),
    )
    .expect("the session context builds");
    let resource = Resource::derive(&idp).expect("the resource derives");
    let server = ResourceServer::new(&config, resource, jwks, store.clone(), kernel.clone());
    let bearer = RequireBearer::new(server, BearerRoutes::new().route(API_PREFIX));

    let app = Router::new().route(ENDPOINT, post(whoami).get(whoami));
    let state = AppState::new(kernel, store, ledger, config.clone());
    let edge = Edge::builder(state)
        .mount("/", app)
        .csrf_exemption(csrf_exemption(config.cookie_scheme))
        .build();
    let router = with_identity_layers(edge, Some(sessions.clone()), Some(bearer));

    Cell {
        router,
        sessions,
        key,
        origin,
        node,
        _dir: dir,
        stub,
    }
}

/// An authenticated route: 401 for a request no layer authenticated.
async fn whoami(Authenticated(principal): Authenticated) -> String {
    principal.sub.as_str().to_owned()
}

/// Discovery and an empty key set, the two documents the layers load.
fn stub_router(addr: SocketAddr) -> Router {
    let base = format!("http://{addr}");
    let issuer = format!("{base}{}", rahi_idp::ISSUER_PATH);
    let document = json!({
        "issuer": issuer,
        "authorization_endpoint": format!("{base}/auth/v1/oidc/authorize"),
        "token_endpoint": format!("{base}/auth/v1/oidc/token"),
        "userinfo_endpoint": format!("{base}/auth/v1/oidc/userinfo"),
        "end_session_endpoint": format!("{base}/auth/v1/oidc/logout"),
        "revocation_endpoint": format!("{base}/auth/v1/oidc/revoke"),
        "jwks_uri": format!("{base}/auth/v1/oidc/certs"),
    });
    Router::new()
        .route(
            "/auth/v1/.well-known/openid-configuration",
            get(move || {
                let document = document.clone();
                async move { axum::Json(document) }
            }),
        )
        .route(
            "/auth/v1/oidc/certs",
            get(|| async { axum::Json(json!({ "keys": [] })) }),
        )
}

/// The cell's configuration: one origin, and it is the stub's.
fn config(addr: SocketAddr) -> Config {
    let public = format!("http://{addr}");
    let rauthy = addr.to_string();
    let env = BTreeMap::from([
        ("RAHI_PUBLIC_URL", public.as_str()),
        ("RAHI_RAUTHY_ADDR", rauthy.as_str()),
    ]);
    Config::from_env(&env).expect("the fixture environment is well formed")
}

fn post_request(uri: &str, headers: &[(header::HeaderName, &str)]) -> Request<Body> {
    let mut request = Request::builder().method("POST").uri(uri);
    for (name, value) in headers {
        request = request.header(name, *value);
    }
    request.body(Body::empty()).expect("a well-formed request")
}

struct Answer {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

impl Answer {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }
}

async fn send(router: &Router, request: Request<Body>) -> Answer {
    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("the router is infallible");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("a readable body");
    Answer {
        status,
        headers,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
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

/// A loopback port no concurrently running test process was handed (spec
/// 022 D-10): the workspace's one allocator, in 20000..32000 from a
/// pid-derived offset, claimed by an exclusive lock under
/// `<temp>/rahi-test-ports/` and probed with a bind before it is handed out.
fn free_addr() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], loopback_port()))
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
