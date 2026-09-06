//! The outbox: a notify that commits with the resource it describes
//! (spec 012 B-4).
//!
//! A SQL write and a notify cannot be made atomic, because they live in
//! different Raft groups (spec 011 B-2). The outbox is the standard answer:
//! the envelope is staged as a row in the same `txn` as the resource, so it is
//! durable exactly when the resource is, and a drain publishes it afterwards.
//! A row is never notified before it is durable, and a notify that is lost
//! after the drain costs a consumer nothing, because the consumer polls
//! [`crate::Watermark::since`] anyway.

use rahi_types::{Error, Revision};
use serde::Deserialize;

use crate::notify::{Envelope, Notify};
use crate::query::Value;
use crate::store::StoreHandle;
use crate::txn::Statement;

/// The outbox table, created by [`crate::coordination_migration`].
///
/// `AUTOINCREMENT` rather than a bare rowid: ids are never reused after a
/// drain, so `ORDER BY id` is the order the rows were staged in for the life
/// of the store.
pub const OUTBOX_TABLE_SQL: &str = "CREATE TABLE IF NOT EXISTS outbox (\
    id INTEGER PRIMARY KEY AUTOINCREMENT, \
    kind TEXT NOT NULL, \
    tenant TEXT, \
    name TEXT NOT NULL, \
    revision INTEGER NOT NULL)";

const STAGE_SQL: &str = "INSERT INTO outbox (kind, tenant, name, revision) VALUES ($1, $2, $3, $4)";

const READ_SQL: &str = "SELECT id, kind, tenant, name, revision FROM outbox ORDER BY id LIMIT $1";

/// A batch under construction: the statements that will commit together.
///
/// A resource write, its revision stamp ([`crate::Watermark::next`]) and its
/// outbox row ([`Outbox::stage`]) are appended to one builder and submitted
/// through [`StoreHandle::txn`], which is what makes them one atomic unit.
#[derive(Clone, Debug, Default)]
pub struct TxnBuilder {
    statements: Vec<Statement>,
}

impl TxnBuilder {
    /// An empty batch.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a statement.
    pub fn push(&mut self, statement: Statement) -> &mut Self {
        self.statements.push(statement);
        self
    }

    /// The statements staged so far, in order.
    #[must_use]
    pub fn statements(&self) -> &[Statement] {
        &self.statements
    }

    /// How many statements are staged.
    #[must_use]
    pub fn len(&self) -> usize {
        self.statements.len()
    }

    /// Whether nothing is staged.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.statements.is_empty()
    }

    /// The batch, ready for [`StoreHandle::txn`].
    #[must_use]
    pub fn into_statements(self) -> Vec<Statement> {
        self.statements
    }
}

impl From<TxnBuilder> for Vec<Statement> {
    fn from(builder: TxnBuilder) -> Self {
        builder.into_statements()
    }
}

/// Staging and draining the outbox.
#[derive(Clone, Copy, Debug)]
pub struct Outbox;

#[derive(Debug, Deserialize)]
struct OutboxRow {
    id: i64,
    kind: String,
    tenant: Option<String>,
    name: String,
    revision: i64,
}

impl Outbox {
    /// Stage an envelope in the caller's batch.
    ///
    /// Appends one `INSERT INTO outbox` to `txn`; it commits with whatever
    /// else the batch carries, or with none of it.
    pub fn stage(txn: &mut TxnBuilder, env: &Envelope) {
        txn.push(Statement::with_params(
            STAGE_SQL,
            vec![
                Value::from(env.kind.as_str()),
                Value::from(env.tenant.clone()),
                Value::from(env.name.as_str()),
                Value::Integer(revision_to_sql(env.revision)),
            ],
        ));
    }

    /// Publish staged rows and delete them.
    ///
    /// Reads up to `batch_size` rows with `query_consistent`, because a row
    /// that is not yet applied by a quorum is not yet durable and must not be
    /// notified; publishes each in id order; then deletes exactly the rows it
    /// published in one `txn`. Returns how many it drained.
    ///
    /// Delivery is at least once: a crash between the notify and the delete
    /// republishes on the next drain. A consumer is idempotent because the
    /// envelope is key-only and it re-reads the resource anyway.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when `batch_size` is zero; the store's error
    /// when the read, the publish, or the delete fails. A failure leaves the
    /// undeleted rows staged for the next drain.
    pub async fn drain(store: &StoreHandle, batch_size: usize) -> Result<usize, Error> {
        if batch_size == 0 {
            return Err(Error::Validation(
                "outbox drain needs a batch size of at least one".to_owned(),
            ));
        }
        let limit = i64::try_from(batch_size).map_err(|_| {
            Error::Validation(format!("outbox batch size {batch_size} exceeds i64"))
        })?;
        let rows: Vec<OutboxRow> = store
            .query_consistent(READ_SQL, vec![Value::Integer(limit)])
            .await?;
        if rows.is_empty() {
            return Ok(0);
        }

        let notify = Notify::new(store.clone());
        let mut ids = Vec::with_capacity(rows.len());
        for row in rows {
            let revision = u64::try_from(row.revision).map_err(|_| {
                Error::Integrity(format!(
                    "outbox row {} holds a negative revision {}",
                    row.id, row.revision
                ))
            })?;
            notify
                .notify(Envelope::new(
                    row.kind,
                    row.tenant,
                    row.name,
                    Revision::new(revision),
                ))
                .await?;
            ids.push(row.id);
        }

        let placeholders = (1..=ids.len())
            .map(|n| format!("${n}"))
            .collect::<Vec<_>>()
            .join(", ");
        let drained = ids.len();
        store
            .txn(vec![Statement::with_params(
                format!("DELETE FROM outbox WHERE id IN ({placeholders})"),
                ids.into_iter().map(Value::Integer).collect(),
            )])
            .await?;
        Ok(drained)
    }
}

/// A revision as the `i64` SQLite stores. Saturates rather than wrapping: a
/// revision beyond `i64::MAX` is unreachable in practice and a negative one
/// would sort before every real revision.
fn revision_to_sql(revision: Revision) -> i64 {
    i64::try_from(revision.get()).unwrap_or(i64::MAX)
}
