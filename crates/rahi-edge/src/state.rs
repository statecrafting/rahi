//! The state every chassis route is built over (spec 020 B-1).
//!
//! One value, cloned into every handler: the [`Kernel`] that adjudicates, the
//! [`StoreHandle`] that reads and writes, the [`Ledger`] the readiness probe
//! questions, the [`Config`] the middleware derives its cookie scheme and its
//! transport security from, and a type-erased extension slot spec 022 puts
//! its principal extractor in.
//!
//! The slot is what keeps this crate free of `rahi-idp` (spec 020 AC-2): the
//! identity crate inserts its own value and reads it back by type, and the
//! edge never names it.

use std::sync::Arc;

use axum::http::Extensions;
use rahi_kernel::Kernel;
use rahi_ledger::Ledger;
use rahi_store::StoreHandle;
use rahi_types::Config;

/// Everything a chassis route needs, cloned per request.
///
/// Every field is a cheap clone: the kernel and the ledger are `Arc`-backed
/// by their own crates, the store handle is a hiqlite client clone, and the
/// configuration and the extension map are shared behind an [`Arc`].
#[derive(Clone, Debug)]
pub struct AppState {
    kernel: Kernel,
    store: StoreHandle,
    ledger: Ledger,
    config: Arc<Config>,
    extensions: Arc<Extensions>,
}

impl AppState {
    /// Build the state from a booted cell.
    ///
    /// The ledger is the same one the kernel booted against; the probe reads
    /// its head and nothing else writes through this handle.
    #[must_use]
    pub fn new(kernel: Kernel, store: StoreHandle, ledger: Ledger, config: Config) -> Self {
        Self {
            kernel,
            store,
            ledger,
            config: Arc::new(config),
            extensions: Arc::new(Extensions::new()),
        }
    }

    /// Put `value` in the extension slot, replacing any value of its type.
    ///
    /// The slot is how a crate the edge does not depend on hands the edge
    /// something to hold: spec 022 inserts its principal extractor here and
    /// reads it back with [`AppState::extension`].
    #[must_use]
    pub fn with_extension<T>(mut self, value: T) -> Self
    where
        T: Clone + Send + Sync + 'static,
    {
        Arc::make_mut(&mut self.extensions).insert(value);
        self
    }

    /// The value of type `T` in the extension slot, if one was inserted.
    #[must_use]
    pub fn extension<T>(&self) -> Option<T>
    where
        T: Clone + Send + Sync + 'static,
    {
        self.extensions.get::<T>().cloned()
    }

    /// The kernel that adjudicates every governed operation.
    #[must_use]
    pub const fn kernel(&self) -> &Kernel {
        &self.kernel
    }

    /// The store handle.
    #[must_use]
    pub const fn store(&self) -> &StoreHandle {
        &self.store
    }

    /// The decision chain.
    #[must_use]
    pub const fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    /// The cell's configuration.
    #[must_use]
    pub fn config(&self) -> &Config {
        &self.config
    }
}
