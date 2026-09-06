//! The revision watermark every consumer polls (spec 012 B-5).
//!
//! Notify can be lost, replayed, or delivered to a different listener, so no
//! consumer may depend on it. The watermark is what it depends on instead: a
//! write stamps a monotonic revision inside its own transaction, and a
//! consumer that remembers the last revision it handled asks for everything
//! above it on every tick, whether or not an envelope arrived.
//!
//! A stamped table carries an integer `revision` column that a row is
//! inserted or updated with at [`Revision::ZERO`], the value that means "not
//! yet written". [`Watermark::next`] is what turns those rows into a
//! revision, inside the same `txn`.
//!
//! The revision is `max(revision) + 1` over the table, so deleting the newest
//! row hands its revision out a second time. A consumer that must not miss a
//! change through a delete keeps a tombstone row rather than removing it.

use rahi_types::{Error, Revision};
use serde::Deserialize;

use crate::outbox::TxnBuilder;
use crate::query::Value;
use crate::store::StoreHandle;
use crate::txn::Statement;

/// Stamping and reading the revision column.
#[derive(Clone, Copy, Debug)]
pub struct Watermark;

#[derive(Debug, Deserialize)]
struct RevisionRow {
    revision: i64,
}

impl Watermark {
    /// Stamp this transaction's rows in `table` with the next revision.
    ///
    /// Appends one statement that computes `max(revision) + 1` over the table
    /// and writes it to every row the batch left at [`Revision::ZERO`], so
    /// the stamp is committed by the same `txn` as the write it describes and
    /// never by a second round-trip. Call it once per table per batch, after
    /// the writes: one transaction is one revision.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when `table` is not a plain SQL identifier. The
    /// table name is interpolated, not bound, because SQL has no parameter
    /// for an identifier.
    pub fn next(txn: &mut TxnBuilder, table: &str) -> Result<(), Error> {
        let table = identifier(table)?;
        txn.push(Statement::new(format!(
            "UPDATE {table} SET revision = \
             (SELECT COALESCE(MAX(revision), 0) + 1 FROM {table}) WHERE revision = 0"
        )));
        Ok(())
    }

    /// The revisions in `table` above `after`, in order.
    ///
    /// Reads the local replica: a consumer that is one revision behind sees
    /// it on the next tick, which is what a watermark is for, and the leader
    /// round-trip belongs to admission decisions instead.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when `table` is not a plain SQL identifier;
    /// [`Error::Integrity`] when the column holds a negative revision.
    pub async fn since(
        store: &StoreHandle,
        table: &str,
        after: Revision,
    ) -> Result<Vec<Revision>, Error> {
        let table = identifier(table)?;
        let floor = i64::try_from(after.get())
            .map_err(|_| Error::Validation(format!("revision {} exceeds i64", after.get())))?;
        let rows: Vec<RevisionRow> = store
            .query(
                format!(
                    "SELECT DISTINCT revision FROM {table} WHERE revision > $1 ORDER BY revision"
                ),
                vec![Value::Integer(floor)],
            )
            .await?;
        rows.into_iter()
            .map(|row| {
                u64::try_from(row.revision).map(Revision::new).map_err(|_| {
                    Error::Integrity(format!(
                        "{table} holds a negative revision {}",
                        row.revision
                    ))
                })
            })
            .collect()
    }
}

/// A table name that is safe to interpolate: a plain unquoted identifier.
fn identifier(table: &str) -> Result<&str, Error> {
    let refused = || {
        Error::Validation(format!(
            "{table:?} is not a plain SQL identifier; a stamped table is named \
             by letters, digits, and underscores"
        ))
    };
    let mut chars = table.chars();
    let first = chars.next().ok_or_else(refused)?;
    if !first.is_ascii_alphabetic() && first != '_' {
        return Err(refused());
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(refused());
    }
    if table.len() > 64 {
        return Err(refused());
    }
    Ok(table)
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn a_quoted_or_qualified_table_is_refused() {
        for name in [
            "",
            "notes; DROP TABLE notes",
            "main.notes",
            "\"notes\"",
            "1",
        ] {
            let err = identifier(name).expect_err("not a plain identifier");
            assert!(matches!(err, Error::Validation(_)), "{name}: {err}");
        }
    }

    #[test]
    fn a_plain_identifier_is_accepted() {
        for name in ["notes", "_notes", "notes_2"] {
            assert_eq!(identifier(name), Ok(name));
        }
    }
}
