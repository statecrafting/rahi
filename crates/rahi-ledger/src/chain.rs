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

/// Every resident record, and the count of them taken in the same statement
/// (spec 036 D-11).
///
/// The first branch has no `FROM`, so SQLite yields it whatever the table
/// holds: it is the witness that the read answered at all. Its `total` is a
/// scalar subquery evaluated inside this one statement, so it is the same
/// snapshot the rows come from, which a second `COUNT(*)` statement would
/// not be. A missing witness row or a row count below the witnessed total is
/// a read that did not answer or that dropped rows, and neither may be read
/// as "the chain holds nothing".
const RECORDS_CENSUS_SQL: &str = "SELECT 0 AS witness, \
     (SELECT COUNT(*) FROM kernel_decisions) AS total, X'' AS record \
     UNION ALL \
     SELECT 1 AS witness, 0 AS total, record FROM kernel_decisions";

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

#[derive(Debug, Deserialize)]
struct RecordCensusRow {
    witness: i64,
    #[serde(default)]
    total: i64,
    record: Vec<u8>,
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
        let mut ledger = Self {
            store,
            signer,
            genesis_parent,
            opened: OpenedChain::default(),
            interleave: None,
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
                ledger.append(ledger.genesis_decision()).await?;
            }
        }
        // Spec 036 D-11: verification reads the whole resident chain and every
        // segment header, so what it saw is what this handle carries forward.
        // Both facts only ever become more true, and `current_manifest`
        // refuses to walk back past either of them.
        ledger.opened = ledger.verify_chain_witnessed(Depth::Resident).await?;
        Ok(ledger)
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

    /// Run the installed seam, if any.
    pub(crate) async fn read_interleave(&self) {
        if let Some(hook) = self.interleave.clone() {
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
        let rows: Vec<RecordCensusRow> = self
            .store
            .query_consistent(RECORDS_CENSUS_SQL, vec![])
            .await?;
        let mut witnessed = None;
        let mut carried = Vec::new();
        for row in rows {
            if row.witness == 0 {
                witnessed = Some(row.total);
            } else {
                carried.push(row.record);
            }
        }
        check_census("kernel_decisions", witnessed, carried.len())?;
        let records = carried
            .iter()
            .map(|bytes| SignedRecord::from_bytes(bytes))
            .collect::<Result<Vec<_>, Error>>()?;
        order_chain(&self.resident_root().await?, records)
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
