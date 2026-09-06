//! The write path (spec 011 B-2, constitution IX).
//!
//! `execute` is one statement. `txn` is a batch submitted as one Raft
//! operation and applied inside one SQLite transaction: either every
//! statement lands or none does. Anything with an invariant (a resource
//! write and its outbox row, a chain append and its head) goes through
//! `txn`. There is no API that commits a SQL write and a notify atomically,
//! because SQL and notify live in different Raft groups and there cannot be
//! one; notify is a hint and the revision column is truth.

use serde::{Deserialize, Serialize};

use crate::query::Value;

/// One SQL statement with its positional parameters (`$1`, `$2`, ...).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Statement {
    /// The SQL text.
    pub sql: String,
    /// The positional parameters, in order.
    pub params: Vec<Value>,
}

impl Statement {
    /// A statement with no parameters.
    #[must_use]
    pub fn new(sql: impl Into<String>) -> Self {
        Self {
            sql: sql.into(),
            params: Vec::new(),
        }
    }

    /// A statement with parameters.
    #[must_use]
    pub fn with_params(sql: impl Into<String>, params: Vec<Value>) -> Self {
        Self {
            sql: sql.into(),
            params,
        }
    }

    pub(crate) fn into_hiqlite(self) -> (String, hiqlite::Params) {
        (
            self.sql,
            self.params.into_iter().map(Value::into_param).collect(),
        )
    }
}

/// What a write reported.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecuteResult {
    /// Rows the statement inserted, updated, or deleted.
    pub rows_affected: u64,
}

impl ExecuteResult {
    pub(crate) fn from_rows(rows: usize) -> Self {
        Self {
            rows_affected: u64::try_from(rows).unwrap_or(u64::MAX),
        }
    }
}
