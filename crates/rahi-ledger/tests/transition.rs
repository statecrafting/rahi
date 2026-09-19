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
    LedgerSigner, ManifestTransition, Outcome, ReadInterleave, SYSTEM_DEPLOY, SealPolicy,
    SignedRecord, TRANSITION_KIND,
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

/// A chain that answers with no root is damage, not an empty chain.
///
/// `Ledger::open` reads the chain's genesis parent back from the store
/// (B-4), and an absent root has two possible causes: there is no chain
/// yet, or the read did not answer, which a local read reports as no rows
/// rather than as an error (spec 016 D-2). They have opposite consequences,
/// because the genesis record the first case wants written is a valid
/// compare-and-swap onto the real head in the second: it would land, persist,
/// and leave a `ledger.genesis` record in the middle of an audit chain.
/// Constitution XI settles which way to fail.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_chain_with_records_but_no_root_refuses_rather_than_re_genesising() {
    let f = common::open().await;

    // One row whose parent is its own hash: a cycle of one, so the root
    // query answers nothing while the chain is plainly not empty. This is
    // the shape a dropped read is indistinguishable from.
    let mut looped = decision("d-loop");
    looped.prev_hash = common::root();
    let mut record = SignedRecord::build(&looped, &common::signer()).expect("builds");
    record.record.previous_record_hash = record.record.record_hash.clone();
    common::seed_chain(&f.handle(), std::slice::from_ref(&record)).await;

    let err = Ledger::open(f.handle(), common::signer(), common::root())
        .await
        .expect_err("a chain with records and no root is refused");
    assert!(matches!(err, Error::Integrity(_)), "{err}");
    assert_eq!(err.exit_code(), 1, "integrity is fatal at boot: {err}");
    assert!(err.message().contains("no root"), "{err}");

    // And the refusal wrote nothing: the row it found is the only one there.
    let store = f.handle();
    #[derive(serde::Deserialize)]
    struct Count {
        total: i64,
    }
    let rows: Vec<Count> = store
        .query_consistent("SELECT COUNT(*) AS total FROM kernel_decisions", vec![])
        .await
        .unwrap();
    assert_eq!(rows[0].total, 1, "no second genesis record was appended");

    f.store.shutdown().await.unwrap();
}

/// Make every read of `table` come back with no rows while the rows are all
/// still there: the shape spec 016 D-2 leaves behind, reproduced through the
/// store's own DDL rather than by deleting anything.
///
/// The table keeps its rows under a second name and a view of the same name
/// answers nothing, so a read that "did not answer" and a relation that is
/// genuinely empty are indistinguishable to the reader, which is the whole
/// of the hazard.
async fn reads_stop_answering(store: &rahi_store::StoreHandle, table: &str, columns: &str) {
    store
        .execute(
            format!("ALTER TABLE {table} RENAME TO {table}_kept"),
            vec![],
        )
        .await
        .expect("the table is set aside");
    store
        .execute(
            format!("CREATE VIEW {table} AS SELECT {columns} FROM {table}_kept WHERE 0"),
            vec![],
        )
        .await
        .expect("a view of the same name that answers nothing");
}

/// FR-007, D-11: H1 to H2, then a resident read that answers nothing.
///
/// This is the defect D-10 called safe. The walk back reached the genesis
/// parent, which on this chain **is** H1, so an H1 image agrees with the
/// stale answer and boots under a ceiling the chain no longer names, which
/// spec 036 B-10 and D-4 refuse. Now the read stops instead.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_resident_read_that_does_not_answer_never_names_an_older_manifest() {
    let f = common::open().await;
    let ledger = open_ledger(f.handle()).await;
    let h1 = common::root();
    let h2 = manifest("22");

    ledger
        .append_transition(
            &transition(h1.clone(), h2.clone(), r#"{"grants":2}"#),
            ROOMY,
        )
        .await
        .expect("H1 -> H2 is appended");
    assert_eq!(ledger.current_manifest().await.unwrap(), h2);
    ledger
        .verify_chain(Depth::Resident)
        .await
        .expect("the chain verifies before the read is lost");

    reads_stop_answering(
        &f.handle(),
        "kernel_decisions",
        "id, prev_hash, hash, record",
    )
    .await;

    // The answer this read used to give is exactly the manifest an H1 image
    // carries, which is why an older answer is not the safe direction.
    assert_eq!(
        ledger.genesis_parent(),
        &h1,
        "the fallback would have been H1"
    );
    let err = ledger
        .current_manifest()
        .await
        .expect_err("D-11: an absent answer is not an absence");
    assert!(matches!(err, Error::Integrity(_)), "{err:?}");
    assert!(
        err.message().contains("read failing"),
        "the message says what it found: {}",
        err.message()
    );

    f.store.shutdown().await.unwrap();
}

