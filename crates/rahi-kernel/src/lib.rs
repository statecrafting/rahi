//! The governance seam of the rahi chassis (spec 015).
//!
//! Everything an app is permitted to do is written down once, in a
//! [`Manifest`]: the resources it addresses, the capabilities over them, and
//! which service holds which grant. That document is a *ceiling*, and three
//! things make it one rather than a comment.
//!
//! - **The build verifies against it.** [`verify!`] walks the app crate for
//!   every place it constructs a [`Governed`] facade and refuses to build when
//!   a call site is outside the grants ([`verify`], spec 015 B-3). Absence is
//!   never permission.
//! - **The runtime enforces it.** Every store, lock, notify, secret, and
//!   egress operation passes through a facade, which adjudicates before it
//!   performs and returns [`Error::Denied`] when the answer is no
//!   ([`facade`], [`adjudicate`], spec 015 B-4 and B-5).
//! - **The chain commits to it.** [`Manifest::hash`] is the genesis parent of
//!   the decision chain (spec 013 B-2), so a ledger and a manifest that do not
//!   agree cannot boot together ([`Kernel::boot`], spec 015 B-8), and every
//!   denial becomes a record in that chain ([`observe`], spec 015 B-6).
//!
//! ```no_run
//! # use rahi_kernel::{Kernel, Manifest};
//! # use rahi_ledger::{Ledger, LedgerSigner};
//! # async fn boot(store: rahi_store::StoreHandle) -> Result<(), rahi_types::Error> {
//! let manifest = Manifest::parse(include_str!("../testdata/manifests/valid.toml"))?;
//! let ledger = Ledger::open(store.clone(), LedgerSigner::load_default()?, manifest.hash()?).await?;
//! let kernel = Kernel::boot(manifest, store, ledger).await?;
//! # let _ = kernel;
//! # Ok(())
//! # }
//! ```
//!
//! The denial path never blocks the request path. A refused operation builds
//! its [`Decision`], hands it to observers synchronously, drops it in a
//! bounded queue, and returns; one appender task drains the queue into
//! [`Ledger::append`]. What the caller gets back is the decision's id, so the
//! record and the error name the same event even though the append has not
//! happened yet.

#![forbid(unsafe_code)]

pub mod adjudicate;
pub mod capability;
pub mod facade;
pub mod manifest;
pub mod observe;
pub mod verify;

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use action_gate_core::Gate;
use action_gate_types::ActionContext;
use rahi_ledger::{Decision, DecisionId, DecisionKind, Hash, Ledger};
use rahi_store::StoreHandle;
use rahi_types::Error;
use serde_json::json;
use tokio::sync::mpsc;

pub use action_gate_types::{ActionContext as GateContext, Outcome as GateOutcome};
pub use adjudicate::{
    GRANT_CHECK_ID, GrantCheck, OPTIONAL_CHECKS, Request, Verdict, build_gate, is_ledgered,
    nearest_capability, outcome_of,
};
pub use capability::{
    Capability, CapabilityId, CapabilityKind, Constraints, ResourceFamily, ResourceName,
    ServiceName,
};
pub use facade::{Egress, Governed, Permit, SecretSource, Secrets};
pub use manifest::{
    App, Auth, Contract, GatePolicy, LedgerPolicy, Manifest, Observability, Resources, Service,
};
pub use rahi_ledger::Outcome;
pub use verify::{Usage, scan_crate, scan_source, verify_crate, verify_usage};

/// How many decisions the denial queue holds before it starts dropping.
pub const DEFAULT_QUEUE_CAPACITY: usize = 1024;

/// How many hex characters of the boot head seed a decision id.
const NONCE_LEN: usize = 16;

/// A clock the caller supplies. The kernel never reads one of its own.
///
/// Spec 015 B-6 puts wall time in a decision's payload and spec 013 D-3 keeps
/// it out of the record's timestamp slot, which carries the store revision.
/// Both hold if the only clock in sight belongs to the app.
pub type WallClock = Arc<dyn Fn() -> u64 + Send + Sync>;

/// What [`Kernel::boot_with`] can be told beyond the manifest.
#[derive(Clone, Default)]
pub struct KernelOptions {
    /// The denial queue's depth. Zero means [`DEFAULT_QUEUE_CAPACITY`].
    pub queue_capacity: usize,
    /// The wall clock stamped into decision payloads, if the app has one.
    pub clock: Option<WallClock>,
}

impl fmt::Debug for KernelOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KernelOptions")
            .field("queue_capacity", &self.queue_capacity)
            .field("clock", &self.clock.is_some())
            .finish()
    }
}

