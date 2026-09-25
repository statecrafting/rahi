//! spec 045 FR-001, FR-002, FR-003, FR-008: classification, the CAS race, the
//! compile-fail guard on a changed classification, and retention against a
//! real single-node store.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod common;

use rahi_store::{
    Classification, ContentDigest, EraseScope, ReceiptKey, ReceiptMeta, Receipts, Statement,
    StoreHandle, TxnBuilder, Value, coordination_migration, receipt_migration,
};
use rahi_types::{Error, UnixSeconds};

async fn migrated(store: &StoreHandle) {
    store
        .migrate(&[coordination_migration(1), receipt_migration(2)])
        .await
        .unwrap();
}

fn now(secs: u64) -> UnixSeconds {
    UnixSeconds::new(secs)
}

async fn submit(store: &StoreHandle, txn: TxnBuilder) -> Result<(), Error> {
    store.txn(txn.into_statements()).await.map(|_| ())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_classification_outcome_is_reachable() {
    let f = common::open().await;
    let store = f.store.handle();
    migrated(&store).await;

    let key = ReceiptKey::new("acme", "email:imap:acct-7", "msg-1").unwrap();
    let digest_a = ContentDigest::of(b"hello world");
    let digest_b = ContentDigest::of(b"hello world, revised");
    let digest_c = ContentDigest::of(b"a collision body");

    assert_eq!(
        Receipts::classify(&store, &key, &digest_a).await.unwrap(),
        Classification::FirstSeen
    );

    let mut txn = TxnBuilder::new();
    Receipts::stage_first(&mut txn, &key, &digest_a, &ReceiptMeta::default(), now(1)).unwrap();
    submit(&store, txn).await.unwrap();

    match Receipts::classify(&store, &key, &digest_a).await.unwrap() {
        Classification::Redelivered {
            revision,
            is_head,
            outcome,
        } => {
            assert_eq!(revision, 1);
            assert!(is_head);
            assert_eq!(outcome, None);
        }
        other => panic!("expected Redelivered, got {other:?}"),
    }

    let changed = match Receipts::classify(&store, &key, &digest_b).await.unwrap() {
        Classification::Changed(changed) => changed,
        other => panic!("expected Changed, got {other:?}"),
    };
    assert_eq!(changed.head_revision(), 1);
    assert_eq!(changed.head_digest(), digest_a.as_str());

    let mut txn = TxnBuilder::new();
    Receipts::stage_revision(
        &mut txn,
        &key,
        &digest_b,
        &changed,
        &ReceiptMeta::default(),
        now(2),
    )
    .unwrap();
    submit(&store, txn).await.unwrap();

    // The old content redelivers: still recognised, no longer the head.
    match Receipts::classify(&store, &key, &digest_a).await.unwrap() {
        Classification::Redelivered {
            revision, is_head, ..
        } => {
            assert_eq!(revision, 1);
            assert!(!is_head, "an older revision redelivering is not the head");
        }
        other => panic!("expected Redelivered, got {other:?}"),
    }

    // A third, different body under the same key is a collision, not a drop.
    let changed = match Receipts::classify(&store, &key, &digest_c).await.unwrap() {
        Classification::Changed(changed) => changed,
        other => panic!("expected Changed, got {other:?}"),
    };
    assert_eq!(
        changed.head_revision(),
        2,
        "the collision guards on the accepted head"
    );
    let mut txn = TxnBuilder::new();
    Receipts::stage_collision(
        &mut txn,
        &key,
        &digest_c,
        &changed,
        &ReceiptMeta::default(),
        now(3),
    )
    .unwrap();
    submit(&store, txn).await.unwrap();

    match Receipts::classify(&store, &key, &digest_c).await.unwrap() {
        Classification::CollisionRedelivered { revision } => assert_eq!(revision, 3),
        other => panic!("expected CollisionRedelivered, got {other:?}"),
    }
    // The accepted head is unchanged by the collision (FR-003).
    match Receipts::classify(&store, &key, &digest_b).await.unwrap() {
        Classification::Redelivered { is_head, .. } => assert!(is_head),
        other => panic!("expected Redelivered, got {other:?}"),
    }

    // Erasure: classifies as Erased whatever the digest, forever.
    let mut txn = TxnBuilder::new();
    Receipts::stage_erasure(&mut txn, &EraseScope::Identity(key.clone()), now(4));
    submit(&store, txn).await.unwrap();
    for digest in [&digest_a, &digest_b, &digest_c] {
        match Receipts::classify(&store, &key, digest).await.unwrap() {
            Classification::Erased { erased_at } => assert_eq!(erased_at, now(4)),
            other => panic!("expected Erased, got {other:?}"),
        }
    }

    f.store.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_key_digest_is_equal_across_two_processes() {
    let a = ReceiptKey::new("acme", "email:imap:acct-7", "msg-1").unwrap();
    let b = ReceiptKey::new("acme", "email:imap:acct-7", "msg-1").unwrap();
    assert_eq!(a.key_digest(), b.key_digest());
}

/// FR-002: two batches staging `stage_first` for one identity. Exactly one
/// commits; the loser's domain row, processing row, and outbox row are
/// absent (spec 045 B-8, B-9).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_stage_first_for_one_identity_conflicts_and_leaves_nothing() {
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

    let key = ReceiptKey::new("acme", "email:imap:acct-7", "msg-1").unwrap();
    let digest = ContentDigest::of(b"body");

    let mut winner = TxnBuilder::new();
    Receipts::stage_first(&mut winner, &key, &digest, &ReceiptMeta::default(), now(1)).unwrap();
    winner.push(Statement::with_params(
        "INSERT INTO domain (id, note) VALUES ($1, $2)",
        vec![Value::Integer(1), Value::from("winner")],
    ));
    submit(&store, winner).await.unwrap();

    let mut loser = TxnBuilder::new();
    Receipts::stage_first(&mut loser, &key, &digest, &ReceiptMeta::default(), now(1)).unwrap();
    loser.push(Statement::with_params(
        "INSERT INTO domain (id, note) VALUES ($1, $2)",
        vec![Value::Integer(2), Value::from("loser")],
    ));
    let err = submit(&store, loser).await.unwrap_err();
    assert!(matches!(err, Error::Conflict(_)), "{err}");

    let rows: Vec<serde_json::Value> = store
        .query("SELECT id, note FROM domain ORDER BY id", vec![])
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "the loser's domain row never lands");

    f.store.shutdown().await.unwrap();
}

