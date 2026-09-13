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
//!
//! A stop gives that queue a bound ([`Kernel::drain`], spec 035 B-1): what
//! the appender has not written when the bound expires is abandoned, and
//! every abandoned record is counted and named, so a denial answered with an
//! id is in the chain unless a counter says why it is not (spec 035 B-4).

#![forbid(unsafe_code)]

pub mod adjudicate;
pub mod capability;
pub mod facade;
pub mod manifest;
pub mod observe;
pub mod verify;

use std::collections::VecDeque;
use std::fmt;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use action_gate_core::Gate;
use action_gate_types::ActionContext;
use rahi_ledger::{Decision, Hash, Ledger};
use rahi_store::StoreHandle;
use rahi_types::{Error, Sub};
use serde_json::json;
use tokio::sync::Notify;

use crate::observe::Cause;

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
pub use rahi_ledger::{DecisionId, DecisionKind, Outcome};
pub use verify::{Usage, scan_crate, scan_source, verify_crate, verify_usage};

/// How many decisions the denial queue holds before it starts dropping.
pub const DEFAULT_QUEUE_CAPACITY: usize = 1024;

/// The node a kernel names in its decision ids when it is told none: the id
/// hiqlite gives a single node (`StoreConfig::node_id`'s default).
pub const DEFAULT_NODE_ID: u64 = 1;

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
    /// The replica's hiqlite node id (`StoreConfig::node_id`, the pod ordinal
    /// plus one under spec 032), named in every decision id so that no other
    /// replica of the same chain can mint it (spec 035 B-6). Zero means
    /// [`DEFAULT_NODE_ID`].
    pub node_id: u64,
}

impl fmt::Debug for KernelOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KernelOptions")
            .field("queue_capacity", &self.queue_capacity)
            .field("clock", &self.clock.is_some())
            .field("node_id", &self.node_id)
            .finish()
    }
}

/// What a [`Kernel::drain`] left behind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Drained {
    /// The bound the drain was given.
    pub bound: Duration,
    /// The decisions still owed to the appender when the bound expired,
    /// oldest first, each already counted and reported as
    /// [`Cause::Abandoned`]. Empty when the appender caught up in time.
    pub abandoned: Vec<DecisionId>,
}

impl Drained {
    /// Whether every decision owed at the stop was handled within the bound.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.abandoned.is_empty()
    }
}

/// The denial queue, shared by the kernel and its appender.
///
/// A channel would hide what it holds from everyone but the receiver, and the
/// receiver is the task a drain has stopped waiting for. Shared state lets a
/// drain whose bound expired take every owed decision and name each one.
struct Queue {
    pending: Mutex<Pending>,
    ready: Notify,
    capacity: usize,
}

#[derive(Default)]
struct Pending {
    /// Accepted, not yet taken by the appender.
    waiting: VecDeque<Decision>,
    /// The decision the appender is writing now, until its append returns.
    appending: Option<DecisionId>,
    /// Set when the last kernel is dropped: the appender ends once the queue
    /// is empty.
    closed: bool,
}

impl Queue {
    fn new(capacity: usize) -> Self {
        Self {
            pending: Mutex::new(Pending::default()),
            ready: Notify::new(),
            capacity,
        }
    }

    fn lock(&self) -> MutexGuard<'_, Pending> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Accept `decision`, or refuse it when the queue is full.
    #[must_use]
    fn push(&self, decision: Decision) -> bool {
        let mut pending = self.lock();
        if pending.waiting.len() >= self.capacity {
            return false;
        }
        pending.waiting.push_back(decision);
        drop(pending);
        self.ready.notify_one();
        true
    }

    /// How many decisions the appender still owes: those waiting and the one
    /// it is writing.
    fn owed(&self) -> usize {
        let pending = self.lock();
        pending.waiting.len() + usize::from(pending.appending.is_some())
    }

    /// The next decision to append, marked as being written; `None` once the
    /// queue is closed and empty.
    async fn next(&self) -> Option<Decision> {
        loop {
            {
                let mut pending = self.lock();
                if let Some(decision) = pending.waiting.pop_front() {
                    pending.appending = Some(decision.id.clone());
                    return Some(decision);
                }
                if pending.closed {
                    return None;
                }
            }
            // `notify_one` keeps a permit when nobody is waiting, so a push
            // between the check above and this await is not missed.
            self.ready.notified().await;
        }
    }

