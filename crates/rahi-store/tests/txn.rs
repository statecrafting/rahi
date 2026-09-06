//! spec 011 FR-001: txn atomicity.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod common;

use rahi_store::{Statement, Value};
use rahi_types::Error;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Count {
    n: i64,
}

async fn count(store: &rahi_store::StoreHandle) -> i64 {
    let rows: Vec<Count> = store
        .query("SELECT COUNT(*) AS n FROM t", vec![])
        .await
        .unwrap();
    rows[0].n
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_batch_whose_last_statement_fails_leaves_no_row_from_its_first() {
    let f = common::open().await;
    let store = f.store.handle();
    store
        .execute(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT NOT NULL)",
            vec![],
        )
        .await
        .unwrap();

    let insert = "INSERT INTO t (id, v) VALUES ($1, $2)";
    let err = store
        .txn(vec![
            Statement::with_params(insert, vec![Value::from(1i64), Value::from("first")]),
            Statement::with_params(insert, vec![Value::from(2i64), Value::from("second")]),
            Statement::with_params(insert, vec![Value::from(1i64), Value::from("duplicate")]),
        ])
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::Conflict(_) | Error::Validation(_)),
        "{err}"
    );
    assert_eq!(count(&store).await, 0, "the batch rolled back whole");

    let results = store
        .txn(vec![
            Statement::with_params(insert, vec![Value::from(1i64), Value::from("first")]),
            Statement::with_params(insert, vec![Value::from(2i64), Value::from("second")]),
        ])
        .await
        .unwrap();
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|r| r.rows_affected == 1));
    assert_eq!(count(&store).await, 2);

    f.store.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_empty_batch_is_refused() {
    let f = common::open().await;
    let err = f.store.txn(vec![]).await.unwrap_err();
    assert!(matches!(err, Error::Validation(_)), "{err}");
    f.store.shutdown().await.unwrap();
}
