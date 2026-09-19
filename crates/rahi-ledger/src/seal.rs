//! The hot window, sealing the tail, and verifying a chain that has one
//! (spec 014 B-1, B-2, B-4).
//!
//! hiqlite replicates the whole database to every node, so an audit history
//! that only grows cannot live in it. What has to be resident is the head:
//! linearizable read-modify-write is needed to append, and it is needed
//! nowhere else. Everything older is read rarely, never modified, and is
//! exactly what an object store is for.
//!
//! So the chain is split at that seam. [`Ledger::seal_if_needed`] lifts the
//! oldest contiguous run out of the hot table into an immutable
//! [`crate::Segment`], writes the body to the [`Archive`] **first**, and only
//! then commits the header row and the deletion in one `txn` (B-2, FR-004).
//! An archive write that fails leaves the hot table exactly as it was.
//!
//! The chain still verifies end to end. [`Ledger::resident_root`] is the hash
//! the oldest resident record links to: the last segment's `last_hash`, or
//! the genesis parent when nothing has been sealed. That one value is what
//! keeps spec 013's boot verification honest on a ledger whose history is no
//! longer resident, and it is why sealing changes what the chain is rooted at
//! without changing a single stored record.
//!
//! [`Depth`] is the cost dial. [`Depth::Resident`] reads only the store and
//! is what boot uses; [`Depth::Full`] additionally fetches every archived
//! body and re-verifies it, which is what an audit asks for and what no boot
//! path should pay for.

use rahi_store::{Statement, Value};
use rahi_types::Error;
use serde::Deserialize;

use crate::archive::Archive;
use crate::chain::Ledger;
use crate::identity::{Accounted, AccountingOutcome, accounting_statements};
use crate::record::{DecisionId, Hash, SignedRecord};
use crate::segment::{Segment, SegmentHeader, order_segments};

/// Every sealed header, and the count of them taken in the same statement
/// (spec 036 D-11).
///
/// The shape is the decisions table's: a witness row with no `FROM`, so the
/// statement always yields it, carrying a `COUNT(*)` evaluated in this one
/// snapshot, and then the rows themselves. An empty answer from this read
/// used to let [`Ledger::current_manifest`] walk back to the genesis parent,
/// which on a chain that has transitioned is the manifest an older image
/// carries.
const SEGMENTS_CENSUS_SQL: &str = "SELECT 0 AS witness, \
     (SELECT COUNT(*) FROM kernel_segments) AS total, \
     '' AS first_id, '' AS last_id, 0 AS count, '' AS segment_hash, \
     '' AS prev_segment_hash, '' AS last_hash, NULL AS current_manifest \
     UNION ALL \
     SELECT 1 AS witness, 0 AS total, first_id, last_id, count, segment_hash, \
     prev_segment_hash, last_hash, current_manifest FROM kernel_segments";

/// The last segment: the one no other segment claims as its predecessor.
const SEGMENT_HEAD_SQL: &str = "SELECT first_id, last_id, count, segment_hash, \
     prev_segment_hash, last_hash, current_manifest FROM kernel_segments \
     WHERE segment_hash NOT IN (SELECT prev_segment_hash FROM kernel_segments)";

const SEGMENT_COUNT_SQL: &str = "SELECT COUNT(*) AS total FROM kernel_segments";

/// How much history stays resident and how much is sealed at a time
/// (spec 014 B-1).
///
/// The two numbers are validated against each other on construction rather
/// than trusted at use: a segment larger than the window would empty the hot
/// table, and a chain with no resident head cannot be appended to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SealPolicy {
    hot_window: u32,
    segment_size: u32,
}

impl SealPolicy {
    /// How many records stay resident before sealing starts.
    pub const DEFAULT_HOT_WINDOW: u32 = 10_000;

    /// How many records one segment holds.
    pub const DEFAULT_SEGMENT_SIZE: u32 = 1_000;

    /// The largest segment this crate will seal.
    ///
    /// One `DELETE` carries one parameter per archived id, so the bound keeps
    /// the statement far below SQLite's parameter limit and keeps the Raft
    /// entry a seal produces a predictable size.
    pub const MAX_SEGMENT_SIZE: u32 = 10_000;

