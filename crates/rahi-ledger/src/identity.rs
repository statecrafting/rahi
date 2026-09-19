//! Lifetime identity: one id names one decision for the life of the chain
//! (spec 042).
//!
//! Spec 013 D-5 told a lost compare-and-swap from a duplicate id by asking
//! whether the id is **resident**, and spec 014 deletes the row when its
//! segment is sealed. Past the hot window those two facts make "never
//! appended" and "appended and archived" the same answer, so an appender
//! that lost its acknowledgement and retried wrote a second record under one
//! id, and `verify_chain` passed at both depths because a duplicated id is
//! not a broken link.
//!
//! Three narrow tables close that, and each has one job:
//!
//! - [`IDENTITY_TABLE_SQL`] holds one row per decision that **survives
//!   sealing**. Its `TEXT PRIMARY KEY` is the arbitration inside the append
//!   transaction, exactly as the unique parent index is the arbitration for
//!   the compare-and-swap (B-3). Nothing decides a duplicate by reading
//!   first, because a read before a write is not atomic against another
//!   node's append or another task's seal.
//! - [`COLLISIONS_TABLE_SQL`] records every copy beyond the first of an id
//!   history already spent twice. Recording a collision is **evidence, never
//!   repair**: nothing here resolves, corrects, deduplicates or reconciles
//!   anything, no copy is preferred, no archived byte is written, and no row
//!   of either table is ever deleted (B-12, D-4).
//! - [`COVERAGE_TABLE_SQL`] caches, per sealed segment, how many of its
//!   records are accounted for. It is never incremented: every write of it
//!   is a recomputation from the other two tables inside the transaction
//!   that wrote them, so it cannot drift from the rows it counts (B-1).
//!
//! What this module never does is answer from unavailable evidence.
//! [`Presence`] keeps four different answers apart: a decision that is
//! there, one proven absent, history that cannot be vouched for, and an id
//! proven ambiguous. An archive that is missing, corrupt or unreadable is an
//! error and never any of them (B-6).

use std::collections::BTreeMap;

use rahi_store::{Statement, Value};
use rahi_types::Error;
use serde::Deserialize;

use crate::archive::Archive;
use crate::chain::{Ledger, check_census};
use crate::record::{Decision, DecisionId, Hash, SignedRecord};
use crate::segment::{SegmentHeader, fetch_segment};

/// The identity table (B-1).
///
/// Narrow on purpose: an id, the digest of the decision's content with its
/// parent omitted, the hash of the record that landed under it, and the
/// segment it was archived into once it has been. Hashes are the lowercase
/// hex text [`Hash::as_str`] yields, the encoding `kernel_decisions.hash`
/// and `kernel_segments.segment_hash` already use; B-9 prices that against
/// a 32-byte blob and keeps the crate's convention.
pub const IDENTITY_TABLE_SQL: &str = "CREATE TABLE IF NOT EXISTS kernel_decision_identity (\
    id TEXT PRIMARY KEY, \
    identity_digest TEXT NOT NULL, \
    record_hash TEXT NOT NULL, \
    segment_hash TEXT NULL)";

/// Every coverage repair and every per-segment count reads this index (B-1).
pub const IDENTITY_INDEX_SQL: &str = "CREATE INDEX IF NOT EXISTS kernel_decision_identity_segment \
     ON kernel_decision_identity (segment_hash)";

/// The collision table (B-1, B-12).
///
/// Keyed by `(id, record_hash)`, so the same copy met twice by two reindex
/// runs is one row and two different records under one id are two.
pub const COLLISIONS_TABLE_SQL: &str = "CREATE TABLE IF NOT EXISTS kernel_decision_collisions (\
    id TEXT, \
    record_hash TEXT, \
    segment_hash TEXT NULL, \
    identity_digest TEXT NOT NULL, \
    PRIMARY KEY (id, record_hash))";

/// Read beside the identity index by every coverage recomputation (B-12).
pub const COLLISIONS_INDEX_SQL: &str = "CREATE INDEX IF NOT EXISTS kernel_decision_collisions_segment \
     ON kernel_decision_collisions (segment_hash)";

/// The per-segment counter (B-1).
///
/// A cache of a count, never of a fact: every value in it is recomputable
/// from the two tables above, and FR-018 asserts that.
pub const COVERAGE_TABLE_SQL: &str = "CREATE TABLE IF NOT EXISTS kernel_decision_coverage (\
    segment_hash TEXT PRIMARY KEY, \
    stamped INTEGER NOT NULL)";

/// The whole resident state this spec adds, in creation order.
pub const IDENTITY_SCHEMA_SQL: [&str; 5] = [
    IDENTITY_TABLE_SQL,
    IDENTITY_INDEX_SQL,
    COLLISIONS_TABLE_SQL,
    COLLISIONS_INDEX_SQL,
    COVERAGE_TABLE_SQL,
];

/// What the append transaction inserts beside the record (B-3).
const IDENTITY_INSERT_SQL: &str = "INSERT INTO kernel_decision_identity \
     (id, identity_digest, record_hash, segment_hash) VALUES ($1, $2, $3, NULL)";

/// The backfill of a chain restored from a snapshot taken before this spec
/// (FR-008): insert-or-ignore, so a concurrent append on another replica and
/// the backfill converge rather than fight.
const IDENTITY_BACKFILL_SQL: &str = "INSERT INTO kernel_decision_identity \
     (id, identity_digest, record_hash, segment_hash) VALUES ($1, $2, $3, NULL) \
     ON CONFLICT(id) DO NOTHING";

/// The stamp-only half of the accounting's first statement (D-9).
///
/// Its effect is exactly the [`IDENTITY_UPSERT_SQL`] `DO UPDATE` branch,
/// under the same guard, so the committed state is identical whether it runs
/// or not. What it buys is that the upsert that follows it can only ever
/// *create* a row, which is the observation B-13 requires this node to make
/// without a leader read.
const IDENTITY_STAMP_SQL: &str = "UPDATE kernel_decision_identity \
     SET segment_hash = $1 \
     WHERE id = $2 AND record_hash = $3 AND segment_hash IS NULL";

/// The conditional upsert of B-5: it may create a row, and may never
/// redirect one.
///
/// The `WHERE` is what makes it safe on a chain that already spent an id
/// twice. Stamping a row whose `record_hash` names a *different* record
/// would point that identity row at a segment which does not contain the
/// record it names, and [`Ledger::lookup`] would answer `Sealed` with a pair
/// no body satisfies. The guard makes the statement a no-op instead, and
/// [`COLLISION_INSERT_SQL`] accounts for the copy it declined.
const IDENTITY_UPSERT_SQL: &str = "INSERT INTO kernel_decision_identity \
     (id, identity_digest, record_hash, segment_hash) VALUES ($1, $2, $3, $4) \
     ON CONFLICT(id) DO UPDATE SET segment_hash = excluded.segment_hash \
     WHERE kernel_decision_identity.record_hash = excluded.record_hash \
       AND kernel_decision_identity.segment_hash IS NULL";

/// The second statement of B-5: it accounts for whatever the first declined,
/// and writes nothing when the first one accounted for the record.
///
/// Statements run in order inside one transaction, so this one sees the
/// effect of the two above: a record they inserted or stamped is already
/// accounted for and gets no collision row; a record they declined gets one.
const COLLISION_INSERT_SQL: &str = "INSERT INTO kernel_decision_collisions \
     (id, record_hash, segment_hash, identity_digest) \
     SELECT $1, $2, $3, $4 \
     WHERE NOT EXISTS (SELECT 1 FROM kernel_decision_identity \
                       WHERE id = $5 AND record_hash = $6 AND segment_hash = $7) \
     ON CONFLICT(id, record_hash) DO NOTHING";

