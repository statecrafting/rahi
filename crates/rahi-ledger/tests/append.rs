//! spec 013 FR-001 and FR-003: the append is a compare-and-swap that never
//! forks, and a chain survives the process that wrote it.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod common;

use rahi_ledger::{
    APPEND_ATTEMPTS, Decision, DecisionId, DecisionKind, Hash, Ledger, Outcome, SignedRecord,
};
use rahi_store::{Statement, Store, StoreHandle, Value};
use rahi_types::{Error, Revision, Sub};
use serde_json::json;

fn decision(id: &str) -> Decision {
    Decision::new(
        DecisionId::new(id),
        DecisionKind::new("db.write"),
        Sub::new("rauthy-subject-1"),
        Outcome::Allow,
        "covered by a declared grant",
    )
    .with_payload(json!({ "table": "notes", "wall_time": "2026-09-06T00:00:00Z" }))
    .at(Revision::new(4))
}

async fn open_ledger(store: StoreHandle) -> Ledger {
    Ledger::open(store, common::signer(), common::root())
        .await
        .expect("a fresh chain opens")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opening_writes_a_genesis_record_rooted_at_the_manifest_hash() {
    let f = common::open().await;
    let ledger = open_ledger(f.handle()).await;

    let records = ledger.records().await.unwrap();
    assert_eq!(records.len(), 1, "genesis and nothing else");
    let genesis = &records[0];
    assert_eq!(
        genesis.record.previous_record_hash,
        common::root().as_str(),
        "the genesis record links the booted manifest hash (B-2)"
    );
    let decision = genesis.decision().unwrap();
    assert_eq!(decision.kind.as_str(), DecisionKind::GENESIS);
    assert_eq!(decision.actor, Sub::new("system"));
    assert_eq!(ledger.head().await.unwrap(), genesis.hash().unwrap());

    // Opening again is idempotent: genesis is present, so nothing is written.
    let reopened = open_ledger(f.handle()).await;
    assert_eq!(reopened.records().await.unwrap().len(), 1);

    f.store.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_append_chains_onto_the_head_and_the_chain_stays_verifiable() {
    let f = common::open().await;
    let ledger = open_ledger(f.handle()).await;

    let genesis_head = ledger.head().await.unwrap();
    let first = ledger.append(decision("d-1")).await.unwrap();
    assert_eq!(ledger.head().await.unwrap(), first);

    let second = ledger.append(decision("d-2")).await.unwrap();
    assert_ne!(second, first);
    assert_eq!(ledger.head().await.unwrap(), second);

    let records = ledger.records().await.unwrap();
    assert_eq!(records.len(), 3, "genesis plus two decisions");
    assert_eq!(
        records[1].record.previous_record_hash,
        genesis_head.as_str()
    );
    assert_eq!(records[2].record.previous_record_hash, first.as_str());
    assert_eq!(
        records[1].decision().unwrap().prev_hash,
        genesis_head,
        "the payload records the parent the append actually won"
    );
    ledger.verify().await.expect("the chain verifies");

    f.store.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_unique_parent_index_admits_exactly_one_record_per_parent() {
    let f = common::open().await;
    let store = f.handle();
    let ledger = open_ledger(store.clone()).await;
    let head = ledger.head().await.unwrap();

    // Two rival records claiming the same parent, inserted the way append
    // inserts. The second is the loser: the index, not this code, refuses it.
    let mut rivals = Vec::new();
    for id in ["rival-a", "rival-b"] {
        let mut d = decision(id);
        d.prev_hash = head.clone();
        rivals.push(SignedRecord::build(&d, &common::signer()).unwrap());
    }
    let insert = "INSERT INTO kernel_decisions (id, prev_hash, hash, record) \
                  VALUES ($1, $2, $3, $4)";
    let row = |r: &SignedRecord| {
        vec![
            Value::from(r.record.id.as_str()),
            Value::from(r.record.previous_record_hash.as_str()),
            Value::from(r.record.record_hash.as_str()),
            Value::Blob(r.to_canonical_bytes().unwrap()),
        ]
    };

    store
        .txn(vec![Statement::with_params(insert, row(&rivals[0]))])
        .await
        .expect("the first record claims the parent");
    store
        .txn(vec![Statement::with_params(insert, row(&rivals[1]))])
        .await
        .expect_err("the second is refused by kernel_decisions_parent");

    assert_eq!(ledger.count().await.unwrap(), 2, "genesis plus one winner");
    assert_eq!(ledger.head().await.unwrap(), rivals[0].hash().unwrap());
    ledger.verify().await.expect("no fork");

    f.store.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_appends_all_land_on_one_linear_chain() {
    let f = common::open().await;
    let ledger = open_ledger(f.handle()).await;

    // Rounds of two, so a loser always has an attempt left: spec 013 B-3
    // gives an append three tries, and two appenders can cost at most one.
    let rounds = 5;
    for round in 0..rounds {
        let (a, b) = tokio::join!(
            {
                let ledger = ledger.clone();
                async move { ledger.append(decision(&format!("d-{round}-a"))).await }
            },
            {
                let ledger = ledger.clone();
                async move { ledger.append(decision(&format!("d-{round}-b"))).await }
            }
        );
        a.unwrap_or_else(|e| panic!("round {round} first append: {e}"));
        b.unwrap_or_else(|e| panic!("round {round} second append: {e}"));
    }

    let records = ledger.records().await.unwrap();
    assert_eq!(
        records.len(),
        1 + rounds * 2,
        "every append committed exactly once"
    );
    assert_eq!(
        records.last().unwrap().hash().unwrap(),
        ledger.head().await.unwrap(),
        "one head: the chain did not fork"
    );
    ledger.verify().await.expect("the chain verifies");

    f.store.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_chain_written_by_one_process_reopens_and_continues_in_another() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = common::config(&dir.path().join("hiqlite"));

    let store = Store::open(&cfg).await.unwrap();
    let ledger = open_ledger(store.handle()).await;
    ledger.append(decision("d-1")).await.unwrap();
    let head = ledger.append(decision("d-2")).await.unwrap();
    let exported = ledger.export_jsonl().await.unwrap();
    store.shutdown().await.unwrap();
    drop(ledger);
    drop(store);

    let store = Store::open(&cfg).await.unwrap();
    let ledger = open_ledger(store.handle()).await;
    assert_eq!(
        ledger.head().await.unwrap(),
        head,
        "the reopened chain continues from the same head"
    );
    assert_eq!(
        ledger.records().await.unwrap().len(),
        3,
        "nothing re-genesised"
    );
    assert_eq!(ledger.export_jsonl().await.unwrap(), exported);

    let third = ledger.append(decision("d-3")).await.unwrap();
    assert_eq!(ledger.records().await.unwrap()[2].hash().unwrap(), head);
    assert_eq!(ledger.head().await.unwrap(), third);
    ledger.verify().await.expect("the chain verifies");

    store.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reusing_a_decision_id_is_a_conflict_rather_than_a_retry() {
    let f = common::open().await;
    let ledger = open_ledger(f.handle()).await;
    ledger.append(decision("d-1")).await.unwrap();

    let mut different = decision("d-1");
    different.reason = "a different decision under a used name".to_owned();
    let err = ledger.append(different).await.expect_err("refused");
    assert!(
        matches!(err, Error::Conflict(_)),
        "a used id is a conflict, not {APPEND_ATTEMPTS} lost compare-and-swaps: {err}"
    );
    assert_eq!(ledger.count().await.unwrap(), 2);

    f.store.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_decision_without_an_id_never_reaches_the_store() {
    let f = common::open().await;
    let ledger = open_ledger(f.handle()).await;
    let before = ledger.head().await.unwrap();

    let err = ledger.append(decision("")).await.expect_err("refused");
    assert!(matches!(err, Error::Validation(_)), "{err}");
    assert_eq!(ledger.head().await.unwrap(), before);
    assert_eq!(ledger.count().await.unwrap(), 1);

    f.store.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_ledger_booted_against_another_manifest_refuses_the_chain() {
    let f = common::open().await;
    let ledger = open_ledger(f.handle()).await;
    ledger.append(decision("d-1")).await.unwrap();

    let other_manifest = Hash::parse(format!("sha256:{}", "ab".repeat(32))).unwrap();
    let err = Ledger::open(f.handle(), common::signer(), other_manifest)
        .await
        .expect_err("refused");
    assert!(
        matches!(err, Error::Integrity(_)),
        "spec 015 B-8: a chain whose genesis parent is not this manifest's hash is damage: {err}"
    );

    f.store.shutdown().await.unwrap();
}
