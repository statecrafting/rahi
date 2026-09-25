//! The one place a hiqlite error becomes a workspace [`Error`].

use rahi_types::Error;

/// Map a hiqlite error onto the workspace `Error` by what the caller can do
/// about it: a bad statement is `Validation`, a constraint is `Conflict`, a
/// Raft or network failure is `Upstream`, a storage failure is `Io`.
///
/// hiqlite folds every rusqlite error that is not a constraint into
/// `Sqlite`, so the storage-class SQLite conditions are told apart by
/// their result-code names before the rest falls through to `Validation`.
///
/// A batch submitted through `txn` reports a failing statement's constraint
/// violation as `H::Transaction`, not `H::ConstraintViolation` (spec 045
/// D-16 found this: a guard's `NOT NULL` abort, and a bare primary-key
/// collision, both surfaced this way from inside a batch). The rule this
/// function already states for `H::ConstraintViolation` applies the same
/// way here: a constraint failure is `Conflict` whether it is hiqlite's own
/// variant or one folded into a transaction's.
pub(crate) fn map(err: hiqlite::Error) -> Error {
    use hiqlite::Error as H;
    let msg = err.to_string();
    match err {
        H::Config(_) | H::Cryptr(_) => Error::Config(msg),
        H::ConstraintViolation(_) => Error::Conflict(msg),
        H::Transaction(_) if is_constraint_failure(&msg) => Error::Conflict(msg),
        H::Sqlite(_) if is_storage_failure(&msg) => Error::Io(msg),
        H::BadRequest(_)
        | H::PrepareStatement(_)
        | H::QueryParams(_)
        | H::Sqlite(_)
        | H::Transaction(_) => Error::Validation(msg),
        H::QueryReturnedNoRows(_) => Error::NotFound(msg),
        H::Unauthorized(_) | H::Token(_) => Error::Unauthorized(msg),
        H::Bincode(_) => Error::Integrity(msg),
        H::RaftErrorFatal(_) | H::WAL(_) | H::SnapshotError(_) | H::InitializeError(_) => {
            Error::Io(msg)
        }
        H::NodeFailed(m) => Error::Io(format!("NodeFailed: {m}")),
        H::Recovering(m) => Error::Upstream(format!("recovering: {m}")),
        _ => Error::Upstream(msg),
    }
}

/// Whether an error represents a terminal storage failure (hiqlite NodeFailed, spec 043 B-7).
#[must_use]
pub fn is_terminal(err: &Error) -> bool {
    match err {
        Error::Io(msg) => msg.starts_with("NodeFailed:"),
        _ => false,
    }
}

/// Whether an error represents hiqlite's recovering state (spec 043 D-15).
#[must_use]
pub fn is_recovering(err: &Error) -> bool {
    match err {
        Error::Upstream(msg) => msg.starts_with("recovering:"),
        _ => false,
    }
}

/// Whether a transaction's error names a SQL constraint (a `NOT NULL`,
/// `UNIQUE`, or primary-key violation) rather than a bad statement.
fn is_constraint_failure(msg: &str) -> bool {
    msg.to_ascii_uppercase().contains("CONSTRAINT FAILED")
}

/// SQLite result codes that mean the storage, not the statement, failed.
fn is_storage_failure(msg: &str) -> bool {
    const CODES: [&str; 8] = [
        "SQLITE_CORRUPT",
        "SQLITE_FULL",
        "SQLITE_IOERR",
        "SQLITE_NOTADB",
        "SQLITE_CANTOPEN",
        "SQLITE_READONLY",
        "SQLITE_NOMEM",
        "SQLITE_PROTOCOL",
    ];
    let upper = msg.to_ascii_uppercase();
    CODES.iter().any(|c| upper.contains(c))
        || upper.contains("DATABASE DISK IMAGE IS MALFORMED")
        || upper.contains("DATABASE OR DISK IS FULL")
        || upper.contains("DISK I/O ERROR")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_failed_maps_to_terminal_io_error() {
        let h_err = hiqlite::Error::NodeFailed("storage disconnected".into());
        let mapped = map(h_err);
        assert!(matches!(mapped, Error::Io(_)));
        assert!(is_terminal(&mapped));
        assert!(!is_recovering(&mapped));
        assert_eq!(mapped.exit_code(), 3);
    }

    #[test]
    fn recovering_maps_to_upstream_recovering_error() {
        let h_err = hiqlite::Error::Recovering("applying startup log".into());
        let mapped = map(h_err);
        assert!(matches!(mapped, Error::Upstream(_)));
        assert!(is_recovering(&mapped));
        assert!(!is_terminal(&mapped));
        assert_eq!(mapped.exit_code(), 3);
        assert!(mapped.message().starts_with("recovering:"));
    }
}
