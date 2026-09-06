//! The store's ordering vocabulary (spec 010 B-6, constitution IX).
//!
//! Both newtypes are opaque counters. They have an order and nothing else:
//! no arithmetic, and no constructor that reads a clock. A `Revision` moves
//! forward only through [`Revision::next`]; a `FenceToken` is minted by the
//! store's lease and only ever compared.

use serde::{Deserialize, Serialize};

/// A monotonically increasing revision of a stored resource.
///
/// The revision column is the truth about a write; notifications are hints.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(transparent)]
#[serde(transparent)]
pub struct Revision(u64);

impl Revision {
    /// The revision of a resource that has never been written.
    pub const ZERO: Self = Self(0);

    /// Wrap a raw revision read back from the store.
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// The raw counter, for writing to the store.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// The revision that follows this one.
    ///
    /// Saturates at `u64::MAX` rather than wrapping: a wrapped revision would
    /// compare as older than its predecessor and break the store's
    /// compare-and-swap.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

/// A fencing token issued with a lease.
///
/// A lease-guarded write carries its token and the store's predicate
/// `fence <= :token` rejects a write from a lease that has been superseded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(transparent)]
#[serde(transparent)]
pub struct FenceToken(u64);

impl FenceToken {
    /// Wrap a raw token minted by the store's lease.
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// The raw token, for the SQL predicate.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}