/// The last statement of B-5: the counter, recomputed rather than
/// incremented, so it cannot drift from the rows it counts however the
/// statements above resolved.
const COVERAGE_RECOMPUTE_SQL: &str = "INSERT INTO kernel_decision_coverage \
     (segment_hash, stamped) VALUES ($1, \
        (SELECT COUNT(*) FROM kernel_decision_identity WHERE segment_hash = $2) \
      + (SELECT COUNT(*) FROM kernel_decision_collisions WHERE segment_hash = $3)) \
     ON CONFLICT(segment_hash) DO UPDATE SET stamped = excluded.stamped";

/// One segment's counter, read back after the transaction that wrote it.
const COVERAGE_OF_SEGMENT_SQL: &str =
    "SELECT stamped FROM kernel_decision_coverage WHERE segment_hash = $1";

/// One id's identity row and every collision row under it, in one statement
/// (B-4, B-6).
///
/// The first branch has no `FROM`, so SQLite yields it whatever the tables
/// hold: it is the witness that the read answered at all. Without it an
/// absent answer and an absent id are the same empty result, and spec 016
/// D-2 records that a local read which fails while stepping its rows comes
/// back as `Ok(vec![])`. Conflating the two is how a duplicate gets written.
const IDENTITY_OF_ID_SQL: &str = "SELECT 0 AS kind, '' AS identity_digest, \
     '' AS record_hash, '' AS segment_hash \
     UNION ALL \
     SELECT 1, identity_digest, record_hash, COALESCE(segment_hash, '') \
     FROM kernel_decision_identity WHERE id = $1 \
     UNION ALL \
     SELECT 2, identity_digest, record_hash, COALESCE(segment_hash, '') \
     FROM kernel_decision_collisions WHERE id = $2";

/// The whole coverage verdict, from one snapshot (B-7).
///
/// Coverage is an accounting over **records**, not over ids, because an id
/// can name more than one record on a chain written before this spec. Both
/// halves of it are read here in one statement, which is what makes the
/// answer a coherent whole rather than two separately complete reads: a seal
/// commits the new segment header, the deletion of the records it archived
/// and their accounting in one transaction (B-5), so a resident read taken
/// before it and a segment read taken after it could each account for every
/// row they carry and still, between them, describe a chain that never
/// existed.
///
/// Each relation carries its own `COUNT(*)`, evaluated as a scalar subquery
/// inside this same statement and therefore over this same snapshot, and
/// every row of it is carried rather than only the interesting ones, so
/// [`check_census`] is an exact census of both halves rather than a
/// plausibility check.
///
/// The cost is what AC-10 allows and no more: the resident half is bounded
/// by the hot window (spec 014 B-1), each of its two `EXISTS` probes is a
/// primary-key seek, and the sealed half is one metadata row per segment
/// joined to its counter. Nothing here scans or aggregates over
/// `kernel_decision_identity` or `kernel_decision_collisions`, whose size
/// grows with the life of the chain.
const COVERAGE_CENSUS_SQL: &str = "SELECT 0 AS kind, \
     (SELECT COUNT(*) FROM kernel_decisions) AS total, \
     '' AS segment_hash, 0 AS count, 0 AS stamped, '' AS id \
     UNION ALL \
     SELECT 1, (SELECT COUNT(*) FROM kernel_segments), '', 0, 0, '' \
     UNION ALL \
     SELECT 2, 0, '', 0, \
     (CASE WHEN EXISTS (SELECT 1 FROM kernel_decision_identity i \
                         WHERE i.id = d.id AND i.record_hash = d.hash) \
             OR EXISTS (SELECT 1 FROM kernel_decision_collisions c \
                         WHERE c.id = d.id AND c.record_hash = d.hash) \
           THEN 1 ELSE 0 END), d.id \
     FROM kernel_decisions d \
     UNION ALL \
     SELECT 3, 0, s.segment_hash, s.count, \
     COALESCE((SELECT v.stamped FROM kernel_decision_coverage v \
                WHERE v.segment_hash = s.segment_hash), -1), '' \
     FROM kernel_segments s";

/// The resident records no row accounts for, with the bytes a backfill needs
/// (FR-008).
const BACKFILL_CENSUS_SQL: &str = "SELECT 0 AS kind, \
     (SELECT COUNT(*) FROM kernel_decisions) AS total, X'' AS record \
     UNION ALL \
     SELECT 1, 0, d.record FROM kernel_decisions d \
     WHERE NOT EXISTS (SELECT 1 FROM kernel_decision_identity i \
                        WHERE i.id = d.id AND i.record_hash = d.hash) \
       AND NOT EXISTS (SELECT 1 FROM kernel_decision_collisions c \
                        WHERE c.id = d.id AND c.record_hash = d.hash)";

/// The two lifetime aggregates, which only an operator command pays for
/// (B-7).
const IDENTITY_TOTALS_SQL: &str = "SELECT \
     (SELECT COUNT(*) FROM kernel_decision_identity) AS identity_rows, \
     (SELECT COUNT(*) FROM kernel_decision_collisions) AS collisions";

/// The command that clears an uncovered chain, named in every refusal.
pub const REINDEX_COMMAND: &str = "rahi ledger reindex <archive>";

const ID_ROW_WITNESS: i64 = 0;
const ID_ROW_IDENTITY: i64 = 1;
const ID_ROW_COLLISION: i64 = 2;

const COVERAGE_RESIDENT_WITNESS: i64 = 0;
const COVERAGE_SEALED_WITNESS: i64 = 1;
const COVERAGE_RESIDENT_ROW: i64 = 2;
const COVERAGE_SEALED_ROW: i64 = 3;

#[derive(Debug, Deserialize)]
struct IdRow {
    kind: i64,
    #[serde(default)]
    identity_digest: String,
    #[serde(default)]
    record_hash: String,
    #[serde(default)]
    segment_hash: String,
}

#[derive(Debug, Deserialize)]
struct CoverageRow {
    kind: i64,
    #[serde(default)]
    total: i64,
    #[serde(default)]
    segment_hash: String,
    #[serde(default)]
    count: i64,
    #[serde(default)]
    stamped: i64,
    #[serde(default)]
    id: String,
}

#[derive(Debug, Deserialize)]
struct BackfillRow {
    kind: i64,
    #[serde(default)]
    total: i64,
    #[serde(default)]
    record: Vec<u8>,
}

#[derive(Debug, Deserialize)]
struct TotalsRow {
    identity_rows: i64,
    collisions: i64,
}

#[derive(Debug, Deserialize)]
struct StampedRow {
    stamped: i64,
}

/// One copy of an id history spent more than once (B-12).
///
/// Named `DecisionCopy` rather than the `Copy` of B-6's sketch, because a
/// type called `Copy` exported from this crate shadows the `Copy` marker
/// trait in any module that glob-imports it (D-10). Nothing else about it
/// differs: it is one recorded copy, and the set of them is the whole
/// answer for a colliding id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecisionCopy {
    /// The record this copy is.
    pub record_hash: Hash,
    /// The segment it was archived into, or `None` while it is resident.
    pub segment_hash: Option<Hash>,
    /// The digest of its content with its parent omitted (B-2).
    pub identity_digest: Hash,
}

impl std::fmt::Display for DecisionCopy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.segment_hash {
            Some(segment) => write!(f, "{} in segment {segment}", self.record_hash),
            None => write!(f, "{} resident", self.record_hash),
        }
    }
}

