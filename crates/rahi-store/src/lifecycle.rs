//! The node's lifetime as one scope (spec 048).
//!
//! hiqlite writes an unclean-stop marker, `state_machine/lock`, when the node
//! starts and removes it only inside [`Store::shutdown`]. A process that ends
//! any other way leaves it, and the next [`Store::open`] of that directory
//! refuses until an operator intervenes (B-1). Three things end a process
//! without the shutdown: an early return between open and shutdown (a `?`),
//! a panic, and a signal whose default action is to terminate.
//!
//! [`Store::run`] closes all three: it arms SIGTERM and SIGINT before the
//! node starts, hands the body a [`Stopping`] it may watch, and shuts the node
//! down on every way out of the body (B-2). [`Store::close_after`] is the same
//! guarantee for a store the caller opened itself, and [`stop_on_signal`] is
//! the armed signal on its own, for a program that needs it elsewhere.

use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use rahi_types::Error;
use tokio::sync::watch;

use crate::config::StoreConfig;
use crate::store::{Store, StoreHandle};

/// How long a body may run on after a stop is requested before it is
/// dropped and the node shut down (B-3). Spec 031 D-4's drain budget: an
/// orchestrator's termination grace covers it with room for the shutdown.
pub const STOP_GRACE: Duration = Duration::from_secs(10);

/// The prefix of the error a stopped body answers, read by [`is_stopped`].
const STOPPED: &str = "stopped:";

/// Whether an error is the one [`Stopping::bounded`] answers for a body a
/// stop ended: `Error::Io`, so exit `3`, the code of an interrupted run.
#[must_use]
pub fn is_stopped(err: &Error) -> bool {
    matches!(err, Error::Io(msg) if msg.starts_with(STOPPED))
}

/// Whether a stop has been requested, observed from anywhere in the program.
///
/// Cheap to clone; every clone sees the same request. A `Stopping` never
/// stops anything by itself: it is a question the holder asks, and
/// [`Stopping::bounded`] is the one place that acts on the answer.
#[derive(Clone, Debug)]
pub struct Stopping {
    rx: watch::Receiver<bool>,
}

impl Stopping {
    /// A stop that is never requested: for a caller that owns no signal and
    /// wants [`Store::run_until`]'s other guarantees.
    #[must_use]
    pub fn never() -> Self {
        let (_, rx) = watch::channel(false);
        Self { rx }
    }

    /// A stop requested when `stop` resolves. `stop` is driven by a task of
    /// its own from now on, so it is observed even while nothing awaits this
    /// value.
    ///
    /// # Panics
    ///
    /// Outside a Tokio runtime, as `tokio::spawn` does.
    #[must_use]
    pub fn on<F>(stop: F) -> Self
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let (tx, rx) = watch::channel(false);
        tokio::spawn(async move {
            tokio::select! {
                () = stop => {
                    let _ = tx.send(true);
                }
                // Every observer is gone: nobody is left to tell.
                () = tx.closed() => {}
            }
        });
        Self { rx }
    }

    /// Whether the stop has been requested.
    #[must_use]
    pub fn is_requested(&self) -> bool {
        *self.rx.borrow()
    }

    /// Resolve once the stop is requested; never, for [`Stopping::never`].
    /// Takes `self` so the future is `'static`: pass `stopping.clone()
    /// .requested()` to a server's graceful shutdown.
    pub async fn requested(mut self) {
        if self.rx.wait_for(|requested| *requested).await.is_err() {
            // The sender is gone without a request: none will ever come.
            std::future::pending::<()>().await;
        }
    }

    /// Run `body` to its end unless a stop is requested and `body` is still
    /// running [`STOP_GRACE`] later; then `body` is dropped at its current
    /// await and the answer is an error [`is_stopped`] recognises (B-3).
    /// When the stop was requested before this is called, `body` is never
    /// polled.
    ///
    /// Dropping `body` is what a crash at the same point would do to the
    /// store, minus the crash: a `txn` either committed or did not, and the
    /// node is still open for the caller to shut down.
    ///
    /// # Errors
    ///
    /// `body`'s own error, or the stopped error.
    pub async fn bounded<T, F>(&self, body: F) -> Result<T, Error>
    where
        F: Future<Output = Result<T, Error>>,
    {
        if self.is_requested() {
            return Err(Error::Io(format!(
                "{STOPPED} a stop was requested before the work began; it did not run"
            )));
        }
        tokio::pin!(body);
        tokio::select! {
            biased;
            done = &mut body => return done,
            () = self.clone().requested() => {}
        }
        match tokio::time::timeout(STOP_GRACE, body).await {
            Ok(done) => done,
            Err(_) => Err(Error::Io(format!(
                "{STOPPED} a stop was requested and the work was still running {STOP_GRACE:?} \
                 later; it was abandoned"
            ))),
        }
    }
}

