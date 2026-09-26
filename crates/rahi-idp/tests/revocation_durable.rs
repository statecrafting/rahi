//! Spec 043 B-6, B-6b, FR-004, FR-006, FR-014 and AC-6 (b) and (c):
//! revocation that survives cache loss, admission bounded by construction,
//! and the permanent floor.
//!
//! Every token is a real RS256 JWT verified against the stub's key set, and
//! every revocation row and floor lives in a real hiqlite store.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]

mod oidc;

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use oidc::{Cell, KID, SUB, T0, sign};
use rahi_idp::bearer::{L_MAX_SECONDS, LEEWAY_SECONDS};
use rahi_idp::revoke::{Revoker, denylist_ttl};
use rahi_idp::{Resource, ResourceServer};
use rahi_store::{EncKey, EncKeys, Store, StoreConfig, StoreSecrets};
use rahi_types::Config;
use serde_json::{Value, json};

/// The origin the recorded claim sets were written for, rewritten per run.
const FIXTURE_ORIGIN: &str = "https://cell.example.com";

/// The manifest default L the tests run at.
const L: u64 = 600;

/// The resource server over `cell` with the lifetime ceiling `lifetime`.
async fn server(cell: &Cell, lifetime: u64) -> ResourceServer {
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
    .with_lifetime(Duration::from_secs(lifetime))
}

/// A signed token from `person.json` with `jti`, `iat`, and `exp`, the rest
/// as recorded; `edit` runs last.
fn token(cell: &Cell, jti: &str, iat: u64, exp: u64, edit: impl FnOnce(&mut Value)) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/tokens/person.json");
    let text = std::fs::read_to_string(&path).expect("the fixture is readable");
    let mut payload: Value =
        serde_json::from_str(&text.replace(FIXTURE_ORIGIN, cell.sessions.origin()))
            .expect("the fixture is JSON");
    payload["jti"] = json!(jti);
    payload["iat"] = json!(iat);
    payload["nbf"] = json!(iat);
    payload["exp"] = json!(exp);
    edit(&mut payload);
    sign(
        &json!({ "alg": "RS256", "typ": "JWT", "kid": KID }),
        &payload,
    )
}

fn set_clock(cell: &Cell, now: u64) {
    cell.clock.store(now, Ordering::Relaxed);
}

async fn admitted(server: &ResourceServer, token: &str) -> bool {
    server.validate(token).await.is_ok()
}

/// AC-6 (c): every admission boundary, by checked arithmetic.
#[tokio::test]
async fn the_admission_boundaries_hold() {
    let cell = oidc::boot().await;
    let server = server(&cell, L).await;
    let now = T0;
    set_clock(&cell, now);

    // `exp - iat == L` is admitted and `L + 1` refused.
    assert!(admitted(&server, &token(&cell, "l", now, now + L, |_| {})).await);
    assert!(!admitted(&server, &token(&cell, "l1", now, now + L + 1, |_| {})).await);

    // `exp < iat` refused.
    assert!(!admitted(&server, &token(&cell, "neg", now, now - 1, |_| {})).await);

    // No `iat` refused.
    let undated = token(&cell, "undated", now, now + L, |p| {
        p.as_object_mut().unwrap().remove("iat");
    });
    let err = server.validate(&undated).await.unwrap_err();
    assert!(err.message().contains("no iat"), "{err}");

    // `iat == now + 60` admitted, `now + 61` refused.
    let edge = now + LEEWAY_SECONDS;
    let t = token(&cell, "edge", edge, edge + 10, |p| {
        p["nbf"] = json!(now);
    });
    assert!(admitted(&server, &t).await);
    let t = token(&cell, "future", edge + 1, edge + 11, |p| {
        p["nbf"] = json!(now);
    });
    let err = server.validate(&t).await.unwrap_err();
    assert!(err.message().contains("future"), "{err}");

    // `exp - iat == 86,401` refused with L at its maximum, by the hard check.
    let widest = server_with_max(&cell).await;
    assert!(
        admitted(
            &widest,
            &token(&cell, "max", now, now + L_MAX_SECONDS, |_| {})
        )
        .await
    );
    let err = widest
        .validate(&token(&cell, "max1", now, now + L_MAX_SECONDS + 1, |_| {}))
        .await
        .unwrap_err();
    assert!(err.message().contains("86,400"), "{err}");

    // Claims near and beyond `u64::MAX` refuse without overflow.
    let t = token(&cell, "huge", u64::MAX, u64::MAX, |_| {});
    assert!(!admitted(&server, &t).await);
    let t = token(&cell, "huge-exp", now, u64::MAX, |_| {});
    assert!(!admitted(&widest, &t).await);
    let t = token(&cell, "beyond", now, now + L, |p| {
        p["exp"] = serde_json::from_str::<Value>("18446744073709551616").unwrap();
    });
    assert!(
        !admitted(&server, &t).await,
        "a claim beyond u64 does not decode"
    );
    let t = token(&cell, "negative", now, now + L, |p| {
        p["iat"] = json!(-1);
    });
    assert!(
        !admitted(&server, &t).await,
        "a negative claim does not decode"
    );
    set_clock(&cell, u64::MAX - 10);
    let t = token(&cell, "late", u64::MAX - 20, u64::MAX - 15, |_| {});
    assert!(
        !admitted(&widest, &t).await,
        "a clock whose leeway overflows refuses"
    );
}