/// FR-008: erasure leaves a tombstone that classifies as `Erased` and
/// refuses every staging call against it, even one built from a `Changed`
/// handle a caller obtained before the erasure raced it (spec 045 B-22).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn erasure_refuses_every_staging_call_against_it() {
    let f = common::open().await;
    let store = f.store.handle();
    migrated(&store).await;

    let key = ReceiptKey::new("acme", "ns", "msg-1").unwrap();
    let digest = ContentDigest::of(b"body");
    let mut txn = TxnBuilder::new();
    Receipts::stage_first(&mut txn, &key, &digest, &ReceiptMeta::default(), now(1)).unwrap();
    submit(&store, txn).await.unwrap();

    let changed_digest = ContentDigest::of(b"other body");
    let changed = match Receipts::classify(&store, &key, &changed_digest)
        .await
        .unwrap()
    {
        Classification::Changed(changed) => changed,
        other => panic!("expected Changed, got {other:?}"),
    };

    let mut txn = TxnBuilder::new();
    Receipts::stage_erasure(&mut txn, &EraseScope::Identity(key.clone()), now(2));
    submit(&store, txn).await.unwrap();

    // The identity is erased; the pre-erasure `Changed` handle still cannot
    // land: the guard sees the erased head and aborts the batch.
    let mut txn = TxnBuilder::new();
    Receipts::stage_revision(
        &mut txn,
        &key,
        &changed_digest,
        &changed,
        &ReceiptMeta::default(),
        now(3),
    )
    .unwrap();
    let err = submit(&store, txn).await.unwrap_err();
    assert!(matches!(err, Error::Conflict(_)), "{err}");

    match Receipts::classify(&store, &key, &digest).await.unwrap() {
        Classification::Erased { .. } => {}
        other => panic!("expected Erased, got {other:?}"),
    }

    f.store.shutdown().await.unwrap();
}

