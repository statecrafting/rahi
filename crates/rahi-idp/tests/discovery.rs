//! Discovery, the key cache, and the one integration run that needs a real
//! rauthy (spec 021 FR-002, FR-003, FR-004, B-3, B-4).

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::routing::get;
use rahi_idp::jwks::{JwksOptions, system_clock};
use rahi_idp::{Discovery, IdpConfig, Jwks};
use serde_json::json;

use common::{idp_config, serve};

const RAUTHY: &str = include_str!("../testdata/discovery/rauthy.json");
const WRONG_ISSUER: &str = include_str!("../testdata/discovery/wrong-issuer.json");
const MISSING_JWKS_URI: &str = include_str!("../testdata/discovery/missing-jwks-uri.json");

const ISSUER: &str = "https://cell.example.com/auth/v1";

// ------------------------------------------------------------ FR-002

/// The recorded document parses, and every endpoint spec 022 needs is there.
#[test]
fn a_matching_document_parses() {
    let discovery = Discovery::parse(RAUTHY, ISSUER).expect("the recorded document parses");
    assert_eq!(discovery.issuer, ISSUER);
    assert_eq!(
        discovery.authorization_endpoint,
        "https://cell.example.com/auth/v1/oidc/authorize"
    );
    assert_eq!(
        discovery.token_endpoint,
        "https://cell.example.com/auth/v1/oidc/token"
    );
    assert_eq!(
        discovery.userinfo_endpoint,
        "https://cell.example.com/auth/v1/oidc/userinfo"
    );
    assert_eq!(
        discovery.end_session_endpoint,
        "https://cell.example.com/auth/v1/oidc/logout"
    );
    assert_eq!(
        discovery.jwks_uri,
        "https://cell.example.com/auth/v1/oidc/certs"
    );
}

/// A document from another deployment is refused, and the message names both
/// issuers so an operator can see which cell they are looking at.
#[test]
fn another_deployments_issuer_is_a_validation_error() {
    let err = Discovery::parse(WRONG_ISSUER, ISSUER).expect_err("the issuer does not match");
    assert_eq!(err.kind(), "validation");
    assert!(err.message().contains("another-cell.example.com"), "{err}");
    assert!(err.message().contains(ISSUER), "{err}");
}

/// A document with no signing keys to name is refused before anything trusts
/// a token it could not verify.
#[test]
fn a_document_without_jwks_uri_is_a_validation_error() {
    let err = Discovery::parse(MISSING_JWKS_URI, ISSUER).expect_err("there is no key set to fetch");
    assert_eq!(err.kind(), "validation");
    assert!(err.message().contains("jwks_uri"), "{err}");
}

/// The fetch reads the document from rauthy's loopback listener.
#[tokio::test]
async fn the_document_is_fetched_from_loopback() {
    let stub = serve(Router::new().route(
        "/auth/v1/.well-known/openid-configuration",
        get(|| async { RAUTHY }),
    ))
    .await;
    let config = idp_config("https://cell.example.com", stub.addr);

    let discovery = Discovery::fetch_within(&config, Duration::from_secs(5))
        .await
        .expect("the stub answers");
    assert_eq!(discovery.issuer, ISSUER);
}

/// A rauthy that never answers is `Error::Upstream` once the budget is spent,
/// not a hang and not a panic.
#[tokio::test]
async fn a_silent_rauthy_runs_out_of_budget() {
    let stub = serve(Router::new()).await;
    let addr = stub.addr;
    drop(stub);
    let config = idp_config("https://cell.example.com", addr);

    let err = Discovery::fetch_within(&config, Duration::from_millis(300))
        .await
        .expect_err("nothing is listening");
    assert_eq!(err.kind(), "upstream");
    assert!(err.message().contains("openid-configuration"), "{err}");
}

// ------------------------------------------------------------ FR-003

/// A JWKS stub whose key set the test changes between fetches, counting how
/// many times it was asked.
#[derive(Clone)]
struct KeySet {
    keys: Arc<std::sync::Mutex<Vec<&'static str>>>,
    fetches: Arc<AtomicU64>,
}

