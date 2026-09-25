//! spec 045 3.7, FR-009: every row of the crash-point table, against a real
//! single-node store restarted over its own data directory.
//!
//! Each test builds the store once with [`common::config`] (so the second
//! `Store::open` reuses the same ports and data directory, the pattern
//! `tests/cache.rs`'s `a_restart_keeps_the_sql_group_and_replays_the_cache`
//! establishes), drives the store to the named crash point, shuts it down,
//! reopens it, and asserts the table's end state.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod common;

use std::time::Duration;

use rahi_store::{
    Classification, ContentDigest, DeadFilter, Envelope, Outbox, Page, ProcessingKey, ReceiptKey,
    ReceiptMeta, Receipts, RetryPolicy, Store, StoreHandle, TxnBuilder, Value, Work,
    coordination_migration, receipt_migration,
};
use rahi_types::{Error, Revision, UnixSeconds};
use serde::Deserialize;

fn now(secs: u64) -> UnixSeconds {
    UnixSeconds::new(secs)
}

async fn migrated(store: &StoreHandle) {
    store
        .migrate(&[coordination_migration(1), receipt_migration(2)])
        .await
        .unwrap();
}

async fn submit(store: &StoreHandle, txn: TxnBuilder) -> Result<(), Error> {
    store.txn(txn.into_statements()).await.map(|_| ())
}

fn policy() -> RetryPolicy {
    RetryPolicy {
        max_attempts: 3,
        base: Duration::from_millis(1),
        cap: Duration::from_millis(50),
    }
}

#[derive(Debug, Deserialize)]
struct ProcessingRow {
    state: String,
}

#[derive(Debug, Deserialize)]
struct AttemptRow {
    outcome: String,
}

#[derive(Debug, Deserialize)]
struct CountRow {
    n: i64,
}

async fn table_count(store: &StoreHandle, table: &str) -> i64 {
    let rows: Vec<CountRow> = store
        .query(format!("SELECT COUNT(*) AS n FROM {table}"), vec![])
        .await
        .unwrap();
    rows[0].n
}

/// Row 1: nothing durable before the intake batch; a redelivery classifies
/// exactly as a first delivery would.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn after_classify_before_the_intake_batch_a_redelivery_is_a_first_delivery() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = common::config(&dir.path().join("hiqlite"));
    let store = Store::open(&cfg).await.unwrap();
    let handle = store.handle();
    migrated(&handle).await;

    let key = ReceiptKey::new("acme", "ns", "msg-1").unwrap();
    let digest = ContentDigest::of(b"body");
    // Classify, but the crash happens before any staging call.
    assert_eq!(
        Receipts::classify(&handle, &key, &digest).await.unwrap(),
        Classification::FirstSeen
    );

    store.shutdown().await.unwrap();
    drop(store);

    let store = Store::open(&cfg).await.unwrap();
    let handle = store.handle();
    assert_eq!(
        Receipts::classify(&handle, &key, &digest).await.unwrap(),
        Classification::FirstSeen,
        "the redelivery classifies exactly as a first delivery"
    );

    store.shutdown().await.unwrap();
}

