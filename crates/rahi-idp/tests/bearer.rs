//! The resource server, end to end against the in-process OIDC stub
//! (spec 025 FR-001 through FR-006, and B-1 through B-12).
//!
//! Every token here is a real RS256 JWT signed with the fixture key under
//! `testdata/oidc/` and verified against a key set fetched over a socket, and
//! every refusal is the one a client would actually receive: the status, the
//! `WWW-Authenticate` header, and the decision id the chain holds.
//!
//! The claim fixtures under `testdata/tokens/` are the payloads rauthy issues
//! for the three cases this spec turns on: a person's token, a client
//! credentials token, and a token addressed to another resource. The tests
//! sign them here rather than recording whole JWTs, because a recorded
//! signature would be a signature under a key nobody has, which proves
//! nothing about the verification path.

#![allow(clippy::expect_used, clippy::indexing_slicing)]

mod oidc;

use std::collections::BTreeMap;
use std::net::ToSocketAddrs as _;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderValue, Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use oidc::{CLIENT_ID, Cell, KID, SUB, T0, query_param, send, sign};
use rahi_idp::{
    Authenticated, BearerRoutes, Discovery, ISSUER_PATH, IdpConfig, Proxy, Registration,
    RequireBearer, RequireScope, Resource, ResourceServer, SESSION_PREFIX, is_bearer_route,
    proxy_router, resource_router, session_router, with_bearer, with_scope, with_sessions,
};
use rahi_types::Config;
use ring::digest::{SHA256, digest};
use serde_json::{Value, json};

/// The subtree a bearer credential is declared on.
const API_PREFIX: &str = "/api";
/// The subtree a client credentials token may also reach (B-4).
const INGEST_PREFIX: &str = "/api/ingest";
/// The scope the write route requires.
const SCOPE_WRITE: &str = "notes.write";
/// The scope the fixture token actually carries.
const SCOPE_READ: &str = "notes.read";
/// The session cookie's name over plain `http`, which the fixture origin is.
const SESSION_COOKIE: &str = "session";
/// The origin the recorded claim sets were written for, rewritten per run.
const FIXTURE_ORIGIN: &str = "https://cell.example.com";

// ------------------------------------------------------------- the fixtures

/// The resource server over a booted cell, sharing its store, kernel, clock.
async fn resource_server(cell: &Cell) -> ResourceServer {
    let jwks = rahi_idp::Jwks::load(cell.sessions.discovery())
        .await
        .expect("the stub's key set loads");
    let resource = Resource::derive(cell.sessions.idp()).expect("the resource derives");
    let ticking = Arc::clone(&cell.clock);
    ResourceServer::new(
        &config(cell),
        resource,
        jwks,
        cell.sessions.store().clone(),
        cell.kernel.clone(),
    )
    .with_clock(Arc::new(move || ticking.load(Ordering::Relaxed)))
}

/// The cell's configuration, rebuilt from the origin it was booted on.
fn config(cell: &Cell) -> Config {
    let env = BTreeMap::from([("RAHI_PUBLIC_URL", cell.sessions.origin())]);
    Config::from_env(&env).expect("the fixture environment is well formed")
}

/// The whole cell's router: the resource metadata, the proxy, the session
/// routes, and one bearer-gated subtree with a scope gate inside it.
///
/// The bearer gate is applied where the paths are complete, outside every
/// nest, because its declaration is by path prefix (B-11).
fn app(cell: &Cell, server: &ResourceServer) -> Router {
    routed(cell, server, false)
}

/// The same cell with the session layer outermost, which is the other order
/// an app could compose these two in (B-10).
fn app_sessions_outermost(cell: &Cell, server: &ResourceServer) -> Router {
    routed(cell, server, true)
}

fn routed(cell: &Cell, server: &ResourceServer, sessions_outermost: bool) -> Router {
    let scoped = with_scope(
        RequireScope::new(SCOPE_WRITE, cell.kernel.clone()),
        Router::new().route("/notes", get(|| async { "notes" })),
    );
    let guarded = Router::new()
        .route("/me", get(whoami))
        .route("/cookie", get(sets_a_cookie))
        .route("/ingest", get(|| async { "ingested" }))
        .merge(scoped);

    let declared = BearerRoutes::new()
        .route(API_PREFIX)
        .service_route(INGEST_PREFIX);
    let gate = RequireBearer::new(server.clone(), declared);

    let inner = Router::new()
        .nest(
            API_PREFIX,
            with_sessions(cell.sessions.clone(), guarded.clone()),
        )
        .nest(SESSION_PREFIX, session_router(cell.sessions.clone()))
        .merge(resource_router(
            Resource::derive(cell.sessions.idp()).expect("the resource derives"),
        ))
        .merge(proxy_router(
            Proxy::new(cell.sessions.idp()).expect("the proxy builds"),
        ));

    if sessions_outermost {
        with_sessions(
            cell.sessions.clone(),
            Router::new().nest(API_PREFIX, with_bearer(gate, guarded)),
        )
    } else {
        with_bearer(gate, inner)
    }
}

/// What an authenticated route answers, whichever credential resolved it.
async fn whoami(Authenticated(principal): Authenticated) -> axum::Json<Value> {
    axum::Json(json!({
        "sub": principal.sub.as_str(),
        "email_verified": principal.email_verified,
        "roles": principal.roles.iter().map(rahi_types::Role::as_str).collect::<Vec<_>>(),
    }))
}

/// A handler that sets a cookie, to prove a bearer route never keeps one
/// (B-11).
async fn sets_a_cookie() -> Response {
    // Appended, not inserted: two `Set-Cookie` values on one response is the
    // ordinary case, and it is the case a strip that removes one value would
    // get wrong.
    let mut response = "ok".into_response();
    for cookie in ["crumb=1; Path=/", "second=2; Path=/"] {
        response
            .headers_mut()
            .append(header::SET_COOKIE, HeaderValue::from_static(cookie));
    }
    response
}