    /// A policy, checked.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when `segment_size` is zero, larger than
    /// [`SealPolicy::MAX_SEGMENT_SIZE`], or larger than `hot_window`.
    pub fn new(hot_window: u32, segment_size: u32) -> Result<Self, Error> {
        if segment_size == 0 {
            return Err(Error::Config(
                "seal segment_size must be at least 1".to_owned(),
            ));
        }
        if segment_size > Self::MAX_SEGMENT_SIZE {
            return Err(Error::Config(format!(
                "seal segment_size {segment_size} exceeds the maximum {}",
                Self::MAX_SEGMENT_SIZE
            )));
        }
        if segment_size > hot_window {
            return Err(Error::Config(format!(
                "seal segment_size {segment_size} exceeds hot_window {hot_window}: a seal would \
                 leave no resident head to append onto"
            )));
        }
        Ok(Self {
            hot_window,
            segment_size,
        })
    }

    /// How many records stay resident before sealing starts.
    #[must_use]
    pub const fn hot_window(&self) -> u32 {
        self.hot_window
    }

    /// How many records one segment holds.
    #[must_use]
    pub const fn segment_size(&self) -> u32 {
        self.segment_size
    }
}

impl Default for SealPolicy {
    fn default() -> Self {
        Self {
            hot_window: Self::DEFAULT_HOT_WINDOW,
            segment_size: Self::DEFAULT_SEGMENT_SIZE,
        }
    }
}

/// How far a verification reaches (spec 014 B-4).
///
/// The archive is reachable only through [`Depth::Full`], so "bodies are
/// fetched only when full verification is asked for" is a property of the
/// type rather than a promise in a comment: a boot path holding no archive
/// cannot express the deep check.
#[derive(Clone, Copy, Debug)]
pub enum Depth<'a> {
    /// The resident chain and the segment headers. What boot uses.
    Resident,
    /// Also fetch every archived body and re-verify what is inside it.
    Full(&'a dyn Archive),
}

/// One `kernel_segments` row, as every read of that table deserializes it.
///
/// `pub(crate)` since spec 036 D-15: the manifest read takes the segment
/// headers and the resident records in one statement, and builds its headers
/// out of these same rows rather than out of a second shape.
#[derive(Debug, Deserialize)]
pub(crate) struct SegmentRow {
    pub(crate) first_id: String,
    pub(crate) last_id: String,
    pub(crate) count: u32,
    pub(crate) segment_hash: String,
    pub(crate) prev_segment_hash: String,
    pub(crate) last_hash: String,
    /// `NULL` on a segment sealed before spec 036 B-5 (segment.rs).
    #[serde(default)]
    pub(crate) current_manifest: Option<String>,
}

impl SegmentRow {
    pub(crate) fn into_header(self) -> Result<SegmentHeader, Error> {
        Ok(SegmentHeader {
            first_id: DecisionId::new(self.first_id),
            last_id: DecisionId::new(self.last_id),
            count: self.count,
            segment_hash: Hash::parse(self.segment_hash)?,
            prev_segment_hash: Hash::parse(self.prev_segment_hash)?,
            last_hash: Hash::parse(self.last_hash)?,
            current_manifest: self
                .current_manifest
                .filter(|text| !text.is_empty())
                .map(Hash::parse)
                .transpose()?,
        })
    }
}

#[derive(Debug, Deserialize)]
struct SegmentCensusRow {
    witness: i64,
    #[serde(default)]
    total: i64,
    #[serde(flatten)]
    row: SegmentRow,
}

#[derive(Debug, Deserialize)]
struct CountRow {
    total: i64,
}