/// FR-008: retention compacts only once every processing row is terminal,
/// and a compacted receipt still classifies a late redelivery (no outcome)
/// until `tombstone_until`, after which a redelivery is `FirstSeen`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retention_compacts_only_when_processing_is_terminal_and_keeps_classifying_until_tombstone_until()
 {
    let f = common::open().await;
    let store = f.store.handle();
    migrated(&store).await;

    let key = ReceiptKey::new("acme", "ns", "msg-1").unwrap();
    let digest = ContentDigest::of(b"body");
    let meta = ReceiptMeta {
        retain_until: Some(now(10)),
        tombstone_until: Some(now(20)),
        outcome: Some("ref-1".to_owned()),
    };
    let mut txn = TxnBuilder::new();
    Receipts::stage_first(&mut txn, &key, &digest, &meta, now(1)).unwrap();
    submit(&store, txn).await.unwrap();

    let processing_key = rahi_store::ProcessingKey::new(key.clone(), 1, "extract", "v1").unwrap();
    let mut txn = TxnBuilder::new();
    rahi_store::Work::stage_work(&mut txn, &processing_key, now(1));
    submit(&store, txn).await.unwrap();

    let policy = rahi_store::RetryPolicy {
        max_attempts: 3,
        base: std::time::Duration::from_millis(1),
        cap: std::time::Duration::from_secs(1),
    };
    // Past retain_until, but the processing row is still pending: nothing
    // compacts.
    let report = rahi_store::Work::sweep(&store, "ns", &policy, now(15), 10)
        .await
        .unwrap();
    assert_eq!(
        report.compacted, 0,
        "an open processing row blocks compaction"
    );
    match Receipts::classify(&store, &key, &digest).await.unwrap() {
        Classification::Redelivered { outcome, .. } => {
            assert_eq!(outcome.as_deref(), Some("ref-1"));
        }
        other => panic!("expected Redelivered, got {other:?}"),
    }

    // Finish the work, then retention can compact.
    let claim = rahi_store::Work::reserve(
        &store,
        &processing_key,
        "worker-1",
        std::time::Duration::from_secs(10),
        now(15),
    )
    .await
    .unwrap()
    .expect("the row is eligible");
    let mut txn = TxnBuilder::new();
    rahi_store::Work::complete(&mut txn, &claim, now(15));
    submit(&store, txn).await.unwrap();

    let report = rahi_store::Work::sweep(&store, "ns", &policy, now(15), 10)
        .await
        .unwrap();
    assert_eq!(report.compacted, 1, "every processing row is now terminal");

    // Compacted: same content still redelivers, with no outcome.
    match Receipts::classify(&store, &key, &digest).await.unwrap() {
        Classification::Redelivered { outcome, .. } => assert_eq!(outcome, None),
        other => panic!("expected Redelivered with no outcome, got {other:?}"),
    }

    // Past tombstone_until, the sweep deletes the tombstone.
    let report = rahi_store::Work::sweep(&store, "ns", &policy, now(25), 10)
        .await
        .unwrap();
    assert_eq!(report.tombstones_deleted, 1);
    assert_eq!(
        Receipts::classify(&store, &key, &digest).await.unwrap(),
        Classification::FirstSeen,
        "past tombstone_until, a redelivery is FirstSeen"
    );

    f.store.shutdown().await.unwrap();
}

