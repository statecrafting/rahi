//! The chain as it lives in the store: its schema, its head, and its boot
//! (spec 013 B-2, B-4).
//!
//! One table holds the whole resident chain. Its shape is the enforcement
//! mechanism, not a convenience: `prev_hash` carries a unique index, so the
//! store itself refuses a second record claiming a parent that is already
//! claimed, and [`crate::Ledger::append`] is a compare-and-swap without a
//! lock, a lease, or a serializing transaction.
//!
//! Every read here takes the leader round-trip ([`StoreHandle::query_consistent`]).
//! The head is the value an append does a compare-and-swap against and the
//! resident chain is what a boot decides to fail on: a stale replica would
//! turn either into a wrong answer (spec 011 B-3).
//!
//! What is resident is a window, not the history. Spec 014 seals the tail
//! into archived segments, and the one thing that changes here is the hash
//! the oldest resident record links to: [`Ledger::resident_root`], not the
//! genesis parent, once anything has been sealed.

use rahi_store::Statement;
use rahi_store::{StoreHandle, Value};
use rahi_types::{Error, LEDGER_SCHEMA_VERSION, Sub};
use serde::Deserialize;
use serde_json::json;

use crate::record::{Decision, DecisionId, DecisionKind, Hash, Outcome, SignedRecord};
use crate::seal::Depth;
use crate::segment::{
    SEGMENTS_ADD_MANIFEST_SQL, SEGMENTS_COLUMNS_SQL, SEGMENTS_INDEX_SQL, SEGMENTS_MANIFEST_COLUMN,
    SEGMENTS_TABLE_SQL,
};
use crate::signer::{LedgerSigner, LedgerVerifier};
use crate::verify::order_chain;

/// The decision table (spec 013 B-2).
///
/// `record` holds the canonical JSON of the whole signed record, so a row is
/// self-describing: `id`, `prev_hash`, and `hash` are indexed projections of
/// it and never a second source of truth.
pub const DECISIONS_TABLE_SQL: &str = "CREATE TABLE IF NOT EXISTS kernel_decisions (\
    id TEXT PRIMARY KEY, \
    prev_hash TEXT NOT NULL, \
    hash TEXT NOT NULL, \
    record BLOB NOT NULL)";

/// The unique parent index: the whole of the compare-and-swap.
///
/// One record per parent is what makes the chain linear, and a violation of
/// it is what tells a racing appender it lost.
pub const DECISIONS_INDEX_SQL: &str =
    "CREATE UNIQUE INDEX IF NOT EXISTS kernel_decisions_parent ON kernel_decisions (prev_hash)";

const HEAD_SQL: &str = "SELECT hash FROM kernel_decisions \
     WHERE hash NOT IN (SELECT prev_hash FROM kernel_decisions)";

/// The whole resident answer in one statement: the records, the count of
/// them, and the hash they are rooted at (spec 036 D-11, spec 042 D-17).
///
/// The first branch has no `FROM`, so SQLite yields it whatever the tables
/// hold: it is the witness that the read answered at all. Its `total` is a
/// scalar subquery evaluated inside this one statement, so it is the same
/// snapshot the rows come from, which a second `COUNT(*)` statement would
/// not be. A missing witness row or a row count below the witnessed total is
/// a read that did not answer or that dropped rows, and neither may be read
/// as "the chain holds nothing".
///
/// The third branch carries the unclaimed segments, which is what
/// [`Ledger::resident_root`] decides the root from, and the witness row
/// carries `segments`, the count that tells an archive with no last segment
/// from an archive with no segments at all. Taking them here rather than in
/// a second statement is the whole of spec 042 D-17: a seal commits the
/// archived rows and the resident deletions in one transaction, so a root
/// read after the records is a root from *after* a seal the records are from
/// *before*, and ordering the older records against the newer root reports
/// an integrity failure for a chain that is intact.
const RESIDENT_SNAPSHOT_SQL: &str = "SELECT 0 AS witness, \
     (SELECT COUNT(*) FROM kernel_decisions) AS total, \
     (SELECT COUNT(*) FROM kernel_segments) AS segments, \
     X'' AS record, '' AS segment_hash, '' AS last_hash \
     UNION ALL \
     SELECT 1 AS witness, 0 AS total, 0 AS segments, \
     record, '' AS segment_hash, '' AS last_hash FROM kernel_decisions \
     UNION ALL \
     SELECT 2 AS witness, 0 AS total, 0 AS segments, \
     X'' AS record, segment_hash, last_hash FROM kernel_segments \
     WHERE segment_hash NOT IN (SELECT prev_segment_hash FROM kernel_segments)";

const COUNT_SQL: &str = "SELECT COUNT(*) AS total FROM kernel_decisions";

const DUPLICATE_PARENT_SQL: &str = "SELECT prev_hash FROM kernel_decisions \
     GROUP BY prev_hash HAVING COUNT(*) > 1 LIMIT 1";

/// The parent the oldest resident record links to: the record no other
/// resident record is the parent of (spec 036 B-4).
const RECORD_ROOT_SQL: &str = "SELECT prev_hash FROM kernel_decisions \
     WHERE prev_hash NOT IN (SELECT hash FROM kernel_decisions)";

/// The predecessor the oldest sealed segment links to: the ledger's genesis
/// parent once anything has been sealed (spec 014 B-4, spec 036 B-4).
const SEGMENT_ROOT_SQL: &str = "SELECT prev_segment_hash FROM kernel_segments \
     WHERE prev_segment_hash NOT IN (SELECT segment_hash FROM kernel_segments)";

