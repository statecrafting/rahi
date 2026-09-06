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
//!
//! Spec 016 adds bytes and one refusal: a BLOB parameter and a BLOB column
//! are ordinary ([`Blob`], [`MAX_VALUE_BYTES`]), a sweep is a loop over
//! [`StoreHandle::query_paged`], and no SQLite extension is ever loaded on
//! any node, because hiqlite replicates the statement and an engine that
//! differs between nodes diverges silently.
//!
//! The coordination plane (spec 012) adds the four primitives a controller
//! needs and the one rule that ties them together: [`StoreHandle::lease`]
//! takes a fenced lease, [`Notify`] publishes and subscribes to key-only
//! [`Envelope`]s, [`Outbox`] stages an envelope in the same [`TxnBuilder`] as
//! the resource it describes, and [`Watermark`] stamps and re-reads the
//! revision that makes notify a hint rather than a guarantee. The tables
//! those primitives need are DDL like any other, applied by the `migrate`
//! verb through [`coordination_migration`].

#![forbid(unsafe_code)]

pub mod backup;
pub mod blob;
pub mod cache;
pub mod config;
mod error;
pub mod lock;
pub mod migrate;
pub mod notify;
pub mod outbox;
pub mod query;
pub mod store;
pub mod txn;
pub mod watermark;

pub use backup::{BackupId, BackupListing};
pub use blob::{Blob, DEFAULT_PAGE_ROWS, EXTENSIONS, EngineReport, MAX_VALUE_BYTES};
pub use config::{EncKey, EncKeys, Peer, S3Backup, StoreConfig, StoreSecrets};
pub use lock::{FENCE_TABLE_SQL, LEASE_TTL_SECONDS, Lease};
pub use migrate::{Migration, MigrationReport};
pub use notify::{Envelope, Listen, Notify};
pub use outbox::{OUTBOX_TABLE_SQL, Outbox, TxnBuilder};
pub use query::{Page, Value};
pub use store::{Cache, Store, StoreHandle};
pub use txn::{ExecuteResult, Statement};
pub use watermark::Watermark;

/// The coordination plane's own DDL, as one migration for the app's list.
///
/// The lease's fencing sequence and the outbox are chassis tables, but they
/// are still schema: they are created by the `migrate` verb at the `version`
/// the app gives them, never at boot (spec 011 B-4). An app that leases or
/// stages an envelope carries this migration; one that does neither does not
/// need the tables.
///
/// ```no_run
/// # async fn f(store: &rahi_store::StoreHandle) -> Result<(), rahi_types::Error> {
/// store
///     .migrate(&[
///         rahi_store::coordination_migration(1),
///         rahi_store::Migration::new(2, "notes", "CREATE TABLE notes (id TEXT PRIMARY KEY)"),
///     ])
///     .await?;
/// # Ok(())
/// # }
/// ```
#[must_use]
pub fn coordination_migration(version: u32) -> Migration {
    Migration::new(
        version,
        "rahi-store coordination",
        format!("{FENCE_TABLE_SQL}; {OUTBOX_TABLE_SQL}"),
    )
}
