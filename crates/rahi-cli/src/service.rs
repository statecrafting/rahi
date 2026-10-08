//! Managed services (spec 047): application background work whose lifetime
//! is `serve`'s.
//!
//! A cell declares its services once per `serve` composition
//! ([`crate::Cell::services`]). `serve` owns every future and join handle:
//! it spawns each service once after the listener binds, broadcasts one
//! cancellation on the first stop cause, aborts whatever is still running
//! [`rahi_ops::stop::SERVICE_JOIN`] after that broadcast, and joins every
//! service before the kernel's denial queue drains and the store shuts down
//! (B-4 to B-6). The application gets a one-way [`ServiceShutdown`]: it can
//! observe the process's authority, never exercise it (D-1).

use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;
use std::time::Instant;

use rahi_ops::stop::Reason;
use rahi_types::{Error, Result};
use tokio::sync::watch;
use tokio::task::{AbortHandle, Id, JoinError, JoinSet};

/// A managed service's future, owned by `serve`.
type ServiceFuture = Pin<Box<dyn Future<Output = Result<()>> + Send + 'static>>;

/// One named background service (B-1).
///
/// Its future runs for the life of `serve`. Returning `Ok(())` before the
/// stop is an unexpected exit, returning `Err` at any time is a failure, and
/// a panic is a failure; each stops the process, which never restarts the
/// service in-process (B-7, D-2). A service that needs child tasks owns and
/// joins them itself (B-2).
pub struct ManagedService {
    name: String,
    future: ServiceFuture,
}

impl ManagedService {
    /// A service named `name` that runs `future`. The name must be non-empty
    /// and unique within the cell; `serve` refuses the composition otherwise.
    pub fn new(
        name: impl Into<String>,
        future: impl Future<Output = Result<()>> + Send + 'static,
    ) -> Self {
        Self {
            name: name.into(),
            future: Box::pin(future),
        }
    }

    /// The service's stable name, as stop records name it.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl std::fmt::Debug for ManagedService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagedService")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// The process's cancellation, as a service sees it (B-1, D-1): cloneable,
/// observable, and unable to cancel anything.
#[derive(Clone, Debug)]
pub struct ServiceShutdown {
    rx: watch::Receiver<bool>,
}

impl ServiceShutdown {
    /// Resolve once `serve` has broadcast its stop. Resolves at once when it
    /// already has.
    pub async fn cancelled(&self) {
        let mut rx = self.rx.clone();
        // An error is the sender gone, which only happens once `serve` has
        // finished with every service: that is a stop too.
        let _ = rx.wait_for(|cancelled| *cancelled).await;
    }

    /// Whether `serve` has broadcast its stop.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        *self.rx.borrow()
    }
}

/// `serve`'s side of the broadcast: sent once, at the first stop cause (B-4).
#[derive(Debug)]
pub(crate) struct Cancel {
    tx: watch::Sender<bool>,
    at: OnceLock<Instant>,
}

impl Cancel {
    pub(crate) fn new() -> Self {
        Self {
            tx: watch::Sender::new(false),
            at: OnceLock::new(),
        }
    }

    /// The handle a cell's declaration receives.
    pub(crate) fn subscribe(&self) -> ServiceShutdown {
        ServiceShutdown {
            rx: self.tx.subscribe(),
        }
    }

    /// Broadcast the stop. Only the first call does anything; it answers
    /// whether it was the first.
    pub(crate) fn broadcast(&self) -> bool {
        let first = self.at.set(Instant::now()).is_ok();
        if first {
            self.tx.send_replace(true);
        }
        first
    }

    /// When the stop was broadcast, if it has been.
    pub(crate) fn at(&self) -> Option<Instant> {
        self.at.get().copied()
    }

    /// Resolve once the stop has been broadcast.
    pub(crate) async fn broadcast_done(&self) {
        let mut rx = self.tx.subscribe();
        let _ = rx.wait_for(|cancelled| *cancelled).await;
    }

    /// B-6: the instant the services still running are aborted, measured
    /// from the broadcast, never from the end of the HTTP drains.
    #[must_use]
    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.at().map(service_deadline)
    }
}

/// B-6: the join bound's end for a stop broadcast at `cancelled_at`.
#[must_use]
pub fn service_deadline(cancelled_at: Instant) -> Instant {
    cancelled_at + rahi_ops::stop::SERVICE_JOIN
}

/// B-2: a declaration is refused when a name is empty or repeated, or
/// holds a character outside `[A-Za-z0-9._-]` (D-12), which keeps every
/// stop reason that names it unambiguous.
///
/// # Errors
///
/// [`Error::Config`] naming the empty, repeated or ill-formed name.
pub fn validate(services: &[ManagedService]) -> Result<()> {
    let mut seen = BTreeSet::new();
    for service in services {
        if service.name.trim().is_empty() {
            return Err(Error::Config(
                "a managed service has an empty name (spec 047 B-2)".to_owned(),
            ));
        }
        if !service
            .name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        {
            return Err(Error::Config(format!(
                "the managed service name {:?} holds a character outside [A-Za-z0-9._-] \
                 (spec 047 D-12)",
                service.name
            )));
        }
        if !seen.insert(service.name.as_str()) {
            return Err(Error::Config(format!(
                "the managed service name {:?} is declared twice (spec 047 B-2)",
                service.name
            )));
        }
    }
    Ok(())
}

