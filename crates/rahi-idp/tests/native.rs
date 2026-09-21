//! Native clients and revocation (spec 038 FR-003, FR-004, B-3, B-5).
//!
//! Two halves, and neither is a mock of the thing it proves. The
//! provisioning half drives `provision_native_clients` against a stub admin
//! API on a real socket that records every request it was sent, so what is
//! asserted is the upsert rauthy would have received. The revocation half
//! drives the real resource server over the real store from spec 025's OIDC
//! fixture, with tokens signed by the fixture key, so a deny-listed token is
//! refused by the same `validate` a request goes through.

#![allow(clippy::expect_used, clippy::indexing_slicing, clippy::unwrap_used)]

mod oidc;

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router as AxumRouter};
use oidc::{Cell, KID, SUB, T0, sign};
use rahi_idp::native::{NativeSettings, provision_native_clients, read_lifetime};
use rahi_idp::revoke::{Revoker, deny_subject, denylist_ttl};
use rahi_idp::{IdpConfig, Resource, ResourceServer};
use rahi_kernel::{NativeClient, NativeFlow};
use rahi_types::Config;
use serde_json::{Value, json};

/// The origin the recorded claim sets were written for.
const FIXTURE_ORIGIN: &str = "https://cell.example.com";

// ------------------------------------------------------ the stub admin API

/// What the stub was asked to do, in order: method, path, and the body.
type Log = Arc<Mutex<Vec<(String, String, Value)>>>;

#[derive(Clone)]
struct Admin {
    log: Log,
    /// The clients rauthy holds, by id.
    clients: Arc<Mutex<BTreeMap<String, Value>>>,
    /// The scope catalog.
    scopes: Arc<Mutex<Vec<String>>>,
    /// Whether the API key is accepted.
    authorized: bool,
}

impl Admin {
    fn new() -> Self {
        Self {
            log: Arc::new(Mutex::new(Vec::new())),
            clients: Arc::new(Mutex::new(BTreeMap::new())),
            scopes: Arc::new(Mutex::new(
                ["openid", "profile", "email"]
                    .iter()
                    .map(|s| (*s).to_owned())
                    .collect(),
            )),
            authorized: true,
        }
    }

    fn calls(&self) -> Vec<(String, String)> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .map(|(method, path, _)| (method.clone(), path.clone()))
            .collect()
    }

    fn body_of(&self, method: &str, path: &str) -> Value {
        self.log
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|(m, p, _)| m == method && p == path)
            .map(|(_, _, body)| body.clone())
            .unwrap_or_else(|| panic!("the stub was never sent {method} {path}"))
    }

    fn client(&self, id: &str) -> Value {
        self.clients
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .unwrap_or_else(|| panic!("rauthy holds no client {id}"))
    }

    fn router(self) -> Router {
        AxumRouter::new()
            .route("/auth/v1/scopes", get(list_scopes).post(create_scope))
            .route("/auth/v1/clients", post(create_client))
            .route("/auth/v1/clients/{id}", get(read_client).put(update_client))
            .route("/auth/v1/sessions/{id}", delete(end_sessions))
            .with_state(self)
    }

    fn record(&self, method: &str, path: &str, body: Value) {
        self.log
            .lock()
            .unwrap()
            .push((method.to_owned(), path.to_owned(), body));
    }
}

async fn list_scopes(State(admin): State<Admin>) -> Result<Json<Value>, StatusCode> {
    admin.record("GET", "/auth/v1/scopes", Value::Null);
    if !admin.authorized {
        return Err(StatusCode::FORBIDDEN);
    }
    let scopes: Vec<Value> = admin
        .scopes
        .lock()
        .unwrap()
        .iter()
        .map(|name| json!({ "id": name, "name": name }))
        .collect();
    Ok(Json(Value::Array(scopes)))
}

async fn create_scope(State(admin): State<Admin>, Json(body): Json<Value>) -> StatusCode {
    admin.record("POST", "/auth/v1/scopes", body.clone());
    if let Some(name) = body.get("scope").and_then(Value::as_str) {
        admin.scopes.lock().unwrap().push(name.to_owned());
    }
    StatusCode::OK
}