async fn server_with_max(cell: &Cell) -> ResourceServer {
    server(cell, L_MAX_SECONDS).await
}

/// B-6b: the floor is inclusive and permanent.
#[tokio::test]
async fn the_floor_refuses_at_and_before_and_admits_after() {
    let cell = oidc::boot().await;
    let server = server(&cell, L).await;
    let before = T0 - 100;
    set_clock(&cell, T0);
    let at = token(&cell, "at", before, before + L, |_| {});
    let after = token(&cell, "after", before + 1, before + 1 + L, |_| {});
    assert!(admitted(&server, &at).await, "no floor yet");

    server
        .store()
        .raise_revocation_floor(before)
        .await
        .expect("the floor rises");
    let err = server.validate(&at).await.unwrap_err();
    assert!(err.message().contains("floor"), "{err}");
    assert!(
        admitted(&server, &after).await,
        "iat == before + 1 is admitted"
    );

    // It never lifts.
    server
        .store()
        .raise_revocation_floor(before - 50)
        .await
        .expect("a lower raise is a no-op");
    assert_eq!(
        server.store().revocation_floor().await.unwrap(),
        Some(before)
    );
    assert!(!admitted(&server, &at).await);
}

/// FR-014 and AC-6 (b): D-P17's counterexample. Under retention (D-10) a
/// revoked token stays refused after a backward step that follows the point
/// revision 3 would have pruned its row; the same timeline run against
/// revision 3's pruning rule admits it, which is the failure the rule had.
#[tokio::test]
async fn a_backward_step_after_the_old_prune_point_does_not_revive_a_revoked_token() {
    let cell = oidc::boot().await;
    let server = server(&cell, L).await;
    let (iat, exp, revoked_at) = (T0, T0 + L, T0 + 100);
    let t = token(&cell, "d-p17", iat, exp, |_| {});

    set_clock(&cell, revoked_at);
    assert!(admitted(&server, &t).await);
    Revoker::new(server.clone())
        .revoke_token("d-p17")
        .await
        .expect("the revocation is recorded");
    assert!(!admitted(&server, &t).await);

    // Revision 3 pruned at `revoked_at + V(L_max)`.
    let v_max = denylist_ttl(Duration::from_secs(L_MAX_SECONDS)).as_secs();
    let prune_point = revoked_at + v_max;
    set_clock(&cell, prune_point + 1);
    assert!(!admitted(&server, &t).await, "expired anyway");

    // The clock is set back into the token's validity.
    set_clock(&cell, revoked_at + 100);
    assert!(
        !admitted(&server, &t).await,
        "retention: the row is still there, whatever the clock did"
    );

    // Revision 3's rule on the same timeline: prune, then step back.
    server
        .store()
        .execute(
            "DELETE FROM rahi_revocation_jti WHERE revoked_at + ?1 <= ?2",
            vec![
                rahi_store::Value::from(i64::try_from(v_max).unwrap()),
                rahi_store::Value::from(i64::try_from(prune_point + 1).unwrap()),
            ],
        )
        .await
        .expect("revision 3's prune");
    assert!(
        admitted(&server, &t).await,
        "revision 3's pruning rule fails on D-P17's counterexample"
    );
}

/// AC-6 (b), 7.4 item 4's timeline: a token revoked at `r` stays refused
/// after a later deploy raises L, until its own expiry; and after one that
/// lowers it.
#[tokio::test]
async fn a_lifetime_change_after_a_revocation_does_not_revive_the_token() {
    let cell = oidc::boot().await;
    let wide = server(&cell, 3600).await;
    let r = T0 + 10;
    let t = token(&cell, "long", T0, T0 + 3600, |_| {});
    set_clock(&cell, r);
    assert!(admitted(&wide, &t).await);
    Revoker::new(wide.clone())
        .revoke_token("long")
        .await
        .expect("recorded");

    // A deploy that lowers L, then one that raises it back at r + 1000.
    let narrow = server(&cell, L).await;
    assert!(!admitted(&narrow, &t).await);
    set_clock(&cell, r + 1000);
    let raised = server(&cell, 3600).await;
    for at in [r + 1000, T0 + 3600, T0 + 3600 + LEEWAY_SECONDS - 1] {
        set_clock(&cell, at);
        assert!(!admitted(&raised, &t).await, "refused at {at}");
    }
}