/// Row 2: the receipt, its domain rows, its pending processing rows, and its
/// outbox row survive; a redelivery classifies as `Redelivered` and the
/// caller acknowledges without writing again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn after_the_intake_batch_before_acknowledgement_a_redelivery_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = common::config(&dir.path().join("hiqlite"));
    let store = Store::open(&cfg).await.unwrap();
    let handle = store.handle();
    migrated(&handle).await;
    handle
        .execute("CREATE TABLE domain (id INTEGER PRIMARY KEY)", vec![])
        .await
        .unwrap();

    let key = ReceiptKey::new("acme", "ns", "msg-1").unwrap();
    let digest = ContentDigest::of(b"body");
    let receipt_key = ProcessingKey::new(key.clone(), 1, "extract", "v1").unwrap();

    let mut txn = TxnBuilder::new();
    Receipts::stage_first(&mut txn, &key, &digest, &ReceiptMeta::default(), now(1)).unwrap();
    Work::stage_work(&mut txn, &receipt_key, now(1));
    txn.push(rahi_store::Statement::with_params(
        "INSERT INTO domain (id) VALUES ($1)",
        vec![Value::Integer(1)],
    ));
    Outbox::stage(
        &mut txn,
        &Envelope::new("receipt.accepted", Some("acme".to_owned()), key.key_digest(), Revision::new(1)),
    );
    submit(&handle, txn).await.unwrap();
    // The intake batch committed; the crash happens before the caller acks.

    store.shutdown().await.unwrap();
    drop(store);

    let store = Store::open(&cfg).await.unwrap();
    let handle = store.handle();

    match Receipts::classify(&handle, &key, &digest).await.unwrap() {
        Classification::Redelivered { is_head, .. } => assert!(is_head),
        other => panic!("expected Redelivered, got {other:?}"),
    }
    // The caller acknowledges without writing: no further staging call.
    assert_eq!(table_count(&handle, "rahi_receipt").await, 1, "one receipt");
    assert_eq!(table_count(&handle, "domain").await, 1, "one set of work");
    assert_eq!(table_count(&handle, "outbox").await, 1, "one outbox row");

    store.shutdown().await.unwrap();
}