/// What the chain can prove about one decision id (B-6).
///
/// Five answers, deliberately not four: absence, unavailable evidence and
/// proven ambiguity are three different things, and collapsing any two of
/// them is how a duplicate gets written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Presence {
    /// The decision is in the chain and its record is still resident.
    Resident {
        /// The record that landed under this id.
        record_hash: Hash,
    },
    /// The decision is in the chain and its record has been archived.
    Sealed {
        /// The record that landed under this id.
        record_hash: Hash,
        /// The segment that holds it.
        segment_hash: Hash,
    },
    /// The id was never used, proven over a chain whose coverage is
    /// complete.
    Absent,
    /// Coverage is incomplete, so nothing can be concluded: the id may be in
    /// history this chain cannot yet vouch for.
    Unproven {
        /// The one uncovered segment the decision would have to be in, when
        /// exactly one segment is uncovered; `None` when several are.
        segment_hash: Option<Hash>,
    },
    /// History spent this id more than once. Every copy is carried and none
    /// is chosen (B-12, D-4).
    Ambiguous {
        /// Every recorded copy, in the order the rows were read.
        copies: Vec<DecisionCopy>,
    },
}

/// The coverage verdict, and only the verdict (B-7).
///
/// The identity row total and the collision total are deliberately not here:
/// counting either is a lifetime aggregate over a table that grows one row
/// per decision, and the boot path may not pay it (AC-10).
/// [`Ledger::identity_totals`] answers them separately.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Coverage {
    uncovered: Vec<Hash>,
    unstamped_resident: u64,
}

impl Coverage {
    /// Every resident record is accounted for and every sealed segment's
    /// counter equals its header's `count`.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.uncovered.is_empty() && self.unstamped_resident == 0
    }

    /// The segments whose accounting is not complete.
    #[must_use]
    pub fn uncovered(&self) -> &[Hash] {
        &self.uncovered
    }

    /// How many resident records no identity row and no collision row names.
    #[must_use]
    pub const fn unstamped_resident(&self) -> u64 {
        self.unstamped_resident
    }

    /// The refusal B-10 makes at the boot, and the backstop's refusal in
    /// B-11: what is missing and the one command that clears it.
    #[must_use]
    pub fn why(&self) -> String {
        format!(
            "{} sealed segment(s) and {} resident record(s) are not accounted for in the \
             lifetime identity index: run `{REINDEX_COMMAND}` against this cell's archive while \
             nothing appends",
            self.uncovered.len(),
            self.unstamped_resident
        )
    }
}

/// The two lifetime aggregates (B-7).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IdentityTotals {
    /// How many decisions the chain has ever recorded an identity for.
    pub identity_rows: u64,
    /// How many copies beyond the first of a twice-spent id are recorded.
    pub collisions: u64,
}

/// Why a segment could not be covered (B-8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReindexCause {
    /// The archived body is gone.
    NotFound,
    /// The archived body could not be read.
    Io,
    /// The archived body does not verify, or contradicts its header.
    Integrity,
}

impl ReindexCause {
    /// The word a report prints.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotFound => "NotFound",
            Self::Io => "Io",
            Self::Integrity => "Integrity",
        }
    }

    /// How bad this is, for picking the exit code of a walk that collected
    /// several.
    const fn severity(self) -> u8 {
        match self {
            Self::NotFound => 1,
            Self::Io => 2,
            Self::Integrity => 3,
        }
    }

    fn of(err: &Error) -> Self {
        match err {
            Error::NotFound(_) => Self::NotFound,
            Error::Integrity(_) => Self::Integrity,
            _ => Self::Io,
        }
    }
}

/// What one segment's repair did (B-8).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SegmentReindex {
    /// The segment.
    pub segment_hash: Hash,
    /// Whether its accounting is now complete.
    pub covered: bool,
    /// Why it is not, when it is not.
    pub cause: Option<ReindexCause>,
    /// What the cause was, in words.
    pub detail: String,
    /// Identity rows written or stamped for it.
    pub rows_written: u64,
    /// Copies of a twice-spent id recorded under it. Recorded, never
    /// resolved (B-12).
    pub collisions: u64,
}

/// What a whole reindex walk did (B-8).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReindexReport {
    /// Every segment the walk touched, newest first.
    pub segments: Vec<SegmentReindex>,
    /// How many segments it walked.
    pub walked: u64,
    /// Identity rows written or stamped.
    pub rows_written: u64,
    /// Copies of a twice-spent id recorded.
    pub collisions: u64,
}

impl ReindexReport {
    /// Whether every segment the walk touched ended covered.
    #[must_use]
    pub fn all_covered(&self) -> bool {
        self.segments.iter().all(|s| s.covered)
    }

    /// The outcome the verb exits with (B-8).
    ///
    /// Zero only when every segment ended covered and no collision was
    /// recorded. A report that reaches an operator saying "complete" while a
    /// body was unreadable is the one outcome this exists to make
    /// impossible.
    ///
    /// # Errors
    ///
    /// The most severe cause the walk collected, as [`Error::Integrity`],
    /// [`Error::Io`] or [`Error::NotFound`]; [`Error::Conflict`] when the
    /// walk was otherwise clean and recorded a collision.
    pub fn outcome(&self) -> Result<(), Error> {
        let worst = self
            .segments
            .iter()
            .filter_map(|s| s.cause)
            .max_by_key(|cause| cause.severity());
        if let Some(cause) = worst {
            let named: Vec<String> = self
                .segments
                .iter()
                .filter(|s| s.cause == Some(cause))
                .map(|s| format!("{} ({})", s.segment_hash, s.detail))
                .collect();
            let said = format!(
                "reindex left {} segment(s) uncovered with cause {}: {}",
                named.len(),
                cause.as_str(),
                named.join("; ")
            );
            return Err(match cause {
                ReindexCause::NotFound => Error::NotFound(said),
                ReindexCause::Io => Error::Io(said),
                ReindexCause::Integrity => Error::Integrity(said),
            });
        }
        if self.collisions > 0 {
            return Err(Error::Conflict(format!(
                "reindex recorded {} copy(ies) of an id history spent more than once: the \
                 evidence is kept and nothing was chosen between the copies",
                self.collisions
            )));
        }
        Ok(())
    }

    /// The report, as `rahi ledger reindex` prints it.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        for segment in &self.segments {
            let verdict = if segment.covered {
                format!("covered ({} row(s) written)", segment.rows_written)
            } else {
                format!(
                    "uncovered [{}] {}",
                    segment.cause.map_or("unaccounted", ReindexCause::as_str),
                    segment.detail
                )
            };
            out.push_str(&format!(
                "ledger reindex: {} {verdict}\n",
                segment.segment_hash
            ));
        }
        out.push_str(&format!(
            "ledger reindex: {} segment(s) walked, {} identity row(s) written, {} collision(s) \
             recorded, {} segment(s) left uncovered",
            self.walked,
            self.rows_written,
            self.collisions,
            self.segments.iter().filter(|s| !s.covered).count()
        ));
        out
    }
}

/// What this node has observed about coverage, between leader reads (B-11).
///
/// A cached `complete` means "this node has seen no evidence of
/// incompleteness", never "the chain is proven complete now". The stronger
/// reading holds only where coverage is computed through the leader: at
/// `open`, at [`Ledger::recheck_coverage`], and in `ledger verify` and
/// `preflight` (B-13).
///
/// "Seen no evidence" is a claim about every observation this node makes,
/// not only about the two writes that move it: [`Ledger::coverage`]
/// degrades this verdict whenever it computes an incomplete one, so
/// evidence computed on this handle can never sit beside a cached
/// `complete` (D-14). Only [`Ledger::recheck_coverage`] restores it.
#[derive(Clone, Debug)]
pub(crate) struct CachedCoverage {
    pub(crate) complete: bool,
    /// What made it incomplete, in words a refusal can name.
    pub(crate) evidence: String,
    /// The segments the last leader read found uncovered.
    pub(crate) uncovered: Vec<Hash>,
}

