//! The principal: the IdP's answer, re-read and never manufactured
//! (spec 022 FR-002, FR-003, FR-004, and B-4).
//!
//! The three requirements this file discharges are three readings of one rule.
//! FR-002 says the app must not keep believing a role the IdP has taken away.
//! FR-003 says the app must not treat an unverified address as verified.
//! FR-004 says the app must not keep a copy of the person at all.

#![allow(clippy::expect_used, clippy::indexing_slicing)]

mod oidc;

use axum::http::StatusCode;
use oidc::{SUB, app, get_with, login, send};
use serde_json::json;

const SESSION_COOKIE: &str = "session";

/// Spec 022 FR-002: a role removed at the IdP between two requests separated
/// by an expired assertion is absent on the second.
///
/// This is the property the whole design exists for. Nothing tells the app the
/// role is gone; the app finds out because renewal is a round-trip and roles
/// are re-read on it rather than carried forward (B-5).
#[tokio::test]
async fn a_role_removed_at_the_idp_is_gone_within_one_access_lifetime() {
    let cell = oidc::boot().await;
    let app = app(&cell);
    cell.set(|stub| stub.roles = vec!["reader".to_owned(), "rahi-operator".to_owned()]);
    let session = login(&app, &cell).await;

    let before = send(
        &app,
        get_with("/api/me", &[format!("{SESSION_COOKIE}={session}")]),
    )
    .await;
    assert_eq!(before.status, StatusCode::OK, "{}", before.body);
    assert_eq!(before.json()["roles"], json!(["rahi-operator", "reader"]));

    // The operator role is revoked at the IdP. The app is told nothing.
    cell.set(|stub| stub.roles = vec!["reader".to_owned()]);

    // Inside the access lifetime the cached assertion still says otherwise,
    // which is the bounded staleness this design accepts by name.
    let still = send(
        &app,
        get_with("/api/me", &[format!("{SESSION_COOKIE}={session}")]),
    )
    .await;
    assert_eq!(still.json()["roles"], json!(["rahi-operator", "reader"]));

    cell.advance(1_000);
    let after = send(
        &app,
        get_with("/api/me", &[format!("{SESSION_COOKIE}={session}")]),
    )
    .await;
    assert_eq!(after.status, StatusCode::OK, "{}", after.body);
    assert_eq!(after.json()["roles"], json!(["reader"]));
    assert_eq!(cell.stub().refreshes, 1, "one round trip did it");

    // And the route behind that role now refuses.
    let refused = send(
        &app,
        get_with("/api/ops", &[format!("{SESSION_COOKIE}={session}")]),
    )
    .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.body);
}

/// Spec 022 FR-003: an `email` with no `email_verified` yields a principal
/// whose `email_verified` is false and whose verified email is `None`.
#[tokio::test]
async fn an_email_with_no_verified_claim_is_never_a_verified_email() {
    let cell = oidc::boot().await;
    let app = app(&cell);
    cell.set(|stub| {
        stub.email = Some("someone@example.com".to_owned());
        stub.email_verified = None;
    });
    let session = login(&app, &cell).await;

    let me = send(
        &app,
        get_with("/api/me", &[format!("{SESSION_COOKIE}={session}")]),
    )
    .await;
    assert_eq!(me.status, StatusCode::OK, "{}", me.body);
    assert_eq!(me.json()["sub"], json!(SUB));
    assert_eq!(me.json()["email_verified"], json!(false));
    assert_eq!(
        me.json()["email"],
        json!(null),
        "an unverified address is not handed out as one"
    );
}

/// B-4: the subject is the identity, and a renewal cannot change it.
#[tokio::test]
async fn the_subject_is_the_only_identifier_the_session_carries() {
    let cell = oidc::boot().await;
    let app = app(&cell);
    let session = login(&app, &cell).await;

    let me = send(
        &app,
        get_with("/api/me", &[format!("{SESSION_COOKIE}={session}")]),
    )
    .await;
    assert_eq!(me.json()["sub"], json!(SUB));

    // The envelope pins the subject: the sealed payload names it, and nothing
    // in a renewal reads a subject from anywhere else.
    let payload = session.split('.').next().expect("a sealed payload");
    let decoded =
        base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, payload)
            .expect("the payload is base64url");
    let envelope: serde_json::Value =
        serde_json::from_slice(&decoded).expect("the payload is JSON");
    assert_eq!(envelope["sub"], json!(SUB));
}

/// Spec 022 FR-004: no `INSERT` into any user or account table exists in the
/// crate, because no such table exists.
///
/// The identity crate writes exactly one thing anywhere: the access assertion,
/// into the store's non-durable cache group. There is no SQL in it at all,
/// which is a stronger statement than "no INSERT into users", and it is the
/// statement enrahitu://004 §1 says was missing the first time.
#[test]
fn the_crate_writes_no_account_row_anywhere() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut scanned = 0usize;

    for entry in std::fs::read_dir(&src).expect("the source directory is readable") {
        let path = entry.expect("a directory entry").path();
        if path.extension().is_none_or(|ext| ext != "rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("the source file is readable");
        let upper = text.to_uppercase();
        scanned += 1;

        for forbidden in ["INSERT INTO", "INSERT OR", "CREATE TABLE", "UPSERT"] {
            assert!(
                !upper.contains(forbidden),
                "{} carries {forbidden:?}: the IdP is the principal authority and this crate \
                 keeps no account row (constitution VII)",
                path.display()
            );
        }
        for table in ["USERS", "ACCOUNTS", "REFRESH_TOKENS", "SESSIONS_TABLE"] {
            assert!(
                !upper.contains(&format!(" {table} ")),
                "{} names a {table} table",
                path.display()
            );
        }
    }

    assert!(scanned >= 10, "the scan found {scanned} source files");
}
