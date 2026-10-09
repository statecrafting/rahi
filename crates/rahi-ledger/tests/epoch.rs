//! Deployment epochs in the chain (spec 041 FR-001, FR-002).
//!
//! - epochs 1 to 3, with a manifest transition between 1 and 2, chain onto
//!   one another and verify at both depths (FR-001);
//! - the current epoch is read from the resident chain, and still read at
//!   `Depth::Resident` once a seal has moved the epoch record into a segment,
//!   from the header that carries it (B-6);
//! - a chain with no epoch record is at epoch 0, named by its genesis
//!   record's hash, before and after the genesis is sealed away (B-1);
//! - an export holding epoch records passes the published verifier (FR-002).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod common;

use rahi_ledger::{
    Artifact, BinaryVersions, Cause, Decision, DecisionId, DecisionKind, DeploymentEpoch, Depth,
    EPOCH_KIND, FsArchive, Hash, Ledger, ManifestTransition, Outcome, Reference, References,
    SYSTEM_DEPLOY, SealPolicy,
};
use rahi_types::{Revision, Sub};
use serde_json::json;

const ROOMY: u64 = 65_536;

fn hash(byte: &str) -> Hash {
    Hash::parse(format!("sha256:{}", byte.repeat(32))).expect("a hash")
}

fn policy() -> SealPolicy {
    SealPolicy::new(20, 10).expect("a policy")
}

fn decision(id: &str) -> Decision {
    Decision::new(
        DecisionId::new(id),
        DecisionKind::new("db.write"),
        Sub::new("subject"),
        Outcome::Deny,
        "no grant covers it",
    )
    .with_payload(json!({ "table": "notes" }))
    .at(Revision::new(7))
}

fn epoch(number: u64, previous: Hash, manifest: Hash, transition: Option<Hash>) -> DeploymentEpoch {
    DeploymentEpoch {
        epoch: number,
        previous,
        cause: Cause::Deploy,
        manifest,
        transition,
        schema_version: 1,
        artifact: Artifact {
            binary: Some(format!("sha256:{}", format!("{number:02}").repeat(32))),
            platform: Some("linux/amd64".to_owned()),
            image: None,
            rahi_version: env!("CARGO_PKG_VERSION").to_owned(),
            build_revision: None,
        },
        refs: References {
            deployment: Some(Reference {
                kind: "https://statecraft.ing/deployment/v0".to_owned(),
                digest: format!("sha256:{}", "dd".repeat(32)),
                id: Some(format!("rollout-{number}")),
            }),
            ..References::default()
        },
        restore: None,
        wall_time: 1_800_000_000 + number,
    }
}

async fn open_ledger(store: rahi_store::StoreHandle) -> Ledger {
    Ledger::open(store, common::signer(), common::root())
        .await
        .expect("a fresh chain opens")
}

/// Append denials, sealing after each, until nothing older than `keep`
/// newest records is resident.
async fn push_into_segments(ledger: &Ledger, archive: &FsArchive, n: u32) {
    for i in 1..=n {
        ledger.append(decision(&format!("d-{i:03}"))).await.unwrap();
        ledger.seal_if_needed(archive, &policy()).await.unwrap();
    }
}