/// AC-6 (b): a subject revocation holds against a later token's admission
/// only for tokens issued at or before it, and survives what a jti row does.
#[tokio::test]
async fn a_subject_revocation_is_durable_and_bounded_by_its_instant() {
    let cell = oidc::boot().await;
    let server = server(&cell, L).await;
    set_clock(&cell, T0 + 50);
    let before = token(&cell, "s-before", T0, T0 + L, |_| {});
    let after = token(&cell, "s-after", T0 + 51, T0 + 51 + L, |_| {});
    Revoker::new(server.clone())
        .revoke_subject(SUB)
        .await
        .expect("recorded");
    assert!(!admitted(&server, &before).await);
    set_clock(&cell, T0 + 60);
    assert!(admitted(&server, &after).await);
    // A backward step after the revocation fails safe: a token issued
    // "earlier" by the stepped clock carries a smaller iat and is refused.
    set_clock(&cell, T0 + 20);
    let stepped = token(&cell, "s-stepped", T0 + 20, T0 + 20 + L, |_| {});
    assert!(!admitted(&server, &stepped).await);
}

// ------------------------------------------------ store-level: FR-004, FR-006

fn store_config(data_dir: &Path) -> StoreConfig {
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

/// FR-004: revocation rows and the floor survive a restart and the deletion
/// of both cache directories, because they are SQL rows.
#[tokio::test]
async fn revocations_and_the_floor_survive_a_restart_without_the_cache() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("app-store");
    // One configuration for both opens: hiqlite pins a node's membership to
    // the addresses it first started on.
    let cfg = store_config(&data);
    let store = Store::open(&cfg).await.unwrap();
    let handle = store.handle();
    handle.record_jti_revocation("j-1", T0).await.unwrap();
    handle.record_subject_revocation(SUB, T0 + 5).await.unwrap();
    handle.raise_revocation_floor(T0 - 1).await.unwrap();
    store.shutdown().await.unwrap();

    for cache in ["logs_cache", "state_machine_cache"] {
        let path = data.join(cache);
        if path.exists() {
            std::fs::remove_dir_all(&path).unwrap();
        }
    }

    let store = Store::open(&cfg).await.unwrap();
    let handle = store.handle();
    assert_eq!(handle.jti_revoked_at("j-1").await.unwrap(), Some(T0));
    assert_eq!(handle.subject_revoked_at(SUB).await.unwrap(), Some(T0 + 5));
    assert_eq!(handle.revocation_floor().await.unwrap(), Some(T0 - 1));
    store.shutdown().await.unwrap();
}

/// FR-006 under B-6 (i): across a simulated day of revocations, a restart
/// and a backward clock step, the tables only grow.
#[tokio::test]
async fn the_revocation_tables_only_grow() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("app-store");
    // One configuration for both opens: hiqlite pins a node's membership to
    // the addresses it first started on.
    let cfg = store_config(&data);
    let store = Store::open(&cfg).await.unwrap();
    let mut last = (0, 0);
    let grew = |last: &mut (u64, u64), now: (u64, u64)| {
        assert!(now.0 >= last.0 && now.1 >= last.1, "{now:?} after {last:?}");
        *last = now;
    };
    for hour in 0..24u64 {
        let at = T0 + hour * 3600;
        let handle = store.handle();
        handle
            .record_jti_revocation(&format!("day-{hour}"), at)
            .await
            .unwrap();
        handle
            .record_subject_revocation(&format!("sub-{}", hour % 5), at)
            .await
            .unwrap();
        grew(&mut last, handle.revocation_rows().await.unwrap());
    }
    assert_eq!(last, (24, 5));
    store.shutdown().await.unwrap();

    let store = Store::open(&cfg).await.unwrap();
    let handle = store.handle();
    grew(&mut last, handle.revocation_rows().await.unwrap());
    // A backward step: an earlier `revoked_at` never lowers a subject's row
    // and never removes anything.
    handle
        .record_jti_revocation("stepped", T0 - 3600)
        .await
        .unwrap();
    handle
        .record_subject_revocation("sub-0", T0 - 3600)
        .await
        .unwrap();
    grew(&mut last, handle.revocation_rows().await.unwrap());
    assert_eq!(last, (25, 5));
    assert!(handle.subject_revoked_at("sub-0").await.unwrap() > Some(T0));
    store.shutdown().await.unwrap();
}

/// The workspace's shared loopback allocator (spec 022 D-10): 20000..32000,
/// a pid-derived offset, an exclusive lock per port under
/// `<temp>/rahi-test-ports`, and a probe bind.
fn free_addr() -> SocketAddr {
    use std::fs::{File, OpenOptions};
    use std::net::{Ipv4Addr, TcpListener};
    use std::sync::atomic::AtomicU32;
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
        assert!(n < SPAN, "every test port is taken");
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
            return SocketAddr::from(([127, 0, 0, 1], port));
        }
    }
}