impl Ledger {
    /// The hash the oldest resident record links to (spec 014 B-4).
    ///
    /// The last segment's `last_hash` when anything has been sealed, and the
    /// genesis parent when nothing has. Everything spec 013 rooted at the
    /// genesis parent is rooted here instead once a segment exists, which is
    /// the whole of what sealing changes about the resident chain.
    ///
    /// Read through the leader: an append chains onto this value when the hot
    /// table is empty, and a boot decides to fail on it.
    ///
    /// # Errors
    ///
    /// [`Error::Integrity`] when more than one segment is unclaimed (the
    /// archive forked) or when segments exist and none is last (the links
    /// form a cycle); the store's own error when the leader cannot be
    /// reached.
    pub async fn resident_root(&self) -> Result<Hash, Error> {
        let rows: Vec<SegmentRow> = self
            .store()
            .query_consistent(SEGMENT_HEAD_SQL, vec![])
            .await?;
        let mut last = rows.into_iter();
        let Some(head) = last.next() else {
            return if self.segment_count().await? == 0 {
                Ok(self.genesis_parent().clone())
            } else {
                Err(Error::Integrity(
                    "the archive has segments but no last one: its links form a cycle".to_owned(),
                ))
            };
        };
        if let Some(rival) = last.next() {
            return Err(Error::Integrity(format!(
                "the archive forked: both {} and {} are unclaimed segments",
                head.segment_hash, rival.segment_hash
            )));
        }
        Hash::parse(head.last_hash)
    }

    /// Every sealed segment's header, oldest first.
    ///
    /// # Errors
    ///
    /// [`Error::Integrity`] when a row does not parse or when the headers do
    /// not form one list rooted at the genesis parent; the store's own error
    /// when the read fails.
    pub async fn segments(&self) -> Result<Vec<SegmentHeader>, Error> {
        let rows: Vec<SegmentCensusRow> = self
            .store()
            .query_consistent(SEGMENTS_CENSUS_SQL, vec![])
            .await?;
        let mut witnessed = None;
        let mut carried = Vec::new();
        for row in rows {
            if row.witness == 0 {
                witnessed = Some(row.total);
            } else {
                carried.push(row.row);
            }
        }
        crate::chain::check_census("kernel_segments", witnessed, carried.len())?;
        let headers = carried
            .into_iter()
            .map(SegmentRow::into_header)
            .collect::<Result<Vec<_>, Error>>()?;
        order_segments(self.genesis_parent(), headers)
    }

    /// How many segments have been sealed.
    ///
    /// # Errors
    ///
    /// The store's own error, or [`Error::Integrity`] when the count is
    /// negative.
    pub async fn segment_count(&self) -> Result<u64, Error> {
        let rows: Vec<CountRow> = self
            .store()
            .query_consistent(SEGMENT_COUNT_SQL, vec![])
            .await?;
        let total = rows.first().map_or(0, |row| row.total);
        u64::try_from(total)
            .map_err(|_| Error::Integrity(format!("kernel_segments counted {total} rows")))
    }