/// FR-001.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn epochs_chain_verify_and_survive_a_seal() {
    let f = common::open().await;
    let archive = FsArchive::open(f.dir.path().join("archive")).unwrap();
    let ledger = open_ledger(f.handle()).await;

    let genesis = ledger.records().await.unwrap()[0].hash().unwrap();
    assert_eq!(ledger.genesis_record_hash().await.unwrap(), genesis);
    let zero = ledger.current_epoch().await.unwrap();
    assert_eq!(
        (zero.number, &zero.hash),
        (0, &genesis),
        "B-1: epoch 0 is the genesis"
    );

    let h1 = common::root();
    let h2 = hash("22");
    let deploy = Sub::new(SYSTEM_DEPLOY);
    let e1 = ledger
        .append_epoch(&epoch(1, genesis.clone(), h1.clone(), None), deploy.clone())
        .await
        .unwrap();
    let transition = ledger
        .append_transition(
            &ManifestTransition::new(
                h1.clone(),
                h2.clone(),
                r#"{"grants":2}"#,
                1,
                BinaryVersions {
                    rahi: env!("CARGO_PKG_VERSION").to_owned(),
                    contract: "1.0.0".to_owned(),
                },
                deploy.clone(),
            ),
            ROOMY,
        )
        .await
        .unwrap();
    let e2 = ledger
        .append_epoch(
            &epoch(2, e1.clone(), h2.clone(), Some(transition.clone())),
            deploy.clone(),
        )
        .await
        .unwrap();
    let e3 = ledger
        .append_epoch(&epoch(3, e2.clone(), h2.clone(), None), deploy.clone())
        .await
        .unwrap();

    ledger.verify_chain(Depth::Resident).await.unwrap();
    let epochs: Vec<(DeploymentEpoch, Hash)> = ledger
        .records()
        .await
        .unwrap()
        .iter()
        .filter_map(|r| {
            DeploymentEpoch::of(r)
                .unwrap()
                .map(|e| (e, r.hash().unwrap()))
        })
        .collect();
    assert_eq!(
        epochs
            .iter()
            .map(|(e, _)| e.previous.clone())
            .collect::<Vec<_>>(),
        vec![genesis.clone(), e1.clone(), e2.clone()],
        "each previous names its predecessor's record hash"
    );
    assert_eq!(
        epochs[1].0.transition,
        Some(transition),
        "epoch 2 names its transition"
    );
    let record = ledger.records().await.unwrap();
    let e1_record = record.iter().find(|r| r.record.id == "epoch:1").unwrap();
    assert_eq!(e1_record.decision().unwrap().kind.as_str(), EPOCH_KIND);
    assert_eq!(e1_record.decision().unwrap().outcome, Outcome::Allow);

    let current = ledger.current_epoch().await.unwrap();
    assert_eq!(
        (current.number, &current.hash),
        (3, &e3),
        "from the resident chain"
    );

    // Enough denials that every epoch record leaves the hot table.
    push_into_segments(&ledger, &archive, 40).await;
    let resident = ledger.records().await.unwrap();
    assert!(
        resident
            .iter()
            .all(|r| DeploymentEpoch::of(r).unwrap().is_none()),
        "every epoch record has been sealed away"
    );
    let segments = ledger.segments().await.unwrap();
    assert!(
        segments.iter().all(|h| h.current_epoch.is_some()),
        "every segment sealed by this binary names its tail's epoch"
    );
    let current = ledger.current_epoch().await.unwrap();
    assert_eq!(
        (current.number, &current.hash),
        (3, &e3),
        "B-6: still epoch 3 at Depth::Resident, from the newest header"
    );
    assert_eq!(
        current.deployed.as_ref().map(|d| d.manifest.clone()),
        Some(h2.clone()),
        "B-8's comparison values survive the seal with the epoch"
    );
    ledger.verify_chain(Depth::Resident).await.unwrap();
    ledger.verify_chain(Depth::Full(&archive)).await.unwrap();

    // A reopened ledger reads the same answer from the same headers.
    let reopened = open_ledger(f.handle()).await;
    assert_eq!(reopened.current_epoch().await.unwrap(), current);

    f.store.shutdown().await.unwrap();
}

/// B-1: a chain with no epoch record is at epoch 0, named by its genesis
/// record's hash, and stays so once the genesis itself is sealed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_chain_without_epochs_is_at_epoch_zero_before_and_after_a_seal() {
    let f = common::open().await;
    let archive = FsArchive::open(f.dir.path().join("archive")).unwrap();
    let ledger = open_ledger(f.handle()).await;
    let genesis = ledger.records().await.unwrap()[0].hash().unwrap();

    push_into_segments(&ledger, &archive, 30).await;
    assert_ne!(
        ledger.records().await.unwrap()[0].hash().unwrap(),
        genesis,
        "the genesis is no longer resident"
    );
    let zero = ledger.current_epoch().await.unwrap();
    assert_eq!((zero.number, &zero.hash), (0, &genesis));
    assert_eq!(zero.fingerprint, None, "epoch 0 names no deployment");
    assert_eq!(ledger.genesis_record_hash().await.unwrap(), genesis);

    f.store.shutdown().await.unwrap();
}

/// FR-002: an export holding epoch records verifies with the published
/// verifier, unchanged: an epoch is an opaque payload to it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_export_with_epochs_passes_the_published_verifier() {
    let f = common::open().await;
    let ledger = open_ledger(f.handle()).await;
    let genesis = ledger.records().await.unwrap()[0].hash().unwrap();
    let e1 = ledger
        .append_epoch(
            &epoch(1, genesis, common::root(), None),
            Sub::new(SYSTEM_DEPLOY),
        )
        .await
        .unwrap();
    ledger.append(decision("after-epoch")).await.unwrap();
    assert_eq!(ledger.current_epoch().await.unwrap().hash, e1);

    let jsonl = ledger.export_jsonl().await.unwrap();
    assert!(jsonl.contains(EPOCH_KIND), "the export carries the epoch");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("chain.jsonl");
    std::fs::write(&path, &jsonl).unwrap();
    match std::process::Command::new("attest-ledger")
        .arg("verify")
        .arg(&path)
        .output()
    {
        Ok(out) => assert!(
            out.status.success(),
            "attest-ledger verify failed: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => eprintln!(
            "skipped: attest-ledger-cli is not installed (cargo install attest-ledger-cli); \
             verify_chain ran the same check in process"
        ),
        Err(e) => panic!("running attest-ledger: {e}"),
    }
    ledger.verify_chain(Depth::Resident).await.unwrap();

    f.store.shutdown().await.unwrap();
}
