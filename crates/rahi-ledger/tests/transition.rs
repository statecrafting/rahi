//! The manifest transition record, end to end (spec 036 FR-001, FR-002,
//! FR-006).
//!
//! Three properties, one file:
//!
//! - a chain that transitions keeps verifying at both depths, reports the
//!   manifest it currently names, and refuses a transition signed by a key
//!   this cell does not hold (FR-001);
//! - the current manifest survives its transition record being sealed away,
//!   read at `Depth::Resident` from the segment header (FR-002, B-5);
//! - the size boundary D-6 fixes is exactly where D-6 says it is, and one
//!   byte past it nothing is appended (FR-006).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod common;

use rahi_ledger::{
    BinaryVersions, Decision, DecisionId, DecisionKind, Depth, FsArchive, Hash, Ledger,
    LedgerSigner, ManifestTransition, Outcome, SYSTEM_DEPLOY, SealPolicy, SignedRecord,
    TRANSITION_KIND,
};
use rahi_types::{Error, Revision, Sub};
use serde_json::json;

/// Big enough that nothing in this file is refused for size by accident.
const ROOMY: u64 = 65_536;

fn manifest(byte: &str) -> Hash {
    Hash::parse(format!("sha256:{}", byte.repeat(32))).expect("a hash")
}

fn transition(from: Hash, to: Hash, model: &str) -> ManifestTransition {
    ManifestTransition::new(
        from,
        to,
        model,
        2,
        BinaryVersions {
            rahi: "0.2.0".to_owned(),
            contract: "1.0.0".to_owned(),
        },
        Sub::new(SYSTEM_DEPLOY),
    )
}

fn decision(id: &str) -> Decision {
    Decision::new(
        DecisionId::new(id),
        DecisionKind::new("db.write"),
        Sub::new("rauthy-subject-1"),
        Outcome::Allow,
        "covered by a declared grant",
    )
    .with_payload(json!({ "table": "notes" }))
    .at(Revision::new(7))
}

async fn open_ledger(store: rahi_store::StoreHandle) -> Ledger {
    Ledger::open(store, common::signer(), common::root())
        .await
        .expect("a fresh chain opens")
}

