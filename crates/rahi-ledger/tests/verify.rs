//! spec 013 FR-002 and FR-004: a damaged chain stops the boot, and an export
//! is checkable by the published verifier that never saw this code.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod common;

use std::path::Path;

use attest_ledger_types::LedgerRecord;
use rahi_ledger::{
    Decision, DecisionId, DecisionKind, Ledger, LedgerSigner, Outcome, SignedRecord,
};
use rahi_types::{Error, Revision, Sub};
use serde_json::json;

/// Every chain committed under `testdata/chains/`.
const FIXTURES: [&str; 5] = [
    "clean.jsonl",
    "broken-link.jsonl",
    "tampered-payload.jsonl",
    "forged-signature.jsonl",
    "forked.jsonl",
];

/// Seed a fixture chain into a fresh store and try to open the ledger over it.
async fn open_over(name: &str) -> (common::Fixture, Result<Ledger, Error>) {
    let f = common::open().await;
    common::seed_chain(&f.handle(), &common::read_chain(name)).await;
    let opened = Ledger::open(f.handle(), common::signer(), common::root()).await;
    (f, opened)
}

fn assert_integrity(name: &str, opened: Result<Ledger, Error>) {
    match opened {
        Ok(_) => panic!("{name}: a damaged chain must not open"),
        Err(err) => assert!(
            matches!(err, Error::Integrity(_)),
            "{name}: expected an integrity failure, got {err}"
        ),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_clean_fixture_chain_opens_and_continues() {
    let (f, opened) = open_over("clean.jsonl").await;
    let ledger = opened.expect("the clean fixture verifies");
    let committed = common::read_chain("clean.jsonl");

    assert_eq!(ledger.records().await.unwrap(), committed);
    assert_eq!(
        ledger.head().await.unwrap(),
        committed.last().unwrap().hash().unwrap()
    );
    ledger
        .append(Decision::new(
            DecisionId::new("after-the-fixture"),
            DecisionKind::new("db.write"),
            Sub::new("subject"),
            Outcome::Allow,
            "the chain continues where the fixture stopped",
        ))
        .await
        .expect("appends onto the fixture head");
    ledger.verify().await.expect("still verifies");

    f.store.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_broken_link_stops_the_boot() {
    let (f, opened) = open_over("broken-link.jsonl").await;
    assert_integrity("broken-link.jsonl", opened);
    f.store.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tampered_payload_stops_the_boot() {
    let (f, opened) = open_over("tampered-payload.jsonl").await;
    assert_integrity("tampered-payload.jsonl", opened);
    f.store.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_forged_signature_stops_the_boot() {
    let (f, opened) = open_over("forged-signature.jsonl").await;
    assert_integrity("forged-signature.jsonl", opened);
    f.store.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fork_that_predates_the_boot_stops_the_boot() {
    // Two records claim the genesis parent, so the unique parent index cannot
    // be built over what is already resident (spec 013 B-4).
    let (f, opened) = open_over("forked.jsonl").await;
    match opened {
        Ok(_) => panic!("a forked chain must not open"),
        Err(Error::Integrity(message)) => assert!(
            message.contains("index will not build"),
            "the failure should name the index, not just the chain: {message}"
        ),
        Err(err) => panic!("expected an integrity failure, got {err}"),
    }
    f.store.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_export_is_what_the_published_verifier_reads() {
    let f = common::open().await;
    let ledger = Ledger::open(f.handle(), common::signer(), common::root())
        .await
        .unwrap();
    for n in 0..3 {
        ledger
            .append(
                Decision::new(
                    DecisionId::new(format!("d-{n}")),
                    DecisionKind::new("db.write"),
                    Sub::new("subject"),
                    Outcome::Deny,
                    "no grant covers db.write on notes",
                )
                .with_payload(json!({ "wall_time": "2026-09-06T00:00:00Z" }))
                .at(Revision::new(n)),
            )
            .await
            .unwrap();
    }
    let jsonl = ledger.export_jsonl().await.unwrap();

    // The verifier's own view: the envelope alone, with the two rahi fields
    // it does not know dropped. It must still recompute every hash and bind
    // every link (spec 013 B-6).
    let envelopes: Vec<LedgerRecord> = jsonl
        .lines()
        .map(|line| serde_json::from_str(line).expect("an envelope"))
        .collect();
    assert_eq!(envelopes.len(), 4);
    attest_ledger_core::verify_chain(&envelopes).expect("the published verifier's check passes");

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("chain.jsonl");
    std::fs::write(&path, &jsonl).unwrap();
    run_published_cli(&path);

    f.store.shutdown().await.unwrap();
}

/// Run `attest-ledger verify` over an exported chain (spec 013 FR-004).
///
/// Skipped with a message when the published CLI is not installed: it is a
/// separately released binary, so requiring it would make this crate's tests
/// depend on what a developer happens to have on their PATH.
fn run_published_cli(chain: &Path) {
    match std::process::Command::new("attest-ledger")
        .arg("verify")
        .arg(chain)
        .output()
    {
        Ok(out) => assert!(
            out.status.success(),
            "attest-ledger verify failed: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!(
                "skipped: attest-ledger-cli is not installed \
                 (cargo install attest-ledger-cli); the in-process check above \
                 ran the same verification"
            );
        }
        Err(e) => panic!("running attest-ledger: {e}"),
    }
}

/// The provenance of `testdata/chains/`.
///
/// Normally this asserts the committed fixtures still parse. Run it with
/// `RAHI_LEDGER_FIXTURES=write cargo test -p rahi-ledger --test verify --
/// fixtures` to rebuild them from a real ledger, which is how they were made:
/// a clean chain is exported and each damaged variant is one edit away from
/// it, so what the fixtures prove is what the code writes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_committed_fixtures_are_a_real_chain_and_four_edits_to_it() {
    if std::env::var("RAHI_LEDGER_FIXTURES").as_deref() != Ok("write") {
        for name in FIXTURES {
            assert!(
                !common::read_chain(name).is_empty(),
                "{name} parses as a chain"
            );
        }
        return;
    }

    let f = common::open().await;
    let ledger = Ledger::open(f.handle(), common::signer(), common::root())
        .await
        .unwrap();
    for n in 0..2 {
        ledger
            .append(
                Decision::new(
                    DecisionId::new(format!("fixture-d-{n}")),
                    DecisionKind::new("db.write"),
                    Sub::new("rauthy-subject-1"),
                    Outcome::Allow,
                    "covered by a declared grant",
                )
                .with_payload(json!({ "table": "notes" }))
                .at(Revision::new(n + 1)),
            )
            .await
            .unwrap();
    }
    let clean = ledger.records().await.unwrap();
    common::write_chain("clean.jsonl", &clean);

    let signer = common::signer();

    // Only the link is wrong: the hash and the signature are recomputed over
    // the edit, so nothing but the chaining gives it away.
    let mut broken = clean.clone();
    broken[2].record.previous_record_hash = format!("sha256:{}", "ee".repeat(32));
    broken[2].record.record_hash = attest_ledger_core::compute_record_hash(&broken[2].record);
    broken[2].signature = signer.sign(broken[2].record.record_hash.as_bytes());
    common::write_chain("broken-link.jsonl", &broken);

    // Only the content is wrong: the stored hash still binds the chain, so
    // the recompute is what catches it.
    let mut tampered = clean.clone();
    tampered[1].record.payload = json!({ "outcome": "allow", "reason": "inserted afterwards" });
    common::write_chain("tampered-payload.jsonl", &tampered);

    // Only the signature is wrong: the chain is intact and a stranger's key
    // signed one record of it.
    let mut forged = clean.clone();
    let stranger = LedgerSigner::from_seed([0xa5; 32]);
    forged[1].signature = stranger.sign(forged[1].record.record_hash.as_bytes());
    common::write_chain("forged-signature.jsonl", &forged);

    // Two records claim the genesis parent.
    let mut rival = Decision::new(
        DecisionId::new("fixture-rival"),
        DecisionKind::new("db.write"),
        Sub::new("rauthy-subject-2"),
        Outcome::Deny,
        "a second record claiming the genesis parent",
    );
    rival.prev_hash = common::root();
    let forked = vec![
        clean[0].clone(),
        SignedRecord::build(&rival, &signer).unwrap(),
    ];
    common::write_chain("forked.jsonl", &forked);

    f.store.shutdown().await.unwrap();
}