/// Row 3: the intake batch is one Raft entry (all of it or none); a caller
/// unsure whether its batch landed classifies again with `query_consistent`
/// and never ends up with more than one receipt, restart included.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_intake_batch_lands_whole_and_a_repeated_classify_never_makes_a_second_receipt() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = common::config(&dir.path().join("hiqlite"));
    let store = Store::open(&cfg).await.unwrap();
    let handle = store.handle();
    migrated(&handle).await;

    let key = ReceiptKey::new("acme", "ns", "msg-1").unwrap();
    let digest = ContentDigest::of(b"body");

    // A caller unsure of the outcome retries its classify-then-stage loop
    // several times, as B-10 prescribes.
    for _ in 0..3 {
        match Receipts::classify(&handle, &key, &digest).await.unwrap() {
            Classification::FirstSeen => {
                let mut txn = TxnBuilder::new();
                Receipts::stage_first(&mut txn, &key, &digest, &ReceiptMeta::default(), now(1))
                    .unwrap();
                let _ = submit(&handle, txn).await;
            }
            Classification::Redelivered { .. } => {}
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(table_count(&handle, "rahi_receipt").await, 1);

    store.shutdown().await.unwrap();
    drop(store);

    let store = Store::open(&cfg).await.unwrap();
    let handle = store.handle();
    match Receipts::classify(&handle, &key, &digest).await.unwrap() {
        Classification::Redelivered { .. } => {}
        other => panic!("expected Redelivered, got {other:?}"),
    }
    assert_eq!(table_count(&handle, "rahi_receipt").await, 1, "at most one receipt");

    store.shutdown().await.unwrap();
}

/// Row 4: a claimed row with an open attempt survives; the next reserve
/// reclaims it once expired and closes the old attempt as `expired`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn after_reserve_before_processing_ends_the_claim_survives_and_later_reclaims() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = common::config(&dir.path().join("hiqlite"));
    let store = Store::open(&cfg).await.unwrap();
    let handle = store.handle();
    migrated(&handle).await;
    let key = ReceiptKey::new("acme", "ns", "msg-1").unwrap();
    let digest = ContentDigest::of(b"body");
    let mut txn = TxnBuilder::new();
    Receipts::stage_first(&mut txn, &key, &digest, &ReceiptMeta::default(), now(1)).unwrap();
    submit(&handle, txn).await.unwrap();
    let pkey = ProcessingKey::new(key, 1, "extract", "v1").unwrap();
    let mut txn = TxnBuilder::new();
    Work::stage_work(&mut txn, &pkey, now(1));
    submit(&handle, txn).await.unwrap();

    let claim = Work::reserve(&handle, &pkey, "worker-1", Duration::from_secs(5), now(1))
        .await
        .unwrap()
        .expect("eligible");
    // The crash happens after reserve, before processing ends.

    store.shutdown().await.unwrap();
    drop(store);

    let store = Store::open(&cfg).await.unwrap();
    let handle = store.handle();

    let rows: Vec<ProcessingRow> = handle
        .query("SELECT state FROM rahi_processing", vec![])
        .await
        .unwrap();
    assert_eq!(rows[0].state, "claimed", "the claim survives the crash");
    let attempts: Vec<AttemptRow> = handle
        .query("SELECT outcome FROM rahi_processing_attempt", vec![])
        .await
        .unwrap();
    assert_eq!(attempts[0].outcome, "open", "the attempt is still open");

    // Past expiry: the next reserve reclaims and closes the old attempt.
    let reclaimed = Work::reserve(&handle, &pkey, "worker-2", Duration::from_secs(5), now(10))
        .await
        .unwrap()
        .expect("reclaimed after expiry");
    assert_ne!(reclaimed.token, claim.token);
    assert_eq!(reclaimed.attempt, 2, "reprocessed under a new attempt");

    let attempts: Vec<AttemptRow> = handle
        .query(
            "SELECT outcome FROM rahi_processing_attempt ORDER BY attempt",
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(attempts.len(), 2);

    store.shutdown().await.unwrap();
}

/// Row 5: after processing but before the completing batch, only the claim
/// survives; the completing batch, once submitted, commits exactly once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn after_processing_before_the_completing_batch_the_claim_alone_survives() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = common::config(&dir.path().join("hiqlite"));
    let store = Store::open(&cfg).await.unwrap();
    let handle = store.handle();
    migrated(&handle).await;
    let key = ReceiptKey::new("acme", "ns", "msg-1").unwrap();
    let digest = ContentDigest::of(b"body");
    let mut txn = TxnBuilder::new();
    Receipts::stage_first(&mut txn, &key, &digest, &ReceiptMeta::default(), now(1)).unwrap();
    submit(&handle, txn).await.unwrap();
    let pkey = ProcessingKey::new(key, 1, "extract", "v1").unwrap();
    let mut txn = TxnBuilder::new();
    Work::stage_work(&mut txn, &pkey, now(1));
    submit(&handle, txn).await.unwrap();

    let claim = Work::reserve(&handle, &pkey, "worker-1", Duration::from_secs(60), now(1))
        .await
        .unwrap()
        .expect("eligible");
    // Processing happened; the crash is before the completing batch commits.

    store.shutdown().await.unwrap();
    drop(store);

    let store = Store::open(&cfg).await.unwrap();
    let handle = store.handle();

    // The worker resumes with the same claim, and it still lands: committed
    // once.
    let mut txn = TxnBuilder::new();
    Work::complete(&mut txn, &claim, now(2));
    submit(&handle, txn).await.unwrap();
    let rows: Vec<ProcessingRow> = handle
        .query("SELECT state FROM rahi_processing", vec![])
        .await
        .unwrap();
    assert_eq!(rows[0].state, "done");

    store.shutdown().await.unwrap();
}

