//! The generator behind `write.sh`: a chain written by the published 0.1.0
//! chassis crates (spec 042 FR-014).
//!
//! It is compiled in a throwaway workspace that depends on
//! `rahi-ledger = "=0.1.0"` from crates.io, never against this checkout, so
//! the fixture it writes is what the binary a consumer ran actually produced:
//! records signed the 0.1.0 way, segment bodies serialized the 0.1.0 way, and
//! a `kernel_decisions` table with no identity rows in it because 0.1.0 has
//! no such table.
//!
//! Everything is deterministic. The signing seed and the genesis parent are
//! fixed here, no decision reads a clock (`at` is a store revision), and the
//! ids are literals, so a rerun reproduces the committed bytes.

use std::net::TcpListener;
use std::path::{Path, PathBuf};

use rahi_ledger::{
    Decision, DecisionId, DecisionKind, FsArchive, Hash, Ledger, LedgerSigner, Outcome, SealPolicy,
};
use rahi_store::{EncKey, EncKeys, Store, StoreConfig, StoreSecrets};
use rahi_types::{Revision, Sub};

/// The seed the fixture chain is signed with.
const SEED: [u8; 32] = [42u8; 32];

/// The manifest hash the fixture chain is rooted at.
fn genesis_parent() -> Hash {
    Hash::parse(format!("sha256:{}", "42".repeat(32))).expect("a hash")
}

/// Four records stay resident and four are archived across two segments.
const HOT_WINDOW: u32 = 4;
const SEGMENT_SIZE: u32 = 2;

/// The decisions, in order. Genesis is written by `Ledger::open`.
const IDS: [&str; 8] = [
    "d-0001", "d-0002", "d-0003", "d-0004", "d-0005", "d-0006", "d-0007", "d-0008",
];

fn free_port() -> std::net::SocketAddr {
    TcpListener::bind("127.0.0.1:0")
        .expect("a free port")
        .local_addr()
        .expect("an address")
}

fn decision(id: &str, at: u64) -> Decision {
    Decision::new(
        DecisionId::new(id),
        DecisionKind::new("db.write"),
        Sub::new("rauthy-subject-1"),
        Outcome::Allow,
        "covered by a declared grant",
    )
    .with_payload(serde_json::json!({ "table": "notes", "seq": at }))
    .at(Revision::new(at))
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let out = PathBuf::from(std::env::args().nth(1).expect("the fixture directory"));
    let segments_dir = out.join("archive");
    if segments_dir.exists() {
        std::fs::remove_dir_all(&segments_dir).expect("the archive is rewritable");
    }
    std::fs::create_dir_all(&segments_dir).expect("the archive directory");

    let data = tempfile::tempdir().expect("a temporary volume");
    let store = Store::open(&StoreConfig {
        node_id: 1,
        nodes: Vec::new(),
        data_dir: data.path().join("hiqlite"),
        raft_addr: free_port(),
        api_addr: free_port(),
        secrets: StoreSecrets {
            secret_raft: "raft-secret-for-tests-0000".to_owned(),
            secret_api: "api-secret-for-tests-00000".to_owned(),
            enc_keys: EncKeys {
                active: "test".to_owned(),
                keys: vec![EncKey {
                    id: "test".to_owned(),
                    key: vec![7u8; 32],
                }],
            },
        },
        backup_keep_days: 1,
        s3: None,
    })
    .await
    .expect("a single-voter node opens");

    let signer = LedgerSigner::from_seed(SEED);
    let ledger = Ledger::open(store.handle(), signer.clone(), genesis_parent())
        .await
        .expect("a fresh chain opens under 0.1.0");

    let archive = FsArchive::open(&segments_dir).expect("the archive opens");
    let policy = SealPolicy::new(HOT_WINDOW, SEGMENT_SIZE).expect("a policy");
    for (index, id) in IDS.iter().enumerate() {
        ledger
            .append(decision(id, index as u64 + 1))
            .await
            .expect("the append lands");
        while ledger
            .seal_if_needed(&archive, &policy)
            .await
            .expect("the seal lands")
            .is_some()
        {}
    }

    write(
        &out.join("signing-key.b64"),
        &format!("{}\n", base64_seed()),
    );
    write(
        &out.join("genesis-parent.txt"),
        &format!("{}\n", genesis_parent()),
    );
    write(
        &out.join("resident.jsonl"),
        &ledger.export_jsonl().await.expect("the resident chain"),
    );
    let headers = ledger.segments().await.expect("the sealed headers");
    let mut header_lines = String::new();
    for header in &headers {
        header_lines.push_str(&serde_json::to_string(header).expect("a header"));
        header_lines.push('\n');
    }
    write(&out.join("segments.jsonl"), &header_lines);
    write(
        &out.join("version.txt"),
        &format!(
            "rahi-ledger {}\nrahi-store {}\nrahi-types {}\n",
            resolved("rahi-ledger"),
            resolved("rahi-store"),
            resolved("rahi-types"),
        ),
    );

    println!(
        "wrote {} resident record(s) and {} sealed segment(s) to {}",
        ledger.count().await.expect("a count"),
        headers.len(),
        out.display()
    );
    let _ = store.shutdown().await;
}

/// The base64 seed `LedgerSigner::load` reads.
fn base64_seed() -> String {
    use std::fmt::Write as _;
    // The same alphabet `rahi-ledger` writes its key file in, spelled out
    // here so this script needs no extra dependency for three lines.
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in SEED.chunks(3) {
        let b = [
            *chunk.first().unwrap_or(&0),
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                let _ = write!(
                    out,
                    "{}",
                    ALPHABET[((n >> (18 - 6 * i)) & 0x3f) as usize] as char
                );
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The version cargo actually resolved for `name`, read from the lock file
/// of the throwaway workspace this runs in.
fn resolved(name: &str) -> String {
    let lock = std::fs::read_to_string("Cargo.lock").unwrap_or_default();
    let mut wanted = false;
    for line in lock.lines() {
        if line.trim() == format!("name = \"{name}\"") {
            wanted = true;
        } else if wanted && let Some(rest) = line.trim().strip_prefix("version = ") {
            return rest.trim_matches('"').to_owned();
        }
    }
    "unknown".to_owned()
}

fn write(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}