struct Inner {
    manifest: Manifest,
    hash: Hash,
    gate: Gate,
    store: StoreHandle,
    denials: mpsc::Sender<Decision>,
    nonce: String,
    seq: AtomicU64,
    queued: AtomicU64,
    handled: Arc<AtomicU64>,
    clock: Option<WallClock>,
}

/// The booted kernel: the manifest, the gate it describes, and the denial
/// queue that feeds the chain.
///
/// Cheap to clone; every clone is the same kernel. Dropping the last one
/// closes the queue, which ends the appender task after it has drained.
#[derive(Clone)]
pub struct Kernel {
    inner: Arc<Inner>,
}

impl fmt::Debug for Kernel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Kernel")
            .field("app", &self.inner.manifest.app.name)
            .field("manifest_hash", &self.inner.hash)
            .field("checks", &self.inner.gate.check_ids())
            .finish_non_exhaustive()
    }
}

impl Kernel {
    /// Boot against an already-open ledger (spec 015 B-8).
    ///
    /// The ledger's genesis parent must be this manifest's hash. A cell whose
    /// manifest changed without a deploy genesis record would otherwise keep
    /// appending to a chain that commits to the *old* ceiling, and the whole
    /// point of rooting the chain at the manifest is that it does not.
    ///
    /// # Errors
    ///
    /// [`Error::Integrity`] when the ledger is rooted at a different hash;
    /// [`Error::Validation`] when the manifest's gate cannot be assembled;
    /// the ledger's own error when the head cannot be read.
    pub async fn boot(
        manifest: Manifest,
        store: StoreHandle,
        ledger: Ledger,
    ) -> Result<Self, Error> {
        Self::boot_with(manifest, store, ledger, KernelOptions::default()).await
    }

    /// [`Kernel::boot`], with the queue depth and the wall clock named.
    ///
    /// # Errors
    ///
    /// As [`Kernel::boot`].
    pub async fn boot_with(
        manifest: Manifest,
        store: StoreHandle,
        ledger: Ledger,
        options: KernelOptions,
    ) -> Result<Self, Error> {
        let hash = manifest.hash()?;
        if ledger.genesis_parent() != &hash {
            return Err(Error::Integrity(format!(
                "the ledger is rooted at {} and the booted manifest hashes to {hash}: the \
                 manifest changed without a deploy genesis record",
                ledger.genesis_parent()
            )));
        }
        let gate = manifest.gate()?;
        let nonce = boot_nonce(&ledger).await?;

        let capacity = if options.queue_capacity == 0 {
            DEFAULT_QUEUE_CAPACITY
        } else {
            options.queue_capacity
        };
        let (denials, mut inbox) = mpsc::channel::<Decision>(capacity);
        let handled = Arc::new(AtomicU64::new(0));
        let drained = Arc::clone(&handled);
        let appender = ledger.clone();
        tokio::spawn(async move {
            while let Some(decision) = inbox.recv().await {
                let id = decision.id.clone();
                if let Err(err) = appender.append(decision).await {
                    observe::record_ledger_failure(&id, &err);
                }
                drained.fetch_add(1, Ordering::Relaxed);
            }
        });

        Ok(Self {
            inner: Arc::new(Inner {
                manifest,
                hash,
                gate,
                store,
                denials,
                nonce,
                seq: AtomicU64::new(0),
                queued: AtomicU64::new(0),
                handled,
                clock: options.clock,
            }),
        })
    }

    /// The declared ceiling.
    #[must_use]
    pub fn manifest(&self) -> &Manifest {
        &self.inner.manifest
    }

    /// The manifest hash the chain is rooted at.
    #[must_use]
    pub fn manifest_hash(&self) -> &Hash {
        &self.inner.hash
    }

    /// The gate's configuration hash, folded into [`Kernel::manifest_hash`].
    #[must_use]
    pub fn gate_config_hash(&self) -> String {
        self.inner.gate.config_hash()
    }

    /// The gate's checks, in evaluation order.
    #[must_use]
    pub fn check_ids(&self) -> Vec<&str> {
        self.inner.gate.check_ids()
    }

    /// Adjudicate one context (spec 015 B-4).
    ///
    /// Pure: no store read, no clock, no append. The same manifest and the
    /// same context always give the same verdict, which is what lets an
    /// auditor re-derive a ledgered decision without running the cell.
    #[must_use]
    pub fn adjudicate(&self, ctx: &ActionContext) -> Verdict {
        self.inner.gate.evaluate(ctx)
    }

