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

const RECORDS_SQL: &str = "SELECT record FROM kernel_decisions";

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
struct RecordRow {
    record: Vec<u8>,
}

#[derive(Debug, Deserialize)]
struct CountRow {
    total: i64,
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
        };
        ledger.create_schema().await?;

        // A chain whose whole head window has been sealed is not a fresh
        // chain: spec 014's segments are the rest of it, and re-genesising
        // over them would fork the ledger at its root. Both cases are one
        // question, asked of the chain rather than of the caller.
        match ledger.stored_genesis_parent().await? {
            Some(stored) => ledger.genesis_parent = stored,
            None => {
                ledger.append(ledger.genesis_decision()).await?;
            }
        }
        ledger.verify_chain(Depth::Resident).await?;
        Ok(ledger)
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
        let rows: Vec<RecordRow> = self.store.query_consistent(RECORDS_SQL, vec![]).await?;
        let records = rows
            .into_iter()
            .map(|row| SignedRecord::from_bytes(&row.record))
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
