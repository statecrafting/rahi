//! spec 045 FR-004, FR-005, FR-006, FR-010, FR-011: processing identity,
//! fenced claims, bounded retry and the dead letter, and the redacted
//! `/metrics` gauge, against a real single-node store.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod common;

use std::time::Duration;

use rahi_store::{
    ContentDigest, DeadFilter, Page, ProcessingKey, ReceiptKey, ReceiptMeta, Receipts, RetryPolicy,
    StoreHandle, TxnBuilder, Work, coordination_set, receipt_set,
};
use rahi_types::{Error, UnixSeconds};

async fn migrated(store: &StoreHandle) {
    store
        .migrate_sets(&[], &[coordination_set(), receipt_set()])
        .await
        .unwrap();
}

fn now(secs: u64) -> UnixSeconds {
    UnixSeconds::new(secs)
}

async fn accepted_receipt(store: &StoreHandle, key: &ReceiptKey) {
    let digest = ContentDigest::of(b"body");
    let mut txn = TxnBuilder::new();
    Receipts::stage_first(&mut txn, key, &digest, &ReceiptMeta::default(), now(1)).unwrap();
    store.txn(txn.into_statements()).await.unwrap();
}

fn policy(max_attempts: u32) -> RetryPolicy {
    RetryPolicy {
        max_attempts,
        base: Duration::from_millis(1),
        cap: Duration::from_millis(50),
    }
}

/// FR-004: a new `processor_revision` over an accepted receipt enqueues new
/// work; `stage_work` for an existing processing identity is a no-op.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_processor_revision_enqueues_new_work_and_stage_work_is_idempotent() {
    let f = common::open().await;
    let store = f.store.handle();
    migrated(&store).await;
    let receipt = ReceiptKey::new("acme", "ns", "msg-1").unwrap();
    accepted_receipt(&store, &receipt).await;

    let v1 = ProcessingKey::new(receipt.clone(), 1, "extract", "v1").unwrap();
    let mut txn = TxnBuilder::new();
    Work::stage_work(&mut txn, &v1, now(1));
    store.txn(txn.into_statements()).await.unwrap();
    let mut txn = TxnBuilder::new();
    Work::stage_work(&mut txn, &v1, now(1));
    store.txn(txn.into_statements()).await.unwrap();

    let counts = Work::counts(&store).await.unwrap();
    let extract = counts.iter().find(|c| c.processor == "extract").unwrap();
    assert_eq!(extract.pending, 1, "the second stage_work is a no-op");

    let v2 = ProcessingKey::new(receipt, 1, "extract", "v2").unwrap();
    let mut txn = TxnBuilder::new();
    Work::stage_work(&mut txn, &v2, now(1));
    store.txn(txn.into_statements()).await.unwrap();

    let counts = Work::counts(&store).await.unwrap();
    let extract = counts.iter().find(|c| c.processor == "extract").unwrap();
    assert_eq!(extract.pending, 2, "a new processor_revision is new work");

    f.store.shutdown().await.unwrap();
}

/// FR-005: a claim whose token was superseded by a reclaim commits nothing
/// and returns `Error::Conflict`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_superseded_claim_commits_nothing() {
    let f = common::open().await;
    let store = f.store.handle();
    migrated(&store).await;
    store
        .execute(
            "CREATE TABLE domain (id INTEGER PRIMARY KEY, note TEXT)",
            vec![],
        )
        .await
        .unwrap();
    let receipt = ReceiptKey::new("acme", "ns", "msg-1").unwrap();
    accepted_receipt(&store, &receipt).await;
    let key = ProcessingKey::new(receipt, 1, "extract", "v1").unwrap();
    let mut txn = TxnBuilder::new();
    Work::stage_work(&mut txn, &key, now(1));
    store.txn(txn.into_statements()).await.unwrap();

    let zombie = Work::reserve(&store, &key, "worker-1", Duration::from_secs(1), now(1))
        .await
        .unwrap()
        .expect("eligible");

    // The claim expires; a fresh reserve reclaims it with a new token.
    let fresh = Work::reserve(&store, &key, "worker-2", Duration::from_secs(10), now(5))
        .await
        .unwrap()
        .expect("reclaimed after expiry");
    assert_ne!(fresh.token, zombie.token);

    let mut txn = TxnBuilder::new();
    Work::complete(&mut txn, &zombie, now(6));
    txn.push(rahi_store::Statement::with_params(
        "INSERT INTO domain (id, note) VALUES ($1, $2)",
        vec![
            rahi_store::Value::Integer(1),
            rahi_store::Value::from("zombie"),
        ],
    ));
    let err = store.txn(txn.into_statements()).await.unwrap_err();
    assert!(matches!(err, Error::Conflict(_)), "{err}");

    let rows: Vec<serde_json::Value> = store.query("SELECT id FROM domain", vec![]).await.unwrap();
    assert!(rows.is_empty(), "the zombie's domain write never lands");

    let mut txn = TxnBuilder::new();
    Work::complete(&mut txn, &fresh, now(6));
    store.txn(txn.into_statements()).await.unwrap();
    let counts = Work::counts(&store).await.unwrap();
    assert!(
        counts
            .iter()
            .all(|c| c.processor != "extract" || c.claimed == 0),
        "the legitimate holder's completion lands"
    );

    f.store.shutdown().await.unwrap();
}