/// SIGTERM or SIGINT, armed now (B-4).
///
/// Arming at the call, not at the first poll, is the point: a signal that
/// arrives while the node is starting is recorded rather than ending the
/// process with the marker on disk. Once armed, the process no longer
/// terminates on either signal by default for the rest of its life; the
/// program stops by returning, which is what lets the node shut down.
/// Arm it in a binary's entry point, not in a library.
///
/// # Errors
///
/// [`Error::Io`] when a handler cannot be installed, which Tokio reports
/// outside a runtime with signal support.
pub fn stop_on_signal() -> Result<Stopping, Error> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let arm = |kind, name: &str| {
            signal(kind)
                .map_err(|err| Error::Io(format!("the {name} handler cannot be installed: {err}")))
        };
        let mut term = arm(SignalKind::terminate(), "SIGTERM")?;
        let mut int = arm(SignalKind::interrupt(), "SIGINT")?;
        Ok(Stopping::on(async move {
            tokio::select! {
                _ = term.recv() => {}
                _ = int.recv() => {}
            }
        }))
    }
    #[cfg(not(unix))]
    {
        Ok(Stopping::on(async {
            let _ = tokio::signal::ctrl_c().await;
        }))
    }
}

impl Store {
    /// Open the node, run `body` on it, and shut the node down whatever
    /// `body` did: returned, failed, panicked, or outlived a SIGTERM or
    /// SIGINT by [`STOP_GRACE`] (B-2). This is the pattern spec 048 asks
    /// every program that opens a node to use.
    ///
    /// Both signals are armed before the node starts ([`stop_on_signal`]);
    /// `body` receives the [`Stopping`] to watch, so a server can drain
    /// gracefully on it and a batch job can ignore it.
    ///
    /// ```no_run
    /// # async fn f(cfg: &rahi_store::StoreConfig) -> Result<(), rahi_types::Error> {
    /// rahi_store::Store::run(cfg, |store, stopping| async move {
    ///     store.migrate(&[]).await?; // an error here still shuts the node down
    ///     stopping.requested().await; // serve until SIGTERM or SIGINT
    ///     Ok(())
    /// })
    /// .await
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// As [`Store::open`]; then `body`'s error, or the stopped error
    /// ([`is_stopped`]), or, when `body` succeeded, the shutdown's.
    pub async fn run<T, F, Fut>(cfg: &StoreConfig, body: F) -> Result<T, Error>
    where
        F: FnOnce(StoreHandle, Stopping) -> Fut,
        Fut: Future<Output = Result<T, Error>>,
    {
        Self::run_until(cfg, stop_on_signal()?, body).await
    }

    /// [`Store::run`] with the caller's [`Stopping`] instead of the signals.
    ///
    /// # Errors
    ///
    /// As [`Store::run`].
    pub async fn run_until<T, F, Fut>(
        cfg: &StoreConfig,
        stopping: Stopping,
        body: F,
    ) -> Result<T, Error>
    where
        F: FnOnce(StoreHandle, Stopping) -> Fut,
        Fut: Future<Output = Result<T, Error>>,
    {
        let store = Self::open(cfg).await?;
        let work = body(store.handle(), stopping.clone());
        store.close_after(&stopping, work).await
    }