/// A GET carrying a bearer credential.
fn with_token(uri: &str, token: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .expect("a request")
}

/// One recorded claim set from `testdata/tokens/`, on this run's origin.
fn claims(cell: &Cell, name: &str) -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testdata/tokens")
        .join(name);
    let text = std::fs::read_to_string(&path).expect("the fixture is readable");
    serde_json::from_str(&text.replace(FIXTURE_ORIGIN, cell.sessions.origin()))
        .expect("the fixture is JSON")
}

/// A signed token from the fixture `name`, with `edit` applied to its claims.
fn issued(cell: &Cell, name: &str, edit: impl FnOnce(&mut Value)) -> String {
    let mut payload = claims(cell, name);
    edit(&mut payload);
    signed(&payload, KID)
}

/// One access token for a person, exactly as recorded.
fn access_token(cell: &Cell) -> String {
    issued(cell, "person.json", |_| {})
}

/// `payload`, signed by the fixture key under the key id `kid`.
fn signed(payload: &Value, kid: &str) -> String {
    sign(
        &json!({ "alg": "RS256", "typ": "JWT", "kid": kid }),
        payload,
    )
}

// ------------------------------------------------------------- FR-001

/// FR-001: a token for this resource authenticates, and the principal is the
/// IdP's subject with the roles the token carries (B-3, B-4).
#[tokio::test]
async fn a_token_addressed_to_this_resource_authenticates() {
    let cell = oidc::boot().await;
    let server = resource_server(&cell).await;
    let app = app(&cell, &server);

    let admitted = send(&app, with_token("/api/me", &access_token(&cell))).await;
    assert_eq!(admitted.status, StatusCode::OK, "{}", admitted.body);
    assert_eq!(admitted.json()["sub"], json!(SUB));
    assert_eq!(admitted.json()["roles"], json!(["reader"]));
    assert_eq!(
        cell.stub().userinfo_reads,
        0,
        "validation is local: no introspection and no userinfo on the request path (D-1)"
    );
}