/// spec 045 D-20: a new revision staged over a compacted tombstone makes the
/// identity live again, so the old tombstone's horizon never deletes it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_revision_over_a_compacted_tombstone_is_live_and_outlives_the_old_horizon() {
    let f = common::open().await;
    let store = f.store.handle();
    migrated(&store).await;
    let policy = rahi_store::RetryPolicy {
        max_attempts: 3,
        base: std::time::Duration::from_millis(1),
        cap: std::time::Duration::from_secs(1),
    };

    let key = ReceiptKey::new("acme", "ns", "msg-1").unwrap();
    let first = ContentDigest::of(b"body-a");
    let meta = ReceiptMeta {
        retain_until: Some(now(10)),
        tombstone_until: Some(now(20)),
        outcome: None,
    };
    let mut txn = TxnBuilder::new();
    Receipts::stage_first(&mut txn, &key, &first, &meta, now(1)).unwrap();
    submit(&store, txn).await.unwrap();
    let report = rahi_store::Work::sweep(&store, "ns", &policy, now(15), 10)
        .await
        .unwrap();
    assert_eq!(report.compacted, 1, "no processing rows, so it compacts");

    // Changed content arrives against the tombstone and is accepted.
    let second = ContentDigest::of(b"body-b");
    let Classification::Changed(changed) = Receipts::classify(&store, &key, &second).await.unwrap()
    else {
        panic!("a different digest against a tombstone is Changed");
    };
    let mut txn = TxnBuilder::new();
    Receipts::stage_revision(
        &mut txn,
        &key,
        &second,
        &changed,
        &ReceiptMeta::default(),
        now(16),
    )
    .unwrap();
    submit(&store, txn).await.unwrap();

    // Past the old tombstone_until, the live identity is untouched.
    let report = rahi_store::Work::sweep(&store, "ns", &policy, now(25), 10)
        .await
        .unwrap();
    assert_eq!(report.tombstones_deleted, 0, "the identity is live again");
    match Receipts::classify(&store, &key, &second).await.unwrap() {
        Classification::Redelivered {
            revision, is_head, ..
        } => {
            assert_eq!(revision, 2);
            assert!(is_head);
        }
        other => panic!("expected Redelivered of revision 2, got {other:?}"),
    }
    #[derive(serde::Deserialize)]
    struct KeyRow {
        key: Option<String>,
    }
    let rows: Vec<KeyRow> = store
        .query(
            "SELECT key FROM rahi_receipt_head WHERE key_digest = $1",
            vec![Value::from(key.key_digest())],
        )
        .await
        .unwrap();
    assert_eq!(rows[0].key.as_deref(), Some("msg-1"), "the raw key is back");

    f.store.shutdown().await.unwrap();
}

/// spec 045 B-21, B-22: erasing an identity that was never delivered still
/// leaves its tombstone, so its first delivery afterwards is `Erased` and
/// cannot be staged.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn erasing_a_never_delivered_identity_still_refuses_its_first_delivery() {
    let f = common::open().await;
    let store = f.store.handle();
    migrated(&store).await;

    let key = ReceiptKey::new("acme", "ns", "never-seen").unwrap();
    let digest = ContentDigest::of(b"body");
    let mut txn = TxnBuilder::new();
    Receipts::stage_erasure(&mut txn, &EraseScope::Identity(key.clone()), now(5));
    submit(&store, txn).await.unwrap();

    assert_eq!(
        Receipts::classify(&store, &key, &digest).await.unwrap(),
        Classification::Erased { erased_at: now(5) }
    );
    let mut txn = TxnBuilder::new();
    Receipts::stage_first(&mut txn, &key, &digest, &ReceiptMeta::default(), now(6)).unwrap();
    let err = submit(&store, txn).await.unwrap_err();
    assert!(matches!(err, Error::Conflict(_)), "{err}");

    // A namespace-scoped erasure finds its identities by the scope columns
    // it then clears, and every one of them classifies as erased.
    let others: Vec<ReceiptKey> = (1..=2)
        .map(|n| ReceiptKey::new("acme", "ns2", format!("m-{n}")).unwrap())
        .collect();
    for other in &others {
        let mut txn = TxnBuilder::new();
        Receipts::stage_first(&mut txn, other, &digest, &ReceiptMeta::default(), now(7)).unwrap();
        submit(&store, txn).await.unwrap();
    }
    let mut txn = TxnBuilder::new();
    Receipts::stage_erasure(
        &mut txn,
        &EraseScope::Namespace {
            tenant: "acme".to_owned(),
            namespace: "ns2".to_owned(),
        },
        now(8),
    );
    submit(&store, txn).await.unwrap();
    for other in &others {
        assert_eq!(
            Receipts::classify(&store, other, &digest).await.unwrap(),
            Classification::Erased { erased_at: now(8) }
        );
    }

    f.store.shutdown().await.unwrap();
}