/// How one service ended, as `serve` judges it (B-7, B-8).
#[derive(Debug)]
pub(crate) enum Ended {
    /// Completed `Ok(())` after the stop: normal.
    Completed,
    /// A failure, with the reason the stop record carries.
    Failed(Reason),
    /// Aborted at the bound; its timeout reason is already recorded.
    Aborted,
}

/// The running services: every join handle, owned here and nowhere else.
pub(crate) struct Running {
    /// Each service's own result, with whether the stop had been broadcast
    /// when it returned (B-7, B-8).
    set: JoinSet<(Result<()>, bool)>,
    /// Every service not yet joined, in declaration order.
    pending: Vec<(Id, String, AbortHandle)>,
    started: bool,
    timed_out: bool,
}

impl Running {
    /// B-3: spawn every declared service, once.
    pub(crate) fn start(services: Vec<ManagedService>, cancel: &std::sync::Arc<Cancel>) -> Self {
        let mut set = JoinSet::new();
        let pending: Vec<_> = services
            .into_iter()
            .map(|service| {
                let cancel = cancel.clone();
                let future = service.future;
                // The broadcast is read the instant the service returns, so an
                // early `Ok(())` stays unexpected however late it is joined.
                let handle = set.spawn(async move {
                    let result = future.await;
                    (result, cancel.at().is_some())
                });
                (handle.id(), service.name, handle)
            })
            .collect();
        Self {
            started: !pending.is_empty(),
            set,
            pending,
            timed_out: false,
        }
    }

    /// Whether any service was started.
    pub(crate) fn started(&self) -> bool {
        self.started
    }

    /// Whether every service has been joined.
    pub(crate) fn is_empty(&self) -> bool {
        self.set.is_empty()
    }

    /// Whether the bound has already aborted the stragglers.
    pub(crate) fn timed_out(&self) -> bool {
        self.timed_out
    }

    /// Join the next service to end; `None` when none is left.
    pub(crate) async fn join_next(&mut self) -> Option<(String, Ended)> {
        let joined = self.set.join_next_with_id().await?;
        let (id, ended) = match joined {
            Ok((id, (Ok(()), true))) => (id, Ended::Completed),
            Ok((id, (Ok(()), false))) => (
                id,
                Ended::Failed(Reason::ServiceExited {
                    service: String::new(),
                }),
            ),
            Ok((id, (Err(err), _))) => (
                id,
                Ended::Failed(Reason::ServiceError {
                    service: String::new(),
                    error: err.to_string(),
                }),
            ),
            Err(err) => (err.id(), classify_join_error(err)),
        };
        let name = match self.pending.iter().position(|(known, _, _)| *known == id) {
            Some(at) => self.pending.remove(at).1,
            None => format!("task {id}"),
        };
        let ended = ended.named(&name);
        Some((name, ended))
    }

    /// B-6: abort every service still running at the bound, answering their
    /// names in declaration order. Their handles are still joined through
    /// [`Self::join_next`] before the store shuts down.
    pub(crate) fn abort_remaining(&mut self) -> Vec<String> {
        self.timed_out = true;
        self.pending
            .iter()
            .filter(|(_, _, handle)| !handle.is_finished())
            .map(|(_, name, handle)| {
                handle.abort();
                name.clone()
            })
            .collect()
    }
}

fn classify_join_error(err: JoinError) -> Ended {
    if err.is_cancelled() {
        return Ended::Aborted;
    }
    let panic = err.into_panic();
    let message = panic
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a panic with a non-string payload".to_owned());
    Ended::Failed(Reason::ServicePanicked {
        service: String::new(),
        panic: message,
    })
}

impl Ended {
    fn named(self, name: &str) -> Self {
        let service = name.to_owned();
        match self {
            Self::Failed(Reason::ServiceExited { .. }) => {
                Self::Failed(Reason::ServiceExited { service })
            }
            Self::Failed(Reason::ServiceError { error, .. }) => {
                Self::Failed(Reason::ServiceError { service, error })
            }
            Self::Failed(Reason::ServicePanicked { panic, .. }) => {
                Self::Failed(Reason::ServicePanicked { service, panic })
            }
            other => other,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn named(names: &[&str]) -> Vec<ManagedService> {
        names
            .iter()
            .map(|n| ManagedService::new(*n, async { Ok(()) }))
            .collect()
    }

    #[test]
    fn names_are_non_empty_unique_and_plain() {
        validate(&named(&["outbox-worker", "embed.v2", "a_b"])).unwrap();
        for bad in [&["", "a"][..], &["a", "a"], &["a,b"], &["a}"], &["a b"]] {
            let err = validate(&named(bad)).unwrap_err();
            assert_eq!(
                err.exit_code(),
                rahi_types::error::EXIT_INFRA,
                "{bad:?}: {err}"
            );
        }
    }

    #[test]
    fn the_broadcast_is_sent_once_and_observed_by_every_clone() {
        let cancel = Cancel::new();
        let a = cancel.subscribe();
        let b = a.clone();
        assert!(!a.is_cancelled());
        assert!(cancel.broadcast());
        let first = cancel.at().unwrap();
        assert!(!cancel.broadcast(), "only the first broadcast counts");
        assert_eq!(cancel.at(), Some(first));
        assert!(a.is_cancelled() && b.is_cancelled());
        assert_eq!(cancel.deadline(), Some(service_deadline(first)));
    }
}
