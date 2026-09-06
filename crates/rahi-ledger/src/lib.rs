//! The decision chain of the rahi chassis (spec 013).
//!
//! Every admitted mutation and every denial the kernel makes becomes a
//! [`Decision`]: a signed, hash-linked record in one linear chain kept in the
//! app's own hiqlite. What a buyer of a governed cell is paying for is that
//! the link between any record and its predecessor can be checked offline, by
//! someone who does not run this code, which is why the record format is the
//! `attest-ledger` envelope over `canonical-keysort-json` bytes rather than
//! anything invented here.
//!
//! Three properties carry the whole design, and each has one home:
//!
//! - **Append is a compare-and-swap.** [`Ledger::append`] reads the head,
//!   chains onto it, and inserts. A unique index on the parent hash lets the
//!   store admit one record per parent, so a race has a winner and a loser
//!   rather than a fork ([`append`]).
//! - **The chain is verified whole at boot.** [`Ledger::open`] checks hash
//!   links, signatures, and linearity over everything resident, and fails
//!   closed: on [`rahi_types::Error::Integrity`] the caller's boot path exits
//!   the process rather than serve requests under a broken audit proof
//!   ([`verify`]).
//! - **The key never leaks.** [`LedgerSigner`] holds the cell's Ed25519 key,
//!   prints nothing, and serializes to nothing; verification takes the public
//!   half only ([`signer`]).
//!
//! ```no_run
//! # use rahi_ledger::{Decision, DecisionId, DecisionKind, Hash, Ledger, LedgerSigner, Outcome};
//! # use rahi_types::{Error, Sub};
//! # async fn boot(store: rahi_store::StoreHandle, manifest_hash: Hash) -> Result<(), Error> {
//! let signer = LedgerSigner::load_default()?;
//! let ledger = Ledger::open(store, signer, manifest_hash).await?; // fatal on Integrity
//!
//! ledger
//!     .append(Decision::new(
//!         DecisionId::new("01J8-...-denied"),
//!         DecisionKind::new("db.write"),
//!         Sub::new("rauthy-subject"),
//!         Outcome::Deny,
//!         "no grant covers db.write on notes",
//!     ))
//!     .await?;
//! # Ok(())
//! # }
//! ```
//!
//! The tail is not unbounded. hiqlite replicates the whole database to every
//! node, so history that only grows cannot stay resident: spec 014 seals the
//! oldest run into an immutable [`Segment`], writes it to an [`Archive`], and
//! deletes it from the hot table ([`seal`], [`segment`], [`archive`]). The
//! chain still verifies end to end, because the segments carry their own
//! hash links and [`Ledger::resident_root`] names the hash the oldest
//! resident record chains onto.
//!
//! What a decision is *about* is spec 015's, which owns the kinds and emits
//! them.

#![forbid(unsafe_code)]

pub mod append;
pub mod archive;
pub mod chain;
pub mod record;
pub mod seal;
pub mod segment;
pub mod signer;
pub mod verify;

pub use append::APPEND_ATTEMPTS;
pub use archive::{Archive, FsArchive, S3Archive, S3Config};
pub use chain::{DECISIONS_INDEX_SQL, DECISIONS_TABLE_SQL, Ledger};
pub use record::{
    CanonicalJson, CapabilityId, Decision, DecisionId, DecisionKind, Hash, Outcome, SignedRecord,
    revision_stamp,
};
pub use seal::{Depth, SealPolicy};
pub use segment::{
    SEGMENT_PREFIX, SEGMENTS_INDEX_SQL, SEGMENTS_TABLE_SQL, Segment, SegmentHeader, order_segments,
    segment_hash,
};
pub use signer::{DEFAULT_KEY_PATH, LedgerSigner, LedgerVerifier};
pub use verify::{order_chain, verify_chain};