    /// The appender is done with `id`. False when a drain abandoned it in the
    /// meantime, which has already counted it.
    fn settle(&self, id: &DecisionId) -> bool {
        let mut pending = self.lock();
        if pending.appending.as_ref() == Some(id) {
            pending.appending = None;
            true
        } else {
            false
        }
    }

    /// Take every owed decision's id, oldest first, leaving nothing owed.
    fn abandon(&self) -> Vec<DecisionId> {
        let mut pending = self.lock();
        let mut ids: Vec<DecisionId> = pending.appending.take().into_iter().collect();
        ids.extend(pending.waiting.drain(..).map(|decision| decision.id));
        ids
    }

    fn close(&self) {
        self.lock().closed = true;
        self.ready.notify_one();
    }
}

/// Append queued decisions one at a time until the queue closes.
async fn append_all<A, F>(queue: Arc<Queue>, append: A)
where
    A: Fn(Decision) -> F,
    F: Future<Output = Result<Hash, Error>>,
{
    let _unfinished = AbandonOnDrop(Arc::clone(&queue));
    while let Some(decision) = queue.next().await {
        let id = decision.id.clone();
        let appended = append(decision).await;
        if queue.settle(&id)
            && let Err(err) = appended
        {
            observe::record_loss(&id, Cause::Failed, &err);
        }
    }
}

/// The appender's last word. A runtime that ends the task with decisions
/// still owed drops this, and each of them is counted as abandoned rather
/// than lost in silence; a supervisor that stops waiting for `serve` ends the
/// runtime exactly that way.
struct AbandonOnDrop(Arc<Queue>);

impl Drop for AbandonOnDrop {
    fn drop(&mut self) {
        let ids = self.0.abandon();
        if ids.is_empty() {
            return;
        }
        let err = Error::Upstream(
            "the appender stopped with the decision still owed: the runtime ended before it \
             was written"
                .to_owned(),
        );
        for id in &ids {
            observe::record_loss(id, Cause::Abandoned, &err);
        }
    }
}

