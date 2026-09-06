//! The sealed segment: the immutable unit of archived history (spec 014 B-2).
//!
//! A segment is a contiguous run of records lifted out of the hot table and
//! written to the archive once, never again. Two hashes carry two different
//! jobs and the difference is the whole of the design:
//!
//! - [`SegmentHeader::segment_hash`] is a content digest, sha256 over the
//!   canonical bytes of the records. It is what the *next* segment binds
//!   through [`SegmentHeader::prev_segment_hash`], so the segments form their
//!   own hash-linked list, and it is what a fetched body is checked against.
//! - [`SegmentHeader::last_hash`] is the record hash of the segment's newest
//!   record. It is what the next *record* links to, whether that record is
//!   the oldest one still resident or the first of the following segment.
//!
//! Keeping them apart is what lets the chain verify end to end with no
//! archived history resident (B-4): a header alone proves where the segment
//! sits in the segment chain and where the record chain continues, and only
//! [`crate::Depth::Full`] has to fetch a body to prove what is inside it.
//!
//! The header list is the same shape as the decision chain: a unique index on
//! the parent hash makes it linear by construction, and [`order_segments`] is
//! the segment-level twin of [`crate::order_chain`].

use std::collections::HashMap;

use rahi_types::Error;
use serde::{Deserialize, Serialize};

use crate::record::{DecisionId, Hash, SignedRecord};

/// The segment table (spec 014 B-2).
///
/// One row per sealed segment, holding everything but the records: the body
/// is in the archive and this table is how the chain is checked without it.
pub const SEGMENTS_TABLE_SQL: &str = "CREATE TABLE IF NOT EXISTS kernel_segments (\
    segment_hash TEXT PRIMARY KEY, \
    prev_segment_hash TEXT NOT NULL, \
    last_hash TEXT NOT NULL, \
    first_id TEXT NOT NULL, \
    last_id TEXT NOT NULL, \
    count INTEGER NOT NULL)";

/// The unique parent index over the segment chain.
///
/// The same arbitration the decision chain uses (spec 013 B-2): one segment
/// per predecessor, so the archive is a list rather than a tree and a second
/// sealer racing on the same tail loses at the store.
pub const SEGMENTS_INDEX_SQL: &str = "CREATE UNIQUE INDEX IF NOT EXISTS kernel_segments_parent \
     ON kernel_segments (prev_segment_hash)";

/// The key prefix every archived segment body lives under (spec 014 B-3).
///
/// Nothing else is ever written under it, and no backup verb reads or writes
/// it (B-6): an archive is not a snapshot schedule.
pub const SEGMENT_PREFIX: &str = "ledger/segments/";

/// One sealed segment as the store keeps it: everything but the records.
///
/// This is what a `kernel_segments` row holds and what the archived body
/// repeats, so a fetched body can be checked against the row that claims it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentHeader {
    /// The decision id of the oldest record in the segment.
    pub first_id: DecisionId,
    /// The decision id of the newest record in the segment.
    pub last_id: DecisionId,
    /// How many records the segment holds.
    pub count: u32,
    /// sha256 over the canonical bytes of the records (B-2).
    pub segment_hash: Hash,
    /// The predecessor segment's `segment_hash`, or the ledger's genesis
    /// parent for the first segment sealed, which is where a backward walk
    /// stops (B-4).
    pub prev_segment_hash: Hash,
    /// The record hash of the newest record in the segment: what the next
    /// record in the chain links to.
    pub last_hash: Hash,
}

impl SegmentHeader {
    /// The archive key of this segment's body (spec 014 B-3).
    #[must_use]
    pub fn key(&self) -> String {
        format!("{SEGMENT_PREFIX}{}-{}.json", self.first_id, self.last_id)
    }
}

/// A sealed segment with its records: the body written to the archive.
///
/// The header is flattened into the same JSON object as `records`, the way
/// [`SignedRecord`] flattens its envelope, so the archived object is one flat
/// document an auditor can read without knowing this crate's types.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segment {
    /// What the store keeps about this segment.
    #[serde(flatten)]
    pub header: SegmentHeader,
    /// The records, oldest first, exactly as they were resident.
    pub records: Vec<SignedRecord>,
}