#[derive(Debug, Deserialize)]
struct HashRow {
    hash: String,
}

#[derive(Debug, Deserialize)]
struct ParentRow {
    #[serde(default)]
    prev_hash: String,
    #[serde(default)]
    prev_segment_hash: String,
}

#[derive(Debug, Deserialize)]
struct ColumnRow {
    name: String,
}

/// One row of [`RESIDENT_SNAPSHOT_SQL`]: the witness, a record, or an
/// unclaimed segment, told apart by `witness`.
#[derive(Debug, Deserialize)]
struct ResidentSnapshotRow {
    witness: i64,
    #[serde(default)]
    total: i64,
    #[serde(default)]
    segments: i64,
    #[serde(default)]
    record: Vec<u8>,
    #[serde(default)]
    segment_hash: String,
    #[serde(default)]
    last_hash: String,
}

#[derive(Debug, Deserialize)]
struct CountRow {
    total: i64,
}

/// A hook run inside the chain read, so a test can commit a seal at the one
/// instant the read is vulnerable to it (spec 036 D-15).
///
/// Test scaffolding, carried deliberately and named as such: `None` on every
/// ledger this crate builds, installed only by
/// [`Ledger::with_read_interleave`], and doing nothing at all until it is.
/// It is `#[doc(hidden)]` rather than feature-gated because the feature that
/// would hide it can only be turned on for this crate's own tests by a
/// dependency on itself, which `cargo package` cannot resolve.
#[doc(hidden)]
#[derive(Clone)]
pub struct ReadInterleave(
    std::sync::Arc<
        dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + Sync,
    >,
);

impl ReadInterleave {
    /// Install `hook` as the interleave point of the chain read.
    #[doc(hidden)]
    pub fn new<F, Fut>(hook: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        Self(std::sync::Arc::new(move || Box::pin(hook())))
    }

    pub(crate) async fn run(&self) {
        (self.0)().await;
    }
}

impl std::fmt::Debug for ReadInterleave {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ReadInterleave")
    }
}

/// A hook run inside the resident snapshot, after its one statement has
/// answered and before the answer is ordered (spec 042 D-17).
///
/// Test scaffolding, carried deliberately and named as such, in the shape
/// [`ReadInterleave`] established for spec 036 D-15. It is a second hook
/// rather than a second call of the first so a regression can drive the
/// crossing between the identity row and the chain read, and the crossing
/// inside the chain read, one at a time.
#[doc(hidden)]
#[derive(Clone)]
pub struct SnapshotInterleave(
    std::sync::Arc<
        dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + Sync,
    >,
);

impl SnapshotInterleave {
    /// Install `hook` as the interleave point of the resident snapshot.
    #[doc(hidden)]
    pub fn new<F, Fut>(hook: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        Self(std::sync::Arc::new(move || Box::pin(hook())))
    }

    pub(crate) async fn run(&self) {
        (self.0)().await;
    }
}

impl std::fmt::Debug for SnapshotInterleave {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SnapshotInterleave")
    }
}

/// Where an append can be interrupted, so a test can reproduce a race or a
/// lost acknowledgement deterministically (spec 042 FR-002, FR-021).
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppendStage {
    /// After the head has been read and the record built, before the
    /// transaction is sent: where another task's append and seal can be made
    /// to land.
    BeforeInsert,
    /// After the transaction returned, so a test can drop the
    /// acknowledgement of a commit that did happen.
    AfterCommit,
}

/// A hook run at an [`AppendStage`], returning an error to substitute for
/// the stage's own outcome.
///
/// Test scaffolding, carried deliberately and named as such, in the shape
/// [`ReadInterleave`] already established for spec 036 D-15: `None` on every
/// ledger this crate builds, and `#[doc(hidden)]` rather than feature-gated
/// because the feature that would hide it can only be turned on for this
/// crate's own tests by a dependency on itself, which `cargo package` cannot
/// resolve.
#[doc(hidden)]
#[derive(Clone)]
pub struct AppendSeam(std::sync::Arc<SeamHook>);

/// The boxed hook [`AppendSeam`] carries, named so the type stays readable.
type SeamHook = dyn Fn(AppendStage) -> std::pin::Pin<Box<SeamFuture>> + Send + Sync;

/// What one run of the hook resolves to: an error to substitute for the
/// stage's own outcome, or nothing.
type SeamFuture = dyn std::future::Future<Output = Option<Error>> + Send;

impl AppendSeam {
    /// Install `hook` at every append stage.
    #[doc(hidden)]
    pub fn new<F, Fut>(hook: F) -> Self
    where
        F: Fn(AppendStage) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Option<Error>> + Send + 'static,
    {
        Self(std::sync::Arc::new(move |stage| Box::pin(hook(stage))))
    }

    pub(crate) async fn run(&self, stage: AppendStage) -> Option<Error> {
        (self.0)(stage).await
    }
}

impl std::fmt::Debug for AppendSeam {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AppendSeam")
    }
}

