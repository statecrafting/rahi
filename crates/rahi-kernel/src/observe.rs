//! The kernel's outward surface: decision observers and counters
//! (spec 015 B-7).
//!
//! The kernel imports nothing from the edge. The tracer (spec 023) is a
//! subscriber here rather than a dependency there, so the dependency arrow
//! keeps pointing downward and the kernel stays testable without an HTTP
//! stack.
//!
//! Registration is process-global because observation is: one cell has one
//! tracer, and a decision that only some kernels reported would be a worse
//! audit trail than none. Observers are called synchronously, on the request
//! path, before the decision is queued for the ledger, so an observer that
//! blocks blocks the request; that is the caller's contract and it is why the
//! signature takes a plain `Fn` rather than a future.
//!
//! ```
//! # use rahi_kernel::observe;
//! observe::on_decision(|decision| {
//!     // spec 023's tracer records the span here.
//!     let _ = decision.id.as_str();
//! });
//! ```

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{OnceLock, PoisonError, RwLock};

use rahi_ledger::{Decision, DecisionId};
use rahi_types::Error;

/// The metric raised when the appender cannot write a decision
/// (spec 015 B-6).
pub const METRIC_LEDGER_FAILURES: &str = "kernel_ledger_failures";
/// The metric counting decisions handed to the appender.
pub const METRIC_DECISIONS: &str = "kernel_decisions";
/// The metric counting decisions the bounded queue could not accept.
pub const METRIC_DECISIONS_DROPPED: &str = "kernel_decisions_dropped";

type DecisionObserver = Box<dyn Fn(&Decision) + Send + Sync + 'static>;
type FailureObserver = Box<dyn Fn(&DecisionId, &Error) + Send + Sync + 'static>;

static DECISION_OBSERVERS: OnceLock<RwLock<Vec<DecisionObserver>>> = OnceLock::new();
static FAILURE_OBSERVERS: OnceLock<RwLock<Vec<FailureObserver>>> = OnceLock::new();

static DECISIONS: AtomicU64 = AtomicU64::new(0);
static DROPPED: AtomicU64 = AtomicU64::new(0);
static LEDGER_FAILURES: AtomicU64 = AtomicU64::new(0);

/// Register an observer invoked with every decision before it is queued.
///
/// Observers accumulate; there is no way to remove one, because a tracer that
/// could be unsubscribed at runtime is a tracer that can be turned off by
/// whatever is being traced.
pub fn on_decision<F>(observer: F)
where
    F: Fn(&Decision) + Send + Sync + 'static,
{
    let lock = DECISION_OBSERVERS.get_or_init(|| RwLock::new(Vec::new()));
    let mut observers = lock.write().unwrap_or_else(PoisonError::into_inner);
    observers.push(Box::new(observer));
}

/// Register an observer invoked when the appender cannot write a decision.
///
/// The denial itself already happened; what failed is the record of it, which
/// is the one failure in this crate that must never be silent. With no
/// observer registered the kernel writes the failure to stderr instead
/// (D-7), so it is visible before spec 023's tracer exists.
pub fn on_failure<F>(observer: F)
where
    F: Fn(&DecisionId, &Error) + Send + Sync + 'static,
{
    let lock = FAILURE_OBSERVERS.get_or_init(|| RwLock::new(Vec::new()));
    let mut observers = lock.write().unwrap_or_else(PoisonError::into_inner);
    observers.push(Box::new(observer));
}

/// Decisions handed to the appender since the process started.
#[must_use]
pub fn decisions() -> u64 {
    DECISIONS.load(Ordering::Relaxed)
}

/// Decisions the bounded queue could not accept.
#[must_use]
pub fn dropped() -> u64 {
    DROPPED.load(Ordering::Relaxed)
}

/// Appends that failed (spec 015 B-6's `kernel_ledger_failures`).
#[must_use]
pub fn ledger_failures() -> u64 {
    LEDGER_FAILURES.load(Ordering::Relaxed)
}

/// Every counter, for a metrics exposition to read (spec 023).
#[must_use]
pub fn metrics() -> [(&'static str, u64); 3] {
    [
        (METRIC_DECISIONS, decisions()),
        (METRIC_DECISIONS_DROPPED, dropped()),
        (METRIC_LEDGER_FAILURES, ledger_failures()),
    ]
}

/// Tell every observer about `decision`, then count it.
pub(crate) fn record_decision(decision: &Decision) {
    if let Some(lock) = DECISION_OBSERVERS.get() {
        let observers = lock.read().unwrap_or_else(PoisonError::into_inner);
        for observer in observers.iter() {
            observer(decision);
        }
    }
    DECISIONS.fetch_add(1, Ordering::Relaxed);
}

/// Count a decision the queue refused.
pub(crate) fn record_dropped(id: &DecisionId, err: &Error) {
    DROPPED.fetch_add(1, Ordering::Relaxed);
    report(METRIC_DECISIONS_DROPPED, id, err);
}

/// Count and report an append that failed.
pub(crate) fn record_ledger_failure(id: &DecisionId, err: &Error) {
    LEDGER_FAILURES.fetch_add(1, Ordering::Relaxed);
    report(METRIC_LEDGER_FAILURES, id, err);
}

/// Hand a failure to the observers, or to stderr when there are none.
fn report(metric: &str, id: &DecisionId, err: &Error) {
    if let Some(lock) = FAILURE_OBSERVERS.get() {
        let observers = lock.read().unwrap_or_else(PoisonError::into_inner);
        if !observers.is_empty() {
            for observer in observers.iter() {
                observer(id, err);
            }
            return;
        }
    }
    eprintln!("{metric}: decision {id} was not written to the chain: {err}");
}