/// FR-001: the same token addressed to another resource is refused, however
/// well formed it is (B-3, D-2, RFC 8707).
#[tokio::test]
async fn a_token_for_another_resource_is_refused() {
    let cell = oidc::boot().await;
    let server = resource_server(&cell).await;
    let app = app(&cell, &server);

    // The recorded token of another resource behind the same issuer.
    let elsewhere = issued(&cell, "other-resource.json", |_| {});
    let refused = send(&app, with_token("/api/me", &elsewhere)).await;
    assert_eq!(refused.status, StatusCode::UNAUTHORIZED, "{}", refused.body);
    assert!(
        refused.body.contains("audience"),
        "the refusal says why: {}",
        refused.body
    );

    // A token addressed to this cell's client but not to this resource is
    // refused for the same reason: the audience is mandatory, never inferred
    // (D-2). This is the shape rauthy issues when a client asks for no
    // `resource`, which is why it is worth its own assertion.
    let audienceless = issued(&cell, "person.json", |payload| {
        payload["aud"] = json!(CLIENT_ID);
    });
    assert_eq!(
        send(&app, with_token("/api/me", &audienceless))
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
}

/// FR-001: expired, unsigned by a published key, and subject-less tokens are
/// each refused; the sixty second leeway is real (B-3).
#[tokio::test]
async fn the_four_malformed_credentials_are_each_refused() {
    let cell = oidc::boot().await;
    let server = resource_server(&cell).await;
    let app = app(&cell, &server);

    let expired = issued(&cell, "person.json", |payload| {
        payload["exp"] = json!(T0 - 3600);
    });
    assert_eq!(
        send(&app, with_token("/api/me", &expired)).await.status,
        StatusCode::UNAUTHORIZED,
        "an expired token"
    );

    let inside_leeway = issued(&cell, "person.json", |payload| {
        payload["exp"] = json!(T0 - 30);
    });
    assert_eq!(
        send(&app, with_token("/api/me", &inside_leeway))
            .await
            .status,
        StatusCode::OK,
        "thirty seconds past expiry is inside the sixty second leeway"
    );

    let unknown_key = signed(
        &claims(&cell, "person.json"),
        "a-key-rauthy-never-published",
    );
    assert_eq!(
        send(&app, with_token("/api/me", &unknown_key)).await.status,
        StatusCode::UNAUTHORIZED,
        "a key absent from the JWKS"
    );

    let subjectless = issued(&cell, "person.json", |payload| {
        payload
            .as_object_mut()
            .expect("the claims are an object")
            .remove("sub");
    });
    assert_eq!(
        send(&app, with_token("/api/me", &subjectless)).await.status,
        StatusCode::UNAUTHORIZED,
        "a token with no subject names nobody"
    );

    let not_yet = issued(&cell, "person.json", |payload| {
        payload["nbf"] = json!(T0 + 3600);
    });
    assert_eq!(
        send(&app, with_token("/api/me", &not_yet)).await.status,
        StatusCode::UNAUTHORIZED,
        "a token that is not valid yet"
    );
}

// ------------------------------------------------------------- FR-002

/// FR-002: the 401 carries the challenge, the challenge names a document that
/// is fetchable, and that document's authorization server resolves to a
/// discovery document through the proxy (B-2, B-6).
#[tokio::test]
async fn the_challenge_bootstraps_a_client_that_has_never_seen_this_cell() {
    let cell = oidc::boot().await;
    let server = resource_server(&cell).await;
    let app = app(&cell, &server);

    let challenged = send(&app, oidc::get_with("/api/me", &[])).await;
    assert_eq!(challenged.status, StatusCode::UNAUTHORIZED);
    let header = challenged
        .headers
        .get(header::WWW_AUTHENTICATE)
        .and_then(|value| value.to_str().ok())
        .expect("a bearer route challenges")
        .to_owned();
    assert!(header.starts_with("Bearer realm="), "{header}");

    let metadata_url = between(&header, "resource_metadata=\"", "\"").expect("the metadata URL");
    let path = metadata_url
        .strip_prefix(cell.sessions.origin())
        .expect("the document is on this origin");
    assert_eq!(path, rahi_idp::METADATA_PATH);

    let document = send(&app, oidc::get_with(path, &[])).await;
    assert_eq!(document.status, StatusCode::OK, "{}", document.body);
    assert_eq!(
        document
            .headers
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some("public, max-age=3600"),
        "the document is cached for an hour (B-2)"
    );
    let document = document.json();
    assert_eq!(document["resource"], json!(cell.sessions.origin()));
    assert_eq!(document["bearer_methods_supported"], json!(["header"]));
    assert!(
        document["scopes_supported"]
            .as_array()
            .expect("an array")
            .contains(&json!(SCOPE_WRITE)),
        "a scope gate publishes what it requires: {document}"
    );

    // The authorization server, one path below, is rauthy's own discovery
    // document, read through the raw proxy of spec 021.
    let issuer = document["authorization_servers"][0]
        .as_str()
        .expect("an authorization server")
        .to_owned();
    let discovery_path = format!(
        "{}/.well-known/openid-configuration",
        issuer
            .strip_prefix(cell.sessions.origin())
            .expect("the authorization server is on this origin")
            .trim_end_matches('/')
    );
    let discovery = send(&app, oidc::get_with(&discovery_path, &[])).await;
    assert_eq!(discovery.status, StatusCode::OK, "{}", discovery.body);
    assert_eq!(discovery.json()["issuer"], json!(issuer));
}

/// B-2, B-7: the document names rauthy's registration endpoint when
/// registration is not `off`, and names none when it is.
#[tokio::test]
async fn registration_is_advertised_exactly_when_it_is_offered() {
    let cell = oidc::boot().await;
    let resource = Resource::derive(cell.sessions.idp()).expect("the resource derives");

    let advertised = Router::new().merge(resource_router(resource.clone()));
    let document = send(&advertised, oidc::get_with(rahi_idp::METADATA_PATH, &[])).await;
    assert_eq!(
        document.json()["registration_endpoint"],
        json!(format!("{}/auth/v1/clients_dyn", cell.sessions.origin())),
        "the default is token, which is advertised (B-7)"
    );

    let off = Router::new().merge(resource_router(
        resource.with_registration(Registration::Off),
    ));
    let document = send(&off, oidc::get_with(rahi_idp::METADATA_PATH, &[])).await;
    assert_eq!(document.json().get("registration_endpoint"), None);
}

// ------------------------------------------------------------- FR-003

/// FR-003: a request carrying both credentials is 400, whichever layer is
/// outermost, and nothing about it suggests either succeeded (B-10).
#[tokio::test]
async fn a_cookie_and_a_token_together_are_refused() {
    let cell = oidc::boot().await;
    let server = resource_server(&cell).await;
    let app = app(&cell, &server);
    let session = oidc::login(&app, &cell).await;

    let token = access_token(&cell);
    for router in [&app, &app_sessions_outermost(&cell, &server)] {
        let refused = send(
            router,
            Request::builder()
                .method("GET")
                .uri("/api/me")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::COOKIE, format!("{SESSION_COOKIE}={session}"))
                .body(Body::empty())
                .expect("a request"),
        )
        .await;
        assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{}", refused.body);
        assert_eq!(refused.json()["error"], json!("validation"));
        assert_eq!(
            refused.json().get("decision"),
            None,
            "an ambiguous request writes no decision that reads as an outcome"
        );
        assert!(
            refused.set_cookies().is_empty(),
            "no rotation on a refusal: {:?}",
            refused.set_cookies()
        );
    }
}

// ------------------------------------------------------------- FR-004

/// FR-004: a deny-listed `jti` is refused inside the lag, and the entry
/// expires on its own (B-5).
#[tokio::test]
async fn a_deny_listed_token_is_refused_until_the_lag_elapses() {
    let cell = oidc::boot().await;
    let server = resource_server(&cell).await;
    let app = app(&cell, &server);
    let token = issued(&cell, "person.json", |payload| {
        payload["jti"] = json!("revoked-token");
        payload["exp"] = json!(T0 + 86_400);
    });

    assert_eq!(
        send(&app, with_token("/api/me", &token)).await.status,
        StatusCode::OK,
        "before the revocation"
    );

    server
        .deny("revoked-token")
        .await
        .expect("the deny-list writes");
    let refused = send(&app, with_token("/api/me", &token)).await;
    assert_eq!(refused.status, StatusCode::UNAUTHORIZED, "{}", refused.body);
    assert!(refused.body.contains("revoked"), "{}", refused.body);

    // The entry is remembered for one access lifetime and no longer: past it
    // the bound has been paid and the token stands on its own expiry again.
    cell.advance(rahi_idp::DEFAULT_REVOCATION_LAG.as_secs() + 1);
    assert_eq!(
        send(&app, with_token("/api/me", &token)).await.status,
        StatusCode::OK,
        "the deny-list entry expires on its own"
    );
}

// ------------------------------------------------------------- FR-005