/// The cell's decision ledger.
///
/// Cheap to clone: it holds the store handle, the ledger key, and the hash
/// the chain is rooted at. Spec 015 clones it into the appender task that
/// drains its denial channel.
#[derive(Clone, Debug)]
pub struct Ledger {
    store: StoreHandle,
    signer: LedgerSigner,
    genesis_parent: Hash,
    opened: OpenedChain,
    /// Spec 036 D-15's test seam: `None` everywhere but this crate's own
    /// regressions.
    interleave: Option<ReadInterleave>,
    /// Spec 042 D-17's test seam: `None` everywhere but this crate's own
    /// regressions.
    snapshot_interleave: Option<SnapshotInterleave>,
    /// Spec 042 B-15: a handle opened for repair, which never appends.
    repair: bool,
    /// Spec 042 B-11: what this node has observed about coverage since its
    /// last leader read. Shared across clones, because spec 015 clones this
    /// handle into the appender task that drains its denial channel and a
    /// verdict one clone moved has to be the verdict the other reads.
    verdict: std::sync::Arc<std::sync::Mutex<crate::identity::CachedCoverage>>,
    /// Spec 042 FR-002 and FR-021's test seam: `None` everywhere but this
    /// crate's own regressions.
    append_seam: Option<AppendSeam>,
    /// Spec 042 FR-018 and AC-10's instrument: how many leader reads this
    /// handle has issued, by the category the criteria are stated in.
    tally: std::sync::Arc<ReadTally>,
}

/// Leader reads this handle has issued, counted by the category spec 042
/// FR-018 and AC-10 are stated in.
///
/// Carried in the shipped code rather than in a test because the quantity
/// under test is a property of the call sites, and a test that counted
/// something else (total statements, say, or wall time) would assert
/// something the criteria do not say. Three counters and nothing more: the
/// head read spec 013 B-3 has always made, the reads this spec's coverage
/// answer issues, and the rows those reads carried.
#[derive(Debug, Default)]
pub(crate) struct ReadTally {
    /// The compare-and-swap's own head read (spec 013 B-3), unchanged by
    /// this spec in count and in kind.
    pub(crate) head: std::sync::atomic::AtomicU64,
    /// Reads against `kernel_decision_coverage`,
    /// `kernel_decision_identity`, `kernel_decision_collisions` and
    /// `kernel_segments` issued to answer coverage (FR-018).
    pub(crate) coverage: std::sync::atomic::AtomicU64,
    /// How many rows those reads carried (AC-10).
    pub(crate) coverage_rows: std::sync::atomic::AtomicU64,
    /// Full verdict reads (`Ledger::coverage`) alone, which is the quantity
    /// B-11 means when it says a verdict moved "without a leader read".
    pub(crate) census: std::sync::atomic::AtomicU64,
}

/// A snapshot of [`ReadTally`], as a test reads it.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReadCounts {
    /// Head reads.
    pub head: u64,
    /// Coverage-attributable leader reads.
    pub coverage: u64,
    /// Rows those reads carried.
    pub coverage_rows: u64,
    /// Full verdict reads.
    pub census: u64,
}

/// What [`Ledger::open`] established about the chain it verified (spec 036
/// D-11).
///
/// Both facts are monotone: records are only appended, segments are only
/// added, and a seal that empties the hot window commits its segment in the
/// same transaction that removes the rows. So neither can become false while
/// this handle lives, and a later read that contradicts one is the read
/// failing rather than the chain shrinking.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct OpenedChain {
    /// `open` saw at least one resident record.
    pub(crate) resident: bool,
    /// `open` saw at least one sealed segment.
    pub(crate) sealed: bool,
}

impl Ledger {
    /// Open the chain, writing genesis if it is absent, and verify it whole.
    ///
    /// `genesis_parent` is the booted manifest's hash (spec 015 B-8), and it
    /// is what a **fresh** chain's genesis record links to, so two cells
    /// starting from different manifests cannot produce chains that could be
    /// spliced together. On a chain that already exists it is not the
    /// anchor: spec 036 B-4 and D-5 re-anchor verification on the chain's
    /// own stored genesis record, read back by
    /// [`Ledger::stored_genesis_parent`], with the cell's ledger key's
    /// signatures as the proof (spec 013 B-5). A booted manifest that
    /// differs from the chain's *current* manifest is a missing deploy step
    /// rather than damage, and spec 036 B-4 makes `Kernel::boot` say so with
    /// [`Error::Stale`]; nothing about it is decided here.
    ///
    /// Verification runs over the whole resident chain and every sealed
    /// segment's header on every open ([`crate::Depth::Resident`], spec 014
    /// B-4) and fails closed. The caller is a boot path and constitution XI
    /// makes the consequence explicit: on [`Error::Integrity`] the process
    /// exits rather than serves. Verification is re-anchored here, never
    /// relaxed.
    ///
    /// # Errors
    ///
    /// [`Error::Integrity`] when the chain does not verify, when the unique
    /// parent index cannot be built over records already resident, when the
    /// stored chain has more than one root, or when a stored row is not a
    /// record this crate wrote. Store failures keep their own variants: a
    /// leader that cannot be reached is not tamper evidence.
    pub async fn open(
        store: StoreHandle,
        signer: LedgerSigner,
        genesis_parent: Hash,
    ) -> Result<Self, Error> {
        Self::open_inner(store, signer, genesis_parent, false).await
    }

    /// Open the chain for repair: no coverage gate, and no appends
    /// (spec 042 B-15).
    ///
    /// The repair cannot be gated on the state it repairs, so `ledger
    /// reindex` needs a handle that opens on an uncovered chain, and `ledger
    /// verify` and `ledger export` need one so that an operator working the
    /// incident keeps diagnosis and export. It is narrow so that it cannot
    /// become a way to serve an unproven chain: [`Ledger::append`] and
    /// [`Ledger::append_once`] on such a handle are [`Error::Conflict`]
    /// whatever coverage says, which makes the refusal a property of the
    /// handle rather than a check someone can forget, and `serve` has no
    /// flag, argument or environment variable that reaches it.
    ///
    /// The backfill of FR-008 still runs, and every read still reports
    /// coverage alongside its answer.
    ///
    /// # Errors
    ///
    /// As [`Ledger::open`], except that incomplete coverage is not one of
    /// them.
    pub async fn open_for_repair(
        store: StoreHandle,
        signer: LedgerSigner,
        genesis_parent: Hash,
    ) -> Result<Self, Error> {
        Self::open_inner(store, signer, genesis_parent, true).await
    }

