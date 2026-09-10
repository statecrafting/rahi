//! spec 032 B-5: a client attached to a running node takes the backup the
//! node writes, knows whether that node leads, and starts nothing itself.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod common;

use rahi_store::Store;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_attached_client_backs_up_through_the_running_node() {
    let f = common::open().await;
    f.store
        .execute("CREATE TABLE t (id INTEGER PRIMARY KEY)", vec![])
        .await
        .unwrap();

    let attached = Store::attach(f.store.config())
        .await
        .expect("the running node answers a client");
    assert!(attached.is_attached());
    assert!(!f.store.is_attached());
    assert!(attached.is_leader().await, "the single voter leads");

    let before = attached.backup_list_local().await.unwrap();
    let id = attached
        .backup()
        .await
        .expect("the attached client takes a backup");
    let path = f.store.config().backup_dir().join(id.as_str());
    assert!(path.is_file(), "{} exists", path.display());
    let after = attached.backup_list_local().await.unwrap();
    assert_eq!(after.len(), before.len() + 1, "{after:?}");
    assert!(after.iter().any(|l| l.id == id));

    // The node's own listing agrees with the filesystem read.
    let native = f.store.backup_list_local().await.unwrap();
    assert_eq!(
        native.iter().map(|l| l.id.clone()).collect::<Vec<_>>(),
        after.iter().map(|l| l.id.clone()).collect::<Vec<_>>()
    );

    attached.shutdown().await.unwrap();
    f.store.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pure_client_reaches_the_cluster_through_the_peer_list() {
    let f = common::open().await;
    let mut cfg = f.store.config().clone();
    assert!(
        matches!(
            Store::connect(&cfg).await,
            Err(rahi_types::Error::Config(_))
        ),
        "no peers, no client"
    );
    cfg.nodes = vec![rahi_store::Peer {
        id: 1,
        raft_addr: cfg.raft_addr.to_string(),
        api_addr: cfg.api_addr.to_string(),
    }];
    let client = Store::connect(&cfg).await.expect("the peer answers");
    assert!(client.is_attached());
    assert!(
        client.is_leader().await,
        "a client writes through the leader"
    );
    client
        .execute("CREATE TABLE c (id INTEGER PRIMARY KEY)", vec![])
        .await
        .unwrap();
    let rows: Vec<(String,)> = f
        .store
        .query("SELECT name FROM sqlite_master WHERE name = 'c'", vec![])
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "the write landed on the node");
    let err = client.backup_list_local().await.unwrap_err();
    assert!(matches!(err, rahi_types::Error::Config(_)), "{err}");
    client.shutdown().await.unwrap();
    f.store.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attach_to_nothing_is_an_error_and_opens_no_data_dir() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = common::config(&dir.path().join("hiqlite"));
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), Store::attach(&cfg)).await;
    assert!(
        !matches!(result, Ok(Ok(_))),
        "nothing listens at {}",
        cfg.api_addr
    );
    assert!(
        !cfg.data_dir.exists(),
        "attach never creates the data directory"
    );
}