/// Row 6: whether the completing batch landed is answered by
/// `query_consistent` on the row, and a retry after another holder won
/// aborts on the guard: domain writes commit at most once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn during_the_completing_batch_a_retry_after_it_landed_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = common::config(&dir.path().join("hiqlite"));
    let store = Store::open(&cfg).await.unwrap();
    let handle = store.handle();
    migrated(&handle).await;
    let key = ReceiptKey::new("acme", "ns", "msg-1").unwrap();
    let digest = ContentDigest::of(b"body");
    let mut txn = TxnBuilder::new();
    Receipts::stage_first(&mut txn, &key, &digest, &ReceiptMeta::default(), now(1)).unwrap();
    submit(&handle, txn).await.unwrap();
    let pkey = ProcessingKey::new(key, 1, "extract", "v1").unwrap();
    let mut txn = TxnBuilder::new();
    Work::stage_work(&mut txn, &pkey, now(1));
    submit(&handle, txn).await.unwrap();

    let claim = Work::reserve(&handle, &pkey, "worker-1", Duration::from_secs(60), now(1))
        .await
        .unwrap()
        .expect("eligible");
    let mut txn = TxnBuilder::new();
    Work::complete(&mut txn, &claim, now(2));
    submit(&handle, txn).await.unwrap();
    // The completing batch committed; the crash happens before the worker
    // learns the outcome.

    store.shutdown().await.unwrap();
    drop(store);

    let store = Store::open(&cfg).await.unwrap();
    let handle = store.handle();

    let rows: Vec<ProcessingRow> = handle
        .query("SELECT state FROM rahi_processing", vec![])
        .await
        .unwrap();
    assert_eq!(rows[0].state, "done", "query_consistent would answer done");

    // The uncertain worker retries the same completing batch: the guard
    // (state is no longer claimed) aborts it.
    let mut txn = TxnBuilder::new();
    Work::complete(&mut txn, &claim, now(3));
    let err = submit(&handle, txn).await.unwrap_err();
    assert!(matches!(err, Error::Conflict(_)), "{err}");

    store.shutdown().await.unwrap();
}

/// Row 7: the done row, domain rows, and outbox row all survive; the next
/// drain publishes, and a caller that retries its request is answered
/// `Redelivered` with the recorded outcome.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn after_the_completing_batch_the_outbox_row_survives_for_the_next_drain() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = common::config(&dir.path().join("hiqlite"));
    let store = Store::open(&cfg).await.unwrap();
    let handle = store.handle();
    migrated(&handle).await;
    let key = ReceiptKey::new("acme", "ns", "msg-1").unwrap();
    let digest = ContentDigest::of(b"body");
    let meta = ReceiptMeta {
        outcome: Some("ref-42".to_owned()),
        ..ReceiptMeta::default()
    };
    let mut txn = TxnBuilder::new();
    Receipts::stage_first(&mut txn, &key, &digest, &meta, now(1)).unwrap();
    Outbox::stage(
        &mut txn,
        &Envelope::new("receipt.accepted", Some("acme".to_owned()), key.key_digest(), Revision::new(1)),
    );
    submit(&handle, txn).await.unwrap();
    // The crash happens before the drain and before the caller's ack.

    store.shutdown().await.unwrap();
    drop(store);

    let store = Store::open(&cfg).await.unwrap();
    let handle = store.handle();

    assert_eq!(table_count(&handle, "outbox").await, 1, "the outbox row survives");
    let drained = Outbox::drain(&handle, 10).await.unwrap();
    assert_eq!(drained, 1, "the next drain publishes it");

    match Receipts::classify(&handle, &key, &digest).await.unwrap() {
        Classification::Redelivered { outcome, .. } => {
            assert_eq!(outcome.as_deref(), Some("ref-42"));
        }
        other => panic!("expected Redelivered with the recorded outcome, got {other:?}"),
    }

    store.shutdown().await.unwrap();
}