impl Default for CachedCoverage {
    /// A handle that has not read coverage yet assumes nothing: it is
    /// incomplete until a leader read says otherwise, so a verdict is never
    /// a default.
    fn default() -> Self {
        Self {
            complete: false,
            evidence: "coverage has not been established on this handle".to_owned(),
            uncovered: Vec::new(),
        }
    }
}

impl CachedCoverage {
    pub(crate) fn of(coverage: &Coverage) -> Self {
        Self {
            complete: coverage.is_complete(),
            evidence: if coverage.is_complete() {
                String::new()
            } else {
                coverage.why()
            },
            uncovered: coverage.uncovered().to_vec(),
        }
    }
}

/// The identity digest of `decision` (B-2).
///
/// sha256 over the canonical bytes of the decision with `prev_hash` omitted.
///
/// # Errors
///
/// [`Error::Integrity`] when the decision does not serialize.
pub fn identity_digest(decision: &Decision) -> Result<Hash, Error> {
    let bytes = decision.identity_bytes()?;
    Hash::parse(attest_ledger_core::sha256_hex(&bytes))
}

/// One record's three identity facts, as every accounting statement takes
/// them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Accounted {
    pub(crate) id: DecisionId,
    pub(crate) record_hash: Hash,
    pub(crate) identity_digest: Hash,
}

impl Accounted {
    /// What one signed record is, for the accounting.
    ///
    /// # Errors
    ///
    /// [`Error::Integrity`] when the record does not carry a decision or its
    /// hash does not parse.
    pub(crate) fn of(record: &SignedRecord) -> Result<Self, Error> {
        Ok(Self {
            id: DecisionId::new(record.record.id.clone()),
            record_hash: record.hash()?,
            identity_digest: identity_digest(&record.decision()?)?,
        })
    }
}

/// The statement an append commits beside the record (B-3).
pub(crate) fn identity_insert(accounted: &Accounted) -> Statement {
    Statement::with_params(
        IDENTITY_INSERT_SQL,
        vec![
            Value::from(accounted.id.as_str()),
            Value::from(accounted.identity_digest.as_str()),
            Value::from(accounted.record_hash.as_str()),
        ],
    )
}

/// The backfill's insert-or-ignore (FR-008).
fn identity_backfill(accounted: &Accounted) -> Statement {
    Statement::with_params(
        IDENTITY_BACKFILL_SQL,
        vec![
            Value::from(accounted.id.as_str()),
            Value::from(accounted.identity_digest.as_str()),
            Value::from(accounted.record_hash.as_str()),
        ],
    )
}

/// How many statements [`accounting_statements`] emits per record.
pub(crate) const STATEMENTS_PER_RECORD: usize = 3;

/// The accounting for every record of a segment, and the recomputation of
/// its counter (B-5).
///
/// Exactly the statements B-5 prescribes, in the order it prescribes them,
/// preceded per record by the stamp-only update D-9 adds: a seal and a
/// reindex therefore run one accounting rather than two that have to be kept
/// agreeing, and reach the same counter for the same rows because they run
/// the same recomputation.
pub(crate) fn accounting_statements(rows: &[Accounted], segment_hash: &Hash) -> Vec<Statement> {
    let mut out = Vec::with_capacity(rows.len() * STATEMENTS_PER_RECORD + 1);
    for row in rows {
        let id = Value::from(row.id.as_str());
        let digest = Value::from(row.identity_digest.as_str());
        let record_hash = Value::from(row.record_hash.as_str());
        let segment = Value::from(segment_hash.as_str());
        out.push(Statement::with_params(
            IDENTITY_STAMP_SQL,
            vec![segment.clone(), id.clone(), record_hash.clone()],
        ));
        out.push(Statement::with_params(
            IDENTITY_UPSERT_SQL,
            vec![
                id.clone(),
                digest.clone(),
                record_hash.clone(),
                segment.clone(),
            ],
        ));
        out.push(Statement::with_params(
            COLLISION_INSERT_SQL,
            vec![
                id.clone(),
                record_hash.clone(),
                segment.clone(),
                digest,
                id,
                record_hash,
                segment,
            ],
        ));
    }
    out.push(Statement::with_params(
        COVERAGE_RECOMPUTE_SQL,
        vec![
            Value::from(segment_hash.as_str()),
            Value::from(segment_hash.as_str()),
            Value::from(segment_hash.as_str()),
        ],
    ));
    out
}

/// What the accounting statements reported, read off the transaction's own
/// results (B-13).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AccountingOutcome {
    /// Identity rows this transaction had to **create** because no row
    /// existed: the detector for a writer that does not stamp.
    pub(crate) created: Vec<DecisionId>,
    /// Identity rows it stamped, which is the ordinary case.
    pub(crate) stamped: u64,
    /// Copies of a twice-spent id it recorded.
    pub(crate) collisions: u64,
}

impl AccountingOutcome {
    /// Read the outcome out of the results of [`accounting_statements`].
    ///
    /// `offset` is where the accounting starts in the batch, because a seal
    /// commits the segment header and the deletion ahead of it.
    pub(crate) fn read(
        results: &[rahi_store::ExecuteResult],
        rows: &[Accounted],
        offset: usize,
    ) -> Self {
        let mut out = Self::default();
        for (index, row) in rows.iter().enumerate() {
            let base = offset + index * STATEMENTS_PER_RECORD;
            let stamped = results.get(base).is_some_and(|r| r.rows_affected == 1);
            let created = results.get(base + 1).is_some_and(|r| r.rows_affected == 1);
            let collided = results.get(base + 2).is_some_and(|r| r.rows_affected == 1);
            if stamped {
                out.stamped += 1;
            }
            if created {
                out.created.push(row.id.clone());
            }
            if collided {
                out.collisions += 1;
            }
        }
        out
    }
}

/// The post-commit detection both a seal and a reindex make (B-5, B-8).
///
/// Not a rollback: `txn` commits the batch before any result is inspected,
/// so what this reports is that the segment is not fully accounted for,
/// which is exactly the uncovered state B-7 reports and B-8 repairs. The
/// operator is handed a repairable condition rather than a half-written one,
/// and this spec claims no rollback it cannot perform.
///
/// # Errors
///
/// [`Error::Integrity`] naming the segment, what it accounted for, and what
/// its header claims.
pub(crate) fn accounted_fully(what: &str, stamped: i64, count: u32) -> Result<(), Error> {
    if stamped == i64::from(count) {
        return Ok(());
    }
    Err(Error::Integrity(format!(
        "segment {what} accounted for {stamped} of the {count} record(s) its header names: the \
         segment is not fully accounted for, and `{REINDEX_COMMAND}` over this cell's archive is \
         what closes it"
    )))
}

impl Ledger {
    /// Create the identity, collision and coverage tables, idempotently
    /// (B-1, D-6).
    ///
    /// The chassis's own baseline rather than an application migration, for
    /// the reason spec 013 D-4 gives for `kernel_decisions`: the shape is
    /// fixed here, no app versions it, and a boot that found no table could
    /// not tell an empty chain from a deleted one. D-6 records exactly what
    /// that choice does and does not fence, which is: nothing, in either
    /// direction, from any store version check.
    pub(crate) async fn create_identity_schema(&self) -> Result<(), Error> {
        for sql in IDENTITY_SCHEMA_SQL {
            self.store().txn(vec![Statement::new(sql)]).await?;
        }
        Ok(())
    }

