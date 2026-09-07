//! The session, end to end against an in-process OIDC stub (spec 022
//! FR-001, and B-1, B-2, B-5, B-6, B-7).
//!
//! Every test here drives a real axum router over a real single-voter store
//! and a real signed id token. Nothing about the login flow is mocked: the
//! code exchange is a form post to a listener, the id token's RS256 signature
//! is checked against a key set fetched over a socket, and the assertion lives
//! in the store's cache group.

#![allow(clippy::expect_used, clippy::indexing_slicing)]

mod oidc;

use axum::http::StatusCode;
use oidc::{SUB, app, get_with, login, query_param, request, send, sign};
use rahi_idp::{SESSION_PREFIX, SESSION_RATE_LIMIT};
use serde_json::json;

/// The cookie names over plain `http`, which is what the fixture origin is.
const SESSION_COOKIE: &str = "session";
const LOGIN_COOKIE: &str = "login";

/// Spec 022 FR-001: the round trip establishes a session whose `sub` is the
/// stub's, and the browser never holds anything but a sealed envelope.
#[tokio::test]
async fn a_login_round_trip_establishes_a_session_with_the_idps_subject() {
    let cell = oidc::boot().await;
    let app = app(&cell);

    let started = send(&app, get_with("/session/login", &[])).await;
    assert_eq!(started.status, StatusCode::SEE_OTHER);
    let authorize = started.location().to_owned();
    assert_eq!(
        query_param(&authorize, "code_challenge_method").as_deref(),
        Some("S256"),
        "PKCE is not optional (B-1)"
    );
    assert!(query_param(&authorize, "code_challenge").is_some());
    assert_eq!(
        query_param(&authorize, "redirect_uri").as_deref(),
        Some(format!("{}/session/callback", cell.sessions.origin()).as_str()),
        "the callback lives outside the raw proxy prefix (B-1)"
    );
    let login_cookie = started
        .set_cookies()
        .into_iter()
        .find(|cookie| cookie.starts_with("login="))
        .expect("the login cookie is set")
        .to_owned();
    assert!(login_cookie.contains("HttpOnly"), "{login_cookie}");

    let session = login(&app, &cell).await;

    let me = send(
        &app,
        get_with("/api/me", &[format!("{SESSION_COOKIE}={session}")]),
    )
    .await;
    assert_eq!(me.status, StatusCode::OK, "{}", me.body);
    assert_eq!(me.json()["sub"], json!(SUB));
    assert_eq!(me.json()["roles"], json!(["reader"]));

    // The envelope carries the subject and the refresh token and nothing that
    // reads as a claim: no roles, no email, no verification flag (B-2).
    let payload = session.split('.').next().expect("a sealed payload");
    let decoded = String::from_utf8(base64_decode(payload).expect("the payload is base64url"))
        .expect("the payload is text");
    assert!(decoded.contains(SUB), "{decoded}");
    assert!(
        !decoded.contains("reader"),
        "no roles in the cookie: {decoded}"
    );
    assert!(
        !decoded.contains("email"),
        "no email in the cookie: {decoded}"
    );
}

/// Spec 022 FR-001: a tampered envelope is rejected.
#[tokio::test]
async fn a_tampered_envelope_is_rejected_and_the_session_ends() {
    let cell = oidc::boot().await;
    let app = app(&cell);
    let session = login(&app, &cell).await;

    // One byte of the sealed payload, rewritten. The tag no longer covers it.
    let (payload, mac) = session.rsplit_once('.').expect("a sealed value");
    let forged = format!("{}A.{mac}", &payload[..payload.len() - 1]);

    let refused = send(
        &app,
        get_with("/api/me", &[format!("{SESSION_COOKIE}={forged}")]),
    )
    .await;
    assert_eq!(refused.status, StatusCode::UNAUTHORIZED, "{}", refused.body);
    assert_eq!(refused.json()["error"], json!("unauthorized"));
    assert!(
        refused.cleared(SESSION_COOKIE),
        "{:?}",
        refused.set_cookies()
    );
    assert!(refused.cleared(LOGIN_COOKIE), "{:?}", refused.set_cookies());
    assert_eq!(
        cell.stub().refreshes,
        0,
        "a forged cookie never reaches rauthy"
    );
}

