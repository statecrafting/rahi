//! spec 012 FR-004 and B-5: the revision column is what a consumer trusts.
//! Nothing in this file listens for a notify.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod common;

use rahi_store::{Migration, Statement, StoreHandle, TxnBuilder, Value, Watermark};
use rahi_types::{Error, Revision};
use serde::Deserialize;

const NOTES: &str = "CREATE TABLE notes (\
    id TEXT PRIMARY KEY, \
    body TEXT NOT NULL, \
    revision INTEGER NOT NULL DEFAULT 0)";

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct Note {
    id: String,
    revision: i64,
}

async fn prepare(store: &StoreHandle) {
    store
        .migrate(&[Migration::new(1, "notes", NOTES)])
        .await
        .unwrap();
}

fn insert(id: &str) -> Statement {
    Statement::with_params(
        "INSERT INTO notes (id, body) VALUES ($1, $2)",
        vec![Value::from(id), Value::from("body")],
    )
}

async fn notes(store: &StoreHandle) -> Vec<Note> {
    store
        .query("SELECT id, revision FROM notes ORDER BY id", vec![])
        .await
        .unwrap()
}

/// FR-004: a consumer that never sees a notification still observes every
/// revision, by asking for everything above the last one it handled.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_consumer_polling_since_observes_every_revision() {
    let f = common::open().await;
    let store = f.store.handle();
    prepare(&store).await;

    let mut seen = Revision::ZERO;
    let mut observed = Vec::new();
    for id in ["n-1", "n-2", "n-3"] {
        let mut txn = TxnBuilder::new();
        txn.push(insert(id));
        Watermark::next(&mut txn, "notes").unwrap();
        store.txn(txn.into_statements()).await.unwrap();

        // The consumer's tick: no envelope arrived, and it polls anyway.
        let unseen = Watermark::since(&store, "notes", seen).await.unwrap();
        assert_eq!(unseen.len(), 1, "one write, one unseen revision");
        observed.extend(unseen.iter().copied());
        seen = *unseen.last().unwrap();
    }

    assert_eq!(
        observed,
        [Revision::new(1), Revision::new(2), Revision::new(3)],
        "stamped in order, none missed"
    );
    assert!(
        Watermark::since(&store, "notes", seen)
            .await
            .unwrap()
            .is_empty(),
        "a caught-up consumer sees nothing"
    );
    assert_eq!(
        Watermark::since(&store, "notes", Revision::ZERO)
            .await
            .unwrap()
            .len(),
        3,
        "a consumer starting from scratch sees the whole history"
    );

    f.store.shutdown().await.unwrap();
}

/// B-5: the stamp is computed and applied inside the caller's transaction,
/// and one transaction is one revision.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_transaction_is_one_revision() {
    let f = common::open().await;
    let store = f.store.handle();
    prepare(&store).await;

    let mut txn = TxnBuilder::new();
    txn.push(insert("n-1"));
    txn.push(insert("n-2"));
    Watermark::next(&mut txn, "notes").unwrap();
    store.txn(txn.into_statements()).await.unwrap();
    assert_eq!(
        notes(&store).await,
        [
            Note {
                id: "n-1".to_owned(),
                revision: 1
            },
            Note {
                id: "n-2".to_owned(),
                revision: 1
            }
        ],
        "both rows of one batch carry that batch's revision"
    );

    // An update is stamped the same way: the row goes back to ZERO and the
    // stamp lifts it above everything already written.
    let mut txn = TxnBuilder::new();
    txn.push(Statement::with_params(
        "UPDATE notes SET body = $1, revision = 0 WHERE id = $2",
        vec![Value::from("edited"), Value::from("n-1")],
    ));
    Watermark::next(&mut txn, "notes").unwrap();
    store.txn(txn.into_statements()).await.unwrap();
    assert_eq!(notes(&store).await[0].revision, 2);
    assert_eq!(
        Watermark::since(&store, "notes", Revision::new(1))
            .await
            .unwrap(),
        [Revision::new(2)],
        "the consumer sees the update as a new revision"
    );

    f.store.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_table_name_that_is_not_an_identifier_is_refused() {
    let f = common::open().await;
    let store = f.store.handle();
    prepare(&store).await;

    let mut txn = TxnBuilder::new();
    let err = Watermark::next(&mut txn, "notes; DROP TABLE notes").unwrap_err();
    assert!(matches!(err, Error::Validation(_)), "{err}");
    assert!(txn.is_empty(), "a refused stamp stages nothing");

    let err = Watermark::since(&store, "\"notes\"", Revision::ZERO)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Validation(_)), "{err}");

    f.store.shutdown().await.unwrap();
}
