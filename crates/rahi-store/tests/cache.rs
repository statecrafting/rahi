//! spec 012 B-6: the cache group is derived state, and this file pins what
//! "derived" actually costs with hiqlite 0.14.
//!
//! B-6 expects a restart to clear the cache. It does not: hiqlite persists
//! the cache Raft log under `data_dir` and replays it at startup, so a KV
//! value and a counter both outlive the process (spec 012 D-6). What makes
//! the group unfit for durable state is everything else about it: entries
//! expire on their own TTL, a cleared cache is gone, a restore resets the
//! cluster, and nothing in it is written by the transaction that decided
//! anything. The assertions below record hiqlite's real behaviour so an
//! upgrade that changes it is caught here rather than inside a controller.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod common;

use std::time::Duration;

use rahi_store::{Store, Value};
use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Session {
    sub: String,
}

#[derive(Debug, Deserialize)]
struct Count {
    n: i64,
}

fn session(sub: &str) -> Session {
    Session {
        sub: sub.to_owned(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cached_value_expires_and_can_be_deleted() {
    let f = common::open().await;
    let store = f.store.handle();

    store
        .kv_put("hello:s-1", &session("s-1"), None)
        .await
        .unwrap();
    assert_eq!(
        store.kv_get::<Session>("hello:s-1").await.unwrap(),
        Some(session("s-1"))
    );
    store.kv_del("hello:s-1").await.unwrap();
    assert_eq!(store.kv_get::<Session>("hello:s-1").await.unwrap(), None);

    store
        .kv_put("hello:s-2", &session("s-2"), Some(1))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        store.kv_get::<Session>("hello:s-2").await.unwrap(),
        None,
        "a value with a TTL is gone when it expires, which is why it is not a record"
    );

    assert_eq!(store.counter_get("rate:s-1").await.unwrap(), None);
    assert_eq!(store.counter_add("rate:s-1", 2).await.unwrap(), 2);
    assert_eq!(store.counter_add("rate:s-1", 1).await.unwrap(), 3);
    assert_eq!(store.counter_get("rate:s-1").await.unwrap(), Some(3));

    f.store.shutdown().await.unwrap();
}

/// The SQL group is durable. The cache group is replayed, which is not the
/// same thing and is not something an application may lean on: see this
/// file's header and spec 012 D-6.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restart_keeps_the_sql_group_and_replays_the_cache() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = common::config(&dir.path().join("hiqlite"));

    let store = Store::open(&cfg).await.unwrap();
    let handle = store.handle();
    handle
        .execute("CREATE TABLE durable (id INTEGER PRIMARY KEY)", vec![])
        .await
        .unwrap();
    handle
        .execute(
            "INSERT INTO durable (id) VALUES ($1)",
            vec![Value::from(1i64)],
        )
        .await
        .unwrap();
    handle
        .kv_put("hello:s-1", &session("s-1"), None)
        .await
        .unwrap();
    handle
        .kv_put("hello:s-2", &session("s-2"), None)
        .await
        .unwrap();
    handle.kv_del("hello:s-2").await.unwrap();
    handle.counter_add("rate:s-1", 3).await.unwrap();

    store.shutdown().await.unwrap();
    drop(store);

    let store = Store::open(&cfg).await.unwrap();
    let handle = store.handle();
    let rows: Vec<Count> = handle
        .query("SELECT COUNT(*) AS n FROM durable", vec![])
        .await
        .unwrap();
    assert_eq!(rows[0].n, 1, "the SQL group is durable");
    assert_eq!(
        handle.kv_get::<Session>("hello:s-1").await.unwrap(),
        Some(session("s-1")),
        "hiqlite 0.14 replays the cache log at startup: B-6's restart is not the eviction"
    );
    assert_eq!(
        handle.kv_get::<Session>("hello:s-2").await.unwrap(),
        None,
        "the replay honours the delete"
    );
    assert_eq!(
        handle.counter_get("rate:s-1").await.unwrap(),
        Some(3),
        "counters are replayed with the rest of the cache log"
    );

    store.shutdown().await.unwrap();
}
