//! The application store of the rahi chassis (spec 011).
//!
//! hiqlite is a library, and this crate is the one place the chassis opens
//! it. The surface is deliberately narrow: [`Store::open`] starts the node;
//! [`StoreHandle`] is the cheap clone every other crate holds; a write is
//! [`StoreHandle::execute`] or, for anything with an invariant,
//! [`StoreHandle::txn`]; a read names its consistency by calling either
//! [`StoreHandle::query`] (local replica) or [`StoreHandle::query_consistent`]
//! (leader round-trip); migrations are versioned DDL applied by
//! [`StoreHandle::migrate`] from the `migrate` verb, never at boot; and the
//! backup verbs are leader-only.
//!
//! Nothing else in the workspace names hiqlite. Coordination (locks, notify,
//! the outbox, the revision watermark) is spec 012's territory in this same
//! crate.

#![forbid(unsafe_code)]

pub mod backup;
pub mod config;
mod error;
pub mod migrate;
pub mod query;
pub mod store;
pub mod txn;

pub use backup::{BackupId, BackupListing};
pub use config::{EncKey, EncKeys, Peer, S3Backup, StoreConfig, StoreSecrets};
pub use migrate::{Migration, MigrationReport};
pub use query::Value;
pub use store::{Cache, Store, StoreHandle};
pub use txn::{ExecuteResult, Statement};
