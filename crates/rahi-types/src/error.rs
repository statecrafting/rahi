//! The one `Error` for the workspace's library crates (spec 010 B-4).

use std::fmt;

use serde::{Deserialize, Serialize};

/// The single error type shared by every rahi library crate.
///
/// Each variant carries an owned message. The variant, not the message, is
/// the contract: callers match on it and binaries map it to a process exit
/// code through [`Error::exit_code`], in exactly one place.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Error {
    /// Input failed validation before any effect took place.
    Validation(String),
    /// The addressed thing does not exist.
    NotFound(String),
    /// The write lost a race: a revision, fence, or unique constraint.
    Conflict(String),
    /// A hash, chain, or checksum did not verify. Fail closed.
    Integrity(String),
    /// The kernel refused the action; the denial is ledgered.
    Denied(String),
    /// No principal, or the principal's proof is not acceptable.
    Unauthorized(String),
    /// A derived artifact or index is behind its inputs.
    Stale(String),
    /// The operating system or filesystem failed.
    Io(String),
    /// The configuration is missing or malformed.
    Config(String),
    /// A co-deployed or remote dependency failed.
    Upstream(String),
}

/// The workspace `Result`, fixed on [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// Exit code for a validation failure or a refused, missing, or conflicting
/// operation.
pub const EXIT_FAILURE: i32 = 1;
/// Exit code for stale derived state.
pub const EXIT_STALE: i32 = 2;
/// Exit code for I/O, configuration, or upstream failure.
pub const EXIT_INFRA: i32 = 3;

impl Error {
    /// The process exit code this error maps to.
    ///
    /// `1` for validation, not found, conflict, integrity, denied, and
    /// unauthorized; `2` for stale; `3` for io, config, and upstream. The
    /// mapping mirrors `spec-spine`'s four codes (with `0` for success).
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Validation(_)
            | Self::NotFound(_)
            | Self::Conflict(_)
            | Self::Integrity(_)
            | Self::Denied(_)
            | Self::Unauthorized(_) => EXIT_FAILURE,
            Self::Stale(_) => EXIT_STALE,
            Self::Io(_) | Self::Config(_) | Self::Upstream(_) => EXIT_INFRA,
        }
    }

    /// The stable lowercase name of the variant, for logs and metrics labels.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Validation(_) => "validation",
            Self::NotFound(_) => "not_found",
            Self::Conflict(_) => "conflict",
            Self::Integrity(_) => "integrity",
            Self::Denied(_) => "denied",
            Self::Unauthorized(_) => "unauthorized",
            Self::Stale(_) => "stale",
            Self::Io(_) => "io",
            Self::Config(_) => "config",
            Self::Upstream(_) => "upstream",
        }
    }

    /// The owned message the variant carries.
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::Validation(m)
            | Self::NotFound(m)
            | Self::Conflict(m)
            | Self::Integrity(m)
            | Self::Denied(m)
            | Self::Unauthorized(m)
            | Self::Stale(m)
            | Self::Io(m)
            | Self::Config(m)
            | Self::Upstream(m) => m,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind(), self.message())
    }
}

impl std::error::Error for Error {}