    /// Adjudicate `request` and, when it is refused, ledger the refusal.
    ///
    /// The one path a facade takes. A `Degrade` refuses too: the chassis has
    /// no reduced form of a store write to perform, so the honest answer is
    /// the denial plus the reason, and the record says `degrade` (D-5).
    ///
    /// # Errors
    ///
    /// [`Error::Denied`], whose message starts with the id of the decision the
    /// chain will hold.
    pub async fn admit(&self, request: &Request) -> Result<(), Error> {
        let verdict = self.adjudicate(&request.to_context());
        if verdict.is_allow() {
            return Ok(());
        }
        let id = self.emit(request, &verdict);
        Err(Error::Denied(format!("{id}: {}", verdict.reason)))
    }

    /// Build the decision for a refusal, observe it, and queue it.
    ///
    /// Returns before the append: spec 015 B-6 is explicit that the request
    /// path does not wait for the chain. A full queue drops the record rather
    /// than blocking the caller, and says so on
    /// [`observe::METRIC_DECISIONS_DROPPED`]; blocking here would turn a
    /// slow leader into a stalled cell.
    fn emit(&self, request: &Request, verdict: &Verdict) -> DecisionId {
        let id = self.next_id();
        let mut payload = serde_json::Map::new();
        payload.insert("service".to_owned(), json!(request.service.as_str()));
        payload.insert("resource".to_owned(), json!(request.resource));
        payload.insert("manifest".to_owned(), json!(self.inner.hash.as_str()));
        payload.insert("checks".to_owned(), json!(verdict.check_ids));
        payload.insert("blocking".to_owned(), json!(verdict.blocking));
        if let Some(key) = &request.key {
            payload.insert("key".to_owned(), json!(key));
        }
        if let Some(table) = &request.table {
            payload.insert("table".to_owned(), json!(table));
        }
        if let Some(host) = &request.host {
            payload.insert("host".to_owned(), json!(host));
        }
        if let Some(clock) = &self.inner.clock {
            payload.insert("wall_time".to_owned(), json!(clock()));
        }

        let mut decision = Decision::new(
            id.clone(),
            DecisionKind::new(request.kind.as_str()),
            request.actor.clone(),
            outcome_of(verdict),
            verdict.reason.clone(),
        )
        .with_payload(serde_json::Value::Object(payload))
        .at(request.at);
        if let Some(capability) = nearest_capability(&self.inner.manifest, request) {
            decision = decision.with_capability(capability);
        }

        observe::record_decision(&decision);
        match self.inner.denials.try_send(decision) {
            Ok(()) => {
                self.inner.queued.fetch_add(1, Ordering::Relaxed);
            }
            Err(err) => {
                observe::record_dropped(
                    &id,
                    &Error::Upstream(format!("the denial queue refused the record: {err}")),
                );
            }
        }
        id
    }

    /// Wait until every queued decision has been through the appender.
    ///
    /// For a shutdown path and for tests. It waits on the appender, not on
    /// success: a decision whose append failed is handled, counted on
    /// [`observe::METRIC_LEDGER_FAILURES`], and no longer pending.
    ///
    /// # Errors
    ///
    /// [`Error::Upstream`] when `within` elapses first.
    pub async fn flush(&self, within: Duration) -> Result<(), Error> {
        let deadline = tokio::time::Instant::now() + within;
        loop {
            let queued = self.inner.queued.load(Ordering::Relaxed);
            if self.inner.handled.load(Ordering::Relaxed) >= queued {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(Error::Upstream(format!(
                    "the appender still owes {} decision(s) after {within:?}",
                    queued.saturating_sub(self.inner.handled.load(Ordering::Relaxed))
                )));
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }

    /// The store the facades perform on. Never public: B-5's whole point.
    pub(crate) fn store_handle(&self) -> &StoreHandle {
        &self.inner.store
    }

    /// The next decision id: the boot's nonce and a counter.
    ///
    /// The nonce is the chain head this kernel booted on, so ids from two runs
    /// of the same cell cannot collide unless the second run booted on exactly
    /// the head the first one did, which means the first appended nothing.
    /// No clock and no randomness, so the id is reproducible from the chain.
    fn next_id(&self) -> DecisionId {
        let seq = self.inner.seq.fetch_add(1, Ordering::Relaxed);
        DecisionId::new(format!("kernel:{}:{seq:012}", self.inner.nonce))
    }
}

/// The tail of the chain head this kernel booted on.
async fn boot_nonce(ledger: &Ledger) -> Result<String, Error> {
    let head = ledger.head().await?;
    let text = head.as_str();
    Ok(text
        .get(text.len().saturating_sub(NONCE_LEN)..)
        .unwrap_or(text)
        .to_owned())
}
