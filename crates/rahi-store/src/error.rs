//! The one place a hiqlite error becomes a workspace [`Error`].

use rahi_types::Error;

/// Map a hiqlite error onto the workspace `Error` by what the caller can do
/// about it: a bad statement is `Validation`, a constraint is `Conflict`, a
/// Raft or network failure is `Upstream`, a storage failure is `Io`.
pub(crate) fn map(err: hiqlite::Error) -> Error {
    use hiqlite::Error as H;
    let msg = err.to_string();
    match err {
        H::Config(_) | H::Cryptr(_) => Error::Config(msg),
        H::ConstraintViolation(_) => Error::Conflict(msg),
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
        _ => Error::Upstream(msg),
    }
}