/// FR-005: an insufficient scope is 403, names the required scope in the
/// header, and emits exactly one decision (B-6, B-9).
#[tokio::test]
async fn an_insufficient_scope_is_403_naming_the_scope_it_wanted() {
    let cell = oidc::boot().await;
    let server = resource_server(&cell).await;
    let app = app(&cell, &server);
    let token = issued(&cell, "person.json", |payload| {
        payload["scope"] = json!(SCOPE_READ);
    });

    let refused = send(&app, with_token("/api/notes", &token)).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.body);
    assert_eq!(refused.json()["error"], json!("denied"));
    assert_eq!(
        refused
            .headers
            .get(header::WWW_AUTHENTICATE)
            .and_then(|value| value.to_str().ok()),
        Some("Bearer error=\"insufficient_scope\", scope=\"notes.write\""),
        "the client is told what to ask for (B-6)"
    );
    let first = decision_seq(&refused);

    // Exactly one decision: the next refusal is the very next sequence
    // number, so nothing else was recorded in between.
    let again = send(&app, with_token("/api/notes", &token)).await;
    assert_eq!(again.status, StatusCode::FORBIDDEN);
    assert_eq!(decision_seq(&again), first + 1, "exactly one decision each");

    cell.kernel
        .flush(Duration::from_secs(5))
        .await
        .expect("the appender drains");

    // The same route with the scope granted is admitted: the gate is about
    // the grant, not about the person.
    let granted = issued(&cell, "person.json", |payload| {
        payload["scope"] = json!(format!("{SCOPE_READ} {SCOPE_WRITE}"));
    });
    assert_eq!(
        send(&app, with_token("/api/notes", &granted)).await.status,
        StatusCode::OK
    );
}

/// The sequence number of the decision a refusal names.
fn decision_seq(answer: &oidc::Answer) -> u64 {
    let id = answer.json()["decision"]
        .as_str()
        .expect("a refusal names its decision")
        .to_owned();
    assert!(id.starts_with("kernel:"), "{id}");
    id.rsplit(':')
        .next()
        .expect("the id ends in a sequence number")
        .parse()
        .expect("the sequence number is a number")
}

// ------------------------------------------------------------- B-4

/// B-4: a client credentials token reaches only a route a spec declared
/// service-callable, and the refusal is ledgered.
#[tokio::test]
async fn a_service_token_reaches_only_a_declared_service_route() {
    let cell = oidc::boot().await;
    let server = resource_server(&cell).await;
    let app = app(&cell, &server);

    let token = issued(&cell, "service.json", |_| {});

    let refused = send(&app, with_token("/api/me", &token)).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.body);
    assert!(
        refused.json()["decision"].is_string(),
        "the refusal is in the chain: {}",
        refused.body
    );

    let admitted = send(&app, with_token("/api/ingest", &token)).await;
    assert_eq!(
        admitted.status,
        StatusCode::OK,
        "the route that declared itself service-callable admits it: {}",
        admitted.body
    );

    cell.kernel
        .flush(Duration::from_secs(5))
        .await
        .expect("the appender drains");
}

// ------------------------------------------------------------- B-11, B-12

/// B-11: a bearer-authenticated route sets no cookie, and the declaration is
/// published for the composer that exempts it from CSRF.
#[tokio::test]
async fn a_bearer_authenticated_route_keeps_no_cookie() {
    let cell = oidc::boot().await;
    let server = resource_server(&cell).await;
    let app = app(&cell, &server);

    let bearer = send(&app, with_token("/api/cookie", &access_token(&cell))).await;
    assert_eq!(bearer.status, StatusCode::OK, "{}", bearer.body);
    assert!(
        bearer.set_cookies().is_empty(),
        "a token client has no cookie jar to keep one safe in: {:?}",
        bearer.set_cookies()
    );

    // The same handler reached with a cookie session still sets its cookie:
    // what is exempt is the credential kind, not the route's handler.
    let session = oidc::login(&app, &cell).await;
    let cookied = send(
        &app,
        oidc::get_with("/api/cookie", &[format!("{SESSION_COOKIE}={session}")]),
    )
    .await;
    assert_eq!(cookied.status, StatusCode::OK, "{}", cookied.body);
    assert!(
        cookied
            .set_cookies()
            .iter()
            .any(|c| c.starts_with("crumb=")),
        "{:?}",
        cookied.set_cookies()
    );

    assert!(is_bearer_route("/api/me"), "the declaration is published");
    assert!(!is_bearer_route("/session/login"));
}

/// B-12: the ceiling is per `(client_id, sub)`, so one runtime cannot spend
/// another's budget from the same address.
#[tokio::test]
async fn the_bearer_budget_is_per_client_and_subject() {
    let cell = oidc::boot().await;
    let server = resource_server(&cell).await.with_rate_limit(2);
    let app = app(&cell, &server);
    let token = access_token(&cell);

    for attempt in 1..=2 {
        assert_eq!(
            send(&app, with_token("/api/me", &token)).await.status,
            StatusCode::OK,
            "attempt {attempt} is inside the ceiling"
        );
    }
    let spent = send(&app, with_token("/api/me", &token)).await;
    assert_eq!(
        spent.status,
        StatusCode::TOO_MANY_REQUESTS,
        "{}",
        spent.body
    );
    assert!(
        spent.headers.contains_key(header::RETRY_AFTER),
        "a spent window says when it rolls"
    );

    // Another subject under the same client has its own budget.
    let other = issued(&cell, "person.json", |payload| {
        payload["sub"] = json!("another-subject");
    });
    assert_eq!(
        send(&app, with_token("/api/me", &other)).await.status,
        StatusCode::OK,
        "one user's runtime cannot exhaust another's"
    );
}

/// B-11: a credential presented on a route nobody declared bearer is refused
/// rather than ignored, and a route with no credential and no declaration is
/// left alone.
#[tokio::test]
async fn a_credential_on_an_undeclared_route_is_refused() {
    let cell = oidc::boot().await;
    let server = resource_server(&cell).await;
    let app = app(&cell, &server);

    let refused = send(
        &app,
        with_token(rahi_idp::METADATA_PATH, &access_token(&cell)),
    )
    .await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{}", refused.body);

    let public = send(&app, oidc::get_with(rahi_idp::METADATA_PATH, &[])).await;
    assert_eq!(
        public.status,
        StatusCode::OK,
        "and it is public without one"
    );
}

