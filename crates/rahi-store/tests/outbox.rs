//! spec 012 FR-003 and B-4: the outbox row commits with the resource or not
//! at all, and a drain publishes only what is already durable.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod common;

use std::time::Duration;

use rahi_store::{
    Envelope, Migration, Notify, Outbox, Statement, StoreHandle, TxnBuilder, Value,
    coordination_migration,
};
use rahi_types::Revision;
use serde::Deserialize;

const NOTES: &str = "CREATE TABLE notes (id TEXT PRIMARY KEY, body TEXT NOT NULL)";

#[derive(Debug, Deserialize)]
struct Count {
    n: i64,
}

async fn prepare(store: &StoreHandle) {
    store
        .migrate(&[coordination_migration(1), Migration::new(2, "notes", NOTES)])
        .await
        .unwrap();
}

async fn count(store: &StoreHandle, table: &str) -> i64 {
    let rows: Vec<Count> = store
        .query(format!("SELECT COUNT(*) AS n FROM {table}"), vec![])
        .await
        .unwrap();
    rows[0].n
}

fn insert(id: &str) -> Statement {
    Statement::with_params(
        "INSERT INTO notes (id, body) VALUES ($1, $2)",
        vec![Value::from(id), Value::from("body")],
    )
}

/// FR-003: a batch that stages a resource and its envelope and then fails
/// leaves neither behind.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failing_statement_leaves_neither_the_resource_nor_its_outbox_row() {
    let f = common::open().await;
    let store = f.store.handle();
    prepare(&store).await;

    let mut txn = TxnBuilder::new();
    txn.push(insert("n-1"));
    Outbox::stage(
        &mut txn,
        &Envelope::new("note", None, "n-1", Revision::new(1)),
    );
    txn.push(insert("n-1"));
    assert_eq!(txn.len(), 3);

    store
        .txn(txn.into_statements())
        .await
        .expect_err("the duplicate id fails the batch");
    assert_eq!(count(&store, "notes").await, 0, "no resource");
    assert_eq!(count(&store, "outbox").await, 0, "and no envelope");

    let mut txn = TxnBuilder::new();
    txn.push(insert("n-1"));
    Outbox::stage(
        &mut txn,
        &Envelope::new("note", None, "n-1", Revision::new(1)),
    );
    store.txn(txn.into_statements()).await.unwrap();
    assert_eq!(count(&store, "notes").await, 1);
    assert_eq!(count(&store, "outbox").await, 1, "both or neither");

    f.store.shutdown().await.unwrap();
}

/// B-4: a drain publishes each staged row in order and deletes exactly those,
/// and nothing is left to publish twice.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_drain_publishes_staged_rows_in_order_and_deletes_them() {
    let f = common::open().await;
    let store = f.store.handle();
    prepare(&store).await;
    let notify = Notify::new(store.clone());
    let mut listen = notify.listen();

    let mut txn = TxnBuilder::new();
    for (revision, id) in [(1u64, "n-1"), (2, "n-2"), (3, "n-3")] {
        txn.push(insert(id));
        Outbox::stage(
            &mut txn,
            &Envelope::new("note", Some("acme".to_owned()), id, Revision::new(revision)),
        );
    }
    store.txn(txn.into_statements()).await.unwrap();
    assert_eq!(count(&store, "outbox").await, 3);

    assert_eq!(
        Outbox::drain(&store, 2).await.unwrap(),
        2,
        "the batch size is respected"
    );
    assert_eq!(count(&store, "outbox").await, 1, "only the drained rows go");
    assert_eq!(Outbox::drain(&store, 10).await.unwrap(), 1);
    assert_eq!(count(&store, "outbox").await, 0);
    assert_eq!(
        Outbox::drain(&store, 10).await.unwrap(),
        0,
        "an empty outbox drains nothing"
    );

    let mut received = Vec::new();
    while received.len() < 3 {
        let env = tokio::time::timeout(Duration::from_secs(5), listen.recv())
            .await
            .expect("the drain published within five seconds")
            .expect("the stream stays open");
        received.push(env);
    }
    assert_eq!(
        received.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
        ["n-1", "n-2", "n-3"],
        "published in the order they were staged"
    );
    assert_eq!(received[2].revision, Revision::new(3));
    assert_eq!(received[0].tenant.as_deref(), Some("acme"));

    f.store.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_drain_of_nothing_is_refused_rather_than_looping() {
    let f = common::open().await;
    let store = f.store.handle();
    prepare(&store).await;
    let err = Outbox::drain(&store, 0).await.unwrap_err();
    assert!(matches!(err, rahi_types::Error::Validation(_)), "{err}");
    f.store.shutdown().await.unwrap();
}
