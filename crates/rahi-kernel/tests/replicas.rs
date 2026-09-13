//! Decision ids across replicas of one chain (spec 035 FR-004, B-6).
//!
//! Two kernels booted over one store and one chain are two replicas that
//! read the same head: both boot on the same nonce and both counters start at
//! zero. The node segment is what keeps their ids apart. The same test with
//! equal node ids is the old shape's collision, kept as the regression: one
//! id held by two callers, one record in the chain, one conflict reported.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]

use std::net::{SocketAddr, TcpListener};
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use rahi_kernel::observe::{self, Cause};
use rahi_kernel::{DecisionId, Kernel, KernelOptions, Manifest};
use rahi_ledger::{Ledger, LedgerSigner};
use rahi_store::{EncKey, EncKeys, Store, StoreConfig, StoreHandle, StoreSecrets};
use rahi_types::Sub;

const VALID: &str = include_str!("../testdata/manifests/valid.toml");

fn free_addr() -> SocketAddr {
    TcpListener::bind("127.0.0.1:0")
        .expect("a free port")
        .local_addr()
        .expect("an address")
}

fn store_config(data_dir: &Path) -> StoreConfig {
    StoreConfig {
        node_id: 1,
        nodes: Vec::new(),
        data_dir: data_dir.to_path_buf(),
        raft_addr: free_addr(),
        api_addr: free_addr(),
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
    }
}

/// Every loss reported in this test binary: id, cause, and error kind.
fn losses() -> &'static Mutex<Vec<(DecisionId, Cause, String)>> {
    static LOSSES: OnceLock<Mutex<Vec<(DecisionId, Cause, String)>>> = OnceLock::new();
    LOSSES.get_or_init(|| {
        observe::on_failure(|id, cause, err| {
            losses()
                .lock()
                .expect("not poisoned")
                .push((id.clone(), cause, err.kind().to_owned()));
        });
        Mutex::new(Vec::new())
    })
}

fn losses_of(id: &DecisionId) -> Vec<(Cause, String)> {
    losses()
        .lock()
        .expect("not poisoned")
        .iter()
        .filter(|(seen, _, _)| seen == id)
        .map(|(_, cause, kind)| (*cause, kind.clone()))
        .collect()
}

/// One store and one chain, the way replicas share a cluster's.
///
/// Every fresh chain of one manifest and one key has the same genesis head,
/// so the tests in this binary boot on one nonce; each test uses node ids no
/// other test uses, or their observations would mix.
struct Chain {
    store: StoreHandle,
    manifest: Manifest,
    _node: Store,
    _dir: tempfile::TempDir,
}

impl Chain {
    async fn open() -> Self {
        let _ = losses();
        let dir = tempfile::tempdir().expect("a temp dir");
        let node = Store::open(&store_config(&dir.path().join("hiqlite")))
            .await
            .expect("a single-voter node opens");
        let manifest = Manifest::parse(VALID).expect("the fixture manifest parses");
        let chain = Self {
            store: node.handle(),
            manifest,
            _node: node,
            _dir: dir,
        };
        let _ = chain.ledger().await;
        chain
    }

    async fn ledger(&self) -> Ledger {
        Ledger::open(
            self.store.clone(),
            LedgerSigner::from_seed([9u8; 32]),
            self.manifest.hash().expect("hashes"),
        )
        .await
        .expect("the chain opens and verifies")
    }

    /// A replica's kernel, booted as node `node_id` on the chain's head now.
    async fn replica(&self, node_id: u64) -> Kernel {
        Kernel::boot_with(
            self.manifest.clone(),
            self.store.clone(),
            self.ledger().await,
            KernelOptions {
                node_id,
                ..KernelOptions::default()
            },
        )
        .await
        .expect("the kernel boots")
    }

    async fn resident(&self) -> Vec<String> {
        self.ledger()
            .await
            .records()
            .await
            .expect("records")
            .into_iter()
            .map(|record| record.record.id)
            .collect()
    }
}

fn deny(kernel: &Kernel, caller: &str) -> DecisionId {
    kernel.refuse(
        "replica.probe",
        &Sub::new(caller),
        "the replica test denies it",
        serde_json::Map::new(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_replicas_on_one_head_mint_distinct_ids_and_both_land() {
    let chain = Chain::open().await;
    let one = chain.replica(1).await;
    let two = chain.replica(2).await;
    assert_eq!(one.nonce(), two.nonce(), "both booted on the same head");

    // Each denies before either appends.
    let id_one = deny(&one, "caller-of-replica-1");
    let id_two = deny(&two, "caller-of-replica-2");
    one.flush(Duration::from_secs(10))
        .await
        .expect("replica 1 drains");
    two.flush(Duration::from_secs(10))
        .await
        .expect("replica 2 drains");

    assert_ne!(id_one, id_two, "two replicas, two ids");
    assert_eq!(
        id_one.as_str(),
        format!("kernel:{}:1:000000000000", one.nonce())
    );
    assert_eq!(
        id_two.as_str(),
        format!("kernel:{}:2:000000000000", two.nonce())
    );
    let resident = chain.resident().await;
    assert!(
        resident.contains(&id_one.as_str().to_owned()),
        "{resident:?}"
    );
    assert!(
        resident.contains(&id_two.as_str().to_owned()),
        "{resident:?}"
    );
    assert!(losses_of(&id_one).is_empty(), "{:?}", losses_of(&id_one));
    assert!(losses_of(&id_two).is_empty(), "{:?}", losses_of(&id_two));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_replicas_with_equal_node_ids_collide_as_the_old_shape_did() {
    let chain = Chain::open().await;
    let first = chain.replica(3).await;
    let second = chain.replica(3).await;

    let id_first = deny(&first, "caller-of-the-first");
    let id_second = deny(&second, "caller-of-the-second");
    first.flush(Duration::from_secs(10)).await.expect("drains");
    second.flush(Duration::from_secs(10)).await.expect("drains");

    assert_eq!(
        id_first, id_second,
        "without a node segment, one id for two denials"
    );
    let resident = chain.resident().await;
    assert_eq!(
        resident
            .iter()
            .filter(|id| id.as_str() == id_first.as_str())
            .count(),
        1,
        "the chain holds one of the two denials: {resident:?}"
    );
    assert_eq!(
        losses_of(&id_first),
        vec![(Cause::Failed, "conflict".to_owned())],
        "the other was refused as a conflict and counted as a failed append"
    );
}