/// FR-001: H1 to H2 to H1, verified at both depths, with the current
/// manifest following the latest transition and a forged one refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transitions_chain_verify_and_name_the_current_manifest() {
    let f = common::open().await;
    let ledger = open_ledger(f.handle()).await;
    let archive = FsArchive::open(f.dir.path().join("archive")).expect("the archive opens");

    let h1 = common::root();
    let h2 = manifest("22");
    assert_eq!(
        ledger.current_manifest().await.unwrap(),
        h1,
        "B-1: the genesis parent, until the first transition"
    );

    ledger
        .append_transition(
            &transition(h1.clone(), h2.clone(), r#"{"grants":2}"#),
            ROOMY,
        )
        .await
        .expect("H1 to H2 is appended");
    assert_eq!(ledger.current_manifest().await.unwrap(), h2);
    ledger
        .verify_chain(Depth::Resident)
        .await
        .expect("resident");
    ledger
        .verify_chain(Depth::Full(&archive))
        .await
        .expect("full");

    // An ordinary decision between the two transitions: the current manifest
    // follows the latest transition, not the latest record.
    ledger.append(decision("d-1")).await.unwrap();
    assert_eq!(ledger.current_manifest().await.unwrap(), h2);

    ledger
        .append_transition(
            &transition(h2.clone(), h1.clone(), r#"{"grants":1}"#),
            ROOMY,
        )
        .await
        .expect("the rollback H2 to H1 is appended");
    assert_eq!(
        ledger.current_manifest().await.unwrap(),
        h1,
        "B-1: the to of the latest transition, even when it is where we started"
    );
    ledger
        .verify_chain(Depth::Resident)
        .await
        .expect("resident");
    ledger
        .verify_chain(Depth::Full(&archive))
        .await
        .expect("full");

    // The genesis parent never moves: a transition changes what the chain
    // currently names, not what it is rooted at.
    assert_eq!(ledger.genesis_parent(), &h1);

    let records = ledger.records().await.unwrap();
    let transitions: Vec<_> = records
        .iter()
        .filter_map(|r| ManifestTransition::of(r).unwrap())
        .collect();
    assert_eq!(transitions.len(), 2, "two transitions, two records");
    let ids: Vec<String> = records
        .iter()
        .filter(|r| r.decision().unwrap().kind.as_str() == TRANSITION_KIND)
        .map(|r| r.record.id.clone())
        .collect();
    assert_eq!(ids.len(), 2);
    assert_ne!(
        ids[0], ids[1],
        "each transition is its own record with its own id, H1 to H2 to H1 included"
    );

    f.store.shutdown().await.unwrap();
}

/// FR-001: a transition signed by another key is `Error::Integrity`.
///
/// The forged record is put into the store directly, as a hostile writer
/// with database access would, and the next verification is what catches it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_transition_signed_by_another_key_is_an_integrity_failure() {
    let f = common::open().await;
    let ledger = open_ledger(f.handle()).await;

    let head = ledger.head().await.unwrap();
    let mut forged = transition(common::root(), manifest("33"), r#"{"grants":9}"#)
        .decision(&head)
        .unwrap();
    forged.prev_hash = head;
    let stranger = LedgerSigner::from_seed([9u8; 32]);
    let record = SignedRecord::build(&forged, &stranger).expect("the stranger signs it");
    common::seed_chain(&f.handle(), std::slice::from_ref(&record)).await;

    let err = ledger
        .verify_chain(Depth::Resident)
        .await
        .expect_err("a record this cell did not sign is refused");
    assert!(matches!(err, Error::Integrity(_)), "{err}");
    assert_eq!(err.exit_code(), 1, "integrity stays fatal: {err}");

    // And a reopen says the same thing: the anchor moved to the chain's own
    // genesis record, but nothing about damage was relaxed.
    let err = Ledger::open(f.handle(), common::signer(), common::root())
        .await
        .expect_err("the reopen refuses too");
    assert!(matches!(err, Error::Integrity(_)), "{err}");

    f.store.shutdown().await.unwrap();
}

/// FR-002 and B-5: the current manifest survives its transition being sealed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_current_manifest_survives_the_seal_of_its_transition() {
    let f = common::open().await;
    let ledger = open_ledger(f.handle()).await;
    let archive = FsArchive::open(f.dir.path().join("archive")).expect("the archive opens");
    let policy = SealPolicy::new(4, 4).expect("a tiny window");

    let h2 = manifest("22");
    ledger
        .append_transition(
            &transition(common::root(), h2.clone(), r#"{"grants":2}"#),
            ROOMY,
        )
        .await
        .expect("the transition is appended");

    // Fill past the window so the run holding the transition is sealed away.
    for i in 1..=12 {
        ledger.append(decision(&format!("d-{i:02}"))).await.unwrap();
        ledger.seal_if_needed(&archive, &policy).await.unwrap();
    }

    let sealed_ids: Vec<String> = {
        let segments = ledger.segments().await.unwrap();
        assert!(!segments.is_empty(), "something was sealed");
        assert_eq!(
            segments[0].current_manifest,
            Some(h2.clone()),
            "B-5: the segment holding the transition names the manifest at its tail"
        );
        for header in &segments {
            assert!(
                header.current_manifest.is_some(),
                "every segment sealed under this spec names one: {header:?}"
            );
        }
        segments.iter().map(|h| h.last_id.to_string()).collect()
    };
    assert!(!sealed_ids.is_empty());

    let resident = ledger.records().await.unwrap();
    assert!(
        resident
            .iter()
            .all(|r| ManifestTransition::of(r).unwrap().is_none()),
        "the transition is no longer resident"
    );
    assert_eq!(
        ledger.current_manifest().await.unwrap(),
        h2,
        "FR-002: and it is still the current manifest, read at Depth::Resident"
    );

    ledger
        .verify_chain(Depth::Resident)
        .await
        .expect("resident");
    ledger
        .verify_chain(Depth::Full(&archive))
        .await
        .expect("full");

    // A reopen of the sealed chain still reads its genesis parent back from
    // the archive rather than from the manifest it is handed (B-4).
    let reopened = Ledger::open(f.handle(), common::signer(), manifest("ee"))
        .await
        .expect("the sealed chain reopens");
    assert_eq!(reopened.genesis_parent(), &common::root());
    assert_eq!(reopened.current_manifest().await.unwrap(), h2);

    f.store.shutdown().await.unwrap();
}

/// FR-006 and AC-4: the size boundary, and one byte past it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_record_is_admitted_at_the_bound_and_refused_one_byte_over() {
    let f = common::open().await;
    let ledger = open_ledger(f.handle()).await;

    let big = transition(
        common::root(),
        manifest("22"),
        &format!(r#"{{"filler":"{}"}}"#, "x".repeat(4_000)),
    );
    let measured = ledger.measure_transition(&big).expect("measures");
    assert_eq!(
        measured,
        ledger.measure_transition(&big).expect("measures"),
        "the measure is deterministic"
    );

    let too_small = u64::try_from(measured).unwrap() - 1;
    let err = ledger
        .append_transition(&big, too_small)
        .await
        .expect_err("one byte over the bound is refused");
    assert!(matches!(err, Error::Validation(_)), "{err}");
    assert_eq!(err.exit_code(), 1, "{err}");
    for fragment in [
        measured.to_string(),
        too_small.to_string(),
        "ledger.max_record_bytes".to_owned(),
    ] {
        assert!(err.message().contains(&fragment), "{fragment}: {err}");
    }
    assert_eq!(
        ledger.count().await.unwrap(),
        1,
        "nothing was appended: only the genesis record is resident"
    );
    assert_eq!(
        ledger.current_manifest().await.unwrap(),
        common::root(),
        "and the chain still names the manifest it named before"
    );

    let hash = ledger
        .append_transition(&big, u64::try_from(measured).unwrap())
        .await
        .expect("exactly the bound is admitted");
    assert_eq!(ledger.head().await.unwrap(), hash);
    let stored = ledger
        .records()
        .await
        .unwrap()
        .into_iter()
        .find(|r| ManifestTransition::of(r).unwrap().is_some())
        .expect("the transition is in the chain");
    assert_eq!(
        ManifestTransition::of(&stored).unwrap().as_ref(),
        Some(&big),
        "the model is retained whole: nothing was truncated or moved"
    );
    assert!(
        stored.to_canonical_json().unwrap().len() <= measured,
        "AC-4: the appended record is no longer than the measure the preflight took"
    );

    f.store.shutdown().await.unwrap();
}

/// AC-2: a chain with no transition is byte for byte what it was.
///
/// The records never gained a field, so the only thing this spec could have
/// moved is the archived segment body, which flattens the header. A segment
/// that names no manifest serializes without the field at all, so a body
/// written before this spec round-trips unchanged and the committed fixture
/// chains under `testdata/chains/` (which `tests/verify.rs` checks) keep
/// verifying against the same bytes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_chain_with_no_transition_is_unchanged_by_this_spec() {
    let f = common::open().await;
    let ledger = open_ledger(f.handle()).await;
    let archive = FsArchive::open(f.dir.path().join("archive")).expect("the archive opens");
    let policy = SealPolicy::new(4, 4).expect("a tiny window");

    for i in 1..=8 {
        ledger.append(decision(&format!("d-{i:02}"))).await.unwrap();
        ledger.seal_if_needed(&archive, &policy).await.unwrap();
    }

    // Sealed under this spec, with no transition anywhere: the header names
    // the genesis parent, which is what B-1 calls the current manifest until
    // one exists.
    let segments = ledger.segments().await.unwrap();
    assert_eq!(segments[0].current_manifest, Some(common::root()));
    assert_eq!(ledger.current_manifest().await.unwrap(), common::root());

    // A header with no manifest at all, as a pre-036 seal wrote it, carries
    // the field in neither direction.
    let mut before = segments[0].clone();
    before.current_manifest = None;
    let json = serde_json::to_string(&before).expect("serializes");
    assert!(
        !json.contains("current_manifest"),
        "a header that names no manifest is the shape it always was: {json}"
    );
    let back: rahi_ledger::SegmentHeader = serde_json::from_str(&json).expect("parses");
    assert_eq!(back, before, "and it round-trips");

    ledger
        .verify_chain(Depth::Resident)
        .await
        .expect("resident");
    ledger
        .verify_chain(Depth::Full(&archive))
        .await
        .expect("full");

    f.store.shutdown().await.unwrap();
}
