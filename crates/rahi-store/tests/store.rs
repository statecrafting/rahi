//! spec 011 FR-001 (open, health, execute, query), FR-002, FR-004.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod common;

use std::collections::BTreeMap;
use std::path::PathBuf;

use rahi_store::{Store, StoreConfig, Value};
use rahi_types::{Config, Error};
use serde::Deserialize;

#[derive(Debug, Deserialize, PartialEq)]
struct Row {
    id: i64,
    name: String,
    note: Option<String>,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opens_reports_healthy_and_round_trips_a_write() {
    let f = common::open().await;
    let store = f.store.handle();
    store.health().await.expect("both groups healthy");
    assert!(store.is_leader().await, "a single voter leads");

    store
        .execute(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT NOT NULL, note TEXT)",
            vec![],
        )
        .await
        .unwrap();
    let res = store
        .execute(
            "INSERT INTO t (id, name, note) VALUES ($1, $2, $3)",
            vec![Value::from(1i64), Value::from("one"), Value::Null],
        )
        .await
        .unwrap();
    assert_eq!(res.rows_affected, 1);

    let local: Vec<Row> = store
        .query("SELECT id, name, note FROM t", vec![])
        .await
        .unwrap();
    assert_eq!(
        local,
        [Row {
            id: 1,
            name: "one".to_owned(),
            note: None
        }]
    );

    let consistent: Vec<Row> = store
        .query_consistent(
            "SELECT id, name, note FROM t WHERE id = $1",
            vec![Value::from(1i64)],
        )
        .await
        .unwrap();
    assert_eq!(consistent, local, "both read calls map into the same type");

    f.store.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bad_statement_is_a_validation_error() {
    let f = common::open().await;
    let err = f
        .store
        .execute("THIS IS NOT SQL", vec![])
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Validation(_)), "{err}");
    let err = f
        .store
        .query::<Row>("SELECT nope FROM nowhere", vec![])
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Validation(_)), "{err}");
    f.store.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refuses_a_data_dir_inside_rauthys() {
    for dir in ["/data/rauthy", "/data/rauthy/hiqlite", "rauthy"] {
        let cfg = common::config(&PathBuf::from(dir));
        let err = Store::open(&cfg).await.unwrap_err();
        assert!(matches!(err, Error::Config(_)), "{dir}: {err}");
        assert!(err.message().contains("rauthy"), "{dir}: {err}");
    }
}

#[test]
fn store_config_derives_from_the_chassis_config() {
    let env: BTreeMap<String, String> = BTreeMap::from([(
        "RAHI_PUBLIC_URL".to_owned(),
        "https://cell.example.test".to_owned(),
    )]);
    let chassis = Config::from_env(&env).unwrap();
    let cfg = StoreConfig::from_config(&chassis, common::secrets());
    assert_eq!(cfg.data_dir, PathBuf::from("/data/hiqlite"));
    assert_eq!(cfg.api_addr.port(), 8300);
    assert_eq!(cfg.raft_addr.port(), 8400);
    for port in [cfg.api_addr.port(), cfg.raft_addr.port()] {
        assert!(
            port != 8100 && port != 8200,
            "port {port} is rauthy's hiqlite"
        );
    }
    assert_eq!(cfg.node_id, 1);
    assert!(cfg.nodes.is_empty());
    assert_eq!(
        cfg.backup_dir(),
        PathBuf::from("/data/hiqlite/state_machine/backups")
    );
    assert!(
        !format!("{cfg:?}").contains("raft-secret"),
        "secrets are redacted in Debug"
    );
}

#[test]
fn the_config_has_no_field_for_rauthys_directory() {
    let cfg = common::config(&PathBuf::from("/tmp/x"));
    let json = serde_json::to_value(&cfg).unwrap();
    let keys: Vec<&str> = json
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert!(!keys.iter().any(|k| k.contains("rauthy")), "{keys:?}");
}