    async fn open_inner(
        store: StoreHandle,
        signer: LedgerSigner,
        genesis_parent: Hash,
        repair: bool,
    ) -> Result<Self, Error> {
        let mut ledger = Self {
            store,
            signer,
            genesis_parent,
            opened: OpenedChain::default(),
            interleave: None,
            snapshot_interleave: None,
            repair,
            verdict: std::sync::Arc::new(std::sync::Mutex::new(
                crate::identity::CachedCoverage::default(),
            )),
            append_seam: None,
            tally: std::sync::Arc::new(ReadTally::default()),
        };
        ledger.create_schema().await?;

        // A chain whose whole head window has been sealed is not a fresh
        // chain: spec 014's segments are the rest of it, and re-genesising
        // over them would fork the ledger at its root. Both cases are one
        // question, asked of the chain rather than of the caller.
        match ledger.stored_genesis_parent().await? {
            Some(stored) => ledger.genesis_parent = stored,
            None => {
                ledger.refuse_a_second_genesis().await?;
                ledger.append_genesis(ledger.genesis_decision()).await?;
            }
        }
        // Spec 036 D-11: verification reads the whole resident chain and every
        // segment header, so what it saw is what this handle carries forward.
        // Both facts only ever become more true, and `current_manifest`
        // refuses to walk back past either of them.
        ledger.opened = ledger.verify_chain_witnessed(Depth::Resident).await?;

        // Spec 042 FR-008: a store restored from a snapshot taken before
        // this spec has records and no rows for them, and the rows are
        // recomputable from what is resident. This runs before the gate
        // because a chain with nothing sealed is closed by it entirely.
        ledger.backfill_identity().await?;

        // Spec 042 B-10, and D-8's precedence: verification has already run
        // and an integrity failure has already been returned, so a chain
        // that reaches here is intact and the only question left is whether
        // this binary can vouch for the uniqueness of the ids in it. That is
        // a missing upgrade step rather than damage, so it is
        // `Error::Stale` naming `rahi ledger reindex`, never
        // `Error::Integrity`, and an operator is never sent to reindex a
        // chain whose real problem is that it has been tampered with.
        let coverage = ledger.recheck_coverage().await?;
        if !coverage.is_complete() && !repair {
            return Err(Error::Stale(coverage.why()));
        }
        Ok(ledger)
    }

    /// Whether this handle was opened for repair (spec 042 B-15).
    #[must_use]
    pub fn is_repair(&self) -> bool {
        self.repair
    }

    /// The coverage verdict this node currently holds (spec 042 B-11).
    pub(crate) fn cached_verdict(&self) -> crate::identity::CachedCoverage {
        self.verdict
            .lock()
            .map_or_else(|poisoned| poisoned.into_inner().clone(), |v| v.clone())
    }

    /// The segments the last leader read found uncovered.
    pub(crate) fn cached_uncovered(&self) -> Vec<Hash> {
        self.cached_verdict().uncovered
    }

    /// Adopt a verdict computed through the leader (spec 042 B-7).
    pub(crate) fn adopt_verdict(&self, coverage: &crate::identity::Coverage) {
        let next = crate::identity::CachedCoverage::of(coverage);
        match self.verdict.lock() {
            Ok(mut held) => *held = next,
            Err(poisoned) => *poisoned.into_inner() = next,
        }
    }

    /// Move the verdict to incomplete on this node's own observation of an
    /// incomplete accounting, with no leader read of its own (spec 042
    /// B-11).
    ///
    /// Degrade only, and that asymmetry is the whole point: a negative
    /// observation is adopted here, a positive one is not. Restoring
    /// confidence is [`Ledger::adopt_verdict`] under
    /// [`Ledger::recheck_coverage`], which `serve` never calls, so a
    /// degraded cell cannot talk itself back into confidence by reading
    /// coverage again.
    ///
    /// The evidence this consumes was computed for another purpose, so the
    /// move itself costs nothing: nothing here reads the store.
    pub(crate) fn degrade_verdict(&self, coverage: &crate::identity::Coverage) {
        if coverage.is_complete() {
            return;
        }
        let next = crate::identity::CachedCoverage::of(coverage);
        match self.verdict.lock() {
            Ok(mut held) => *held = next,
            Err(poisoned) => *poisoned.into_inner() = next,
        }
    }

    /// Move the verdict to incomplete on this node's own observation, with
    /// no leader read (spec 042 B-11, B-13).
    ///
    /// A seal that had to **create** an identity row rather than stamp one
    /// is a record an old writer appended without stamping. The observation
    /// is late and eventual, never preventive: in the window between that
    /// append and this observation the cached verdict still said complete.
    /// What it buys is that the duplicate is discovered rather than silent.
    pub(crate) fn note_unstamped_writer(&self, outcome: &crate::identity::AccountingOutcome) {
        if outcome.created.is_empty() {
            return;
        }
        for id in &outcome.created {
            tracing::warn!(
                decision_id = %id,
                "sealing had to create the identity row for this decision: it was appended by a \
                 writer that does not stamp, so this chain's coverage is no longer proven"
            );
        }
        let evidence = format!(
            "sealing had to create {} identity row(s) for records appended without one, which is \
             a replica on a binary that does not stamp: run `{}` against this cell's archive \
             while nothing appends",
            outcome.created.len(),
            crate::identity::REINDEX_COMMAND
        );
        match self.verdict.lock() {
            Ok(mut held) => {
                held.complete = false;
                held.evidence = evidence;
            }
            Err(poisoned) => {
                let mut held = poisoned.into_inner();
                held.complete = false;
                held.evidence = evidence;
            }
        }
    }