/// FR-006: with `max_attempts = 3`, three failures reach `dead`; the
/// attempt history holds three rows; `requeue` restores `pending` and adds a
/// `requeued` attempt.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn three_failures_reach_dead_and_requeue_restores_pending() {
    let f = common::open().await;
    let store = f.store.handle();
    migrated(&store).await;
    let receipt = ReceiptKey::new("acme", "ns", "msg-1").unwrap();
    accepted_receipt(&store, &receipt).await;
    let key = ProcessingKey::new(receipt, 1, "extract", "v1").unwrap();
    let mut txn = TxnBuilder::new();
    Work::stage_work(&mut txn, &key, now(1));
    store.txn(txn.into_statements()).await.unwrap();

    let pol = policy(3);
    for attempt in 1..=3u32 {
        let claim = Work::reserve(&store, &key, "worker-1", Duration::from_secs(10), now(1))
            .await
            .unwrap()
            .expect("eligible");
        assert_eq!(claim.attempt, attempt);
        let mut txn = TxnBuilder::new();
        Work::fail(
            &mut txn,
            &claim,
            &rahi_store::FailureDetail {
                class: "timeout".to_owned(),
                detail: Some("no response".to_owned()),
            },
            &pol,
            now(1),
        )
        .unwrap();
        store.txn(txn.into_statements()).await.unwrap();
    }

    let dead = Work::dead(&store, &DeadFilter::default(), Page::default())
        .await
        .unwrap();
    assert_eq!(dead.len(), 1);
    assert_eq!(
        dead[0].attempts.len(),
        3,
        "the attempt history holds three rows"
    );
    assert!(
        dead[0]
            .attempts
            .iter()
            .all(|a| a.outcome == "dead" || a.outcome == "failed")
    );

    let mut txn = TxnBuilder::new();
    Work::requeue(&mut txn, &key, now(2));
    store.txn(txn.into_statements()).await.unwrap();

    let dead_after = Work::dead(&store, &DeadFilter::default(), Page::default())
        .await
        .unwrap();
    assert!(dead_after.is_empty(), "the row left the dead letter");

    let requeued = Work::reserve(&store, &key, "worker-2", Duration::from_secs(10), now(2))
        .await
        .unwrap()
        .expect("pending again");
    assert_eq!(requeued.attempt, 4, "the attempt count is kept, not reset");
    let history = Work::dead(&store, &DeadFilter::default(), Page::default())
        .await
        .unwrap();
    assert!(history.is_empty(), "it is claimed, not dead");

    f.store.shutdown().await.unwrap();
}

/// FR-010: a reservation and a renewal hold the queue's lease and write
/// through `fenced_txn`; a reservation whose lease was superseded commits
/// nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn renew_advances_the_fence_and_a_stale_renewal_is_refused() {
    let f = common::open().await;
    let store = f.store.handle();
    migrated(&store).await;
    let receipt = ReceiptKey::new("acme", "ns", "msg-1").unwrap();
    accepted_receipt(&store, &receipt).await;
    let key = ProcessingKey::new(receipt, 1, "extract", "v1").unwrap();
    let mut txn = TxnBuilder::new();
    Work::stage_work(&mut txn, &key, now(1));
    store.txn(txn.into_statements()).await.unwrap();

    let claim = Work::reserve(&store, &key, "worker-1", Duration::from_secs(2), now(1))
        .await
        .unwrap()
        .expect("eligible");

    let renewed = Work::renew(&store, &claim, Duration::from_secs(10), now(2))
        .await
        .unwrap();
    assert_ne!(renewed.token, claim.token, "the fence advances on renewal");
    assert_eq!(renewed.expires_at, now(12));

    // The original (now stale) claim can no longer renew.
    let err = Work::renew(&store, &claim, Duration::from_secs(10), now(2))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Conflict(_)), "{err}");

    let mut txn = TxnBuilder::new();
    Work::complete(&mut txn, &renewed, now(3));
    store.txn(txn.into_statements()).await.unwrap();

    f.store.shutdown().await.unwrap();
}

/// FR-010's read side: `Work::counts` aggregates over every tenant and
/// namespace, giving `rahi-edge`'s `/metrics` gauge (spec 045 B-17, tested
/// in `rahi-edge`'s `tests/obs.rs`, FR-011) exactly the redacted shape it
/// renders.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn counts_aggregate_by_processor_over_every_tenant_and_namespace() {
    let f = common::open().await;
    let store = f.store.handle();
    migrated(&store).await;
    let a = ReceiptKey::new("tenant-a", "ns-1", "msg-1").unwrap();
    let b = ReceiptKey::new("tenant-b", "ns-2", "msg-2").unwrap();
    accepted_receipt(&store, &a).await;
    accepted_receipt(&store, &b).await;
    let key_a = ProcessingKey::new(a, 1, "extract", "v1").unwrap();
    let key_b = ProcessingKey::new(b, 1, "extract", "v1").unwrap();
    let mut txn = TxnBuilder::new();
    Work::stage_work(&mut txn, &key_a, now(1));
    Work::stage_work(&mut txn, &key_b, now(1));
    store.txn(txn.into_statements()).await.unwrap();

    let counts = Work::counts(&store).await.unwrap();
    let extract = counts.iter().find(|c| c.processor == "extract").unwrap();
    assert_eq!(
        extract.pending, 2,
        "aggregated over both tenants and namespaces"
    );

    f.store.shutdown().await.unwrap();
}