    /// Give every resident record an identity row (FR-008).
    ///
    /// A store restored from a snapshot taken before this spec has records
    /// and no rows for them. The walk is bounded by the hot window because
    /// it reads only what is resident, it is idempotent across reopens, and
    /// every insert is insert-or-ignore so that a concurrent append on
    /// another replica and this backfill converge rather than fight.
    ///
    /// Nothing here can reconstruct a row for an **archived** record: the
    /// evidence is in the archive, which a boot path does not hold. That is
    /// B-10's refusal and B-8's repair, not this.
    ///
    /// # Errors
    ///
    /// [`Error::Integrity`] when the read did not answer or a stored row is
    /// not a record this crate wrote; the store's own error otherwise.
    pub(crate) async fn backfill_identity(&self) -> Result<u64, Error> {
        let rows: Vec<BackfillRow> = self
            .store()
            .query_consistent(BACKFILL_CENSUS_SQL, vec![])
            .await?;
        let mut witnessed = None;
        let mut carried = Vec::new();
        for row in rows {
            if row.kind == 0 {
                witnessed = Some(row.total);
            } else {
                carried.push(row.record);
            }
        }
        // The rows carried here are the *subset* of resident records that no
        // row accounts for, so the witness is an upper bound rather than an
        // equality: what it proves is that the read answered, and that the
        // subset it carried is not larger than the relation it came from.
        // A read that under-reports leaves rows for the next open, which is
        // safe because this walk is idempotent; a read that did not answer
        // at all is not, because silence here would read as "every resident
        // record already has a row".
        if let Some(total) = witnessed {
            let carried_count = u64::try_from(carried.len()).unwrap_or(u64::MAX);
            let total = u64::try_from(total).map_err(|_| {
                Error::Integrity(format!(
                    "kernel_decisions witnessed a negative count of {total}"
                ))
            })?;
            if carried_count > total {
                return Err(Error::Integrity(format!(
                    "the backfill read witnessed {total} resident record(s) in its own snapshot \
                     and carried {carried_count} unaccounted one(s): the answer does not \
                     account for itself, so nothing may be concluded from it"
                )));
            }
        }
        if witnessed.is_none() {
            return Err(Error::Integrity(
                "the backfill read carried no witness row, so it did not answer: an empty result \
                 is not evidence that every resident record already has an identity row"
                    .to_owned(),
            ));
        }
        if carried.is_empty() {
            return Ok(0);
        }
        let mut statements = Vec::with_capacity(carried.len());
        for bytes in &carried {
            let record = SignedRecord::from_bytes(bytes)?;
            statements.push(identity_backfill(&Accounted::of(&record)?));
        }
        let written = statements.len();
        self.store().txn(statements).await?;
        Ok(u64::try_from(written).unwrap_or(u64::MAX))
    }

    /// The coverage verdict, computed through the leader from one snapshot
    /// (B-7).
    ///
    /// Read through the leader (`query_consistent`, spec 011 B-3) because it
    /// is the input to a refusal, and taken in one statement because the two
    /// halves of the answer are mutable evidence that a single seal moves
    /// between: separately complete reads of each half do not establish a
    /// coherent whole.
    ///
    /// # Errors
    ///
    /// [`Error::Integrity`] when either half did not account for its own
    /// rows or the statement answered a row kind it was not asked for; the
    /// store's own error when the leader cannot be reached.
    pub async fn coverage(&self) -> Result<Coverage, Error> {
        let rows: Vec<CoverageRow> = self
            .store()
            .query_consistent(COVERAGE_CENSUS_SQL, vec![])
            .await?;
        self.tally_coverage_read(rows.len());
        self.tally_census();
        let mut resident_witness = None;
        let mut sealed_witness = None;
        let mut residents = Vec::new();
        let mut segments = Vec::new();
        for row in rows {
            match row.kind {
                COVERAGE_RESIDENT_WITNESS => resident_witness = Some(row.total),
                COVERAGE_SEALED_WITNESS => sealed_witness = Some(row.total),
                COVERAGE_RESIDENT_ROW => residents.push((row.id, row.stamped)),
                COVERAGE_SEALED_ROW => segments.push((row.segment_hash, row.count, row.stamped)),
                other => {
                    return Err(Error::Integrity(format!(
                        "the coverage read carried a row of kind {other}, which this statement \
                         does not ask for, so nothing may be concluded from the answer"
                    )));
                }
            }
        }
        check_census("kernel_decisions", resident_witness, residents.len())?;
        check_census("kernel_segments", sealed_witness, segments.len())?;

        // And what `open` established, carried forward (spec 036 D-11):
        // records are only appended and segments only added, so an empty
        // answer from either half on a chain `open` found rows in is the
        // read failing rather than the chain shrinking. A coverage verdict
        // is the input to a refusal, and a verdict of "complete" concluded
        // from a half that did not answer is exactly the silently wrong
        // answer this spec exists to remove.
        let opened = self.opened();
        if segments.is_empty() && opened.sealed {
            return Err(Error::Integrity(
                "the segment headers read back empty, and this ledger verified segments when it \
                 opened: segments are only ever added, so this is the read failing rather than \
                 the archive emptying, and coverage cannot be concluded from it"
                    .to_owned(),
            ));
        }
        if residents.is_empty() && opened.resident {
            return Err(Error::Integrity(
                "the resident chain reads back empty, and this ledger verified records in it \
                 when it opened: records are only ever appended, so this is the read failing \
                 rather than the chain emptying, and coverage cannot be concluded from it"
                    .to_owned(),
            ));
        }

        let unstamped_resident = residents
            .iter()
            .filter(|(_, accounted)| *accounted == 0)
            .count();
        let mut uncovered = Vec::new();
        for (segment_hash, count, stamped) in segments {
            if stamped != count {
                uncovered.push(Hash::parse(segment_hash)?);
            }
        }
        let coverage = Coverage {
            uncovered,
            unstamped_resident: u64::try_from(unstamped_resident).unwrap_or(u64::MAX),
        };
        // B-11's second observation, made where the evidence is: a resident
        // record met without a row, or a segment whose counter is below its
        // header's count, is this node observing incompleteness itself. The
        // verdict this handle carries has to move on it, because a verdict
        // that stayed `complete` while this same call computed the evidence
        // for the opposite would let `lookup` answer `Absent` and the
        // backstop admit an unknown id over history this chain has just
        // been told it cannot vouch for. Degrade only: a complete answer is
        // adopted by `recheck_coverage` and never here.
        self.degrade_verdict(&coverage);
        Ok(coverage)
    }

    /// Recompute the verdict through the leader and adopt it (B-7, B-11).
    ///
    /// The only way a cached verdict moves back to complete. `serve` never
    /// calls it, so a degraded cell never talks itself back into confidence;
    /// the reindex verb does, because it has just repaired what made the
    /// verdict incomplete.
    ///
    /// # Errors
    ///
    /// As [`Ledger::coverage`].
    pub async fn recheck_coverage(&self) -> Result<Coverage, Error> {
        let coverage = self.coverage().await?;
        self.adopt_verdict(&coverage);
        Ok(coverage)
    }

