//! Shared fixture: a single-voter node in a temp directory on free ports.

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing
)]

use std::net::{SocketAddr, TcpListener};
use std::path::Path;

use rahi_store::{EncKey, EncKeys, Store, StoreConfig, StoreSecrets};

pub fn free_addr() -> SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap()
}

pub fn secrets() -> StoreSecrets {
    StoreSecrets {
        secret_raft: "raft-secret-for-tests-0000".to_owned(),
        secret_api: "api-secret-for-tests-00000".to_owned(),
        enc_keys: EncKeys {
            active: "test".to_owned(),
            keys: vec![EncKey {
                id: "test".to_owned(),
                key: vec![7u8; 32],
            }],
        },
    }
}

pub fn config(data_dir: &Path) -> StoreConfig {
    StoreConfig {
        node_id: 1,
        nodes: Vec::new(),
        data_dir: data_dir.to_path_buf(),
        raft_addr: free_addr(),
        api_addr: free_addr(),
        secrets: secrets(),
        backup_keep_days: 1,
        s3: None,
    }
}

pub struct Fixture {
    pub store: Store,
    pub dir: tempfile::TempDir,
}

pub async fn open() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(&dir.path().join("hiqlite"));
    let store = Store::open(&cfg).await.expect("single-voter node opens");
    Fixture { store, dir }
}
