//! Shared fixture: a single-voter store, the fixture ledger key, and the
//! committed chains under `testdata/chains/`.

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing
)]

use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};

use rahi_ledger::{DECISIONS_TABLE_SQL, Hash, LedgerSigner, SignedRecord};
use rahi_store::{EncKey, EncKeys, Store, StoreConfig, StoreHandle, StoreSecrets, Value};

/// The base64 Ed25519 seed the committed fixture chains are signed with.
pub const KEY_FILE: &str = "signing-key.b64";

/// The manifest hash the fixture chains are rooted at (spec 013 B-2: a real
/// cell passes `Manifest::hash()` here).
pub const ROOT_FILE: &str = "genesis-parent.txt";

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

impl Fixture {
    pub fn handle(&self) -> StoreHandle {
        self.store.handle()
    }
}

pub async fn open() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&config(&dir.path().join("hiqlite")))
        .await
        .expect("single-voter node opens");
    Fixture { store, dir }
}

/// The key the fixtures are signed with, read from beside them.
///
/// The key material lives in `testdata/chains/` rather than in this file so
/// the committed directory is self-contained: an auditor can check every
/// signature in it without compiling anything.
pub fn signer() -> LedgerSigner {
    LedgerSigner::load(&chains_dir().join(KEY_FILE)).expect("the fixture key loads")
}

/// The hash the fixture chains are rooted at, read from beside them.
pub fn root() -> Hash {
    let path = chains_dir().join(ROOT_FILE);
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    Hash::parse(text.trim()).expect("the fixture root is a hash")
}

pub fn chains_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testdata")
        .join("chains")
}

/// Read a committed fixture chain, one record per line.
pub fn read_chain(name: &str) -> Vec<SignedRecord> {
    let path = chains_dir().join(name);
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| SignedRecord::from_bytes(line.as_bytes()).expect("a fixture record"))
        .collect()
}

pub fn write_chain(name: &str, records: &[SignedRecord]) {
    let mut text = String::new();
    for record in records {
        text.push_str(&record.to_canonical_json().expect("serializes"));
        text.push('\n');
    }
    let path = chains_dir().join(name);
    std::fs::write(&path, text).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}

/// Put a fixture chain into the store as rows, without the unique parent
/// index.
///
/// The index is deliberately left for [`rahi_ledger::Ledger::open`] to
/// create: a fixture that forked before boot must make the index fail to
/// build, which is exactly what spec 013 B-4 calls an integrity failure.
pub async fn seed_chain(store: &StoreHandle, records: &[SignedRecord]) {
    store.execute(DECISIONS_TABLE_SQL, vec![]).await.unwrap();
    for record in records {
        store
            .execute(
                "INSERT INTO kernel_decisions (id, prev_hash, hash, record) \
                 VALUES ($1, $2, $3, $4)",
                vec![
                    Value::from(record.record.id.as_str()),
                    Value::from(record.record.previous_record_hash.as_str()),
                    Value::from(record.record.record_hash.as_str()),
                    Value::Blob(record.to_canonical_bytes().unwrap()),
                ],
            )
            .await
            .unwrap();
    }
}