    /// The two lifetime aggregates (B-7).
    ///
    /// This pays an aggregate proportional to the chain, which is why it is
    /// not part of the verdict and why nothing on the boot path or the
    /// append path calls it: only `rahi ledger verify` and `rahi preflight`
    /// do, where an operator asked for the number.
    ///
    /// # Errors
    ///
    /// [`Error::Integrity`] when the read did not answer; the store's own
    /// error when the leader cannot be reached.
    pub async fn identity_totals(&self) -> Result<IdentityTotals, Error> {
        let rows: Vec<TotalsRow> = self
            .store()
            .query_consistent(IDENTITY_TOTALS_SQL, vec![])
            .await?;
        self.tally_coverage_read(rows.len());
        let row = rows.first().ok_or_else(|| {
            Error::Integrity(
                "the identity totals read answered no rows: a statement with no FROM always \
                 yields one, so the read did not answer and nothing may be concluded from it"
                    .to_owned(),
            )
        })?;
        Ok(IdentityTotals {
            identity_rows: u64::try_from(row.identity_rows).unwrap_or(0),
            collisions: u64::try_from(row.collisions).unwrap_or(0),
        })
    }

    /// What the chain can prove about `id` (B-6).
    ///
    /// One primary-key read against the identity table and that id's
    /// collision rows, taken in one statement so a copy written between two
    /// reads cannot be missed by both. The coverage verdict this handle
    /// carries is what separates [`Presence::Absent`] from
    /// [`Presence::Unproven`]: absence is only ever answered over a chain
    /// whose coverage is complete, and B-13 states exactly how far a cached
    /// `complete` reaches.
    ///
    /// # Errors
    ///
    /// [`Error::Integrity`] when the read did not answer or a stored hash
    /// does not parse; the store's own error when the leader cannot be
    /// reached.
    pub async fn lookup(&self, id: &DecisionId) -> Result<Presence, Error> {
        let (identity, collisions) = self.identity_rows_of(id).await?;
        if !collisions.is_empty() {
            let mut copies = Vec::with_capacity(collisions.len() + 1);
            if let Some(row) = identity.clone() {
                copies.push(row);
            }
            copies.extend(collisions);
            return Ok(Presence::Ambiguous { copies });
        }
        let Some(row) = identity else {
            let verdict = self.cached_verdict();
            if verdict.complete {
                return Ok(Presence::Absent);
            }
            let uncovered = self.uncovered_hint();
            return Ok(Presence::Unproven {
                segment_hash: uncovered,
            });
        };
        Ok(match row.segment_hash {
            Some(segment_hash) => Presence::Sealed {
                record_hash: row.record_hash,
                segment_hash,
            },
            None => Presence::Resident {
                record_hash: row.record_hash,
            },
        })
    }

    /// The decision `id` names, fetching the one archived body that holds it
    /// (B-6).
    ///
    /// Exactly one body is fetched, the one the identity row names, and it
    /// is verified the way spec 014 B-4 verifies a body at full depth before
    /// a record is taken out of it.
    ///
    /// The identity row is read first and the record is read second, and a
    /// seal may commit between them: a record that was resident when the row
    /// was read is archived by the time the resident chain is. That
    /// crossing is legitimate history moving, not damage, and D-15 records
    /// why it is settled by asking the row once more rather than by a lock.
    /// [`Ledger::recover_resident`] carries the bound.
    ///
    /// # Errors
    ///
    /// [`Error::NotFound`] when the id is absent or the body is gone;
    /// [`Error::Integrity`] when the body does not verify or does not hold
    /// the record it was fetched for; [`Error::Io`] when the body cannot be
    /// read; [`Error::Conflict`] when the id is ambiguous; [`Error::Stale`]
    /// when coverage cannot vouch for the answer.
    pub async fn recover(
        &self,
        id: &DecisionId,
        archive: &dyn Archive,
    ) -> Result<SignedRecord, Error> {
        let presence = self.lookup(id).await?;
        // Spec 042 D-15's seam: the one instant this read is vulnerable to a
        // seal, and the only place a regression can put one there without a
        // clock. `None` on every ledger this crate builds (spec 036 D-15).
        self.read_interleave().await;
        match presence {
            Presence::Absent => Err(Error::NotFound(format!(
                "decision {id} is not in the chain"
            ))),
            Presence::Unproven { .. } => Err(Error::Stale(format!(
                "decision {id} cannot be looked up: {}",
                self.cached_verdict().evidence
            ))),
            Presence::Ambiguous { copies } => Err(ambiguous_copies(id, &copies)),
            Presence::Resident { record_hash } => {
                self.recover_resident(id, &record_hash, archive).await
            }
            Presence::Sealed {
                record_hash,
                segment_hash,
            } => {
                self.recover_sealed(id, &record_hash, &segment_hash, archive)
                    .await
            }
        }
    }

    /// The record an identity row named as resident (B-6).
    ///
    /// The resident chain answers first. When it does not hold the record,
    /// exactly one thing legitimately explains it: a seal committed between
    /// the row read and this one, which archives the record and stamps the
    /// same row with the segment it went into (B-5). Sealing is the only
    /// mover here, because an identity row is never deleted and never
    /// redirected (B-12), so the row itself is the coherent evidence, and
    /// asking it once more is what tells a moved record from a missing one.
    ///
    /// Bounded by construction, and D-15 states the bound: exactly one
    /// revalidation, which never re-enters this branch. A row that still
    /// says resident is the genuine integrity failure and is reported as
    /// one; a row that now says sealed under the *same* record hash is the
    /// crossing, and the archived body is fetched and verified exactly as it
    /// would have been had the lookup happened a moment later; anything else
    /// is history the row cannot account for and is an error, never an
    /// absence. Nothing retries an integrity failure and nothing here takes
    /// a lock: a lock would be process-local and this chain has replicas.
    async fn recover_resident(
        &self,
        id: &DecisionId,
        record_hash: &Hash,
        archive: &dyn Archive,
    ) -> Result<SignedRecord, Error> {
        for record in self.records().await? {
            if &record.hash()? == record_hash {
                return Ok(record);
            }
        }
        let missing = || {
            Error::Integrity(format!(
                "the identity row for decision {id} names resident record {record_hash}, \
                 which is not in the resident chain"
            ))
        };
        match self.lookup(id).await? {
            Presence::Sealed {
                record_hash: again,
                segment_hash,
            } if &again == record_hash => {
                self.recover_sealed(id, record_hash, &segment_hash, archive)
                    .await
            }
            Presence::Ambiguous { copies } => Err(ambiguous_copies(id, &copies)),
            _ => Err(missing()),
        }
    }

    /// The record an identity row named as archived, from the one body that
    /// holds it (B-6).
    async fn recover_sealed(
        &self,
        id: &DecisionId,
        record_hash: &Hash,
        segment_hash: &Hash,
        archive: &dyn Archive,
    ) -> Result<SignedRecord, Error> {
        let headers = self.segments().await?;
        let at = headers
            .iter()
            .position(|h| &h.segment_hash == segment_hash)
            .ok_or_else(|| {
                Error::Integrity(format!(
                    "the identity row for decision {id} names segment {segment_hash}, \
                     which the store does not hold a header for"
                ))
            })?;
        let header = headers
            .get(at)
            .ok_or_else(|| Error::Integrity("the header list moved under this read".to_owned()))?;
        let segment = fetch_segment(archive, header).await?;
        // Verified the way spec 014 B-4 verifies a body at full
        // depth before a record is taken out of it: the parent is
        // the genesis parent for the oldest segment and the previous
        // segment's terminal record hash for every other (B-6).
        let parent = match at.checked_sub(1).and_then(|before| headers.get(before)) {
            Some(before) => before.last_hash.clone(),
            None => self.genesis_parent().clone(),
        };
        crate::verify::verify_segment(&parent, &segment, &self.verifier())?;
        for record in segment.records {
            if &record.hash()? == record_hash {
                return Ok(record);
            }
        }
        Err(Error::Integrity(format!(
            "archived segment {segment_hash} does not hold record {record_hash}, which \
             the identity row for decision {id} names"
        )))
    }

