//! spec 012 FR-001 and FR-002: the lease, its fencing token, and the batch
//! a superseded holder cannot land.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use rahi_store::{Migration, Statement, StoreHandle, Value, coordination_migration};
use rahi_types::Error;
use serde::Deserialize;

const GUARDED: &str = "CREATE TABLE guarded (\
    id INTEGER PRIMARY KEY, \
    v TEXT NOT NULL, \
    fence INTEGER NOT NULL DEFAULT 0)";

const UPDATE: &str = "UPDATE guarded SET v = $1 WHERE id = $2";

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct Row {
    id: i64,
    v: String,
    fence: i64,
}

async fn prepare(store: &StoreHandle) {
    store
        .migrate(&[
            coordination_migration(1),
            Migration::new(2, "guarded", GUARDED),
        ])
        .await
        .unwrap();
    store
        .txn(vec![
            Statement::with_params(
                "INSERT INTO guarded (id, v) VALUES ($1, $2)",
                vec![Value::from(1i64), Value::from("first")],
            ),
            Statement::with_params(
                "INSERT INTO guarded (id, v) VALUES ($1, $2)",
                vec![Value::from(2i64), Value::from("second")],
            ),
        ])
        .await
        .unwrap();
}

async fn rows(store: &StoreHandle) -> Vec<Row> {
    store
        .query("SELECT id, v, fence FROM guarded ORDER BY id", vec![])
        .await
        .unwrap()
}

/// FR-001, the release half: the second lease waits for the first, and the
/// token it is minted is strictly greater.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_lease_on_one_key_waits_for_the_first_and_gets_a_higher_token() {
    let f = common::open().await;
    let store = f.store.handle();
    prepare(&store).await;

    let first = store.lease("guarded").await.unwrap();
    let first_token = first.token;

    let acquired = Arc::new(AtomicBool::new(false));
    let waiter = tokio::spawn({
        let store = store.clone();
        let acquired = Arc::clone(&acquired);
        async move {
            let second = store.lease("guarded").await.unwrap();
            acquired.store(true, Ordering::SeqCst);
            second
        }
    });

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !acquired.load(Ordering::SeqCst),
        "the second lease waits while the first is held"
    );

    first.release().await;
    let second = tokio::time::timeout(Duration::from_secs(5), waiter)
        .await
        .expect("the release wakes the waiter")
        .unwrap();
    assert!(
        second.token > first_token,
        "tokens are strictly increasing: {:?} then {:?}",
        first_token,
        second.token
    );

    second.release().await;
    f.store.shutdown().await.unwrap();
}

/// FR-002: a stale token affects no row and is a conflict; a fresh one lands
/// and records itself in the row's `fence` column.
///
/// The takeover is simulated by minting a newer token for the key directly,
/// which is exactly what a second acquisition does, so the assertion is about
/// the fencing predicate and not about the ten-second TTL (which
/// `a_lease_that_outlives_its_ttl_...` covers).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stale_token_affects_no_row_and_a_fresh_one_succeeds() {
    let f = common::open().await;
    let store = f.store.handle();
    prepare(&store).await;

    let lease = store.lease("guarded").await.unwrap();
    let results = store
        .fenced_txn(
            &lease,
            vec![Statement::with_params(
                UPDATE,
                vec![Value::from("held"), Value::from(1i64)],
            )],
        )
        .await
        .expect("a fresh token succeeds");
    assert_eq!(results.len(), 1, "the guard's result is not the caller's");
    assert_eq!(results[0].rows_affected, 1);
    let after_fresh = rows(&store).await;
    assert_eq!(after_fresh[0].v, "held");
    assert_eq!(
        after_fresh[0].fence,
        i64::try_from(lease.token.get()).unwrap(),
        "the row records the lease that wrote it"
    );

    store
        .execute(
            "UPDATE lease_fence SET token = token + 1 WHERE lease_key = $1",
            vec![Value::from("guarded")],
        )
        .await
        .unwrap();

    let err = store
        .fenced_txn(
            &lease,
            vec![
                Statement::with_params(UPDATE, vec![Value::from("zombie"), Value::from(1i64)]),
                Statement::with_params(UPDATE, vec![Value::from("zombie"), Value::from(2i64)]),
            ],
        )
        .await
        .expect_err("a superseded lease cannot write");
    assert!(matches!(err, Error::Conflict(_)), "{err}");
    assert_eq!(
        rows(&store).await,
        after_fresh,
        "the whole batch rolled back: no row from it landed"
    );

    f.store.shutdown().await.unwrap();
}

/// FR-001, the TTL half: a lease that is never released is handed to the next
/// asker after the ten-second TTL, with a strictly greater token, and the
/// holder that lost it finds out through a conflict.
///
/// This test spends eleven seconds by design: the TTL is hiqlite's, hardcoded
/// and not configurable (B-1).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lease_that_outlives_its_ttl_is_taken_over_and_its_writes_are_refused() {
    let f = common::open().await;
    let store = f.store.handle();
    prepare(&store).await;

    let zombie = store.lease("guarded").await.unwrap();
    tokio::time::sleep(Duration::from_secs(11)).await;

    let fresh = tokio::time::timeout(Duration::from_secs(5), store.lease("guarded"))
        .await
        .expect("the expired lease is granted to the next asker")
        .unwrap();
    assert!(
        fresh.token > zombie.token,
        "tokens are strictly increasing across a TTL takeover: {:?} then {:?}",
        zombie.token,
        fresh.token
    );

    store
        .fenced_txn(
            &fresh,
            vec![Statement::with_params(
                UPDATE,
                vec![Value::from("fresh"), Value::from(1i64)],
            )],
        )
        .await
        .expect("the new holder writes");

    let err = store
        .fenced_txn(
            &zombie,
            vec![Statement::with_params(
                UPDATE,
                vec![Value::from("zombie"), Value::from(1i64)],
            )],
        )
        .await
        .expect_err("the superseded holder cannot write");
    assert!(matches!(err, Error::Conflict(_)), "{err}");
    assert_eq!(rows(&store).await[0].v, "fresh");

    fresh.release().await;
    f.store.shutdown().await.unwrap();
}

/// A batch under a lease is still a batch: a failing statement takes the rest
/// of it down, and the guard does not turn a caller's own conflict into a
/// fencing conflict.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failing_statement_rolls_the_fenced_batch_back() {
    let f = common::open().await;
    let store = f.store.handle();
    prepare(&store).await;
    let before = rows(&store).await;

    let lease = store.lease("guarded").await.unwrap();
    let err = store
        .fenced_txn(
            &lease,
            vec![
                Statement::with_params(UPDATE, vec![Value::from("held"), Value::from(1i64)]),
                Statement::new("UPDATE guarded SET v = NULL WHERE id = 2"),
            ],
        )
        .await
        .expect_err("a NOT NULL violation fails the batch");
    assert!(
        matches!(err, Error::Conflict(_) | Error::Validation(_)),
        "{err}"
    );
    assert_eq!(rows(&store).await, before, "the batch rolled back whole");

    let err = store.fenced_txn(&lease, vec![]).await.unwrap_err();
    assert!(matches!(err, Error::Validation(_)), "{err}");

    lease.release().await;
    f.store.shutdown().await.unwrap();
}