    /// Seal the oldest run when the hot window is exceeded (spec 014 B-1).
    ///
    /// Called after an append. When the resident count is within
    /// `policy.hot_window()` this is one `COUNT(*)` and nothing else; when it
    /// is not, the oldest `policy.segment_size()` records become a segment,
    /// the body is written to `archive`, and only after that write is
    /// acknowledged do the header row and the deletion commit together in one
    /// `txn` (B-2, FR-004).
    ///
    /// A body written for a seal whose transaction then loses to another
    /// sealer stays in the archive, referenced by no header and read by no
    /// verification. That is the safe direction of the two: a segment is
    /// never deleted from the hot table without a durable body, and an
    /// unreferenced body is inert.
    ///
    /// # Errors
    ///
    /// The archive's error when the body cannot be written, including
    /// [`Error::Conflict`] when the key already exists (B-5); the store's own
    /// error when the transaction fails; [`Error::Integrity`] when the
    /// transaction commits without moving the rows it named.
    pub async fn seal_if_needed(
        &self,
        archive: &dyn Archive,
        policy: &SealPolicy,
    ) -> Result<Option<SegmentHeader>, Error> {
        if self.count().await? <= u64::from(policy.hot_window()) {
            return Ok(None);
        }

        let size = usize::try_from(policy.segment_size()).unwrap_or(usize::MAX);
        let mut records = self.records().await?;
        if records.len() <= size {
            // Everything resident is inside the run: sealing it would leave
            // no head to append onto. Another sealer got there first.
            return Ok(None);
        }
        records.truncate(size);

        let sealed = self.segments().await?;
        let previous = sealed
            .last()
            .map_or_else(|| self.genesis_parent().clone(), |h| h.segment_hash.clone());
        // Spec 036 B-5: what the tail of this run leaves current. A run with
        // no transition in it inherits what the segment before it named, and
        // the first segment of a chain that has never transitioned inherits
        // the genesis parent, which is what B-1 calls the current manifest
        // until a transition exists.
        let before = sealed
            .last()
            .and_then(|h| h.current_manifest.clone())
            .unwrap_or_else(|| self.genesis_parent().clone());
        let current_manifest = Some(Self::manifest_at_tail(&records, before)?);
        let segment = Segment::seal(previous, records, current_manifest)?;

        archive
            .put(&segment.key(), segment.to_canonical_bytes()?)
            .await?;

        let deleted = u64::from(segment.header.count);
        // Spec 042 B-5: the transaction that inserts the header and deletes
        // the archived rows additionally accounts for every record it
        // archives, and recomputes that segment's counter from the two
        // tables it just wrote. It decides nothing by reading first, because
        // `txn` takes a batch of statements and offers no read between them:
        // each record's accounting is self-arbitrating statements, in the
        // same spirit as B-3's insert.
        let accounted = segment
            .records
            .iter()
            .map(Accounted::of)
            .collect::<Result<Vec<_>, Error>>()?;
        let mut statements = vec![
            insert_segment(&segment.header),
            delete_records(&segment.records),
        ];
        let accounting_at = statements.len();
        statements.extend(accounting_statements(
            &accounted,
            &segment.header.segment_hash,
        ));
        let results = self.store().txn(statements).await?;
        let sealed = results.first().is_some_and(|r| r.rows_affected == 1);
        let removed = results.get(1).is_some_and(|r| r.rows_affected == deleted);
        if !sealed || !removed {
            return Err(Error::Integrity(format!(
                "sealing {} committed without moving the rows it named",
                segment.key()
            )));
        }
        // Spec 042 B-13: a row this seal had to **create** rather than stamp
        // is a record appended by a binary that does not stamp. It is
        // counted, logged at `warn` naming the id, and moves this node's
        // cached verdict to incomplete with no leader read.
        let outcome = AccountingOutcome::read(&results, &accounted, accounting_at);
        self.note_unstamped_writer(&outcome);

        // Spec 042 B-5: a post-commit detection, not a rollback, because
        // `txn` commits the batch before any result is inspected. What it
        // reports is that the segment is not fully accounted for, which is
        // exactly the uncovered state B-7 reports and B-8 repairs, so the
        // operator is handed a repairable condition rather than a
        // half-written one. This spec claims no rollback it cannot perform.
        let stamped = self.stamped_of(&segment.header.segment_hash).await?;
        crate::identity::accounted_fully(&segment.key(), stamped, segment.header.count)?;
        Ok(Some(segment.header))
    }

    /// Verify the chain, with its archived history, to `depth`
    /// (spec 014 B-4).
    ///
    /// At either depth this checks the segment headers form one list rooted
    /// at the genesis parent, that the resident chain is whole and signed,
    /// and that its oldest record links the last segment. At
    /// [`Depth::Full`] it additionally fetches every body, recomputes its
    /// content digest, and verifies the records inside it link the segment
    /// before them, so the whole history from the genesis record to the head
    /// is checked without any of it being resident.
    ///
    /// # Errors
    ///
    /// [`Error::Integrity`], naming the record or the segment that failed;
    /// the archive's own error when a body cannot be fetched at
    /// [`Depth::Full`].
    pub async fn verify_chain(&self, depth: Depth<'_>) -> Result<(), Error> {
        self.verify_chain_witnessed(depth).await.map(|_| ())
    }

