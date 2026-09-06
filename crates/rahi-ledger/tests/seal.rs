//! spec 014 FR-001 to FR-004: the window is bounded, the archive is
//! immutable, deep verification catches what a resident one cannot, and a
//! refused archive write leaves the hot table exactly as it was.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod common;

use async_trait::async_trait;
use rahi_ledger::{
    Archive, Decision, DecisionId, DecisionKind, Depth, FsArchive, Ledger, Outcome, SEGMENT_PREFIX,
    SealPolicy, Segment,
};
use rahi_types::{Error, Revision, Sub};
use serde_json::json;

/// FR-001's policy: twenty resident, ten to a segment.
fn policy() -> SealPolicy {
    SealPolicy::new(20, 10).expect("a policy the spec names")
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

fn archive(dir: &std::path::Path) -> FsArchive {
    FsArchive::open(dir.join("archive")).expect("the archive opens")
}

/// Append `n` decisions, sealing after each one the way a cell does.
async fn append_and_seal(ledger: &Ledger, archive: &dyn Archive, n: u32) {
    for i in 1..=n {
        ledger
            .append(decision(&format!("d-{i:02}")))
            .await
            .unwrap_or_else(|e| panic!("append d-{i:02}: {e}"));
        ledger
            .seal_if_needed(archive, &policy())
            .await
            .unwrap_or_else(|e| panic!("seal after d-{i:02}: {e}"));
    }
}

/// An archive whose bucket is unreachable: every write is refused.
#[derive(Debug)]
struct RefusingArchive;

#[async_trait]
impl Archive for RefusingArchive {
    async fn put(&self, key: &str, _bytes: Vec<u8>) -> Result<(), Error> {
        Err(Error::Upstream(format!("the bucket is unreachable: {key}")))
    }

    async fn get(&self, key: &str) -> Result<Vec<u8>, Error> {
        Err(Error::NotFound(key.to_owned()))
    }

    async fn list(&self, _prefix: &str) -> Result<Vec<String>, Error> {
        Ok(Vec::new())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_window_stays_bounded_and_the_rest_is_one_verified_segment() {
    let f = common::open().await;
    let archive = archive(f.dir.path());
    let ledger = open_ledger(f.handle()).await;

    // Thirty records: the genesis record spec 013 writes at open, and
    // twenty-nine appends after it (FR-001).
    append_and_seal(&ledger, &archive, 29).await;

    assert_eq!(ledger.count().await.unwrap(), 20, "the hot window is bound");
    let segments = ledger.segments().await.unwrap();
    assert_eq!(segments.len(), 1, "ten records left the hot table");
    assert_eq!(segments[0].count, 10);
    assert_eq!(
        segments[0].prev_segment_hash,
        common::root(),
        "the first segment links the genesis parent, where a backward walk stops"
    );

    // The archive holds that segment and nothing else, under the one prefix
    // a ledger ever writes (B-3, B-6).
    let keys = archive.list("").await.unwrap();
    assert_eq!(keys, vec![segments[0].key()]);
    assert!(keys[0].starts_with(SEGMENT_PREFIX), "{}", keys[0]);

    // The seam: the oldest resident record links the segment's last record.
    let records = ledger.records().await.unwrap();
    assert_eq!(records.len(), 20);
    assert_eq!(records[0].prev_hash().unwrap(), segments[0].last_hash);
    assert_eq!(ledger.resident_root().await.unwrap(), segments[0].last_hash);

    // The body holds the ten archived records, chained onto the genesis
    // parent, and the sealed ids are gone from the hot table.
    let body = Segment::from_bytes(&archive.get(&segments[0].key()).await.unwrap()).unwrap();
    assert_eq!(body.records.len(), 10);
    assert_eq!(body.records[0].prev_hash().unwrap(), common::root());
    assert_eq!(body.records[9].hash().unwrap(), segments[0].last_hash);
    assert!(
        !records
            .iter()
            .any(|r| r.record.id == body.records[0].record.id),
        "an archived record is not also resident"
    );

    ledger
        .verify_chain(Depth::Full(&archive))
        .await
        .expect("the whole chain verifies, archived history included");

    f.store.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_corrupted_body_fails_the_deep_check_and_names_the_segment() {
    let f = common::open().await;
    let archive = archive(f.dir.path());
    let ledger = open_ledger(f.handle()).await;
    append_and_seal(&ledger, &archive, 29).await;

    let key = ledger.segments().await.unwrap()[0].key();
    let path = archive.root().join(&key);
    let body = std::fs::read_to_string(&path).unwrap();
    // One byte, inside a payload string: the JSON still parses and the
    // records still link, so only the content digest can tell.
    let corrupted = body.replacen("covered", "covereX", 1);
    assert_ne!(corrupted, body, "the fixture text was there to corrupt");
    std::fs::write(&path, corrupted).unwrap();

    ledger
        .verify_chain(Depth::Resident)
        .await
        .expect("FR-002: what is resident is untouched, so boot still passes");

    let err = ledger
        .verify_chain(Depth::Full(&archive))
        .await
        .expect_err("FR-002: the deep check reads the body and refuses it");
    assert!(matches!(err, Error::Integrity(_)), "{err}");
    assert!(err.message().contains(&key), "the failure names it: {err}");

    f.store.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sealed_key_is_never_rewritten() {
    let dir = tempfile::tempdir().unwrap();
    let archive = archive(dir.path());
    let key = format!("{SEGMENT_PREFIX}d-01-d-10.json");

    archive
        .put(&key, b"the sealed body".to_vec())
        .await
        .unwrap();
    let err = archive
        .put(&key, b"a different body".to_vec())
        .await
        .expect_err("FR-003: put to an existing key is a conflict");
    assert!(matches!(err, Error::Conflict(_)), "{err}");
    assert_eq!(
        archive.get(&key).await.unwrap(),
        b"the sealed body".to_vec(),
        "the refused write changed nothing"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_archive_that_refuses_the_body_leaves_the_hot_table_whole() {
    let f = common::open().await;
    let ledger = open_ledger(f.handle()).await;
    let refusing = RefusingArchive;

    // Twenty-one records: one past the window, so a seal is due.
    for i in 1..=20 {
        ledger.append(decision(&format!("d-{i:02}"))).await.unwrap();
    }
    let before = ledger.records().await.unwrap();
    assert_eq!(before.len(), 21, "nothing was sealed");

    let err = ledger
        .seal_if_needed(&refusing, &policy())
        .await
        .expect_err("FR-004: the archive refused the body");
    assert!(matches!(err, Error::Upstream(_)), "{err}");

    assert_eq!(ledger.count().await.unwrap(), 21, "no row was deleted");
    assert_eq!(ledger.segment_count().await.unwrap(), 0, "no header landed");
    assert_eq!(ledger.records().await.unwrap(), before);
    ledger.verify().await.expect("the chain is untouched");

    f.store.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sealed_chain_reopens_where_it_left_off() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = common::config(&dir.path().join("hiqlite"));
    let archive = archive(dir.path());

    let store = rahi_store::Store::open(&cfg).await.unwrap();
    let ledger = open_ledger(store.handle()).await;
    append_and_seal(&ledger, &archive, 29).await;
    let head = ledger.head().await.unwrap();
    let sealed = ledger.segments().await.unwrap();
    store.shutdown().await.unwrap();
    drop(ledger);
    drop(store);

    // Boot verification is Depth::Resident (B-4): it passes without the
    // archive, on a chain whose oldest history is no longer resident.
    let store = rahi_store::Store::open(&cfg).await.unwrap();
    let ledger = open_ledger(store.handle()).await;
    assert_eq!(ledger.count().await.unwrap(), 20, "nothing re-genesised");
    assert_eq!(ledger.segments().await.unwrap(), sealed);
    assert_eq!(ledger.head().await.unwrap(), head);

    // And the chain continues from that head, sealing again when it is due.
    for i in 30..=39 {
        ledger.append(decision(&format!("d-{i:02}"))).await.unwrap();
        ledger.seal_if_needed(&archive, &policy()).await.unwrap();
    }
    let segments = ledger.segments().await.unwrap();
    assert_eq!(segments.len(), 2, "a second run was sealed");
    assert_eq!(
        segments[1].prev_segment_hash, segments[0].segment_hash,
        "the segments are hash-linked to their predecessor"
    );
    assert_eq!(ledger.count().await.unwrap(), 20);
    ledger
        .verify_chain(Depth::Full(&archive))
        .await
        .expect("thirty-nine records verify across two segments and the window");

    store.shutdown().await.unwrap();
}