impl Segment {
    /// Seal `records` into a segment linked to `prev_segment_hash`.
    ///
    /// `records` must already be in chain order; sealing does not reorder
    /// them, because the order is the chain and re-deriving it here would be
    /// a second implementation of linearity.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when `records` is empty or longer than a `u32`;
    /// [`Error::Integrity`] when a record does not serialize or its hash does
    /// not parse.
    pub fn seal(prev_segment_hash: Hash, records: Vec<SignedRecord>) -> Result<Self, Error> {
        let (Some(first), Some(last)) = (records.first(), records.last()) else {
            return Err(Error::Validation(
                "a segment seals at least one record".to_owned(),
            ));
        };
        let count = u32::try_from(records.len()).map_err(|_| {
            Error::Validation(format!("{} records do not fit a segment", records.len()))
        })?;
        let header = SegmentHeader {
            first_id: DecisionId::new(first.record.id.clone()),
            last_id: DecisionId::new(last.record.id.clone()),
            count,
            segment_hash: segment_hash(&records)?,
            prev_segment_hash,
            last_hash: last.hash()?,
        };
        Ok(Self { header, records })
    }

    /// The archive key of this segment's body.
    #[must_use]
    pub fn key(&self) -> String {
        self.header.key()
    }

    /// The canonical bytes written to the archive.
    ///
    /// # Errors
    ///
    /// [`Error::Integrity`] when the segment does not serialize to JSON.
    pub fn to_canonical_bytes(&self) -> Result<Vec<u8>, Error> {
        let value = serde_json::to_value(self)
            .map_err(|e| Error::Integrity(format!("segment does not serialize: {e}")))?;
        Ok(canonical_keysort_json::to_canonical_string(&value).into_bytes())
    }

    /// Read a segment back from an archived body.
    ///
    /// # Errors
    ///
    /// [`Error::Integrity`] when the bytes are not a segment.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        serde_json::from_slice(bytes)
            .map_err(|e| Error::Integrity(format!("archived segment does not parse: {e}")))
    }

    /// Check that the body agrees with the header it carries.
    ///
    /// Recomputes `segment_hash` over the records and checks the three
    /// projections of them the header repeats. What this cannot check is
    /// where the segment sits in the chain: that is the walk's job in
    /// [`crate::Ledger::verify_chain`].
    ///
    /// # Errors
    ///
    /// [`Error::Integrity`], naming the segment by its key.
    pub fn verify(&self) -> Result<(), Error> {
        let key = self.key();
        let recomputed = segment_hash(&self.records)?;
        if recomputed != self.header.segment_hash {
            return Err(Error::Integrity(format!(
                "archived segment {key} hashes to {recomputed} but claims {}: the body was \
                 rewritten after it was sealed",
                self.header.segment_hash
            )));
        }
        let count = u32::try_from(self.records.len()).unwrap_or(u32::MAX);
        if count != self.header.count {
            return Err(Error::Integrity(format!(
                "archived segment {key} holds {count} records and claims {}",
                self.header.count
            )));
        }
        let Some(last) = self.records.last() else {
            return Err(Error::Integrity(format!(
                "archived segment {key} holds no records"
            )));
        };
        if last.hash()? != self.header.last_hash {
            return Err(Error::Integrity(format!(
                "archived segment {key} ends at {} and claims {}",
                last.record.record_hash, self.header.last_hash
            )));
        }
        Ok(())
    }
}

/// sha256 over the canonical bytes of `records` (spec 014 B-2).
///
/// The bytes are the canonical JSON array of the records, the same
/// key-sorted serialization every hash in this crate is taken over, so an
/// auditor recomputes it from the archived body with a JSON canonicalizer and
/// a sha256, and nothing else.
///
/// # Errors
///
/// [`Error::Integrity`] when the records do not serialize.
pub fn segment_hash(records: &[SignedRecord]) -> Result<Hash, Error> {
    let value = serde_json::to_value(records)
        .map_err(|e| Error::Integrity(format!("segment records do not serialize: {e}")))?;
    let bytes = canonical_keysort_json::to_canonical_string(&value);
    Hash::parse(attest_ledger_core::sha256_hex(bytes.as_bytes()))
}

