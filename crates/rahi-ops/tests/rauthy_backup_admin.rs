//! spec 037 B-1 and FR-006, against a live rauthy: the dedicated backup
//! admin is provisioned, is passkey only, completes rauthy's admin MFA from
//! a process holding nothing but the custodied key, and takes a snapshot
//! that is this call's own and not the one already on disk.
//!
//! Gated on `RAHI_TEST_RAUTHY_URL` naming a running rauthy and
//! `RAHI_TEST_RAUTHY_API_KEY` carrying an admin API key as `name$secret`,
//! the two variables spec 025's live bearer test already reads. Without
//! them the test says which is missing and passes, unless
//! `RAHI_REQUIRE_RAUTHY=1` is set, which turns every such skip into a
//! failure (B-4).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::time::{Duration, Instant};

use rahi_ops::rauthy_api::RauthyApi;
use rahi_ops::rauthy_session::{self, Passkey, Provisioned};
use rahi_types::Config;

const LIVE_URL: &str = "RAHI_TEST_RAUTHY_URL";
const LIVE_API_KEY: &str = "RAHI_TEST_RAUTHY_API_KEY";
const REQUIRE: &str = "RAHI_REQUIRE_RAUTHY";

/// B-4: a skip is loud when the runner said rauthy must be there.
fn skip(what: &str) -> bool {
    let message = format!("skipped: {what}");
    assert!(
        std::env::var(REQUIRE).as_deref() != Ok("1"),
        "{REQUIRE}=1 and {message}"
    );
    eprintln!("{message}");
    true
}

fn live() -> Option<(String, String)> {
    let (Ok(url), Ok(key)) = (std::env::var(LIVE_URL), std::env::var(LIVE_API_KEY)) else {
        skip(&format!(
            "set {LIVE_URL} and {LIVE_API_KEY} to run the live backup admin proof \
             (spec 037 B-1)"
        ));
        return None;
    };
    Some((url.trim_end_matches('/').to_owned(), key))
}

fn config(origin: &str) -> Config {
    let env = std::collections::BTreeMap::from([("RAHI_PUBLIC_URL", origin)]);
    Config::from_env(&env).expect("the origin parses")
}

fn ts_of(name: &str) -> i64 {
    rahi_store::backup::snapshot_ts(name).unwrap_or_else(|| panic!("{name} carries no timestamp"))
}

/// The whole of B-1 against the pinned release, in one test because one
/// rauthy holds one backup admin and one registered credential: provision,
/// log in with the custodied key alone, take two snapshots inside one
/// suppression window, and refuse a deadline that cannot outlast it.
/// FR-006.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_backup_admin_takes_a_fresh_snapshot_twice_inside_the_window() {
    let Some((base, api_key)) = live() else {
        return;
    };
    let config = config(&base);

    // The key set mints the passkey; rauthy never sees the private half.
    let passkey = Passkey::generate(&config).expect("the passkey mints");
    assert_eq!(passkey.email(), rauthy_session::BACKUP_ADMIN_EMAIL);

    let outcome = rauthy_session::ensure_backup_admin(&base, &api_key, &passkey)
        .await
        .expect("the backup admin is provisioned");
    assert_eq!(
        outcome,
        Provisioned::Created,
        "a freshly minted credential is a new registration"
    );

    // Idempotent: the same call again finds a passkey-only admin and does
    // nothing, which is what every start after the first does.
    assert_eq!(
        rauthy_session::ensure_backup_admin(&base, &api_key, &passkey)
            .await
            .expect("the second call is a no-op"),
        Provisioned::AlreadyPresent
    );

    // A process holding only the custodied document: no password exists.
    let custodied = passkey.to_json().expect("serialises");
    assert!(
        !custodied.contains("password"),
        "the custodied document holds no password: {custodied}"
    );
    let reloaded = Passkey::from_json(&custodied).expect("reads back");
    let api = RauthyApi::new(&base, &api_key)
        .expect("the client builds")
        .with_passkey(Some(reloaded));

    let first = Instant::now();
    let (one, bytes_one) = api.backup().await.expect("the first backup");
    eprintln!(
        "037 FR-006: first snapshot {one} ({} bytes) in {:?}",
        bytes_one.len(),
        first.elapsed()
    );
    assert!(bytes_one.starts_with(b"SQLite format 3"), "a database");

    // Inside hiqlite's suppression window, which rauthy's store has too:
    // the second backup waits it out rather than reporting the first.
    let second = Instant::now();
    let (two, bytes_two) = api.backup().await.expect("the second backup");
    eprintln!(
        "037 FR-006: second snapshot {two} ({} bytes) in {:?}",
        bytes_two.len(),
        second.elapsed()
    );
    assert_ne!(one, two, "the second backup is not the first file");
    assert!(
        ts_of(&two) > ts_of(&one),
        "the second snapshot was taken after the first: {one} then {two}"
    );
    assert!(
        second.elapsed() >= Duration::from_secs(30),
        "the second backup waited the window out rather than reporting a stale file"
    );
    assert!(bytes_two.starts_with(b"SQLite format 3"), "a database");

    // A deadline shorter than the remaining window is a refusal naming the
    // deadline, never the file that is already there (B-1, D-4). The two
    // backups above put the window in place.
    let err = api
        .backup_within(Duration::from_secs(5))
        .await
        .expect_err("a deadline inside the window is a refusal");
    let text = err.to_string();
    assert!(text.contains("5 second deadline"), "{text}");
    assert!(text.contains("ignores a backup request"), "{text}");
    eprintln!("037 FR-006: inside the window, a short deadline says: {text}");
}

/// A key set with no passkey is told so by name, not by a `401` from
/// rauthy (B-1).
#[tokio::test]
async fn a_key_set_without_a_passkey_is_named() {
    let api = RauthyApi::new("http://127.0.0.1:1", "rahi$secret").expect("the client builds");
    let err = api.backup().await.expect_err("nothing to log in with");
    let text = err.to_string();
    assert!(text.contains("backup_passkey.json"), "{text}");
    assert!(text.contains("first-boot"), "{text}");
}