/// B-7, B-8: rauthy's own subtree is forwarded raw, credentials and all. Its
/// registration endpoint and its device grant are reached with tokens
/// addressed to rauthy, and this layer reads none of them.
#[tokio::test]
async fn the_proxy_subtree_carries_its_own_credentials_through() {
    let cell = oidc::boot().await;
    let server = resource_server(&cell).await;
    let app = app(&cell, &server);

    let forwarded = send(
        &app,
        Request::builder()
            .method("GET")
            .uri("/auth/v1/.well-known/openid-configuration")
            .header(header::AUTHORIZATION, "Bearer a-registration-token")
            .body(Body::empty())
            .expect("a request"),
    )
    .await;
    assert_eq!(forwarded.status, StatusCode::OK, "{}", forwarded.body);
    assert_eq!(
        forwarded.json()["issuer"],
        json!(format!("{}{ISSUER_PATH}", cell.sessions.origin()))
    );
}

// ------------------------------------------------------------- B-1, FR-006

/// B-1 and FR-006: this workspace mints no credential of its own.
///
/// The check greps every source file outside comments for the three names an
/// app-minted credential arrives under, and the allowlist below is the whole
/// of what is permitted: two credentials this codebase *presents* to
/// somebody else (rauthy's admin API key, the operator's S3 key) and one
/// fixture that proves the kernel refuses a payload carrying one. Nothing in
/// the list creates, stores, hashes, or compares a credential of this app's.
///
/// The allowlist is asserted to be exact, so an entry that stops offending
/// has to be removed rather than left to rot into a permanent hole.
#[test]
fn no_credential_is_minted_anywhere_in_this_workspace() {
    let allowed: BTreeMap<&str, &str> = BTreeMap::from([
        (
            "crates/rahi-idp/tests/bearer.rs",
            "this check itself, which has to name what it forbids to look for it",
        ),
        (
            "crates/rahi-idp/src/config.rs",
            "rauthy's admin API-Key scheme: a credential rauthy minted, presented to rauthy",
        ),
        (
            "crates/rahi-idp/src/bootstrap.rs",
            "presents that same admin key on the bootstrap call",
        ),
        (
            "crates/rahi-idp/src/lib.rs",
            "re-exports the constant naming that scheme",
        ),
        (
            "crates/rahi-kernel/tests/adjudicate.rs",
            "the fixture proving the gate denies a payload that carries a credential",
        ),
        (
            "crates/rahi-store/src/config.rs",
            "the operator's S3 access key for backups, supplied by the deployment",
        ),
        (
            "crates/rahi-store/src/store.rs",
            "hands that same S3 key to the object store client",
        ),
        (
            "crates/rahi-store/tests/backup.rs",
            "the S3 fixture specification for the RAHI_TEST_S3 run",
        ),
        (
            "crates/rahi-ledger/src/archive.rs",
            "the operator's S3 access key for the segment archive",
        ),
        (
            "crates/rahi-ops/src/backup.rs",
            "the operator's S3 access key for an s3:// backup destination (spec 030 D-7)",
        ),
        (
            "crates/rahi-ops/src/rauthy_api.rs",
            "presents rauthy's admin key on the backup calls (spec 030 B-5)",
        ),
        (
            "crates/rahi-ops/src/keys.rs",
            "first boot provisions rauthy's own bootstrap API key (spec 031 B-2)",
        ),
        (
            "crates/rahi-ops/src/rauthy_env.rs",
            "renders that key into rauthy's environment (spec 031 B-2)",
        ),
        (
            "crates/rahi-ops/src/supervise.rs",
            "presents that key on the client bootstrap and secret read (spec 031 B-3)",
        ),
        (
            "crates/rahi-ops/tests/first_boot.rs",
            "asserts the provisioned key and rauthy's floor on its length",
        ),
    ]);

    let mut offenders: Vec<String> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for path in sources() {
        let relative = path
            .strip_prefix(workspace())
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let names = offences(&text);
        if names.is_empty() {
            continue;
        }
        if allowed.contains_key(relative.as_str()) {
            seen.push(relative);
        } else {
            offenders.push(format!("{relative}: {}", names.join(", ")));
        }
    }
    assert!(
        offenders.is_empty(),
        "this chassis mints no credential of its own (B-1); found: {offenders:#?}"
    );

    let mut unused: Vec<&str> = allowed
        .keys()
        .filter(|path| !seen.iter().any(|found| found == *path))
        .copied()
        .collect();
    unused.sort_unstable();
    assert!(
        unused.is_empty(),
        "these allowlist entries no longer name anything, so remove them: {unused:?}"
    );
}

/// FR-006: the same check fails when a migration introduces the column.
#[test]
fn the_check_catches_an_api_key_column_in_a_migration() {
    let migration = rahi_store::Migration::new(
        1,
        "tokens",
        "CREATE TABLE tokens (api_key TEXT NOT NULL, sub TEXT NOT NULL)",
    );
    assert_eq!(offences(&migration.sql), vec!["api_key"]);
    assert!(
        offences("CREATE TABLE notes (id TEXT PRIMARY KEY, sub TEXT NOT NULL)").is_empty(),
        "an ordinary migration is not an offence"
    );
    assert!(
        offences("// an api_key column would be an offence").is_empty(),
        "a comment saying so is not one (B-1)"
    );
}

/// The names an app-minted credential arrives under, found in `text` outside
/// its comments.
fn offences(text: &str) -> Vec<&'static str> {
    let code: String = text
        .lines()
        .filter(|line| {
            let line = line.trim_start();
            !(line.starts_with("//")
                || line.starts_with('#')
                || line.starts_with('*')
                || line.starts_with("/*"))
        })
        .collect::<Vec<_>>()
        .join("\n")
        .to_ascii_lowercase();
    ["api_key", "apikey", "access_key"]
        .into_iter()
        .filter(|name| code.contains(name))
        .collect()
}