struct Inner {
    manifest: Manifest,
    hash: Hash,
    gate: Gate,
    store: StoreHandle,
    queue: Arc<Queue>,
    nonce: String,
    node_id: u64,
    seq: AtomicU64,
    clock: Option<WallClock>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.queue.close();
    }
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

    /// [`Kernel::boot`], with the queue depth, the wall clock, and the node
    /// named.
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
        let appender = ledger.clone();
        Self::boot_appending(manifest, store, ledger, options, move |decision| {
            let ledger = appender.clone();
            async move { ledger.append(decision).await }
        })
        .await
    }

    /// [`Kernel::boot_with`], with the appender's write named: the ledger in
    /// every build, and a write held behind a gate in this crate's tests.
    async fn boot_appending<A, F>(
        manifest: Manifest,
        store: StoreHandle,
        ledger: Ledger,
        options: KernelOptions,
        append: A,
    ) -> Result<Self, Error>
    where
        A: Fn(Decision) -> F + Send + 'static,
        F: Future<Output = Result<Hash, Error>> + Send + 'static,
    {
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
        let node_id = if options.node_id == 0 {
            DEFAULT_NODE_ID
        } else {
            options.node_id
        };
        let queue = Arc::new(Queue::new(capacity));
        tokio::spawn(append_all(Arc::clone(&queue), append));

        Ok(Self {
            inner: Arc::new(Inner {
                manifest,
                hash,
                gate,
                store,
                queue,
                nonce,
                node_id,
                seq: AtomicU64::new(0),
                clock: options.clock,
            }),
        })
    }

    /// The nonce this boot mints decision ids under: the last sixteen hex
    /// characters of the chain head it booted on (spec 015 D-8).
    #[must_use]
    pub fn nonce(&self) -> &str {
        &self.inner.nonce
    }

    /// The node every decision id of this kernel names (spec 035 B-6).
    #[must_use]
    pub fn node_id(&self) -> u64 {
        self.inner.node_id
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

    /// Ledger a refusal this kernel did not adjudicate (spec 022 B-6).
    ///
    /// The identity layer refuses a request whose principal lacks a role.
    /// That is not a capability question: the manifest declares a ceiling over
    /// resources, and a role is the IdP's answer about a person, so the gate
    /// has nothing to say about it and `admit` would have to lie about why it
    /// denied. What the two refusals share is everything after the answer.
    /// B-6 is that *every* denial becomes a record without the request path
    /// waiting for the append, and that mechanism is this kernel's bounded
    /// queue. A second appender in the identity crate would be a second copy
    /// of it with its own failure semantics, and a synchronous append there
    /// would put a Raft write on the request path that B-6 exists to keep off.
    ///
    /// Returns the id of the decision the chain will hold, which the caller
    /// puts in front of its [`Error::Denied`] message so the refusal the
    /// client reads and the record an auditor finds name the same event.
    pub fn refuse(
        &self,
        kind: &str,
        actor: &Sub,
        reason: impl Into<String>,
        payload: serde_json::Map<String, serde_json::Value>,
    ) -> DecisionId {
        let id = self.next_id();
        let mut payload = payload;
        payload.insert("manifest".to_owned(), json!(self.inner.hash.as_str()));
        if let Some(clock) = &self.inner.clock {
            payload.insert("wall_time".to_owned(), json!(clock()));
        }
        let decision = Decision::new(
            id.clone(),
            DecisionKind::new(kind),
            actor.clone(),
            Outcome::Deny,
            reason.into(),
        )
        .with_payload(serde_json::Value::Object(payload));

        observe::record_decision(&decision);
        self.enqueue(&id, decision);
        id
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
        self.enqueue(&id, decision);
        id
    }

    /// Queue `decision` for the appender, or count it as dropped when the
    /// queue is full.
    fn enqueue(&self, id: &DecisionId, decision: Decision) {
        if !self.inner.queue.push(decision) {
            observe::record_loss(
                id,
                Cause::Dropped,
                &Error::Upstream(format!(
                    "the denial queue is full at {} record(s) and refused the record",
                    self.inner.queue.capacity
                )),
            );
        }
    }

    /// Wait until every queued decision has been through the appender.
    ///
    /// For tests and for a caller that wants to know the chain has caught up.
    /// It waits on the appender, not on success: a decision whose append
    /// failed is handled, counted on [`observe::METRIC_LEDGER_FAILURES`], and
    /// no longer pending. It abandons nothing: a flush that times out leaves
    /// every owed decision with the appender and reports how many are still
    /// owed. The stop that gives up on them is [`Kernel::drain`].
    ///
    /// # Errors
    ///
    /// [`Error::Upstream`] when `within` elapses first, naming how many
    /// decisions the appender still owes.
    pub async fn flush(&self, within: Duration) -> Result<(), Error> {
        let deadline = tokio::time::Instant::now() + within;
        loop {
            let owed = self.inner.queue.owed();
            if owed == 0 {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(Error::Upstream(format!(
                    "the appender still owes {owed} decision(s) after {within:?}"
                )));
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }

    /// Drain the denial queue for a stop (spec 035 B-1 and B-2).
    ///
    /// Waits up to `within` for the appender to owe nothing. When the bound
    /// expires first, every decision still owed, waiting or being written, is
    /// abandoned: taken from the queue, counted on
    /// [`observe::METRIC_DECISIONS_ABANDONED`], and reported to the failure
    /// observers with [`Cause::Abandoned`], once per id. A decision that was
    /// being written when it was abandoned can still land; it is counted
    /// anyway, because a count that can overstate a loss is better than one
    /// that can miss one.
    ///
    /// The kernel stays usable: a denial after the drain is queued and
    /// appended as before. The caller shuts the store only after this
    /// returns, since the appender writes through it.
    pub async fn drain(&self, within: Duration) -> Drained {
        if self.flush(within).await.is_ok() {
            return Drained {
                bound: within,
                abandoned: Vec::new(),
            };
        }
        let abandoned = self.inner.queue.abandon();
        let err = Error::Upstream(format!(
            "the decision was still owed to the appender when the {within:?} drain bound expired"
        ));
        for id in &abandoned {
            observe::record_loss(id, Cause::Abandoned, &err);
        }
        Drained {
            bound: within,
            abandoned,
        }
    }

    /// The store the facades perform on. Never public: B-5's whole point.
    pub(crate) fn store_handle(&self) -> &StoreHandle {
        &self.inner.store
    }

    /// The next decision id: `kernel:<nonce>:<node>:<counter>` (spec 035
    /// B-6 and D-2).
    ///
    /// The nonce is the chain head this kernel booted on, so ids from two runs
    /// of the same replica cannot collide unless the second run booted on
    /// exactly the head the first one did, which means the first appended
    /// nothing (spec 035 D-3 accepts that residual). The node separates
    /// replicas, which boot on the same head whenever none appended between
    /// their boots, and whose counters would otherwise repeat each other for
    /// as long as they run. No clock and no randomness, so the id is
    /// reproducible from the chain and the deployment's node list.
    fn next_id(&self) -> DecisionId {
        let seq = self.inner.seq.fetch_add(1, Ordering::Relaxed);
        DecisionId::new(format!(
            "kernel:{}:{}:{seq:012}",
            self.inner.nonce, self.inner.node_id
        ))
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

/// The drain and the queue with the appender held (spec 035 FR-002, FR-003).
///
/// These live beside the queue because the gate does: the appender's write
/// is private, and a test that could only reach the public boot could not
/// hold a real ledger append open for longer than a bound.
#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use std::net::{SocketAddr, TcpListener};
    use std::sync::OnceLock;

    use rahi_ledger::LedgerSigner;
    use rahi_store::{EncKey, EncKeys, Store, StoreConfig, StoreSecrets};
    use tokio::sync::Semaphore;

    use super::*;

    fn free_addr() -> SocketAddr {
        TcpListener::bind("127.0.0.1:0")
            .expect("a free port")
            .local_addr()
            .expect("an address")
    }

    fn store_config(dir: &std::path::Path) -> StoreConfig {
        StoreConfig {
            node_id: 1,
            nodes: Vec::new(),
            data_dir: dir.to_path_buf(),
            raft_addr: free_addr(),
            api_addr: free_addr(),
            secrets: StoreSecrets {
                secret_raft: "raft-secret-for-tests-0000".to_owned(),
                secret_api: "api-secret-for-tests-00000".to_owned(),
                enc_keys: EncKeys {
                    active: "test".to_owned(),
                    keys: vec![EncKey {
                        id: "test".to_owned(),
                        key: vec![7u8; 32],
                    }],
                },
            },
            backup_keep_days: 1,
            s3: None,
        }
    }

    /// Every loss reported in this test binary, by id and cause.
    fn losses() -> &'static Mutex<Vec<(DecisionId, Cause)>> {
        static LOSSES: OnceLock<Mutex<Vec<(DecisionId, Cause)>>> = OnceLock::new();
        LOSSES.get_or_init(|| {
            observe::on_failure(|id, cause, _err| {
                losses()
                    .lock()
                    .expect("not poisoned")
                    .push((id.clone(), cause));
            });
            Mutex::new(Vec::new())
        })
    }

    /// The causes reported for `id`, in order.
    fn causes_of(id: &DecisionId) -> Vec<Cause> {
        losses()
            .lock()
            .expect("not poisoned")
            .iter()
            .filter(|(seen, _)| seen == id)
            .map(|(_, cause)| *cause)
            .collect()
    }

    /// A kernel whose appender waits on `gate` before every append.
    struct Held {
        kernel: Kernel,
        ledger: Ledger,
        gate: Arc<Semaphore>,
        _node: Store,
        _dir: tempfile::TempDir,
    }

    impl Held {
        /// Boot with `node_id`. Every test here boots a fresh chain from one
        /// manifest and one key, so every kernel boots on the same head; the
        /// node is what keeps one test's ids out of another's observations,
        /// which is spec 035 B-6 in miniature.
        async fn boot(queue_capacity: usize, node_id: u64) -> Self {
            let _ = losses();
            let dir = tempfile::tempdir().expect("a temp dir");
            let node = Store::open(&store_config(&dir.path().join("hiqlite")))
                .await
                .expect("a single-voter node opens");
            let store = node.handle();
            let manifest = Manifest::parse(include_str!("../testdata/manifests/valid.toml"))
                .expect("the fixture manifest parses");
            let ledger = Ledger::open(
                store.clone(),
                LedgerSigner::from_seed([9u8; 32]),
                manifest.hash().expect("hashes"),
            )
            .await
            .expect("the chain opens");
            let gate = Arc::new(Semaphore::new(0));
            let permits = Arc::clone(&gate);
            let appender = ledger.clone();
            let kernel = Kernel::boot_appending(
                manifest,
                store,
                ledger.clone(),
                KernelOptions {
                    queue_capacity,
                    node_id,
                    ..KernelOptions::default()
                },
                move |decision| {
                    let permits = Arc::clone(&permits);
                    let ledger = appender.clone();
                    async move {
                        let _permit = permits.acquire().await.expect("the gate stays open");
                        ledger.append(decision).await
                    }
                },
            )
            .await
            .expect("the kernel boots");
            Self {
                kernel,
                ledger,
                gate,
                _node: node,
                _dir: dir,
            }
        }

        fn deny(&self, actor: &str) -> DecisionId {
            self.kernel.refuse(
                "test.denial",
                &Sub::new(actor),
                "the test denies it",
                serde_json::Map::new(),
            )
        }

        async fn resident(&self) -> Vec<String> {
            self.ledger
                .records()
                .await
                .expect("records")
                .into_iter()
                .map(|record| record.record.id)
                .collect()
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_drain_whose_bound_expires_abandons_every_owed_decision_by_name() {
        let held = Held::boot(64, 2).await;
        let ids: Vec<DecisionId> = (0..5).map(|_| held.deny("fr002-actor")).collect();

        let started = tokio::time::Instant::now();
        let drained = held.kernel.drain(Duration::from_secs(1)).await;
        assert!(
            started.elapsed() >= Duration::from_secs(1),
            "the drain waited its bound before giving up"
        );
        assert_eq!(drained.bound, Duration::from_secs(1));
        assert!(!drained.is_complete());
        assert_eq!(
            drained.abandoned, ids,
            "the abandoned count is every record still owed, oldest first"
        );
        for id in &ids {
            assert_eq!(
                causes_of(id),
                vec![Cause::Abandoned],
                "{id} reached the observer once, as abandoned"
            );
        }
        assert_eq!(held.kernel.inner.queue.owed(), 0, "nothing is owed now");

        // The append the gate held returns after the drain: it was counted
        // already, so it reports nothing more, and the abandoned records
        // behind it are never written.
        held.gate.add_permits(64);
        let later = held.deny("fr002-after-the-drain");
        held.kernel
            .flush(Duration::from_secs(10))
            .await
            .expect("the kernel keeps working after a drain");
        for id in &ids {
            assert_eq!(causes_of(id), vec![Cause::Abandoned], "{id}");
        }
        assert!(causes_of(&later).is_empty(), "the later denial landed");
        let resident = held.resident().await;
        assert!(
            resident.contains(&later.as_str().to_owned()),
            "{resident:?}"
        );
        for id in &ids[1..] {
            assert!(
                !resident.contains(&id.as_str().to_owned()),
                "{id} was abandoned while waiting and never written"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_drain_that_catches_up_abandons_nothing() {
        let held = Held::boot(64, 3).await;
        let ids: Vec<DecisionId> = (0..3).map(|_| held.deny("fr002-caught-up")).collect();
        held.gate.add_permits(64);

        let drained = held.kernel.drain(Duration::from_secs(10)).await;
        assert!(drained.is_complete(), "{drained:?}");
        let resident = held.resident().await;
        for id in &ids {
            assert!(causes_of(id).is_empty(), "{id}");
            assert!(resident.contains(&id.as_str().to_owned()), "{id}");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_full_queue_drops_the_overflow_as_dropped_not_failed() {
        let held = Held::boot(1, 4).await;
        // The gate is held: at most one decision is being written and one
        // waits, so of four at least two find the queue full.
        let ids: Vec<DecisionId> = (0..4).map(|_| held.deny("fr003-actor")).collect();
        let dropped: Vec<&DecisionId> = ids
            .iter()
            .filter(|id| causes_of(id) == vec![Cause::Dropped])
            .collect();
        assert!(
            (2..=3).contains(&dropped.len()),
            "the overflow is dropped: {dropped:?}"
        );

        held.gate.add_permits(16);
        held.kernel
            .flush(Duration::from_secs(10))
            .await
            .expect("the appender catches up");
        let resident = held.resident().await;
        for id in &ids {
            let causes = causes_of(id);
            assert!(!causes.contains(&Cause::Failed), "{id}: {causes:?}");
            assert_eq!(
                resident.contains(&id.as_str().to_owned()),
                !dropped.contains(&id),
                "{id} is in the chain exactly when it was not dropped"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_id_names_the_nonce_the_node_and_the_counter() {
        let held = Held::boot(8, 0).await;
        let first = held.deny("b6-shape");
        let second = held.deny("b6-shape");
        let nonce = held.kernel.nonce().to_owned();
        assert_eq!(nonce.len(), NONCE_LEN);
        assert_eq!(held.kernel.node_id(), DEFAULT_NODE_ID);
        assert_eq!(first.as_str(), format!("kernel:{nonce}:1:000000000000"));
        assert_eq!(second.as_str(), format!("kernel:{nonce}:1:000000000001"));
        held.gate.add_permits(8);
    }
}
