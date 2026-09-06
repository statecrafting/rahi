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

use rahi_store::Statement;
use rahi_store::{StoreHandle, Value};
use rahi_types::{Error, LEDGER_SCHEMA_VERSION, Sub};
use serde::Deserialize;
use serde_json::json;

use crate::record::{Decision, DecisionId, DecisionKind, Hash, Outcome, SignedRecord};
use crate::signer::{LedgerSigner, LedgerVerifier};
use crate::verify::{order_chain, verify_chain};

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

#[derive(Debug, Deserialize)]
struct HashRow {
    hash: String,
}

#[derive(Debug, Deserialize)]
struct ParentRow {
    prev_hash: String,
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
    /// `genesis_parent` is the booted manifest's hash (spec 015 B-8). It is
    /// what the genesis record links to, so two cells running different
    /// manifests cannot produce chains that could be spliced together, and a
    /// manifest change without a deploy genesis record is caught here rather
    /// than believed.
    ///
    /// Verification runs over the whole resident chain on every open and
    /// fails closed. The caller is a boot path and constitution XI makes the
    /// consequence explicit: on [`Error::Integrity`] the process exits rather
    /// than serves.
    ///
    /// # Errors
    ///
    /// [`Error::Integrity`] when the chain does not verify, when the unique
    /// parent index cannot be built over records already resident, or when a
    /// stored row is not a record this crate wrote. Store failures keep their
    /// own variants: a leader that cannot be reached is not tamper evidence.
    pub async fn open(
        store: StoreHandle,
        signer: LedgerSigner,
        genesis_parent: Hash,
    ) -> Result<Self, Error> {
        let ledger = Self {
            store,
            signer,
            genesis_parent,
        };
        ledger.create_schema().await?;

        let mut records = ledger.records().await?;
        if records.is_empty() {
            ledger.append(ledger.genesis_decision()).await?;
            records = ledger.records().await?;
        }
        verify_chain(&ledger.genesis_parent, &records, &ledger.verifier())?;
        Ok(ledger)
    }

    /// The hash the genesis record links to: the booted manifest's hash.
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
    /// genesis parent when nothing is resident yet. Read through the leader:
    /// this is the value the compare-and-swap is against.
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
                Ok(self.genesis_parent.clone())
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
    /// # Errors
    ///
    /// [`Error::Integrity`] when a stored row does not parse or when the
    /// records do not form one list rooted at the genesis parent; the store's
    /// own error when the read fails.
    pub async fn records(&self) -> Result<Vec<SignedRecord>, Error> {
        let rows: Vec<RecordRow> = self.store.query_consistent(RECORDS_SQL, vec![]).await?;
        let records = rows
            .into_iter()
            .map(|row| SignedRecord::from_bytes(&row.record))
            .collect::<Result<Vec<_>, Error>>()?;
        order_chain(&self.genesis_parent, records)
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

    /// Verify the resident chain again, without reopening.
    ///
    /// # Errors
    ///
    /// [`Error::Integrity`] as [`Ledger::open`].
    pub async fn verify(&self) -> Result<(), Error> {
        verify_chain(
            &self.genesis_parent,
            &self.records().await?,
            &self.verifier(),
        )
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