/// The workspace root, from this crate's manifest directory.
fn workspace() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("the crate lives two levels below the workspace root")
        .to_path_buf()
}

/// Every Rust and TOML source in the workspace's crates and apps.
fn sources() -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    for root in ["crates", "apps"] {
        walk(&workspace().join(root), &mut found);
    }
    found.sort();
    found
}

fn walk(dir: &std::path::Path, into: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name == "target") {
                continue;
            }
            walk(&path, into);
        } else if path
            .extension()
            .is_some_and(|ext| ext == "rs" || ext == "toml" || ext == "sql")
        {
            into.push(path);
        }
    }
}

/// The text between `open` and the next `close`.
fn between<'t>(text: &'t str, open: &str, close: &str) -> Option<&'t str> {
    let rest = text.split_once(open)?.1;
    rest.split_once(close).map(|(value, _)| value)
}

// ------------------------------------------------------------- AC-2

/// The origin of an already running rauthy, which opts this run in (D-10).
const LIVE_URL: &str = "RAHI_TEST_RAUTHY_URL";
/// The registration token that rauthy's `token` mode requires (B-7).
const LIVE_REG_TOKEN: &str = "RAHI_TEST_RAUTHY_REG_TOKEN";
/// An admin API key, `name$secret`, for the two provisioning calls.
const LIVE_API_KEY: &str = "RAHI_TEST_RAUTHY_API_KEY";
/// The person who completes the authorization code login.
const LIVE_USER: &str = "RAHI_TEST_RAUTHY_USER";
/// That person's password.
const LIVE_PASSWORD: &str = "RAHI_TEST_RAUTHY_PASSWORD";

/// The scope the registered client is granted, and the gate it opens.
const LIVE_SCOPE_GRANTED: &str = "api:read";
/// A scope the same client is never granted, and the gate it does not open.
const LIVE_SCOPE_WITHHELD: &str = "api:write";
/// The loopback redirect the client registers, which is never listened on:
/// rauthy answers the authorization request with the code in a `Location`,
/// so a CLI reads it there rather than from a browser round trip.
const LIVE_REDIRECT: &str = "http://127.0.0.1:9876/callback";

/// AC-2: a dynamically registered client, a real PKCE login, a real token.
///
/// With [`LIVE_URL`] naming the origin of a running rauthy, this runs every
/// step AC-2 names against it: register a client through the dynamic
/// registration endpoint, complete authorization code with PKCE against a
/// loopback redirect, present the resulting access token to a scope-gated
/// route and be admitted, and watch the same token refused by a route
/// requiring a scope it lacks.
///
/// Nothing here is a fixture. The token is signed by rauthy's own key, the
/// key set is fetched from rauthy over the network, the issuer is the one
/// rauthy publishes, and the audience is this cell's origin because the
/// client asked for it with RFC 8707's `resource`. Only the store and the
/// kernel are borrowed from the stub cell, because a decision has to be
/// ledgered somewhere and that fixture already opens both.
///
/// `testdata/tokens/README.md` records the rauthy configuration this expects.
#[tokio::test]
async fn a_real_rauthy_admits_a_registered_client_by_scope() {
    let Ok(origin) = std::env::var(LIVE_URL) else {
        eprintln!(
            "skipped: set {LIVE_URL} to the origin of a running rauthy (for example \
             http://localhost:8080) to run AC-2; see testdata/tokens/README.md"
        );
        return;
    };
    let origin = origin.trim_end_matches('/').to_owned();
    let reg_token = required(LIVE_REG_TOKEN);
    let api_key = required(LIVE_API_KEY);
    let user = std::env::var(LIVE_USER).unwrap_or_else(|_| "admin@localhost".to_owned());
    let password = required(LIVE_PASSWORD);

    // The cell is this origin, so the resource it demands in `aud` is the
    // origin rauthy itself is published on (B-2, B-3). `RAHI_RAUTHY_ADDR` is
    // set from that same origin rather than left at its default, because the
    // discovery fetch goes to the loopback address and the whole point of
    // this test is that it reaches the rauthy the operator named.
    let loopback = loopback_addr(&origin);
    let env = BTreeMap::from([
        ("RAHI_PUBLIC_URL", origin.as_str()),
        ("RAHI_RAUTHY_ADDR", loopback.as_str()),
    ]);
    let config = Config::from_env(&env).expect("the origin is a well formed public url");
    let idp = IdpConfig::derive(&config, CLIENT_ID).expect("the identity configuration derives");

    // The handshake spec 021 owns. It is the first thing AC-2 needs and the
    // thing that was impossible before that spec's issuer carried rauthy's
    // trailing slash (021 D-10, this spec's D-11).
    let discovery = Discovery::fetch_within(&idp, Duration::from_secs(10))
        .await
        .expect("a real rauthy publishes a document this cell accepts");
    assert_eq!(
        discovery.issuer, idp.issuer,
        "the live issuer is the one this cell derives"
    );

    let live = Rauthy::new(&origin);
    live.create_scope(&api_key, LIVE_SCOPE_GRANTED).await;
    live.create_scope(&api_key, LIVE_SCOPE_WITHHELD).await;
    let client_id = live.register(&reg_token).await;
    live.allow_resource(&api_key, &client_id, &origin, LIVE_SCOPE_GRANTED)
        .await;

    let verifier = pkce_verifier();
    let code = live
        .authorize(&client_id, &user, &password, &verifier, &origin)
        .await;
    let token = live.exchange(&client_id, &code, &verifier, &origin).await;

    // The cell: rauthy's real key set, this origin's resource, and the store
    // and kernel of the stub fixture, which the stub itself never serves.
    let cell = oidc::boot().await;
    let jwks = rahi_idp::Jwks::load(&discovery)
        .await
        .expect("rauthy's key set loads");
    let resource = Resource::derive(&idp).expect("the resource derives");
    let server = ResourceServer::new(
        &config,
        resource,
        jwks,
        cell.sessions.store().clone(),
        cell.kernel.clone(),
    );

    let granted = with_scope(
        RequireScope::new(LIVE_SCOPE_GRANTED, cell.kernel.clone()),
        Router::new().route("/read", get(|| async { "read" })),
    );
    let withheld = with_scope(
        RequireScope::new(LIVE_SCOPE_WITHHELD, cell.kernel.clone()),
        Router::new().route("/write", get(|| async { "write" })),
    );
    let app = with_bearer(
        RequireBearer::new(server, BearerRoutes::new().route(API_PREFIX)),
        Router::new().nest(API_PREFIX, granted.merge(withheld)),
    );

    let admitted = send(&app, with_token("/api/read", &token)).await;
    assert_eq!(
        admitted.status,
        StatusCode::OK,
        "a rauthy token bound to this resource opens the scope it carries: {}",
        admitted.body
    );

    let refused = send(&app, with_token("/api/write", &token)).await;
    assert_eq!(
        refused.status,
        StatusCode::FORBIDDEN,
        "the same token does not open a scope it was never granted: {}",
        refused.body
    );
    let challenge = refused
        .headers
        .get(header::WWW_AUTHENTICATE)
        .expect("a scope refusal carries the challenge")
        .to_str()
        .expect("the challenge is ascii");
    assert!(
        challenge.contains("insufficient_scope"),
        "the refusal names the failure: {challenge}"
    );
    assert!(
        challenge.contains(LIVE_SCOPE_WITHHELD),
        "the refusal names the scope it wanted: {challenge}"
    );
}