async fn create_client(State(admin): State<Admin>, Json(body): Json<Value>) -> StatusCode {
    admin.record("POST", "/auth/v1/clients", body.clone());
    if let Some(id) = body.get("id").and_then(Value::as_str) {
        // What a freshly created rauthy client looks like: EdDSA, no
        // audience, and rauthy's own default lifetime. Every one of those is
        // a reason this spec exists.
        let mut client = body.clone();
        client["enabled"] = json!(true);
        client["access_token_alg"] = json!("EdDSA");
        client["id_token_alg"] = json!("EdDSA");
        client["access_token_lifetime"] = json!(1800);
        client["flows_enabled"] = json!(["authorization_code"]);
        client["scopes"] = json!(["openid", "profile", "email"]);
        client["default_scopes"] = json!(["openid"]);
        client["challenges"] = json!(["S256", "plain"]);
        client["scim"] = json!({ "base_uri": "https://scim.example.com" });
        admin.clients.lock().unwrap().insert(id.to_owned(), client);
    }
    StatusCode::OK
}

async fn read_client(
    State(admin): State<Admin>,
    Path(id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    admin.record("GET", &format!("/auth/v1/clients/{id}"), Value::Null);
    if !admin.authorized {
        return Err(StatusCode::FORBIDDEN);
    }
    admin
        .clients
        .lock()
        .unwrap()
        .get(&id)
        .cloned()
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

async fn update_client(
    State(admin): State<Admin>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> StatusCode {
    admin.record("PUT", &format!("/auth/v1/clients/{id}"), body.clone());
    admin.clients.lock().unwrap().insert(id, body);
    StatusCode::OK
}

async fn end_sessions(State(admin): State<Admin>, Path(id): Path<String>) -> StatusCode {
    admin.record("DELETE", &format!("/auth/v1/sessions/{id}"), Value::Null);
    if admin.authorized {
        StatusCode::OK
    } else {
        StatusCode::FORBIDDEN
    }
}

/// Serve `app` on a free loopback port; the guard aborts it when dropped.
struct Served {
    addr: SocketAddr,
    handle: tokio::task::JoinHandle<()>,
}

impl Drop for Served {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

async fn serve(app: Router) -> Served {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a free port");
    let addr = listener.local_addr().expect("its address");
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Served { addr, handle }
}

fn idp_config(rauthy: SocketAddr) -> IdpConfig {
    let addr = rauthy.to_string();
    let env = BTreeMap::from([
        ("RAHI_PUBLIC_URL", FIXTURE_ORIGIN),
        ("RAHI_RAUTHY_ADDR", addr.as_str()),
    ]);
    let config = Config::from_env(&env).expect("the fixture environment");
    IdpConfig::derive(&config, "hello-cell").expect("the fixture derives")
}

fn cli() -> NativeClient {
    NativeClient {
        id: "hello-cli".to_owned(),
        flows: vec![NativeFlow::DeviceCode, NativeFlow::RefreshToken],
        scopes: vec!["notes:write".to_owned()],
        redirect_uris: Vec::new(),
    }
}

// ------------------------------------------------------------------ FR-003

/// FR-003: a declared client is created public, bound to RS256, to this
/// cell's audience, and to the manifest's lifetime, with its scope created
/// first (B-3).
#[tokio::test]
async fn a_declared_client_is_provisioned_with_the_settings_the_spec_fixes() {
    let admin = Admin::new();
    let served = serve(admin.clone().router()).await;
    let config = idp_config(served.addr);

    let done = provision_native_clients(&config, "key$secret", &[cli()], FIXTURE_ORIGIN, 600)
        .await
        .expect("the stub accepts the provisioning");

    assert_eq!(done.len(), 1);
    assert!(done[0].created, "rauthy held no such client");
    assert_eq!(done[0].id, "hello-cli");
    assert_eq!(
        done[0].scopes_created,
        vec!["notes:write".to_owned()],
        "the declared scope is created before the client is given it (B-3)"
    );

    let calls = admin.calls();
    assert_eq!(
        calls.first(),
        Some(&("GET".to_owned(), "/auth/v1/scopes".to_owned())),
        "the catalog is read before anything is created: {calls:?}"
    );
    assert!(
        calls
            .iter()
            .position(|c| c == &("POST".to_owned(), "/auth/v1/scopes".to_owned()))
            < calls
                .iter()
                .position(|c| c == &("POST".to_owned(), "/auth/v1/clients".to_owned())),
        "a client cannot be given a scope that does not exist yet: {calls:?}"
    );

    let created = admin.body_of("POST", "/auth/v1/clients");
    assert_eq!(
        created["confidential"], false,
        "a public client: rauthy's device endpoint asks a confidential one for a secret"
    );

    let update = admin.body_of("PUT", "/auth/v1/clients/hello-cli");
    assert_eq!(update["access_token_alg"], "RS256");
    assert_eq!(update["id_token_alg"], "RS256");
    assert_eq!(update["challenges"], json!(["S256"]));
    assert_eq!(update["confidential"], false);
    assert_eq!(update["enabled"], true);
    assert_eq!(update["access_token_lifetime"], 600);
    assert_eq!(
        update["default_aud"],
        json!([FIXTURE_ORIGIN]),
        "the device grant carries no resource parameter, so the audience is the client's"
    );
    assert_eq!(
        update["flows_enabled"],
        json!([
            "urn:ietf:params:oauth:grant-type:device_code",
            "refresh_token"
        ])
    );
    assert_eq!(
        update["scopes"],
        json!(["openid", "profile", "email", "notes:write"])
    );
    assert_eq!(
        update["scim"]["base_uri"], "https://scim.example.com",
        "a field this crate does not know is sent back untouched"
    );

    // What the operator's preflight reads back (B-6).
    assert_eq!(
        read_lifetime(&config, "key$secret", "hello-cli")
            .await
            .expect("the lifetime reads back"),
        600
    );
}

/// A second boot finds the client settled and writes nothing.
#[tokio::test]
async fn provisioning_twice_writes_once() {
    let admin = Admin::new();
    let served = serve(admin.clone().router()).await;
    let config = idp_config(served.addr);

    provision_native_clients(&config, "key$secret", &[cli()], FIXTURE_ORIGIN, 600)
        .await
        .expect("the first boot");
    let settled = admin.client("hello-cli");
    let done = provision_native_clients(&config, "key$secret", &[cli()], FIXTURE_ORIGIN, 600)
        .await
        .expect("the second boot");

    assert!(!done[0].created, "the client was already there");
    assert!(
        done[0].scopes_created.is_empty(),
        "the scope was already in the catalog"
    );
    assert_eq!(
        admin
            .calls()
            .iter()
            .filter(|(method, path)| method == "PUT" && path == "/auth/v1/clients/hello-cli")
            .count(),
        1,
        "the second boot found the settings it wanted and wrote nothing"
    );
    assert_eq!(admin.client("hello-cli"), settled);
}

/// An operator who widened the client at rauthy is drift, not a decision:
/// the manifest is the ceiling, so the declared settings are written back
/// (D-12). This is deliberately the opposite of `bootstrap_client`.
#[tokio::test]
async fn a_client_widened_at_the_idp_is_narrowed_back_to_the_manifest() {
    let admin = Admin::new();
    let served = serve(admin.clone().router()).await;
    let config = idp_config(served.addr);

    provision_native_clients(&config, "key$secret", &[cli()], FIXTURE_ORIGIN, 600)
        .await
        .expect("the first boot");
    admin
        .clients
        .lock()
        .unwrap()
        .entry("hello-cli".to_owned())
        .and_modify(|client| {
            client["flows_enabled"] = json!([
                "urn:ietf:params:oauth:grant-type:device_code",
                "refresh_token",
                "client_credentials"
            ]);
        });

    provision_native_clients(&config, "key$secret", &[cli()], FIXTURE_ORIGIN, 600)
        .await
        .expect("the next boot");
    assert_eq!(
        admin.client("hello-cli")["flows_enabled"],
        json!([
            "urn:ietf:params:oauth:grant-type:device_code",
            "refresh_token"
        ]),
        "a grant the manifest no longer declares comes off the client"
    );
}

/// The provisioning never deletes a client this manifest did not declare
/// (B-3).
#[tokio::test]
async fn a_client_nobody_declared_is_left_where_it_is() {
    let admin = Admin::new();
    admin.clients.lock().unwrap().insert(
        "somebody-elses".to_owned(),
        json!({ "id": "somebody-elses", "confidential": true }),
    );
    let served = serve(admin.clone().router()).await;
    let config = idp_config(served.addr);

    provision_native_clients(&config, "key$secret", &[cli()], FIXTURE_ORIGIN, 600)
        .await
        .expect("the boot");

    assert!(
        admin.clients.lock().unwrap().contains_key("somebody-elses"),
        "a client this manifest does not declare is somebody else's decision"
    );
    assert!(
        !admin.calls().iter().any(|(method, _)| method == "DELETE"),
        "nothing here deletes"
    );
}

/// A refused admin key is named as one, not reported as an unreachable IdP.
#[tokio::test]
async fn a_refused_admin_key_says_which_groups_it_needs() {
    let mut admin = Admin::new();
    admin.authorized = false;
    let served = serve(admin.router()).await;
    let config = idp_config(served.addr);

    let err = provision_native_clients(&config, "key$secret", &[cli()], FIXTURE_ORIGIN, 600)
        .await
        .expect_err("the stub refuses the key");
    assert_eq!(err.kind(), "unauthorized");
    assert!(err.message().contains("Scopes"), "{err}");
}

/// Declaring nothing asks rauthy nothing.
#[tokio::test]
async fn a_cell_with_no_native_client_talks_to_nobody() {
    let admin = Admin::new();
    let served = serve(admin.clone().router()).await;
    let config = idp_config(served.addr);

    assert!(
        provision_native_clients(&config, "key$secret", &[], FIXTURE_ORIGIN, 600)
            .await
            .expect("nothing to do")
            .is_empty()
    );
    assert!(admin.calls().is_empty(), "{:?}", admin.calls());
}

// ------------------------------------------------------------------ FR-004

/// The resource server over the OIDC fixture's cell, with the deny-list TTL
/// the manifest's lifetime implies (D-7).
async fn resource_server(cell: &Cell) -> ResourceServer {
    let jwks = rahi_idp::Jwks::load(cell.sessions.discovery())
        .await
        .expect("the stub's key set loads");
    let resource = Resource::derive(cell.sessions.idp()).expect("the resource derives");
    let env = BTreeMap::from([("RAHI_PUBLIC_URL", cell.sessions.origin())]);
    let config = Config::from_env(&env).expect("the fixture environment");
    let ticking = Arc::clone(&cell.clock);
    ResourceServer::new(
        &config,
        resource,
        jwks,
        cell.sessions.store().clone(),
        cell.kernel.clone(),
    )
    .with_clock(Arc::new(move || ticking.load(Ordering::Relaxed)))
    .with_lifetime(std::time::Duration::from_secs(600))
}

/// A signed access token from `person.json`, with `edit` applied.
fn issued(cell: &Cell, edit: impl FnOnce(&mut Value)) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/tokens/person.json");
    let text = std::fs::read_to_string(&path).expect("the fixture is readable");
    let mut payload: Value =
        serde_json::from_str(&text.replace(FIXTURE_ORIGIN, cell.sessions.origin()))
            .expect("the fixture is JSON");
    edit(&mut payload);
    sign(
        &json!({ "alg": "RS256", "typ": "JWT", "kid": KID }),
        &payload,
    )
}

/// FR-004, first half: a deny-listed `jti` is refused, and the entry is
/// remembered for the accepted validity rather than the lifetime (D-7).
#[tokio::test]
async fn a_deny_listed_token_is_refused_and_the_memory_outlives_it() {
    let cell = oidc::boot().await;
    let server = resource_server(&cell).await;
    let token = issued(&cell, |_| {});

    server.validate(&token).await.expect("a good token");

    let revoker = Revoker::new(server.clone());
    let revoked = revoker
        .revoke_token("0195f0e0-9b1e-7c1f-9c0e-2c4a4f2b0a11")
        .await
        .expect("the deny-list accepts the write");
    assert_eq!(
        revoked.remembered_for_secs, 720,
        "ten minutes of lifetime is twelve minutes of memory (D-7)"
    );
    assert!(
        !revoked.grant_ended,
        "revoking one token says nothing about the grant behind it (D-8)"
    );

    let err = server
        .validate(&token)
        .await
        .expect_err("the revoked token is refused");
    assert_eq!(err.kind(), "unauthorized");
    assert!(err.message().contains("revoked"), "{err}");

    assert!(
        server
            .is_denied("0195f0e0-9b1e-7c1f-9c0e-2c4a4f2b0a11")
            .await
            .expect("the deny-list reads"),
    );

    // D-7's arithmetic, stated: a token minted at the instant of revocation
    // validates until `exp + LEEWAY`, and a clock a leeway ahead moves that
    // by the leeway again, so the memory is the lifetime plus twice the
    // leeway. It covers the token *of that lifetime*, which is why B-6 has
    // preflight refuse a client whose lifetime at rauthy exceeds the
    // manifest's: the arithmetic is only sound while those two agree.
    for lifetime in [300u64, 600, 1800] {
        let ttl = denylist_ttl(std::time::Duration::from_secs(lifetime)).as_secs();
        assert_eq!(ttl, lifetime + 120, "{lifetime}");
        assert!(
            ttl > lifetime + 60,
            "the entry outlives the last moment a token of this lifetime validates"
        );
    }
}

/// FR-004, second half: a subject revocation refuses a token issued before
/// its instant and admits one issued after (B-5).
#[tokio::test]
async fn a_subject_revocation_bounds_by_the_instant_it_was_made_at() {
    let cell = oidc::boot().await;
    let server = resource_server(&cell).await;

    let before = issued(&cell, |payload| {
        payload["iat"] = json!(T0 - 60);
        payload["jti"] = json!("before-the-instant");
    });
    server.validate(&before).await.expect("a good token");

    deny_subject(server.store(), SUB, server.now(), server.revocation_lag())
        .await
        .expect("the deny-list accepts the write");

    let err = server
        .validate(&before)
        .await
        .expect_err("a token minted before the revocation");
    assert_eq!(err.kind(), "unauthorized");
    assert!(err.message().contains("subject"), "{err}");

    let after = issued(&cell, |payload| {
        payload["iat"] = json!(T0 + 1);
        payload["jti"] = json!("after-the-instant");
    });
    server.validate(&after).await.expect(
        "a token minted after the revocation is admitted: revoking a person is not \
                 locking them out of logging in again",
    );

    let other_person = issued(&cell, |payload| {
        payload["sub"] = json!("somebody-else");
        payload["iat"] = json!(T0 - 60);
        payload["jti"] = json!("another-subject");
    });
    server
        .validate(&other_person)
        .await
        .expect("another subject is untouched");
}

/// A token that will not say when it was minted is refused while its subject
/// is deny-listed: the entry asks a question the token declines to answer.
#[tokio::test]
async fn a_token_with_no_iat_is_refused_while_its_subject_is_deny_listed() {
    let cell = oidc::boot().await;
    let server = resource_server(&cell).await;

    let undated = issued(&cell, |payload| {
        payload.as_object_mut().expect("an object").remove("iat");
        payload["jti"] = json!("undated");
    });
    server.validate(&undated).await.expect("a good token");

    deny_subject(server.store(), SUB, server.now(), server.revocation_lag())
        .await
        .expect("the write");

    assert!(
        server.validate(&undated).await.is_err(),
        "a token that cannot be dated cannot be given the benefit of the doubt"
    );
}

/// D-8: revoking a subject ends the grant at rauthy, and a cell that was
/// given no admin credential says so rather than implying a sign-out.
#[tokio::test]
async fn a_subject_revocation_ends_the_grant_at_the_idp() {
    let cell = oidc::boot().await;
    let server = resource_server(&cell).await;
    let admin = Admin::new();
    let served = serve(admin.clone().router()).await;

    let without = Revoker::new(server.clone());
    assert!(!without.ends_grants());
    let revoked = without
        .revoke_subject(SUB)
        .await
        .expect("the deny-list write");
    assert!(
        !revoked.grant_ended,
        "the refresh token this cell cannot reach is still good, and the answer says so"
    );

    let with = Revoker::new(server.clone())
        .ending_grants(&idp_config(served.addr), "key$secret")
        .expect("the client builds");
    let revoked = with.revoke_subject(SUB).await.expect("the revocation");
    assert!(revoked.grant_ended);
    assert!(
        admin
            .calls()
            .contains(&("DELETE".to_owned(), format!("/auth/v1/sessions/{SUB}"))),
        "rauthy invalidates the sessions and the refresh tokens of that user: {:?}",
        admin.calls()
    );
}

/// A rauthy that refuses the sign-out is an error, not a quieter answer: the
/// operator asked for the grant to end and it did not.
#[tokio::test]
async fn a_refused_sign_out_is_an_error_naming_what_did_happen() {
    let cell = oidc::boot().await;
    let server = resource_server(&cell).await;
    let mut admin = Admin::new();
    admin.authorized = false;
    let served = serve(admin.router()).await;

    let revoker = Revoker::new(server)
        .ending_grants(&idp_config(served.addr), "key$secret")
        .expect("the client builds");
    let err = revoker
        .revoke_subject(SUB)
        .await
        .expect_err("rauthy refused");
    assert_eq!(err.kind(), "unauthorized");
    assert!(err.message().contains("deny-listed"), "{err}");
    assert!(err.message().contains("refresh grant is not"), "{err}");
}

/// The settings type is the one the provisioning sends, so a reader of one
/// is reading the other.
#[test]
fn the_settings_are_what_the_upsert_carries() {
    let settings = NativeSettings::for_client(&cli(), FIXTURE_ORIGIN, 600);
    let update = settings.update_request(&json!({}));
    assert_eq!(settings.first_mismatch(&update), None);
}