    /// Run `body` under [`Stopping::bounded`], then shut this store down on
    /// every way out of it, a panic included: the panic resumes once the
    /// node has stopped (B-2).
    ///
    /// # Errors
    ///
    /// `body`'s error, or the stopped error; when `body` succeeded, the
    /// shutdown's. A shutdown error behind a failed `body` is written to
    /// stderr, since the body's error is the one the caller acts on.
    pub async fn close_after<T, F>(&self, stopping: &Stopping, body: F) -> Result<T, Error>
    where
        F: Future<Output = Result<T, Error>>,
    {
        let outcome = CatchUnwind(Box::pin(stopping.bounded(body))).await;
        let stopped = self.shutdown().await;
        let say = |err: &Error| {
            eprintln!("rahi-store: the shutdown after a failed body did not confirm: {err}");
        };
        match outcome {
            Ok(Ok(value)) => stopped.map(|()| value),
            Ok(Err(err)) => {
                if let Err(shut) = &stopped {
                    say(shut);
                }
                Err(err)
            }
            Err(panic) => {
                if let Err(shut) = &stopped {
                    say(shut);
                }
                std::panic::resume_unwind(panic)
            }
        }
    }
}

/// A future whose panic is caught at its poll, so the node can be shut down
/// before the panic goes on.
struct CatchUnwind<F>(Pin<Box<F>>);

impl<F: Future> Future for CatchUnwind<F> {
    type Output = std::thread::Result<F::Output>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let inner = self.get_mut().0.as_mut();
        match std::panic::catch_unwind(AssertUnwindSafe(|| inner.poll(cx))) {
            Ok(Poll::Pending) => Poll::Pending,
            Ok(Poll::Ready(done)) => Poll::Ready(Ok(done)),
            Err(panic) => Poll::Ready(Err(panic)),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn a_body_that_finishes_answers_its_own_result() {
        let stopping = Stopping::never();
        assert_eq!(stopping.bounded(async { Ok(7) }).await.unwrap(), 7);
        let err = stopping
            .bounded(async { Err::<(), _>(Error::Validation("no".to_owned())) })
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Validation(_)));
        assert!(!is_stopped(&err));
    }

    #[tokio::test(start_paused = true)]
    async fn a_stop_before_the_body_never_polls_it() {
        let stopping = Stopping::on(async {});
        tokio::task::yield_now().await;
        assert!(stopping.is_requested());
        let err = stopping
            .bounded::<(), _>(async { panic!("the body was polled") })
            .await
            .unwrap_err();
        assert!(is_stopped(&err), "{err}");
        assert_eq!(err.exit_code(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn a_body_that_finishes_within_the_grace_keeps_its_result() {
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let stopping = Stopping::on(async {
            let _ = rx.await;
        });
        let watch = stopping.clone();
        let done = stopping.bounded(async move {
            let _ = tx.send(());
            watch.requested().await;
            tokio::time::sleep(STOP_GRACE / 2).await;
            Ok("drained")
        });
        assert_eq!(done.await.unwrap(), "drained");
    }

    #[tokio::test(start_paused = true)]
    async fn a_body_that_outlives_the_grace_is_abandoned() {
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let stopping = Stopping::on(async {
            let _ = rx.await;
        });
        let started = tokio::time::Instant::now();
        let err = stopping
            .bounded(async move {
                let _ = tx.send(());
                std::future::pending::<Result<(), Error>>().await
            })
            .await
            .unwrap_err();
        assert!(is_stopped(&err), "{err}");
        assert_eq!(started.elapsed(), STOP_GRACE);
    }

    #[tokio::test(start_paused = true)]
    async fn never_is_never_requested() {
        let stopping = Stopping::never();
        let waited = tokio::time::timeout(Duration::from_secs(3600), stopping.requested()).await;
        assert!(waited.is_err());
    }

    #[test]
    fn a_panic_is_caught_at_the_poll() {
        let caught = futures_poll_once(CatchUnwind(Box::pin(async { panic!("boom") })));
        let payload = caught.map(|r: std::thread::Result<()>| r.unwrap_err());
        assert_eq!(payload.unwrap().downcast_ref::<&str>(), Some(&"boom"));
    }

    fn futures_poll_once<F: Future>(fut: F) -> Option<F::Output> {
        let mut fut = Box::pin(fut);
        let mut cx = Context::from_waker(std::task::Waker::noop());
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(out) => Some(out),
            Poll::Pending => None,
        }
    }
}