    /// Rebuild the accounting of every uncovered segment from the archive
    /// (B-8).
    ///
    /// Newest segment backwards. Each body is fetched and verified at full
    /// depth *before* anything is written from it, and only then does one
    /// transaction per segment run exactly the accounting a seal runs, so
    /// the walk is idempotent, resumable after an interruption, convergent
    /// under two reindexers on the same segment, and never overwrites,
    /// redirects or deletes a row another transaction wrote.
    ///
    /// Damaged or contradictory history fails visibly and the walk
    /// continues: a missing, unreadable or unverifiable body is never used
    /// as a source of identity rows, its segment stays uncovered under its
    /// cause, and the report says so. An id the archive spent twice is
    /// recorded as a collision and never resolved (B-12).
    ///
    /// # Errors
    ///
    /// The store's own error when a transaction fails, and
    /// [`Error::Integrity`] when a read does not account for itself. A
    /// damaged body is reported in the [`ReindexReport`] rather than
    /// returned, so a walk always reaches every segment it was asked for;
    /// [`ReindexReport::outcome`] is what turns the collected causes into
    /// the verb's exit code.
    pub async fn reindex(&self, archive: &dyn Archive) -> Result<ReindexReport, Error> {
        let coverage = self.coverage().await?;
        let uncovered: std::collections::BTreeSet<String> = coverage
            .uncovered()
            .iter()
            .map(|h| h.as_str().to_owned())
            .collect();
        let headers = self.segments().await?;

        // The parent each segment's first record links to: the genesis
        // parent, then each segment's terminal record hash (spec 014 B-4).
        let mut parents: BTreeMap<String, Hash> = BTreeMap::new();
        let mut parent = self.genesis_parent().clone();
        for header in &headers {
            parents.insert(header.segment_hash.as_str().to_owned(), parent.clone());
            parent = header.last_hash.clone();
        }

        let mut report = ReindexReport::default();
        for header in headers.iter().rev() {
            if !uncovered.contains(header.segment_hash.as_str()) {
                // Already accounted for: skipped without an archive fetch.
                continue;
            }
            report.walked += 1;
            let parent = parents
                .get(header.segment_hash.as_str())
                .cloned()
                .unwrap_or_else(|| self.genesis_parent().clone());
            let outcome = self.reindex_segment(archive, header, &parent).await?;
            report.rows_written += outcome.rows_written;
            report.collisions += outcome.collisions;
            report.segments.push(outcome);
        }
        Ok(report)
    }

    /// One segment's repair: fetch, verify, then account.
    async fn reindex_segment(
        &self,
        archive: &dyn Archive,
        header: &SegmentHeader,
        parent: &Hash,
    ) -> Result<SegmentReindex, Error> {
        let uncovered = |cause: ReindexCause, detail: String| SegmentReindex {
            segment_hash: header.segment_hash.clone(),
            covered: false,
            cause: Some(cause),
            detail,
            rows_written: 0,
            collisions: 0,
        };

        let segment = match fetch_segment(archive, header).await {
            Ok(segment) => segment,
            Err(err) => {
                return Ok(uncovered(ReindexCause::of(&err), err.message().to_owned()));
            }
        };
        if let Err(err) = crate::verify::verify_segment(parent, &segment, &self.verifier()) {
            return Ok(uncovered(ReindexCause::Integrity, err.message().to_owned()));
        }

        let rows = segment
            .records
            .iter()
            .map(Accounted::of)
            .collect::<Result<Vec<_>, Error>>()?;
        let statements = accounting_statements(&rows, &header.segment_hash);
        let results = self.store().txn(statements).await?;
        let outcome = AccountingOutcome::read(&results, &rows, 0);
        self.note_unstamped_writer(&outcome);

        let stamped = self.stamped_of(&header.segment_hash).await?;
        let covered = stamped == i64::from(header.count);
        Ok(SegmentReindex {
            segment_hash: header.segment_hash.clone(),
            covered,
            cause: if covered {
                None
            } else {
                Some(ReindexCause::Integrity)
            },
            detail: if covered {
                String::new()
            } else {
                format!(
                    "the segment accounts for {stamped} of its header's {} record(s)",
                    header.count
                )
            },
            rows_written: outcome.stamped + u64::try_from(outcome.created.len()).unwrap_or(0),
            collisions: outcome.collisions,
        })
    }

    /// One segment's counter, as the transaction that wrote it left it
    /// (B-1, B-5).
    ///
    /// `-1` when no counter row exists at all, which is a segment sealed by
    /// a binary that does not stamp or one a reindex has not reached: a
    /// distinct answer from `0`, which is a segment accounted for by nothing
    /// despite having been reindexed.
    ///
    /// # Errors
    ///
    /// The store's own error when the leader cannot be reached.
    pub async fn stamped_of(&self, segment_hash: &Hash) -> Result<i64, Error> {
        let rows: Vec<StampedRow> = self
            .store()
            .query_consistent(
                COVERAGE_OF_SEGMENT_SQL,
                vec![Value::from(segment_hash.as_str())],
            )
            .await?;
        self.tally_coverage_read(rows.len());
        Ok(rows.first().map_or(-1, |row| row.stamped))
    }

    /// An identity row and every collision row under one id, from one
    /// snapshot.
    pub(crate) async fn identity_rows_of(
        &self,
        id: &DecisionId,
    ) -> Result<(Option<DecisionCopy>, Vec<DecisionCopy>), Error> {
        let rows: Vec<IdRow> = self
            .store()
            .query_consistent(
                IDENTITY_OF_ID_SQL,
                vec![Value::from(id.as_str()), Value::from(id.as_str())],
            )
            .await?;
        self.tally_coverage_read(rows.len());
        let mut answered = false;
        let mut identity = None;
        let mut collisions = Vec::new();
        for row in rows {
            match row.kind {
                ID_ROW_WITNESS => answered = true,
                ID_ROW_IDENTITY => identity = Some(copy_of(&row)?),
                ID_ROW_COLLISION => collisions.push(copy_of(&row)?),
                other => {
                    return Err(Error::Integrity(format!(
                        "the identity read for decision {id} carried a row of kind {other}, \
                         which this statement does not ask for, so nothing may be concluded \
                         from the answer"
                    )));
                }
            }
        }
        if !answered {
            return Err(Error::Integrity(format!(
                "the identity read for decision {id} carried no witness row, so it did not \
                 answer: an empty result is not evidence that the id is free, and an id that \
                 cannot be proven free is not appended"
            )));
        }
        Ok((identity, collisions))
    }

    /// The one uncovered segment a decision would have to be in, when
    /// exactly one is uncovered.
    fn uncovered_hint(&self) -> Option<Hash> {
        let verdict = self.cached_uncovered();
        match verdict.as_slice() {
            [only] => Some(only.clone()),
            _ => None,
        }
    }
}

/// The refusal `recover` gives a twice-spent id: every copy named, none
/// chosen (B-12).
fn ambiguous_copies(id: &DecisionId, copies: &[DecisionCopy]) -> Error {
    Error::Conflict(format!(
        "decision {id} names {} recorded copies and nothing here chooses between them: {}",
        copies.len(),
        copies
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ")
    ))
}