/// Row 8: a zombie holder whose claim was reclaimed, even across a restart,
/// commits nothing when it finally writes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_zombie_holder_commits_nothing_after_its_claim_was_reclaimed_across_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = common::config(&dir.path().join("hiqlite"));
    let store = Store::open(&cfg).await.unwrap();
    let handle = store.handle();
    migrated(&handle).await;
    let key = ReceiptKey::new("acme", "ns", "msg-1").unwrap();
    let digest = ContentDigest::of(b"body");
    let mut txn = TxnBuilder::new();
    Receipts::stage_first(&mut txn, &key, &digest, &ReceiptMeta::default(), now(1)).unwrap();
    submit(&handle, txn).await.unwrap();
    let pkey = ProcessingKey::new(key, 1, "extract", "v1").unwrap();
    let mut txn = TxnBuilder::new();
    Work::stage_work(&mut txn, &pkey, now(1));
    submit(&handle, txn).await.unwrap();

    let zombie = Work::reserve(&handle, &pkey, "worker-1", Duration::from_secs(1), now(1))
        .await
        .unwrap()
        .expect("eligible");
    let fresh = Work::reserve(&handle, &pkey, "worker-2", Duration::from_secs(60), now(5))
        .await
        .unwrap()
        .expect("reclaimed after expiry");

    store.shutdown().await.unwrap();
    drop(store);

    let store = Store::open(&cfg).await.unwrap();
    let handle = store.handle();

    let mut txn = TxnBuilder::new();
    Work::complete(&mut txn, &zombie, now(6));
    let err = submit(&handle, txn).await.unwrap_err();
    assert!(matches!(err, Error::Conflict(_)), "{err}");

    let mut txn = TxnBuilder::new();
    Work::complete(&mut txn, &fresh, now(6));
    submit(&handle, txn).await.unwrap();
    let rows: Vec<ProcessingRow> = handle
        .query("SELECT state FROM rahi_processing", vec![])
        .await
        .unwrap();
    assert_eq!(rows[0].state, "done", "the legitimate holder completed it");

    store.shutdown().await.unwrap();
}

/// Row 9: a sweep's chunks already committed through `fenced_txn` survive a
/// restart, and the next sweep continues with no half-applied chunk.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sweep_interrupted_by_a_restart_continues_with_no_half_applied_chunk() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = common::config(&dir.path().join("hiqlite"));
    let store = Store::open(&cfg).await.unwrap();
    let handle = store.handle();
    migrated(&handle).await;

    // Two expired claims on the same queue.
    for n in 1..=2u8 {
        let key = ReceiptKey::new("acme", "ns", format!("msg-{n}")).unwrap();
        let digest = ContentDigest::of(format!("body-{n}").as_bytes());
        let mut txn = TxnBuilder::new();
        Receipts::stage_first(&mut txn, &key, &digest, &ReceiptMeta::default(), now(1)).unwrap();
        submit(&handle, txn).await.unwrap();
        let pkey = ProcessingKey::new(key, 1, "extract", "v1").unwrap();
        let mut txn = TxnBuilder::new();
        Work::stage_work(&mut txn, &pkey, now(1));
        submit(&handle, txn).await.unwrap();
        Work::reserve(&handle, &pkey, "worker-1", Duration::from_secs(1), now(1))
            .await
            .unwrap()
            .expect("eligible");
    }

    // The sweep processes only the first chunk before the crash.
    let first = Work::sweep(&handle, "ns", &policy(), now(10), 1).await.unwrap();
    assert_eq!(first.expired, 1, "one chunk committed through fenced_txn");

    store.shutdown().await.unwrap();
    drop(store);

    let store = Store::open(&cfg).await.unwrap();
    let handle = store.handle();

    // The next sweep continues; no chunk is applied twice or half-applied.
    let second = Work::sweep(&handle, "ns", &policy(), now(10), 10).await.unwrap();
    assert_eq!(second.expired, 1, "the remaining item, not the already-closed one");

    let attempts: Vec<AttemptRow> = handle
        .query(
            "SELECT outcome FROM rahi_processing_attempt WHERE outcome = 'expired'",
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(attempts.len(), 2, "each item's open attempt was closed exactly once");

    let dead = Work::dead(&handle, &DeadFilter::default(), Page::default()).await.unwrap();
    assert!(dead.is_empty(), "an expiry alone does not dead-letter below max_attempts");

    store.shutdown().await.unwrap();
}