/// Put an unordered set of segment headers into chain order, from
/// `genesis_parent`.
///
/// The segment-level twin of [`crate::order_chain`]: the unique index on
/// `prev_segment_hash` means at most one segment claims each predecessor, so
/// walking from the ledger's genesis parent is the whole of linearity. A
/// header the walk never reaches belongs to another cell's archive or sits
/// behind a segment that was deleted, and both are damage.
///
/// # Errors
///
/// [`Error::Integrity`] when two segments claim one predecessor or when the
/// walk does not reach every header.
pub fn order_segments(
    genesis_parent: &Hash,
    headers: Vec<SegmentHeader>,
) -> Result<Vec<SegmentHeader>, Error> {
    let total = headers.len();
    let mut by_parent: HashMap<Hash, SegmentHeader> = HashMap::with_capacity(total);
    for header in headers {
        let parent = header.prev_segment_hash.clone();
        if let Some(rival) = by_parent.insert(parent.clone(), header) {
            return Err(Error::Integrity(format!(
                "the archive forked at {parent}: segment {} also claims that predecessor",
                rival.segment_hash
            )));
        }
    }

    let mut ordered = Vec::with_capacity(total);
    let mut cursor = genesis_parent.clone();
    while let Some(header) = by_parent.remove(&cursor) {
        cursor = header.segment_hash.clone();
        ordered.push(header);
    }

    if let Some(orphan) = by_parent.values().next() {
        return Err(Error::Integrity(format!(
            "segment {} links to {} which is not in the archive: {} segment(s) unreachable from \
             the genesis parent {genesis_parent}",
            orphan.segment_hash,
            orphan.prev_segment_hash,
            by_parent.len()
        )));
    }
    Ok(ordered)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use rahi_types::Sub;

    use crate::record::{Decision, DecisionKind, Outcome};
    use crate::signer::LedgerSigner;

    fn root() -> Hash {
        Hash::parse(format!("sha256:{}", "11".repeat(32))).expect("a hash")
    }

    /// A chain of `n` records built the way `append` builds them.
    fn chain(n: usize) -> Vec<SignedRecord> {
        let signer = LedgerSigner::from_seed([3u8; 32]);
        let mut parent = root();
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let mut decision = Decision::new(
                DecisionId::new(format!("d-{i}")),
                DecisionKind::new("db.write"),
                Sub::new("subject"),
                Outcome::Allow,
                "granted",
            );
            decision.prev_hash = parent;
            let record = SignedRecord::build(&decision, &signer).expect("builds");
            parent = record.hash().expect("a hash");
            out.push(record);
        }
        out
    }

    #[test]
    fn sealing_names_the_run_and_the_hash_the_next_record_links_to() {
        let records = chain(3);
        let last = records[2].hash().expect("a hash");
        let segment = Segment::seal(root(), records).expect("seals");

        assert_eq!(segment.header.first_id, DecisionId::new("d-0"));
        assert_eq!(segment.header.last_id, DecisionId::new("d-2"));
        assert_eq!(segment.header.count, 3);
        assert_eq!(segment.header.prev_segment_hash, root());
        assert_eq!(
            segment.header.last_hash, last,
            "the next record links the last record, not the content digest"
        );
        assert_ne!(
            segment.header.segment_hash, last,
            "the content digest is a different value with a different job"
        );
        assert_eq!(segment.key(), "ledger/segments/d-0-d-2.json");
        segment.verify().expect("the body agrees with its header");
    }

    #[test]
    fn an_empty_segment_is_refused() {
        let err = Segment::seal(root(), Vec::new()).expect_err("refused");
        assert!(matches!(err, Error::Validation(_)), "{err}");
    }

    #[test]
    fn a_segment_round_trips_through_its_archived_bytes() {
        let segment = Segment::seal(root(), chain(2)).expect("seals");
        let bytes = segment.to_canonical_bytes().expect("serializes");
        assert_eq!(Segment::from_bytes(&bytes).expect("parses"), segment);
    }

    #[test]
    fn a_rewritten_body_no_longer_matches_its_content_digest() {
        let mut segment = Segment::seal(root(), chain(3)).expect("seals");
        segment.records[1].record.payload = serde_json::json!({ "tampered": true });
        let err = segment.verify().expect_err("refused");
        assert!(matches!(err, Error::Integrity(_)), "{err}");
        assert!(
            err.message().contains(&segment.key()),
            "the failure names the segment: {err}"
        );
    }

    #[test]
    fn segments_order_from_the_genesis_parent_however_they_were_read() {
        let first = Segment::seal(root(), chain(2)).expect("seals").header;
        let second = Segment::seal(first.segment_hash.clone(), chain(2))
            .expect("seals")
            .header;
        let ordered = order_segments(&root(), vec![second.clone(), first.clone()]).expect("orders");
        assert_eq!(ordered, vec![first.clone(), second.clone()]);

        // A segment whose predecessor is not in the archive is unreachable.
        let err = order_segments(&root(), vec![second]).expect_err("refused");
        assert!(matches!(err, Error::Integrity(_)), "{err}");

        // Two segments claiming one predecessor are a fork.
        let rival = Segment::seal(root(), chain(3)).expect("seals").header;
        let err = order_segments(&root(), vec![first, rival]).expect_err("refused");
        assert!(matches!(err, Error::Integrity(_)), "{err}");
    }
}