/// The value of `name`, or a failure saying which variable is missing.
///
/// Setting [`LIVE_URL`] is the opt in, so a run that has opted in and then
/// cannot reach rauthy is a failure rather than a skip.
fn required(name: &str) -> String {
    std::env::var(name)
        .unwrap_or_else(|_| panic!("{LIVE_URL} is set, so {name} must be too (AC-2's arrangement)"))
}

/// The socket address behind an origin, which is what `RAHI_RAUTHY_ADDR`
/// wants: a resolved `ip:port`, never a host name.
fn loopback_addr(origin: &str) -> String {
    let authority = origin.split_once("://").map_or(origin, |(_, rest)| rest);
    let authority = if authority.contains(':') {
        authority.to_owned()
    } else {
        format!("{authority}:80")
    };
    authority
        .to_socket_addrs()
        .unwrap_or_else(|err| panic!("{LIVE_URL} names {authority}, which does not resolve: {err}"))
        .next()
        .unwrap_or_else(|| panic!("{LIVE_URL} names {authority}, which resolves to nothing"))
        .to_string()
}

/// A PKCE verifier: 32 random bytes, base64url without padding (RFC 7636).
fn pkce_verifier() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("the system entropy source answers");
    URL_SAFE_NO_PAD.encode(bytes)
}

/// The S256 challenge for a verifier.
fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(digest(&SHA256, verifier.as_bytes()))
}

/// A live rauthy, driven over its own HTTP API.
struct Rauthy {
    origin: String,
    http: reqwest::Client,
}

impl Rauthy {
    fn new(origin: &str) -> Self {
        Self {
            origin: origin.to_owned(),
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                // rauthy refuses a login with an empty `User-Agent`, and a
                // real command line client sends one.
                .user_agent("rahi-ac2")
                .build()
                .expect("a client builds"),
        }
    }

    /// Create a scope, tolerating the one that is already there.
    async fn create_scope(&self, api_key: &str, scope: &str) {
        let answer = self
            .http
            .post(format!("{}/auth/v1/scopes", self.origin))
            .header(header::AUTHORIZATION, format!("API-Key {api_key}"))
            .json(&json!({ "scope": scope }))
            .send()
            .await
            .expect("rauthy answers the scope request");
        let status = answer.status();
        assert!(
            status.is_success() || status == reqwest::StatusCode::BAD_REQUEST,
            "creating the scope {scope} answered {status}: {}",
            answer.text().await.unwrap_or_default()
        );
    }

    /// Register a public client through RFC 7591 dynamic registration (B-7).
    async fn register(&self, reg_token: &str) -> String {
        let answer = self
            .http
            .post(format!("{}/auth/v1/clients_dyn", self.origin))
            .header(header::AUTHORIZATION, format!("Bearer {reg_token}"))
            .json(&json!({
                "client_name": "rahi-ac2",
                "redirect_uris": [LIVE_REDIRECT],
                "grant_types": ["authorization_code", "refresh_token"],
                "token_endpoint_auth_method": "none",
            }))
            .send()
            .await
            .expect("rauthy answers the registration");
        let status = answer.status();
        let body = answer.text().await.unwrap_or_default();
        assert!(
            status.is_success(),
            "dynamic registration answered {status}: {body} (a 429 means rauthy's \
             `dynamic_clients.rate_limit_sec` window has not elapsed since the last run)"
        );
        let body: Value = serde_json::from_str(&body)
            .unwrap_or_else(|err| panic!("the registration is json ({err}): {body}"));
        body["client_id"]
            .as_str()
            .expect("a registered client has an id")
            .to_owned()
    }

