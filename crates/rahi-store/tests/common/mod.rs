//! Shared fixture: a single-voter node in a temp directory on free ports.

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing
)]

use std::net::SocketAddr;
use std::path::Path;

use rahi_store::{EncKey, EncKeys, Store, StoreConfig, StoreSecrets};

/// A loopback port no concurrently running test process was handed (spec
/// 011 D-13).
///
/// Binding port 0 and dropping the listener raced: hiqlite binds the address
/// later, and a parallel test binary could take the port in between. A port
/// now comes from 20000..32000, below every OS ephemeral range, starting at an
/// offset derived from the process id; it is claimed by an exclusive lock on
/// `<temp>/rahi-test-ports/<port>.lock`, held until this process exits, and
/// probed with a bind before it is handed out. Every copy of this allocator
/// in the workspace uses the same range and lock directory.
pub fn free_addr() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], loopback_port()))
}

fn loopback_port() -> u16 {
    use std::fs::{File, OpenOptions};
    use std::net::{Ipv4Addr, TcpListener};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Mutex, PoisonError};

    const FLOOR: u32 = 20_000;
    const SPAN: u32 = 12_000;
    static NEXT: AtomicU32 = AtomicU32::new(0);
    static HELD: Mutex<Vec<File>> = Mutex::new(Vec::new());

    let dir = std::env::temp_dir().join("rahi-test-ports");
    std::fs::create_dir_all(&dir).expect("the port lock directory");
    let offset = std::process::id().wrapping_mul(7919) % SPAN;
    loop {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        assert!(
            n < SPAN,
            "every test port in {FLOOR}..{} is taken",
            FLOOR + SPAN
        );
        let port = u16::try_from(FLOOR + (offset + n) % SPAN).expect("a u16 port");
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(format!("{port}.lock")))
            .expect("a port lock file");
        if lock.try_lock().is_ok() && TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok() {
            HELD.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(lock);
            return port;
        }
    }
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