/// Spec 022 FR-001: an expired assertion triggers exactly one refresh call,
/// and the envelope comes back rotated (B-5).
#[tokio::test]
async fn an_expired_assertion_triggers_exactly_one_refresh() {
    let cell = oidc::boot().await;
    let app = app(&cell);
    let session = login(&app, &cell).await;
    assert_eq!(cell.stub().refreshes, 0);

    // Inside the access lifetime the cached assertion answers and rauthy is
    // not asked at all.
    let cached = send(
        &app,
        get_with("/api/me", &[format!("{SESSION_COOKIE}={session}")]),
    )
    .await;
    assert_eq!(cached.status, StatusCode::OK);
    assert_eq!(cell.stub().refreshes, 0, "a current assertion asks nobody");

    cell.advance(1_000);
    let renewed = send(
        &app,
        get_with("/api/me", &[format!("{SESSION_COOKIE}={session}")]),
    )
    .await;
    assert_eq!(renewed.status, StatusCode::OK, "{}", renewed.body);
    assert_eq!(cell.stub().refreshes, 1, "exactly one refresh");

    let rotated = renewed
        .cookie(SESSION_COOKIE)
        .expect("the envelope is rotated (B-5)");
    assert_ne!(rotated, session, "the rotated envelope is a new one");

    let again = send(
        &app,
        get_with("/api/me", &[format!("{SESSION_COOKIE}={rotated}")]),
    )
    .await;
    assert_eq!(again.status, StatusCode::OK);
    assert_eq!(
        cell.stub().refreshes,
        1,
        "the freshly minted assertion answers without a second round trip"
    );
}

/// Spec 022 FR-001: a refused refresh answers 401 and clears both cookies.
#[tokio::test]
async fn a_refused_refresh_answers_401_and_clears_both_cookies() {
    let cell = oidc::boot().await;
    let app = app(&cell);
    let session = login(&app, &cell).await;

    cell.advance(1_000);
    cell.set(|stub| stub.refuse_refresh = true);

    let refused = send(
        &app,
        get_with("/api/me", &[format!("{SESSION_COOKIE}={session}")]),
    )
    .await;
    assert_eq!(refused.status, StatusCode::UNAUTHORIZED, "{}", refused.body);
    assert_eq!(cell.stub().refreshes, 1);
    assert!(
        refused.cleared(SESSION_COOKIE),
        "{:?}",
        refused.set_cookies()
    );
    assert!(refused.cleared(LOGIN_COOKIE), "{:?}", refused.set_cookies());
}

/// B-1: the callback is matched to the login that started it.
#[tokio::test]
async fn a_callback_whose_state_is_not_this_logins_is_refused() {
    let cell = oidc::boot().await;
    let app = app(&cell);

    let started = send(&app, get_with("/session/login", &[])).await;
    let login_cookie = started.cookie(LOGIN_COOKIE).expect("a login cookie");

    let refused = send(
        &app,
        get_with(
            "/session/callback?code=the-one-code&state=not-the-state",
            &[format!("{LOGIN_COOKIE}={login_cookie}")],
        ),
    )
    .await;
    assert_eq!(refused.status, StatusCode::UNAUTHORIZED, "{}", refused.body);
    assert!(refused.cleared(LOGIN_COOKIE));
    assert_eq!(
        cell.stub().code_exchanges,
        0,
        "a mismatched state never reaches the token endpoint"
    );
}

/// B-1: a callback with no login cookie at all has nothing to be matched to.
#[tokio::test]
async fn a_callback_with_no_login_cookie_is_refused() {
    let cell = oidc::boot().await;
    let app = app(&cell);
    let refused = send(
        &app,
        get_with("/session/callback?code=the-one-code&state=anything", &[]),
    )
    .await;
    assert_eq!(refused.status, StatusCode::UNAUTHORIZED);
    assert_eq!(cell.stub().code_exchanges, 0);
}

/// B-1: the id token's nonce binds it to the login this browser started.
#[tokio::test]
async fn an_id_token_carrying_another_logins_nonce_is_refused() {
    let cell = oidc::boot().await;
    let app = app(&cell);

    let started = send(&app, get_with("/session/login", &[])).await;
    let authorize = started.location().to_owned();
    let state = query_param(&authorize, "state").expect("state");
    let login_cookie = started.cookie(LOGIN_COOKIE).expect("a login cookie");
    cell.set(|stub| stub.nonce = Some("a-nonce-from-another-login".to_owned()));

    let refused = send(
        &app,
        get_with(
            &format!("/session/callback?code=the-one-code&state={state}"),
            &[format!("{LOGIN_COOKIE}={login_cookie}")],
        ),
    )
    .await;
    assert_eq!(refused.status, StatusCode::UNAUTHORIZED, "{}", refused.body);
    assert!(
        refused.body.contains("nonce"),
        "the refusal names what failed: {}",
        refused.body
    );
}

/// B-1: a token signed with a key rauthy does not publish never verifies,
/// however well formed the rest of it is.
#[tokio::test]
async fn an_id_token_naming_an_unpublished_key_is_refused() {
    let cell = oidc::boot().await;
    let forged = sign(
        &json!({ "alg": "RS256", "typ": "JWT", "kid": "not-a-published-kid" }),
        &json!({
            "iss": format!("{}/auth/v1", cell.sessions.origin()),
            "aud": "hello-cell",
            "exp": oidc::T0 + 3600,
            "sub": SUB,
            "nonce": "n",
        }),
    );
    let err = cell
        .sessions
        .verify_id_token(&forged, "n")
        .await
        .expect_err("an unknown key id is refused");
    assert_eq!(err.kind(), "unauthorized");
}