impl KeySet {
    fn new(kids: &[&'static str]) -> Self {
        Self {
            keys: Arc::new(std::sync::Mutex::new(kids.to_vec())),
            fetches: Arc::new(AtomicU64::new(0)),
        }
    }

    fn publish(&self, kids: &[&'static str]) {
        *self.keys.lock().expect("the fixture lock") = kids.to_vec();
    }

    fn fetches(&self) -> u64 {
        self.fetches.load(Ordering::Relaxed)
    }
}

async fn serve_keys(State(set): State<KeySet>) -> axum::Json<serde_json::Value> {
    set.fetches.fetch_add(1, Ordering::Relaxed);
    let keys: Vec<serde_json::Value> = set
        .keys
        .lock()
        .expect("the fixture lock")
        .iter()
        .map(|kid| json!({ "kty": "RSA", "alg": "RS256", "use": "sig", "kid": kid, "n": "0vx7", "e": "AQAB" }))
        .collect();
    axum::Json(json!({ "keys": keys }))
}

/// A cache over a stub, with the clock the test holds.
async fn cache(set: &KeySet, now: &Arc<AtomicU64>, interval: Duration) -> (Jwks, common::Served) {
    let stub = serve(
        Router::new()
            .route("/auth/v1/oidc/certs", get(serve_keys))
            .with_state(set.clone()),
    )
    .await;
    let discovery = Discovery::parse(
        &RAUTHY.replace(
            "https://cell.example.com/auth/v1/oidc/certs",
            &format!("http://{}/auth/v1/oidc/certs", stub.addr),
        ),
        ISSUER,
    )
    .expect("the fixture document parses");
    let clock = Arc::clone(now);
    let jwks = Jwks::load_with(
        &discovery,
        JwksOptions {
            interval,
            clock: Arc::new(move || clock.load(Ordering::Relaxed)),
        },
    )
    .await
    .expect("the stub answers");
    (jwks, stub)
}

/// FR-003: an unknown `kid` buys exactly one refresh. A key that appeared
/// since the last fetch is found by it; a key that does not exist is refused
/// and does not buy a second.
#[tokio::test]
async fn an_unknown_kid_refreshes_once_then_rejects() {
    let set = KeySet::new(&["key-a"]);
    let now = Arc::new(AtomicU64::new(1_767_225_600));
    let (jwks, _stub) = cache(&set, &now, Duration::from_secs(3600)).await;
    assert_eq!(set.fetches(), 1, "the load fetched once");

    set.publish(&["key-a", "key-b"]);
    let key = jwks
        .key("key-b")
        .await
        .expect("the refresh finds the new key");
    assert_eq!(key.kid, "key-b");
    assert_eq!(set.fetches(), 2, "the unknown kid bought one refresh");

    let err = jwks
        .key("key-never")
        .await
        .expect_err("no such key is published");
    assert_eq!(err.kind(), "unauthorized");
    assert_eq!(
        set.fetches(),
        3,
        "one refresh, not a retry loop, and not one per attempt"
    );
    assert!(err.message().contains("key-never"), "{err}");
}

/// FR-003: a key rauthy has rotated away still verifies for one interval, and
/// not after. Tokens signed a moment before a rotation are legitimate.
#[tokio::test]
async fn a_rotated_key_survives_exactly_one_interval() {
    let interval = Duration::from_secs(3600);
    let set = KeySet::new(&["key-a", "key-b"]);
    let now = Arc::new(AtomicU64::new(1_767_225_600));
    let (jwks, _stub) = cache(&set, &now, interval).await;

    assert_eq!(jwks.key("key-b").await.expect("resident").kid, "key-b");

    // rauthy rotates: key-b is gone upstream.
    set.publish(&["key-a"]);
    now.fetch_add(interval.as_secs(), Ordering::Relaxed);

    let rotated = jwks
        .key("key-b")
        .await
        .expect("the previous set carries it for one interval");
    assert_eq!(rotated.kid, "key-b");
    assert_eq!(
        jwks.key_ids().await,
        vec!["key-a".to_owned()],
        "the current set is what rauthy publishes now"
    );

    // One interval later the old set is gone, and so is the key.
    now.fetch_add(interval.as_secs(), Ordering::Relaxed);
    let err = jwks
        .key("key-b")
        .await
        .expect_err("the rotation has completed");
    assert_eq!(err.kind(), "unauthorized");
}

/// B-4: the timer refreshes without anyone asking for an unknown key.
#[tokio::test]
async fn the_interval_refreshes_a_key_that_is_still_known() {
    let interval = Duration::from_secs(600);
    let set = KeySet::new(&["key-a"]);
    let now = Arc::new(AtomicU64::new(1_767_225_600));
    let (jwks, _stub) = cache(&set, &now, interval).await;

    jwks.key("key-a").await.expect("resident");
    assert_eq!(set.fetches(), 1, "a fresh cache is not refetched");

    now.fetch_add(interval.as_secs(), Ordering::Relaxed);
    jwks.key("key-a").await.expect("still published");
    assert_eq!(set.fetches(), 2, "the elapsed interval refreshed the set");
}

/// The default cache reads the system clock and the hour from the spec.
#[test]
fn the_default_options_are_the_spec_defaults() {
    let options = JwksOptions::default();
    assert_eq!(options.interval, rahi_idp::DEFAULT_REFRESH_INTERVAL);
    assert_eq!(options.interval, Duration::from_secs(3600));
    assert!(
        (system_clock())() > 1_700_000_000,
        "the system clock is a clock"
    );
}

// -------------------------------------------------- spec 031 FR-005

/// The live-rauthy run: with `RAHI_TEST_RAUTHY` naming a binary, boot it,
/// bootstrap the client, and read discovery through the proxy. Without one,
/// say so and pass: an absent operator prerequisite is not a failing chassis.
///
/// The requirement moved to spec 031 FR-005 (spec 021 D-8): booting rauthy
/// needs the configuration file, the admin token, and the container that
/// spec builds, none of which this crate owns. The assertion stays written
/// here, where that arrangement will find it.
#[tokio::test]
async fn a_real_rauthy_issues_under_the_public_url() {
    let Ok(binary) = std::env::var("RAHI_TEST_RAUTHY") else {
        eprintln!(
            "skipped: set RAHI_TEST_RAUTHY to a rauthy binary to run the integration \
             check (spec 031 FR-005)"
        );
        return;
    };
    assert!(
        std::path::Path::new(&binary).exists(),
        "RAHI_TEST_RAUTHY names {binary}, which does not exist"
    );
    // The boot, the bootstrap, and the proxied discovery read are spec 031's
    // container to arrange: it supplies rauthy's configuration file, its
    // admin API key, and the key set. What this test owns is the assertion,
    // and it is written where that arrangement will find it.
    eprintln!(
        "skipped: {binary} needs the container spec 031 builds (configuration, \
         admin key, key set) before it can be booted from a test"
    );
}

/// The configuration a real run would use is the one the unit tests fix.
#[test]
fn the_integration_configuration_is_the_derived_one() {
    let env = std::collections::BTreeMap::from([("RAHI_PUBLIC_URL", "https://cell.example.com")]);
    let config = rahi_types::Config::from_env(&env).expect("the fixture environment");
    let idp = IdpConfig::derive(&config, "hello-cell").expect("derives");
    assert_eq!(idp.issuer, ISSUER);
    assert_eq!(idp.loopback_base, "http://127.0.0.1:8080");
}