/// FR-007, D-11: the same shape once the transition has been sealed away.
///
/// The segment header carries H2 (B-5). A segment read that answers nothing
/// used to fall through to the same genesis parent with the same result.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sealed_read_that_does_not_answer_never_names_an_older_manifest() {
    let f = common::open().await;
    let ledger = open_ledger(f.handle()).await;
    let archive = FsArchive::open(f.dir.path().join("archive")).expect("the archive opens");
    let policy = SealPolicy::new(4, 4).expect("a tiny window");
    let h1 = common::root();
    let h2 = manifest("22");

    ledger
        .append_transition(
            &transition(h1.clone(), h2.clone(), r#"{"grants":2}"#),
            ROOMY,
        )
        .await
        .expect("H1 -> H2 is appended");
    for i in 1..=12 {
        ledger.append(decision(&format!("s-{i:02}"))).await.unwrap();
        ledger.seal_if_needed(&archive, &policy).await.unwrap();
    }
    assert!(!ledger.segments().await.unwrap().is_empty());
    assert!(
        ledger
            .records()
            .await
            .unwrap()
            .iter()
            .all(|r| ManifestTransition::of(r).unwrap().is_none()),
        "the transition is no longer resident, so the header is the only evidence"
    );
    assert_eq!(ledger.current_manifest().await.unwrap(), h2);

    // Reopen so this handle's own verification covers the sealed chain, then
    // lose the segment read.
    let reopened = Ledger::open(f.handle(), common::signer(), h1.clone())
        .await
        .expect("the sealed chain reopens and verifies");
    reads_stop_answering(
        &f.handle(),
        "kernel_segments",
        "first_id, last_id, count, segment_hash, prev_segment_hash, last_hash, current_manifest",
    )
    .await;

    let err = reopened
        .current_manifest()
        .await
        .expect_err("D-11: the sealed evidence is not absent, it is unread");
    assert!(matches!(err, Error::Integrity(_)), "{err:?}");
    assert!(
        err.message().contains("segment headers read back empty"),
        "the sealed guard is what refuses: {}",
        err.message()
    );
    assert_ne!(
        reopened.genesis_parent(),
        &h2,
        "H1 is what the fallback would have named"
    );

    f.store.shutdown().await.unwrap();
}

/// FR-011, D-15: a seal that commits between the chain read's two halves.
///
/// The interleave the census could not see: the sealed headers are read,
/// a concurrent seal moves the H1 -> H2 transition out of the hot table and
/// into a segment in one transaction, and the resident read that follows no
/// longer holds the transition. Each half accounts for its own rows, both
/// agree with what `open` verified, and the combined answer is still H1,
/// which is exactly the manifest an old image carries.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_seal_between_the_reads_never_names_an_older_manifest() {
    let f = common::open().await;
    let ledger = open_ledger(f.handle()).await;
    let archive = std::sync::Arc::new(
        FsArchive::open(f.dir.path().join("archive")).expect("the archive opens"),
    );
    let policy = SealPolicy::new(4, 4).expect("a tiny window");
    let h1 = common::root();
    let h2 = manifest("22");

    ledger
        .append_transition(
            &transition(h1.clone(), h2.clone(), r#"{"grants":2}"#),
            ROOMY,
        )
        .await
        .expect("H1 -> H2 is appended");
    for i in 1..=5 {
        ledger.append(decision(&format!("i-{i:02}"))).await.unwrap();
    }
    assert!(
        ledger.segments().await.unwrap().is_empty(),
        "nothing is sealed when the read starts"
    );
    assert_eq!(ledger.current_manifest().await.unwrap(), h2);

    let reader = interleaved(&ledger, &archive, policy);
    let answer = reader
        .current_manifest()
        .await
        .expect("a legitimate seal is not an integrity failure");
    assert_ne!(answer, h1, "D-15: the older manifest an H1 image carries");
    assert_eq!(
        answer, h2,
        "the manifest the chain names on both sides of the seal"
    );

    assert!(
        !ledger.segments().await.unwrap().is_empty(),
        "the interleaved seal really did commit"
    );
    f.store.shutdown().await.unwrap();
}