/// B-1: `alg: none` is not a signature, and is never treated as one.
#[tokio::test]
async fn an_unsigned_id_token_is_refused() {
    let cell = oidc::boot().await;
    let payload = json!({
        "iss": format!("{}/auth/v1", cell.sessions.origin()),
        "aud": "hello-cell",
        "exp": oidc::T0 + 3600,
        "sub": SUB,
        "nonce": "n",
    });
    let unsigned = format!(
        "{}.{}.",
        base64_encode(br#"{"alg":"none","typ":"JWT"}"#),
        base64_encode(serde_json::to_vec(&payload).expect("serialises").as_slice()),
    );
    let err = cell
        .sessions
        .verify_id_token(&unsigned, "n")
        .await
        .expect_err("alg none is refused");
    assert_eq!(err.kind(), "unauthorized");
    assert!(err.message().contains("none"), "{err}");
}

/// B-7: logout revokes the refresh token, forgets the assertion, clears both
/// cookies, and sends the browser to the end-session endpoint.
#[tokio::test]
async fn logout_revokes_the_token_and_ends_the_session() {
    let cell = oidc::boot().await;
    let app = app(&cell);
    let session = login(&app, &cell).await;

    let out = send(
        &app,
        request(
            "POST",
            "/session/logout",
            &[format!("{SESSION_COOKIE}={session}")],
        ),
    )
    .await;
    assert_eq!(out.status, StatusCode::SEE_OTHER, "{}", out.body);
    assert!(
        out.location().contains("/auth/v1/oidc/logout"),
        "{}",
        out.location()
    );
    assert!(out.cleared(SESSION_COOKIE), "{:?}", out.set_cookies());
    assert!(out.cleared(LOGIN_COOKIE), "{:?}", out.set_cookies());
    assert_eq!(cell.stub().revocations, 1);

    // The assertion is gone, so the same envelope now costs a round trip; the
    // stub still holds the old refresh token, so that round trip is refused.
    cell.set(|stub| stub.refuse_refresh = true);
    let after = send(
        &app,
        get_with("/api/me", &[format!("{SESSION_COOKIE}={session}")]),
    )
    .await;
    assert_eq!(after.status, StatusCode::UNAUTHORIZED, "{}", after.body);
}

/// B-6: a request with no session reaches nothing that requires one.
#[tokio::test]
async fn a_request_with_no_session_is_refused_by_an_authenticated_route() {
    let cell = oidc::boot().await;
    let app = app(&cell);
    let refused = send(&app, get_with("/api/me", &[])).await;
    assert_eq!(refused.status, StatusCode::UNAUTHORIZED);
    assert_eq!(refused.json()["error"], json!("unauthorized"));
}

/// B-6: a principal without the role gets 403 and the chain gets a record.
#[tokio::test]
async fn a_role_the_principal_lacks_is_403_with_a_ledgered_decision() {
    let cell = oidc::boot().await;
    let app = app(&cell);
    let session = login(&app, &cell).await;

    let refused = send(
        &app,
        get_with("/api/ops", &[format!("{SESSION_COOKIE}={session}")]),
    )
    .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.body);
    assert_eq!(refused.json()["error"], json!("denied"));
    let decision = refused.json()["decision"]
        .as_str()
        .expect("the refusal names the decision the chain holds")
        .to_owned();
    assert!(decision.starts_with("kernel:"), "{decision}");

    cell.kernel
        .flush(std::time::Duration::from_secs(5))
        .await
        .expect("the appender drains");

    // The same principal, once the IdP grants the role, is admitted. The role
    // is re-read on renewal and nowhere else (B-5).
    cell.set(|stub| stub.roles = vec!["reader".to_owned(), "rahi-operator".to_owned()]);
    cell.advance(1_000);
    let admitted = send(
        &app,
        get_with("/api/ops", &[format!("{SESSION_COOKIE}={session}")]),
    )
    .await;
    assert_eq!(admitted.status, StatusCode::OK, "{}", admitted.body);
}

/// B-8: the session routes are their own rate limit group, and the number the
/// app declares on the edge's limiter lives here.
#[test]
fn the_session_routes_declare_their_own_rate_limit_group() {
    assert_eq!(SESSION_PREFIX, "/session");
    assert_eq!(SESSION_RATE_LIMIT, 30);
}

fn base64_decode(value: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(value)
        .ok()
}

fn base64_encode(value: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value)
}
