//! Shared fixture: a single-voter store, the fixture ledger key, and the
//! committed chains under `testdata/chains/`.

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing
)]

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use rahi_ledger::{DECISIONS_TABLE_SQL, Hash, LedgerSigner, SignedRecord};
use rahi_store::{EncKey, EncKeys, Store, StoreConfig, StoreHandle, StoreSecrets, Value};

/// The base64 Ed25519 seed the committed fixture chains are signed with.
pub const KEY_FILE: &str = "signing-key.b64";

/// The manifest hash the fixture chains are rooted at (spec 013 B-2: a real
/// cell passes `Manifest::hash()` here).
pub const ROOT_FILE: &str = "genesis-parent.txt";

/// A loopback port no concurrently running test process was handed (spec
/// 013 D-10).
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

/// The `kernel_segments` table exactly as the published 0.1.0 crates created
/// it: no `current_manifest` column, because spec 036 had not landed.
///
/// A fixture seeded through this is a pre-036 volume, so `Ledger::open` has
/// to add the column the way it does on a real one (spec 036 B-5).
pub const SEGMENTS_TABLE_V010_SQL: &str = "CREATE TABLE IF NOT EXISTS kernel_segments (\
    segment_hash TEXT PRIMARY KEY, \
    prev_segment_hash TEXT NOT NULL, \
    last_hash TEXT NOT NULL, \
    first_id TEXT NOT NULL, \
    last_id TEXT NOT NULL, \
    count INTEGER NOT NULL)";

/// The committed fixture of spec 042 FR-014: a chain written by the
/// published 0.1.0 crates, with its archive beside it.
///
/// Its absence is a **failure**, never a skip: the fixture is committed and
/// its absence is a broken checkout rather than a missing optional tool. A
/// skip here would let AC-3, the whole migration proof, pass on a tree that
/// proves nothing.
pub fn v010_dir() -> PathBuf {
    let dir = chains_dir().join("v0.1.0-sealed");
    assert!(
        dir.is_dir(),
        "the committed fixture {} is missing: this is a broken checkout, not a missing optional \
         tool. Rebuild it with {}/write.sh, which pulls rahi-ledger = \"=0.1.0\" from crates.io.",
        dir.display(),
        dir.display()
    );
    dir
}

fn v010_file(name: &str) -> String {
    let path = v010_dir().join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "the committed fixture file {} is missing or unreadable ({e}): rebuild the fixture \
             with write.sh",
            path.display()
        )
    })
}

/// The 0.1.0 fixture's signing key.
pub fn v010_signer() -> LedgerSigner {
    LedgerSigner::load(&v010_dir().join(KEY_FILE)).expect("the 0.1.0 fixture key loads")
}

/// The 0.1.0 fixture's genesis parent.
pub fn v010_root() -> Hash {
    Hash::parse(v010_file(ROOT_FILE).trim()).expect("the 0.1.0 fixture root is a hash")
}

/// The 0.1.0 fixture's resident records.
pub fn v010_resident() -> Vec<SignedRecord> {
    v010_file("resident.jsonl")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| SignedRecord::from_bytes(l.as_bytes()).expect("a fixture record"))
        .collect()
}

/// The 0.1.0 fixture's sealed segment headers.
pub fn v010_segments() -> Vec<rahi_ledger::SegmentHeader> {
    v010_file("segments.jsonl")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("a fixture segment header"))
        .collect()
}

/// A copy of the 0.1.0 fixture's archive, so a test may damage it without
/// touching the committed bytes.
pub fn v010_archive_copy(into: &Path) -> PathBuf {
    let root = into.join("archive");
    copy_tree(&v010_dir().join("archive"), &root);
    root
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// Seed segment header rows into a **pre-036** `kernel_segments` table, the
/// way a volume written by 0.1.0 holds them.
pub async fn seed_segments_v010(store: &StoreHandle, headers: &[rahi_ledger::SegmentHeader]) {
    store
        .execute(SEGMENTS_TABLE_V010_SQL, vec![])
        .await
        .unwrap();
    for header in headers {
        store
            .execute(
                "INSERT INTO kernel_segments \
                 (segment_hash, prev_segment_hash, last_hash, first_id, last_id, count) \
                 VALUES ($1, $2, $3, $4, $5, $6)",
                vec![
                    Value::from(header.segment_hash.as_str()),
                    Value::from(header.prev_segment_hash.as_str()),
                    Value::from(header.last_hash.as_str()),
                    Value::from(header.first_id.as_str()),
                    Value::from(header.last_id.as_str()),
                    Value::from(header.count),
                ],
            )
            .await
            .unwrap();
    }
}

/// The whole 0.1.0 fixture in a store: its resident rows and its pre-036
/// segment headers, and no identity row anywhere, because 0.1.0 had no such
/// table.
pub async fn seed_v010(store: &StoreHandle) {
    seed_chain(store, &v010_resident()).await;
    seed_segments_v010(store, &v010_segments()).await;
}
