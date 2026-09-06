//! Key-only local notify (spec 012 B-3).
//!
//! hiqlite's notify is one global stream shared by every listener on the
//! node, and the cache group replays it after a restart. Neither fact can be
//! hidden, so the envelope is shaped to survive them: it carries a routing
//! key and a revision and nothing else, a consumer re-reads the store when it
//! sees one, and a consumer that misses one finds the same change through
//! [`crate::Watermark::since`]. Notify is a latency hint; the revision column
//! is truth.
//!
//! Delivery is local: the `listen_notify_local` feature keeps events inside
//! the node (spec 011 B-6), and the shared stream hands each event to exactly
//! one listener, so a process that runs several consumers gives each of them
//! its own poll loop rather than relying on the stream to fan out.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures_core::Stream;
use rahi_types::{Error, Revision};
use serde::{Deserialize, Serialize};

use crate::error::map;
use crate::store::StoreHandle;

/// What changed, as a routing key.
///
/// Key-only by construction: there is no payload field and no way to add one
/// without changing this type. A consumer that needs the resource reads it
/// back at or above `revision`; a payload on a lossy, replayed, single-reader
/// stream would be a second source of truth.
///
/// The four fields are the whole envelope (spec 012 FR-005):
///
/// ```
/// # use rahi_store::Envelope;
/// # use rahi_types::Revision;
/// let env = Envelope::new("note", None, "n-1", Revision::new(7));
/// assert_eq!((env.kind.as_str(), env.name.as_str()), ("note", "n-1"));
/// assert_eq!((env.tenant, env.revision), (None, Revision::new(7)));
/// ```
///
/// and a payload is not one of them, which the compiler enforces:
///
/// ```compile_fail
/// # use rahi_store::Envelope;
/// # fn payload_of(env: Envelope) {
/// let _ = env.payload;
/// # }
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    /// The resource class, for example `note`. Consumers filter on it.
    pub kind: String,
    /// The tenant the resource belongs to, when the app is multi-tenant.
    pub tenant: Option<String>,
    /// The resource's name or id within its kind.
    pub name: String,
    /// The revision the change was stamped with.
    pub revision: Revision,
}

impl Envelope {
    /// Build an envelope.
    #[must_use]
    pub fn new(
        kind: impl Into<String>,
        tenant: Option<String>,
        name: impl Into<String>,
        revision: Revision,
    ) -> Self {
        Self {
            kind: kind.into(),
            tenant,
            name: name.into(),
            revision,
        }
    }
}

/// The publish and subscribe half of the coordination plane.
///
/// Cheap to clone: it holds a [`StoreHandle`].
#[derive(Clone, Debug)]
pub struct Notify {
    store: StoreHandle,
}

impl Notify {
    /// Wrap a store handle.
    #[must_use]
    pub fn new(store: StoreHandle) -> Self {
        Self { store }
    }

    /// Publish an envelope to this node's listeners.
    ///
    /// Outside any transaction by construction: SQL and notify live in
    /// different Raft groups, so a durable write publishes through
    /// [`crate::Outbox`] instead of calling this directly.
    ///
    /// # Errors
    ///
    /// [`Error::Upstream`] when the cache group refuses the write. A failed
    /// publish is not a failed change: the revision is already durable.
    pub async fn notify(&self, env: Envelope) -> Result<(), Error> {
        self.store.client().notify(&env).await.map_err(map)
    }

    /// Subscribe to this node's envelopes.
    ///
    /// The returned [`Listen`] implements [`Stream<Item = Envelope>`](Stream)
    /// and yields only events published after this process started, because
    /// the cache group replays older ones on restart. It is a hint stream: a
    /// consumer polls [`crate::Watermark::since`] on every tick whether or
    /// not an envelope arrived.
    #[must_use]
    pub fn listen(&self) -> Listen {
        Listen {
            store: self.store.clone(),
            pending: None,
        }
    }
}

type Pending = Pin<Box<dyn Future<Output = Result<Envelope, hiqlite::Error>> + Send>>;

/// A subscription to this node's envelopes, from [`Notify::listen`].
///
/// Ends when the cache group closes or an event on the shared stream does not
/// decode as an [`Envelope`]. Ending is survivable by design: the consumer's
/// watermark poll is what guarantees it sees every revision.
pub struct Listen {
    store: StoreHandle,
    pending: Option<Pending>,
}

impl std::fmt::Debug for Listen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Listen")
    }
}

impl Listen {
    /// The next envelope, or `None` when the stream has ended.
    ///
    /// The same event as the [`Stream`] impl yields, for consumers that do
    /// not want a stream combinator.
    pub async fn recv(&mut self) -> Option<Envelope> {
        self.store
            .client()
            .listen_after_start::<Envelope>()
            .await
            .ok()
    }
}

impl Stream for Listen {
    type Item = Envelope;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Envelope>> {
        let this = self.get_mut();
        if this.pending.is_none() {
            let store = this.store.clone();
            this.pending = Some(Box::pin(async move {
                store.client().listen_after_start::<Envelope>().await
            }));
        }
        let Some(pending) = this.pending.as_mut() else {
            return Poll::Ready(None);
        };
        match pending.as_mut().poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(res) => {
                this.pending = None;
                Poll::Ready(res.ok())
            }
        }
    }
}