/// FR-011, D-15: the model reader answers from the same snapshot.
///
/// `current_manifest_model` reports `None` for a real absence and only that
/// (D-14). A seal landing between the two halves of the old read took the
/// transition out of the resident answer, so the recovery path reported "the
/// chain holds no earlier manifest" about a chain that had just named one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_seal_between_the_reads_never_invents_an_absent_model() {
    let f = common::open().await;
    let ledger = open_ledger(f.handle()).await;
    let archive = std::sync::Arc::new(
        FsArchive::open(f.dir.path().join("archive")).expect("the archive opens"),
    );
    let policy = SealPolicy::new(4, 4).expect("a tiny window");
    let model = r#"{"grants":2}"#;

    ledger
        .append_transition(&transition(common::root(), manifest("22"), model), ROOMY)
        .await
        .expect("H1 -> H2 is appended");
    for i in 1..=5 {
        ledger.append(decision(&format!("m-{i:02}"))).await.unwrap();
    }

    let reader = interleaved(&ledger, &archive, policy);
    assert_eq!(
        reader.current_manifest_model().await.unwrap(),
        Some(model.to_owned()),
        "D-14: an absence is a chain that holds no earlier text, never a seal that moved it"
    );
    f.store.shutdown().await.unwrap();
}

/// FR-011, D-15: the same crossing on a chain that already has history.
///
/// The first variant crosses from nothing sealed to one segment. This one
/// crosses from one segment naming H2 to a second naming H3, which is the
/// shape that survives every guard D-11 added: both censuses account for
/// their rows, `open` saw segments and records, and the segment answer is
/// not empty. Only the seam check refuses it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_seal_between_the_reads_never_names_a_superseded_segment() {
    let f = common::open().await;
    let ledger = open_ledger(f.handle()).await;
    let archive = std::sync::Arc::new(
        FsArchive::open(f.dir.path().join("archive")).expect("the archive opens"),
    );
    let policy = SealPolicy::new(4, 4).expect("a tiny window");
    let h2 = manifest("22");
    let h3 = manifest("33");

    // History first: H1 -> H2 sealed into a segment that names H2.
    ledger
        .append_transition(
            &transition(common::root(), h2.clone(), r#"{"grants":2}"#),
            ROOMY,
        )
        .await
        .expect("H1 -> H2 is appended");
    for i in 1..=5 {
        ledger.append(decision(&format!("h-{i:02}"))).await.unwrap();
        ledger
            .seal_if_needed(archive.as_ref(), &policy)
            .await
            .unwrap();
    }
    let sealed = ledger.segments().await.unwrap();
    assert_eq!(
        sealed.last().unwrap().current_manifest,
        Some(h2.clone()),
        "the segment names the manifest at its tail"
    );

    // A handle that verified that history, so `open` saw both relations.
    let reopened = Ledger::open(f.handle(), common::signer(), common::root())
        .await
        .expect("the sealed chain reopens and verifies");
    reopened
        .append_transition(
            &transition(h2.clone(), h3.clone(), r#"{"grants":3}"#),
            ROOMY,
        )
        .await
        .expect("H2 -> H3 is appended");
    for i in 1..=5 {
        reopened
            .append(decision(&format!("n-{i:02}")))
            .await
            .unwrap();
    }
    assert_eq!(reopened.current_manifest().await.unwrap(), h3);

    let reader = interleaved(&reopened, &archive, policy);
    let answer = reader
        .current_manifest()
        .await
        .expect("a legitimate seal is not an integrity failure");
    assert_ne!(answer, h2, "the manifest the older segment still names");
    assert_eq!(
        answer, h3,
        "D-15: the manifest the chain names, either side of the seal"
    );
    f.store.shutdown().await.unwrap();
}

/// A reader whose chain read is interrupted, exactly once, by a real seal
/// committing through the production path (spec 036 D-15).
fn interleaved(ledger: &Ledger, archive: &std::sync::Arc<FsArchive>, policy: SealPolicy) -> Ledger {
    let sealer = ledger.clone();
    let archive = std::sync::Arc::clone(archive);
    ledger
        .clone()
        .with_read_interleave(ReadInterleave::new(move || {
            let sealer = sealer.clone();
            let archive = std::sync::Arc::clone(&archive);
            async move {
                sealer
                    .seal_if_needed(archive.as_ref(), &policy)
                    .await
                    .expect("the concurrent seal commits");
            }
        }))
}