fn copy_of(row: &IdRow) -> Result<DecisionCopy, Error> {
    Ok(DecisionCopy {
        record_hash: Hash::parse(row.record_hash.clone())?,
        segment_hash: if row.segment_hash.is_empty() {
            None
        } else {
            Some(Hash::parse(row.segment_hash.clone())?)
        },
        identity_digest: Hash::parse(row.identity_digest.clone())?,
    })
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use rahi_types::{Revision, Sub};

    use crate::record::{DecisionKind, Outcome};

    fn decision(id: &str) -> Decision {
        Decision::new(
            DecisionId::new(id),
            DecisionKind::new("db.write"),
            Sub::new("subject"),
            Outcome::Allow,
            "granted",
        )
        .with_payload(serde_json::json!({ "table": "notes" }))
        .at(Revision::new(4))
    }

    /// B-2: the parent is the one field an honest retry is expected to
    /// change, so the digest does not cover it.
    #[test]
    fn the_digest_ignores_the_parent_and_nothing_else() {
        let mut a = decision("d-1");
        let mut b = decision("d-1");
        a.prev_hash = Hash::parse(format!("sha256:{}", "11".repeat(32))).expect("a hash");
        b.prev_hash = Hash::parse(format!("sha256:{}", "22".repeat(32))).expect("a hash");
        assert_eq!(
            identity_digest(&a).expect("a digest"),
            identity_digest(&b).expect("a digest"),
            "a retry re-chained onto another head is the same decision"
        );

        b.reason = "granted under another reason".to_owned();
        assert_ne!(
            identity_digest(&a).expect("a digest"),
            identity_digest(&b).expect("a digest"),
            "every other field is covered"
        );

        let mut c = decision("d-1");
        c.at = Revision::new(5);
        assert_ne!(
            identity_digest(&a).expect("a digest"),
            identity_digest(&c).expect("a digest"),
            "`at` is covered"
        );
    }

    /// B-5: three statements per record, then one recomputation, in that
    /// order, and the same for a seal and for a reindex.
    #[test]
    fn the_accounting_is_three_statements_per_record_and_one_recomputation() {
        let segment = Hash::parse(format!("sha256:{}", "ab".repeat(32))).expect("a hash");
        let rows = vec![
            Accounted {
                id: DecisionId::new("d-1"),
                record_hash: Hash::parse(format!("sha256:{}", "01".repeat(32))).expect("a hash"),
                identity_digest: Hash::parse(format!("sha256:{}", "02".repeat(32)))
                    .expect("a hash"),
            },
            Accounted {
                id: DecisionId::new("d-2"),
                record_hash: Hash::parse(format!("sha256:{}", "03".repeat(32))).expect("a hash"),
                identity_digest: Hash::parse(format!("sha256:{}", "04".repeat(32)))
                    .expect("a hash"),
            },
        ];
        let statements = accounting_statements(&rows, &segment);
        assert_eq!(statements.len(), rows.len() * STATEMENTS_PER_RECORD + 1);
        let sql = |at: usize| {
            statements
                .get(at)
                .map(|s: &Statement| s.sql.clone())
                .unwrap_or_default()
        };
        assert!(sql(0).starts_with("UPDATE kernel_decision_identity"));
        assert!(sql(1).contains("ON CONFLICT(id) DO UPDATE"));
        assert!(sql(2).contains("kernel_decision_collisions"));
        assert!(
            statements
                .last()
                .expect("a recomputation")
                .sql
                .contains("kernel_decision_coverage"),
            "the counter is recomputed last, from the two tables above it"
        );
        for statement in &statements {
            assert!(
                !statement.sql.contains("DELETE"),
                "no path in this crate deletes an identity or a collision row"
            );
        }
    }

    /// B-13: the upsert can only create once the stamp-only update has run,
    /// so its `rows_affected` is the creation counter.
    #[test]
    fn the_transaction_results_separate_a_creation_from_a_stamp() {
        use rahi_store::ExecuteResult;
        let rows = vec![Accounted {
            id: DecisionId::new("d-1"),
            record_hash: Hash::parse(format!("sha256:{}", "01".repeat(32))).expect("a hash"),
            identity_digest: Hash::parse(format!("sha256:{}", "02".repeat(32))).expect("a hash"),
        }];
        let stamped = [
            ExecuteResult { rows_affected: 1 },
            ExecuteResult { rows_affected: 0 },
            ExecuteResult { rows_affected: 0 },
        ];
        let outcome = AccountingOutcome::read(&stamped, &rows, 0);
        assert_eq!(outcome.stamped, 1);
        assert!(outcome.created.is_empty());

        let created = [
            ExecuteResult { rows_affected: 0 },
            ExecuteResult { rows_affected: 1 },
            ExecuteResult { rows_affected: 0 },
        ];
        let outcome = AccountingOutcome::read(&created, &rows, 0);
        assert_eq!(outcome.created, vec![DecisionId::new("d-1")]);
        assert_eq!(outcome.stamped, 0);

        let collided = [
            ExecuteResult { rows_affected: 0 },
            ExecuteResult { rows_affected: 0 },
            ExecuteResult { rows_affected: 1 },
        ];
        assert_eq!(AccountingOutcome::read(&collided, &rows, 0).collisions, 1);
    }

    /// B-8, AC-6: a walk that recorded a collision never exits zero, and
    /// nothing it prints calls the record repaired.
    #[test]
    fn a_recorded_collision_is_never_reported_as_a_repair() {
        let segment = Hash::parse(format!("sha256:{}", "ab".repeat(32))).expect("a hash");
        let report = ReindexReport {
            segments: vec![SegmentReindex {
                segment_hash: segment,
                covered: true,
                cause: None,
                detail: String::new(),
                rows_written: 3,
                collisions: 1,
            }],
            walked: 1,
            rows_written: 3,
            collisions: 1,
        };
        let err = report
            .outcome()
            .expect_err("a collision is never a clean exit");
        assert!(matches!(err, Error::Conflict(_)), "{err:?}");
        let rendered = report.render();
        for forbidden in [
            "repaired",
            "resolved",
            "corrected",
            "deduplicated",
            "reconciled",
        ] {
            assert!(
                !rendered.contains(forbidden) && !err.message().contains(forbidden),
                "{forbidden:?} describes a collision as something it is not"
            );
        }
    }

    /// B-8: the most severe cause is the one the verb exits on.
    #[test]
    fn the_worst_cause_of_a_walk_is_the_one_it_exits_on() {
        let hash = |b: &str| Hash::parse(format!("sha256:{}", b.repeat(32))).expect("a hash");
        let segment = |name: &str, cause: ReindexCause| SegmentReindex {
            segment_hash: hash(name),
            covered: false,
            cause: Some(cause),
            detail: "damaged".to_owned(),
            rows_written: 0,
            collisions: 0,
        };
        let report = ReindexReport {
            segments: vec![
                segment("aa", ReindexCause::NotFound),
                segment("bb", ReindexCause::Integrity),
                segment("cc", ReindexCause::Io),
            ],
            walked: 3,
            ..ReindexReport::default()
        };
        let err = report
            .outcome()
            .expect_err("a damaged walk never exits zero");
        assert!(matches!(err, Error::Integrity(_)), "{err:?}");
        assert!(!report.all_covered());
        assert!(
            !report.render().contains("complete"),
            "no output describes a damaged chain as complete: {}",
            report.render()
        );
    }

    /// B-10: the refusal names what is missing and the one command that
    /// clears it.
    #[test]
    fn the_refusal_names_the_command_that_clears_it() {
        let coverage = Coverage {
            uncovered: vec![Hash::parse(format!("sha256:{}", "ab".repeat(32))).expect("a hash")],
            unstamped_resident: 2,
        };
        assert!(!coverage.is_complete());
        assert!(coverage.why().contains("rahi ledger reindex"));
        assert!(Coverage::default().is_complete());
        assert_eq!(coverage.uncovered().len(), 1);
        assert_eq!(coverage.unstamped_resident(), 2);
    }
}