    /// Bind the client to this resource and grant it one scope.
    ///
    /// Both halves are rauthy's to enforce: without `allowed_resources` it
    /// refuses the `resource` parameter with `invalid_target`, so no token
    /// could carry the audience B-3 demands, and the signing algorithm has to
    /// be RS256 because that is the only one this chassis verifies (B-3).
    async fn allow_resource(&self, api_key: &str, client_id: &str, resource: &str, scope: &str) {
        let answer = self
            .http
            .put(format!(
                "{}/auth/v1/clients/{}",
                self.origin,
                urlencoding(client_id)
            ))
            .header(header::AUTHORIZATION, format!("API-Key {api_key}"))
            .json(&json!({
                "id": client_id,
                "name": "rahi-ac2",
                "enabled": true,
                "confidential": false,
                "redirect_uris": [LIVE_REDIRECT],
                "flows_enabled": ["authorization_code", "refresh_token"],
                "access_token_alg": "RS256",
                "id_token_alg": "RS256",
                "auth_code_lifetime": 60,
                "access_token_lifetime": 1800,
                "scopes": ["openid", "profile", "email", "groups", scope],
                "default_scopes": ["openid", scope],
                "challenges": ["S256"],
                "force_mfa": false,
                "claims_at_root": false,
                "allowed_resources": [resource],
            }))
            .send()
            .await
            .expect("rauthy answers the client update");
        let status = answer.status();
        assert!(
            status.is_success(),
            "binding the client to {resource} answered {status}: {}",
            answer.text().await.unwrap_or_default()
        );
    }

    /// An anonymous session, which rauthy's login endpoint requires.
    async fn session(&self) -> (String, String) {
        let answer = self
            .http
            .post(format!("{}/auth/v1/oidc/session", self.origin))
            .send()
            .await
            .expect("rauthy answers the session request");
        let cookie = answer
            .headers()
            .get(header::SET_COOKIE)
            .expect("a new session sets a cookie")
            .to_str()
            .expect("the cookie is ascii")
            .split(';')
            .next()
            .expect("a cookie has a first pair")
            .to_owned();
        let body: Value = answer.json().await.expect("the session is json");
        let csrf = body["csrf_token"]
            .as_str()
            .expect("a session carries a csrf token")
            .to_owned();
        (cookie, csrf)
    }

    /// Fetch a proof of work challenge and solve it.
    ///
    /// The challenge is `version:difficulty:expiry:salt:hash:`, and the answer
    /// appends the smallest counter whose SHA-256 has `difficulty` leading
    /// zero bits. rauthy asks for this before it will read a password.
    async fn proof_of_work(&self) -> String {
        let challenge = self
            .http
            .post(format!("{}/auth/v1/pow", self.origin))
            .send()
            .await
            .expect("rauthy answers the proof of work request")
            .text()
            .await
            .expect("the challenge is text");
        let challenge = challenge.trim().to_owned();
        let difficulty: u32 = challenge
            .get(2..4)
            .and_then(|field| field.parse().ok())
            .expect("the challenge states its difficulty");

        for counter in 0u64.. {
            let attempt = format!("{challenge}{counter}");
            if leading_zero_bits(digest(&SHA256, attempt.as_bytes()).as_ref()) >= difficulty {
                return attempt;
            }
        }
        unreachable!("the counter is unbounded")
    }

    /// Complete the authorization code login and read the code rauthy puts in
    /// the `Location` of its `202`.
    async fn authorize(
        &self,
        client_id: &str,
        user: &str,
        password: &str,
        verifier: &str,
        resource: &str,
    ) -> String {
        let (cookie, csrf) = self.session().await;
        let answer = self
            .http
            .post(format!("{}/auth/v1/oidc/authorize", self.origin))
            .header(header::COOKIE, cookie)
            .header("x-csrf-token", csrf)
            .json(&json!({
                "email": user,
                "password": password,
                "pow": self.proof_of_work().await,
                "client_id": client_id,
                "redirect_uri": LIVE_REDIRECT,
                "scopes": ["openid", LIVE_SCOPE_GRANTED],
                "code_challenge": pkce_challenge(verifier),
                "code_challenge_method": "S256",
                "resource": resource,
            }))
            .send()
            .await
            .expect("rauthy answers the authorization request");
        let status = answer.status();
        let location = answer
            .headers()
            .get(header::LOCATION)
            .map(|value| value.to_str().expect("the location is ascii").to_owned());
        assert!(
            status.is_success(),
            "the authorization request answered {status}: {}",
            answer.text().await.unwrap_or_default()
        );
        let location = location.expect("a completed login redirects to the loopback");
        query_param(&location, "code").expect("the redirect carries the authorization code")
    }

    /// Exchange the code for an access token bound to `resource` (RFC 8707).
    async fn exchange(
        &self,
        client_id: &str,
        code: &str,
        verifier: &str,
        resource: &str,
    ) -> String {
        let answer = self
            .http
            .post(format!("{}/auth/v1/oidc/token", self.origin))
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("client_id", client_id),
                ("redirect_uri", LIVE_REDIRECT),
                ("code_verifier", verifier),
                ("resource", resource),
            ])
            .send()
            .await
            .expect("rauthy answers the token request");
        let status = answer.status();
        let body = answer.text().await.unwrap_or_default();
        assert!(
            status.is_success(),
            "the token request answered {status}: {body}"
        );
        let body: Value = serde_json::from_str(&body)
            .unwrap_or_else(|err| panic!("the token response is json ({err}): {body}"));
        body["access_token"]
            .as_str()
            .expect("the response carries an access token")
            .to_owned()
    }
}

/// How many leading zero bits `bytes` opens with.
fn leading_zero_bits(bytes: &[u8]) -> u32 {
    let mut bits = 0;
    for byte in bytes {
        bits += byte.leading_zeros();
        if *byte != 0 {
            break;
        }
    }
    bits
}

/// Percent-encode the one character a rauthy client id carries that a path
/// segment must not: the `$` of a dynamically registered id.
fn urlencoding(value: &str) -> String {
    value.replace('$', "%24")
}