    /// What [`Ledger::open`] established about this chain (spec 036 D-11).
    pub(crate) fn opened(&self) -> OpenedChain {
        self.opened
    }

    /// Install the test seam (spec 036 D-15).
    #[doc(hidden)]
    #[must_use]
    pub fn with_read_interleave(mut self, hook: ReadInterleave) -> Self {
        self.interleave = Some(hook);
        self
    }

    /// The leader reads this handle has issued, by category (spec 042
    /// FR-018, AC-10).
    #[doc(hidden)]
    #[must_use]
    pub fn read_counts(&self) -> ReadCounts {
        use std::sync::atomic::Ordering;
        ReadCounts {
            head: self.tally.head.load(Ordering::Relaxed),
            coverage: self.tally.coverage.load(Ordering::Relaxed),
            coverage_rows: self.tally.coverage_rows.load(Ordering::Relaxed),
            census: self.tally.census.load(Ordering::Relaxed),
        }
    }

    /// Count one full verdict read (spec 042 B-11).
    pub(crate) fn tally_census(&self) {
        self.tally
            .census
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Count one coverage-attributable leader read and the rows it carried.
    pub(crate) fn tally_coverage_read(&self, rows: usize) {
        use std::sync::atomic::Ordering;
        self.tally.coverage.fetch_add(1, Ordering::Relaxed);
        self.tally
            .coverage_rows
            .fetch_add(u64::try_from(rows).unwrap_or(0), Ordering::Relaxed);
    }

    /// Install the append test seam (spec 042 FR-002, FR-021).
    #[doc(hidden)]
    #[must_use]
    pub fn with_append_seam(mut self, seam: AppendSeam) -> Self {
        self.append_seam = Some(seam);
        self
    }

    /// Run the append seam at `stage`, if one is installed.
    pub(crate) async fn append_seam(&self, stage: AppendStage) -> Option<Error> {
        let seam = self.append_seam.clone()?;
        seam.run(stage).await
    }

    /// Run the installed seam, if any.
    pub(crate) async fn read_interleave(&self) {
        if let Some(hook) = self.interleave.clone() {
            hook.run().await;
        }
    }

    /// Install the resident-snapshot test seam (spec 042 D-17).
    #[doc(hidden)]
    #[must_use]
    pub fn with_snapshot_interleave(mut self, hook: SnapshotInterleave) -> Self {
        self.snapshot_interleave = Some(hook);
        self
    }

    /// Run the installed resident-snapshot seam, if any.
    pub(crate) async fn snapshot_interleave(&self) {
        if let Some(hook) = self.snapshot_interleave.clone() {
            hook.run().await;
        }
    }

    /// The genesis parent the stored chain itself names, or `None` when
    /// there is no chain yet (spec 036 B-4).
    ///
    /// The archive answers first when anything has been sealed, because the
    /// oldest segment links the genesis parent directly and the oldest
    /// resident record does not once history has moved out from under it.
    ///
    /// # Errors
    ///
    /// [`Error::Integrity`] when either list has more than one root, which
    /// is a fork at the root; the store's own error when the read fails.
    pub async fn stored_genesis_parent(&self) -> Result<Option<Hash>, Error> {
        let segments: Vec<ParentRow> = self
            .store
            .query_consistent(SEGMENT_ROOT_SQL, vec![])
            .await?;
        if let Some(root) = one_root(
            segments.into_iter().map(|row| row.prev_segment_hash),
            "archive",
        )? {
            return Ok(Some(root));
        }
        let records: Vec<ParentRow> = self.store.query_consistent(RECORD_ROOT_SQL, vec![]).await?;
        one_root(records.into_iter().map(|row| row.prev_hash), "chain")
    }

    /// Nothing is resident and nothing is sealed, so writing genesis is
    /// writing the chain's first record rather than a second one.
    ///
    /// [`Ledger::stored_genesis_parent`] answering `None` is not on its own
    /// enough to conclude that. A local read that fails while stepping its
    /// rows comes back as `Ok(vec![])` rather than an error (spec 016 D-2),
    /// so an absent root is either an empty chain or a probe that did not
    /// answer, and the two have opposite consequences: the second genesis
    /// this would write is a valid compare-and-swap onto the real head, so it
    /// lands, persists, and leaves a `ledger.genesis` record in the middle of
    /// an audit chain. Constitution XI settles which way to fail. This is the
    /// same cross-check [`Ledger::head`] makes before it calls an absent head
    /// a fresh chain.
    ///
    /// # Errors
    ///
    /// [`Error::Integrity`] when anything is resident or sealed while no root
    /// was found; the store's own error when a count cannot be read.
    async fn refuse_a_second_genesis(&self) -> Result<(), Error> {
        let resident = self.count().await?;
        let sealed = self.segment_count().await?;
        if resident == 0 && sealed == 0 {
            return Ok(());
        }
        Err(Error::Integrity(format!(
            "the chain has {resident} resident record(s) and {sealed} sealed segment(s) but no \
             root: either its links form a cycle or the read that looks for the root did not \
             answer, and writing a second genesis record over either would be the damage rather \
             than the repair"
        )))
    }

    /// The hash the chain's genesis record links to.
    ///
    /// On an open chain this is what the chain itself stores, whatever
    /// manifest this process booted (spec 036 B-4). The booted manifest is
    /// compared against [`Ledger::current_manifest`] instead, by the kernel.
    #[must_use]
    pub fn genesis_parent(&self) -> &Hash {
        &self.genesis_parent
    }

    /// The public half of the key this ledger signs with.
    #[must_use]
    pub fn verifier(&self) -> LedgerVerifier {
        self.signer.verifier()
    }

    pub(crate) fn signer(&self) -> &LedgerSigner {
        &self.signer
    }

    pub(crate) fn store(&self) -> &StoreHandle {
        &self.store
    }

    /// The hash the next append chains onto.
    ///
    /// The head is the one record no other record claims as a parent, or the
    /// resident root when nothing is resident yet: the genesis parent on a
    /// fresh chain, and the last sealed segment's terminal hash on one whose
    /// window has been archived (spec 014 B-4). Read through the leader: this
    /// is the value the compare-and-swap is against.
    ///
    /// # Errors
    ///
    /// [`Error::Integrity`] when more than one record has no successor (the
    /// chain forked) or when records are resident but none is a head (the
    /// links form a cycle); the store's own error when the leader cannot be
    /// reached.
    pub async fn head(&self) -> Result<Hash, Error> {
        self.tally
            .head
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let rows: Vec<HashRow> = self.store.query_consistent(HEAD_SQL, vec![]).await?;
        let mut heads = rows.into_iter();
        let Some(head) = heads.next() else {
            return if self.count().await? == 0 {
                self.resident_root().await
            } else {
                Err(Error::Integrity(
                    "the chain has records but no head: its links form a cycle".to_owned(),
                ))
            };
        };
        if let Some(rival) = heads.next() {
            return Err(Error::Integrity(format!(
                "the chain forked: both {} and {} are heads",
                head.hash, rival.hash
            )));
        }
        Hash::parse(head.hash)
    }

    /// Every resident record, in chain order.
    ///
    /// Sealed records are not here: they are in the archive, and
    /// [`Ledger::segments`] names them (spec 014 B-1).
    ///
    /// # Errors
    ///
    /// [`Error::Integrity`] when a stored row does not parse or when the
    /// records do not form one list rooted at [`Ledger::resident_root`]; the
    /// store's own error when the read fails.
    pub async fn records(&self) -> Result<Vec<SignedRecord>, Error> {
        let (records, root) = self.resident_snapshot().await?;
        order_chain(&root, records)
    }

    /// The resident records and the hash they are rooted at, read in one
    /// statement (spec 042 D-17).
    ///
    /// The pair is the unit of coherence. A seal is one transaction that
    /// deletes the sealed records and writes the segment that becomes the
    /// new root, so the two halves are only ever consistent with each other
    /// when they come from the same snapshot. Read in two statements they
    /// are not: a seal landing between them leaves records from before it
    /// ordered against a root from after it, which is an integrity failure
    /// reported over an intact chain.
    ///
    /// Completeness is witnessed exactly as spec 036 D-11 requires: the
    /// census row is the evidence the read answered at all, and a carried
    /// count that does not match it is refused rather than read as an empty
    /// chain.
    ///
    /// # Errors
    ///
    /// [`Error::Integrity`] when the read did not account for itself, when a
    /// stored row does not parse, or when the archive has forked at its head
    /// or lost its last segment; the store's own error when the read fails.
    pub(crate) async fn resident_snapshot(&self) -> Result<(Vec<SignedRecord>, Hash), Error> {
        let rows: Vec<ResidentSnapshotRow> = self
            .store
            .query_consistent(RESIDENT_SNAPSHOT_SQL, vec![])
            .await?;
        let mut witnessed = None;
        let mut segments = 0;
        let mut carried = Vec::new();
        let mut heads = Vec::new();
        for row in rows {
            match row.witness {
                0 => {
                    witnessed = Some(row.total);
                    segments = row.segments;
                }
                1 => carried.push(row.record),
                _ => heads.push((row.segment_hash, row.last_hash)),
            }
        }
        check_census("kernel_decisions", witnessed, carried.len())?;
        let records = carried
            .iter()
            .map(|bytes| SignedRecord::from_bytes(bytes))
            .collect::<Result<Vec<_>, Error>>()?;
        // Spec 042 D-17's seam: a seal committed here is a seal committed
        // after the snapshot this read answers from, and must change
        // nothing about the answer. Its own hook rather than the
        // [`ReadInterleave`] of spec 036 D-15, so a regression can drive
        // this crossing and that one independently. `None` on every ledger
        // this crate builds.
        self.snapshot_interleave().await;
        let root = crate::seal::root_of_segment_heads(&heads, segments, self.genesis_parent())?;
        Ok((records, root))
    }

    /// How many records are resident.
    ///
    /// # Errors
    ///
    /// The store's own error, or [`Error::Integrity`] when the count is
    /// negative.
    pub async fn count(&self) -> Result<u64, Error> {
        let rows: Vec<CountRow> = self.store.query_consistent(COUNT_SQL, vec![]).await?;
        let total = rows.first().map_or(0, |row| row.total);
        u64::try_from(total)
            .map_err(|_| Error::Integrity(format!("kernel_decisions counted {total} rows")))
    }

    /// The resident chain as JSON Lines, one record per line.
    ///
    /// This is the export the published `attest-ledger` verifier reads and
    /// what spec 030's `rahi ledger verify` writes (spec 013 B-6). Each line
    /// is the record's canonical bytes exactly as the row holds them.
    ///
    /// # Errors
    ///
    /// As [`Ledger::records`].
    pub async fn export_jsonl(&self) -> Result<String, Error> {
        let mut out = String::new();
        for record in self.records().await? {
            out.push_str(&record.to_canonical_json()?);
            out.push('\n');
        }
        Ok(out)
    }

    /// Verify the chain again, without reopening.
    ///
    /// The boot check: what is resident plus every sealed segment's header
    /// ([`crate::Depth::Resident`]). Reaching the archived bodies is
    /// [`Ledger::verify_chain`] at [`crate::Depth::Full`].
    ///
    /// # Errors
    ///
    /// [`Error::Integrity`] as [`Ledger::open`].
    pub async fn verify(&self) -> Result<(), Error> {
        self.verify_chain(Depth::Resident).await
    }

    /// The genesis record's decision: the chain's own statement of what it is.
    fn genesis_decision(&self) -> Decision {
        Decision::new(
            DecisionId::new(format!("genesis:{}", self.genesis_parent)),
            DecisionKind::new(DecisionKind::GENESIS),
            Sub::new("system"),
            Outcome::Allow,
            "the chain opens against the booted manifest",
        )
        .with_payload(json!({
            "schema_version": LEDGER_SCHEMA_VERSION,
            "genesis_parent": self.genesis_parent.as_str(),
        }))
    }

    /// Create the table and the unique parent index, idempotently.
    ///
    /// The chassis's own baseline, not an application migration: the shape is
    /// fixed by spec 013 B-2 and never versioned by an app, and a boot that
    /// found no table could not tell an empty chain from a deleted one. An
    /// app's own DDL still goes through the `migrate` verb (spec 011 B-4).
    async fn create_schema(&self) -> Result<(), Error> {
        self.store
            .txn(vec![Statement::new(DECISIONS_TABLE_SQL)])
            .await?;
        if let Err(err) = self
            .store
            .txn(vec![Statement::new(DECISIONS_INDEX_SQL)])
            .await
        {
            return Err(self.index_failure(err).await);
        }
        self.store
            .txn(vec![
                Statement::new(SEGMENTS_TABLE_SQL),
                Statement::new(SEGMENTS_INDEX_SQL),
            ])
            .await?;
        self.add_segment_manifest_column().await?;
        // Spec 042 B-1 and D-6: the identity, collision and coverage tables
        // are chassis baseline DDL for the same reason `kernel_decisions`
        // is, and they record nothing in `schema_version`.
        self.create_identity_schema().await?;
        Ok(())
    }

    /// Give an existing segment table spec 036 B-5's column.
    ///
    /// `CREATE TABLE IF NOT EXISTS` leaves a table that already exists
    /// exactly as it was, and SQLite has no `ADD COLUMN IF NOT EXISTS`, so
    /// the column list is read first and the `ALTER` runs only when it is
    /// missing. This is the chassis's own baseline, not an application
    /// migration (spec 013 D-4), so it records nothing in `schema_version`.
    async fn add_segment_manifest_column(&self) -> Result<(), Error> {
        let columns: Vec<ColumnRow> = self
            .store
            .query_consistent(SEGMENTS_COLUMNS_SQL, vec![])
            .await?;
        // A read that answers nothing at all is the store failing, not a
        // table with no columns (spec 016 D-2): the table was just created
        // above, so an empty answer means the probe did not run and the
        // `ALTER` must not be guessed at either way.
        if columns.is_empty() {
            return Err(Error::Integrity(
                "kernel_segments reports no columns immediately after it was created: the \
                 leader did not answer the schema probe"
                    .to_owned(),
            ));
        }
        if columns
            .iter()
            .any(|column| column.name == SEGMENTS_MANIFEST_COLUMN)
        {
            return Ok(());
        }
        self.store
            .txn(vec![Statement::new(SEGMENTS_ADD_MANIFEST_SQL)])
            .await?;
        Ok(())
    }

    /// Tell damage from a store failure when the index will not build.
    ///
    /// An index that cannot be created over records already resident means
    /// two of them claim one parent, which is a fork that predates this boot
    /// and is [`Error::Integrity`] (spec 013 B-4). Anything else is the store
    /// failing, and keeps the store's own error: a leader that cannot be
    /// reached is not tamper evidence.
    async fn index_failure(&self, err: Error) -> Error {
        match self.duplicate_parent().await {
            Ok(Some(parent)) => Error::Integrity(format!(
                "the unique parent index will not build: more than one resident record claims \
                 parent {parent}, so the chain forked before this boot ({})",
                err.message()
            )),
            Ok(None) => err,
            Err(probe) => probe,
        }
    }

    async fn duplicate_parent(&self) -> Result<Option<String>, Error> {
        let rows: Vec<ParentRow> = self
            .store
            .query_consistent(DUPLICATE_PARENT_SQL, vec![])
            .await?;
        Ok(rows.into_iter().next().map(|row| row.prev_hash))
    }
}

/// A census-bearing read answered, and answered whole (spec 036 D-11).
///
/// `witnessed` is the `COUNT(*)` the statement's own witness row carried,
/// evaluated inside the same statement as the rows, and `carried` is how many
/// rows the read actually delivered.
///
/// There are three ways to fail and each is the same class of fault: a read
/// whose evidence does not account for itself. `None` is a read that did not
/// answer at all, which hiqlite reports as an empty result rather than as an
/// error (spec 016 D-2), and which is exactly the shape an absent answer and
/// an empty table share. A `carried` below the witness is a read that lost
/// rows while stepping them. A `carried` above it cannot happen and is not
/// quietly accepted either.
///
/// None of them may be read as "the relation holds nothing": absence is never
/// evidence (constitution), and the caller that would otherwise walk back to
/// an older answer stops here instead, having written nothing.
///
/// # Errors
///
/// [`Error::Integrity`] naming the relation, the witnessed total, and what
/// the read carried.
pub(crate) fn check_census(
    what: &str,
    witnessed: Option<i64>,
    carried: usize,
) -> Result<(), Error> {
    let Some(total) = witnessed else {
        return Err(Error::Integrity(format!(
            "the read of {what} carried no witness row, so it did not answer: an empty result is \
             not evidence that {what} is empty, and nothing may be concluded from it"
        )));
    };
    let expected = u64::try_from(total)
        .map_err(|_| Error::Integrity(format!("{what} witnessed a negative count of {total}")))?;
    let carried = u64::try_from(carried).unwrap_or(u64::MAX);
    if carried == expected {
        return Ok(());
    }
    Err(Error::Integrity(format!(
        "the read of {what} witnessed {expected} row(s) in its own snapshot and carried \
         {carried}: the answer does not account for itself, so nothing may be concluded from it"
    )))
}

/// The one root of a list, or [`Error::Integrity`] when it has several.
///
/// A list with two roots is a fork at the root, which is the one shape
/// walking from a genesis parent cannot detect afterwards: each half would
/// verify against its own.
fn one_root(roots: impl Iterator<Item = String>, what: &str) -> Result<Option<Hash>, Error> {
    let mut roots = roots;
    let Some(first) = roots.next() else {
        return Ok(None);
    };
    if let Some(rival) = roots.next() {
        return Err(Error::Integrity(format!(
            "the {what} has two roots, {first} and {rival}: it forked at the root"
        )));
    }
    Hash::parse(first).map(Some)
}

/// The row an append inserts, as one statement.
///
/// Kept beside the schema it writes so the column list and the DDL cannot
/// drift apart.
pub(crate) fn insert_statement(record: &SignedRecord) -> Result<Statement, Error> {
    Ok(Statement::with_params(
        "INSERT INTO kernel_decisions (id, prev_hash, hash, record) VALUES ($1, $2, $3, $4)",
        vec![
            Value::from(record.record.id.as_str()),
            Value::from(record.record.previous_record_hash.as_str()),
            Value::from(record.record.record_hash.as_str()),
            Value::Blob(record.to_canonical_bytes()?),
        ],
    ))
}

/// Whether a record with `id` is already in the chain, and under which hash.
///
/// No longer on the append path: spec 042 B-4 makes classification
/// lifetime-scoped, reading the identity row rather than `kernel_decisions`,
/// because the resident table answers "never appended" and "appended and
/// archived" identically. Kept because it is spec 013's own statement of
/// what the resident table holds for an id, and removing a helper from a
/// `complete` spec's module is not this spec's to do.
#[allow(dead_code)]
pub(crate) async fn hash_of(store: &StoreHandle, id: &DecisionId) -> Result<Option<Hash>, Error> {
    let rows: Vec<HashRow> = store
        .query_consistent(
            "SELECT hash FROM kernel_decisions WHERE id = $1",
            vec![Value::from(id.as_str())],
        )
        .await?;
    rows.into_iter()
        .next()
        .map(|row| Hash::parse(row.hash))
        .transpose()
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::check_census;
    use rahi_types::Error;

    /// FR-007, D-11: a read that did not answer is not an empty relation.
    ///
    /// This is the defect D-10 called safe. Before the correction the absent
    /// answer fell through to an older manifest, which an older image can
    /// match; now it stops here.
    #[test]
    fn a_read_with_no_witness_row_is_never_an_empty_relation() {
        let err = check_census("kernel_decisions", None, 0).expect_err("no witness row");
        assert!(matches!(err, Error::Integrity(_)), "{err:?}");
        assert!(
            err.message().contains("did not answer"),
            "the message says why: {}",
            err.message()
        );
    }

    /// FR-007, D-11: rows lost while stepping are caught by the same snapshot.
    #[test]
    fn a_read_that_carried_fewer_rows_than_it_witnessed_is_refused() {
        let err = check_census("kernel_decisions", Some(3), 2).expect_err("a short answer");
        assert!(matches!(err, Error::Integrity(_)), "{err:?}");
        assert!(
            err.message().contains("does not account for itself"),
            "{}",
            err.message()
        );

        let err = check_census("kernel_decisions", Some(3), 0).expect_err("nothing carried");
        assert!(matches!(err, Error::Integrity(_)), "{err:?}");
    }

    /// FR-007, D-11: an emptiness the read witnessed for itself is admitted,
    /// which is what keeps a fresh chain working.
    #[test]
    fn a_witnessed_empty_relation_is_accepted() {
        check_census("kernel_decisions", Some(0), 0).expect("a witnessed empty relation");
        check_census("kernel_segments", Some(2), 2).expect("a complete answer");
    }

    /// More rows than the snapshot holds is not quietly accepted either.
    #[test]
    fn a_read_that_carried_more_rows_than_it_witnessed_is_refused() {
        let err = check_census("kernel_segments", Some(1), 4).expect_err("an over-long answer");
        assert!(matches!(err, Error::Integrity(_)), "{err:?}");
    }

    /// A negative witness is damage, not a count.
    #[test]
    fn a_negative_witness_is_refused() {
        let err = check_census("kernel_decisions", Some(-1), 0).expect_err("a negative witness");
        assert!(matches!(err, Error::Integrity(_)), "{err:?}");
    }
}