    /// [`Ledger::verify_chain`], reporting what it saw (spec 036 D-11).
    ///
    /// `open` keeps the answer, because verification has already paid for
    /// both reads and because neither fact it records can become false
    /// afterwards.
    pub(crate) async fn verify_chain_witnessed(
        &self,
        depth: Depth<'_>,
    ) -> Result<crate::chain::OpenedChain, Error> {
        let segments = self.segments().await?;
        let root = self.resident_root().await?;
        // `records` is ordered from `root`, and 013's verifier checks that
        // the oldest record links it: the check B-4 asks for at the seam
        // between what is resident and what is sealed.
        let records = self.records().await?;
        crate::verify::verify_chain(&root, &records, &self.verifier())?;

        if let Depth::Full(archive) = depth {
            self.verify_segment_bodies(archive, &segments).await?;
        }
        Ok(crate::chain::OpenedChain {
            resident: !records.is_empty(),
            sealed: !segments.is_empty(),
        })
    }

    /// Fetch and re-verify every archived body, oldest segment first.
    async fn verify_segment_bodies(
        &self,
        archive: &dyn Archive,
        segments: &[SegmentHeader],
    ) -> Result<(), Error> {
        let verifier = self.verifier();
        // What the first record of the next segment must link to: the
        // genesis parent, then each segment's terminal record hash.
        let mut parent = self.genesis_parent().clone();
        for header in segments {
            let key = header.key();
            let bytes = archive.get(&key).await?;
            let segment = Segment::from_bytes(&bytes).map_err(|e| {
                Error::Integrity(format!("archived segment {key}: {}", e.message()))
            })?;
            if segment.header != *header {
                return Err(Error::Integrity(format!(
                    "archived segment {key} carries a header the store does not agree with"
                )));
            }
            // Spec 042 B-8 needs one body verified on its own, and a reindex
            // must verify exactly what a full-depth boot verifies: one seam,
            // used by both.
            crate::verify::verify_segment(&parent, &segment, &verifier)?;
            parent = header.last_hash.clone();
        }
        Ok(())
    }
}

/// The row a seal inserts.
///
/// Kept beside the deletion it commits with so the column list and the rows
/// it accounts for cannot drift apart.
fn insert_segment(header: &SegmentHeader) -> Statement {
    Statement::with_params(
        "INSERT INTO kernel_segments \
         (segment_hash, prev_segment_hash, last_hash, first_id, last_id, count, \
          current_manifest) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
        vec![
            Value::from(header.segment_hash.as_str()),
            Value::from(header.prev_segment_hash.as_str()),
            Value::from(header.last_hash.as_str()),
            Value::from(header.first_id.as_str()),
            Value::from(header.last_id.as_str()),
            Value::from(header.count),
            header
                .current_manifest
                .as_ref()
                .map_or(Value::Null, |h| Value::from(h.as_str())),
        ],
    )
}

/// The deletion of exactly the records the segment archived.
///
/// By id rather than by a range: the ids are what the segment names, and a
/// range over a table whose order is the hash links would be a second, weaker
/// statement of the same thing.
fn delete_records(records: &[SignedRecord]) -> Statement {
    let placeholders = (1..=records.len())
        .map(|i| format!("${i}"))
        .collect::<Vec<_>>()
        .join(", ");
    Statement::with_params(
        format!("DELETE FROM kernel_decisions WHERE id IN ({placeholders})"),
        records
            .iter()
            .map(|r| Value::from(r.record.id.as_str()))
            .collect(),
    )
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_the_ones_the_spec_names() {
        let policy = SealPolicy::default();
        assert_eq!(policy.hot_window(), 10_000);
        assert_eq!(policy.segment_size(), 1_000);
    }

    #[test]
    fn a_policy_that_would_empty_the_hot_table_is_refused() {
        for (window, size) in [(20, 0), (20, 21), (20, SealPolicy::MAX_SEGMENT_SIZE + 1)] {
            let err = SealPolicy::new(window, size).expect_err("refused");
            assert!(matches!(err, Error::Config(_)), "{window}/{size}: {err}");
        }
        let policy = SealPolicy::new(20, 20).expect("a segment may be the whole window");
        assert_eq!(policy.segment_size(), 20);
    }

    #[test]
    fn the_deletion_names_every_archived_id_and_nothing_else() {
        let statement = delete_records(&[]);
        assert_eq!(
            statement.sql, "DELETE FROM kernel_decisions WHERE id IN ()",
            "the shape is a plain IN list"
        );
        assert!(statement.params.is_empty());
    }
}
