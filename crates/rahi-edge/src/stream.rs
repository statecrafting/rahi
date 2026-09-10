//! Streaming responses (spec 026): server-sent events, declared per route,
//! bounded and cancellable.
//!
//! A stream is not a route feature; it is an exemption from the defaults
//! the edge holds for a request that answers in milliseconds. So a route
//! declares itself streaming ([`StreamRoutes::stream_route`]), the builder
//! puts one gate over every mount ([`enforce`]), and the gate is where the
//! exemptions of B-4 are applied to declared routes and refused to
//! undeclared ones (B-3): a streaming response from a route nobody
//! declared is a 500 with a Decision, not a stream that misbehaves
//! silently.
//!
//! Each stream is one bounded channel ([`channel`]), a [`Producer`] the
//! handler's task writes to, and a [`Receiver`] that [`stream`] turns into
//! the response. Backpressure is a closed stream (`event: overflow`) and a
//! cancelled producer, never a growing heap (B-6). A dropped client cancels
//! the producer (B-7); a shutdown tells every open stream so
//! ([`StreamHub::drain`]) and waits a bounded time for them to close. One
//! span and four metric families per stream (B-9).

use std::collections::{BTreeSet, HashMap};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::{MatchedPath, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::sse::{self, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::MethodRouter;
use futures_core::Stream;
use rahi_kernel::Kernel;
use rahi_types::{Error, Principal, Sub};
use serde_json::json;
use tokio::sync::{Notify, mpsc};

use crate::error;
use crate::middleware::rate_limit::ClientResolver;

/// How often an idle stream carries a comment keep-alive (B-2).
pub const KEEP_ALIVE: Duration = Duration::from_secs(15);

/// The keep-alive comment's text.
pub const KEEP_ALIVE_TEXT: &str = "keep-alive";

/// How many streams one identity may hold open (B-5).
pub const DEFAULT_MAX_CONCURRENT_STREAMS: usize = 4;

/// How many events a stream's channel holds before it overflows (B-6).
pub const DEFAULT_CHANNEL_CAPACITY: usize = 64;

/// How long a shutdown waits for open streams to close (B-7).
pub const DEFAULT_DRAIN_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a declared route may take to produce its response: the request
/// timeout, applied to the time before the first byte only (B-4).
pub const DEFAULT_FIRST_BYTE_TIMEOUT: Duration = Duration::from_secs(30);

/// The event a stream closes with when its consumer fell behind (B-6).
pub const EVENT_OVERFLOW: &str = "overflow";

/// The event every open stream receives at shutdown (B-7).
pub const EVENT_SHUTDOWN: &str = "shutdown";

/// The decision kind for a stream refused over the concurrency budget.
pub const DECISION_STREAM_LIMIT: &str = "edge.stream.limit";

/// The decision kind for a streaming response from an undeclared route.
pub const DECISION_STREAM_UNDECLARED: &str = "edge.stream.undeclared";

/// The actor a decision names when no principal is on the request.
pub const ANONYMOUS: &str = "anonymous";

/// One server-sent event (B-2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    /// The `event` field.
    pub event: String,
    /// The `data` field.
    pub data: String,
    /// The `id` field, monotonic when the producer supplies one (B-8).
    pub id: Option<u64>,
}

impl Event {
    /// An event with no id.
    #[must_use]
    pub fn new(event: impl Into<String>, data: impl Into<String>) -> Self {
        Self {
            event: event.into(),
            data: data.into(),
            id: None,
        }
    }

    /// The same event, carrying `id`.
    #[must_use]
    pub const fn with_id(mut self, id: u64) -> Self {
        self.id = Some(id);
        self
    }

    fn into_sse(self) -> sse::Event {
        let mut event = sse::Event::default().event(self.event).data(self.data);
        if let Some(id) = self.id {
            event = event.id(id.to_string());
        }
        event
    }
}

/// How a stream ended (B-9).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The producer finished and every event was delivered.
    Complete,
    /// The client went away.
    ClientGone,
    /// The consumer fell behind the bounded channel.
    Overflow,
    /// The cell shut down.
    Shutdown,
}

impl Outcome {
    /// Every outcome, in the order the metrics are initialised.
    pub const ALL: [Self; 4] = [
        Self::Complete,
        Self::ClientGone,
        Self::Overflow,
        Self::Shutdown,
    ];

    /// The label value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::ClientGone => "client_gone",
            Self::Overflow => "overflow",
            Self::Shutdown => "shutdown",
        }
    }
}

/// The numbers a deployment can multiply (B-5, B-6, B-7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamOptions {
    /// Open streams per identity before a 429.
    pub max_concurrent_streams: usize,
    /// Events a channel holds before it overflows.
    pub channel_capacity: usize,
    /// How long a shutdown waits for streams to close.
    pub drain_timeout: Duration,
    /// How long a declared route may take to answer.
    pub first_byte_timeout: Duration,
    /// How often an idle stream carries a keep-alive.
    pub keep_alive: Duration,
}

impl Default for StreamOptions {
    fn default() -> Self {
        Self {
            max_concurrent_streams: DEFAULT_MAX_CONCURRENT_STREAMS,
            channel_capacity: DEFAULT_CHANNEL_CAPACITY,
            drain_timeout: DEFAULT_DRAIN_TIMEOUT,
            first_byte_timeout: DEFAULT_FIRST_BYTE_TIMEOUT,
            keep_alive: KEEP_ALIVE,
        }
    }
}

/// Why a producer can no longer emit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Closed {
    /// The channel was full: the stream is closing with `event: overflow`.
    Overflow,
    /// The stream is gone: the client left, or the cell is shutting down.
    Cancelled,
}

impl std::fmt::Display for Closed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Overflow => "the stream overflowed its channel",
            Self::Cancelled => "the stream was cancelled",
        })
    }
}

/// What both ends of a stream share.
struct Shared {
    state: Mutex<StreamState>,
    /// Wakes a producer waiting in [`Producer::cancelled`].
    producer: Notify,
    events: AtomicU64,
    keep_alive: Duration,
}

struct StreamState {
    overflow: bool,
    shutdown: bool,
    closed: bool,
    /// The response side's waker, so a shutdown reaches an idle stream.
    waker: Option<Waker>,
    attachment: Option<Attachment>,
}

struct Attachment {
    hub: StreamHub,
    identity: String,
    route: String,
    started: Instant,
    span: tracing::Span,
}

impl Shared {
    fn new(keep_alive: Duration) -> Self {
        Self {
            state: Mutex::new(StreamState {
                overflow: false,
                shutdown: false,
                closed: false,
                waker: None,
                attachment: None,
            }),
            producer: Notify::new(),
            events: AtomicU64::new(0),
            keep_alive,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, StreamState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn is_cancelled(&self) -> bool {
        let state = self.lock();
        state.overflow || state.shutdown || state.closed
    }

    /// Flip a flag and wake both ends.
    fn signal(&self, set: impl FnOnce(&mut StreamState)) {
        let waker = {
            let mut state = self.lock();
            set(&mut state);
            state.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
        self.producer.notify_waiters();
    }

    /// Close the response side with `outcome`, once.
    fn close(&self, outcome: Outcome) {
        let attachment = {
            let mut state = self.lock();
            if state.closed {
                return;
            }
            state.closed = true;
            state.attachment.take()
        };
        self.producer.notify_waiters();
        if let Some(attachment) = attachment {
            let elapsed = attachment.started.elapsed();
            let events = self.events.load(Ordering::Relaxed);
            attachment.span.record("outcome", outcome.as_str());
            attachment.span.record("events", events);
            attachment.hub.release(&attachment.identity);
            if let Some(obs) = crate::obs::current() {
                obs.metrics().record_stream_closed(outcome, elapsed, events);
            }
            tracing::info!(
                target: "rahi.stream",
                route = %attachment.route,
                outcome = outcome.as_str(),
                events,
                duration_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
                "stream closed"
            );
        }
    }
}

/// The handler's end of a stream: where events go in.
#[derive(Clone)]
pub struct Producer {
    tx: mpsc::Sender<Event>,
    shared: Arc<Shared>,
}

impl std::fmt::Debug for Producer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Producer")
            .field("cancelled", &self.is_cancelled())
            .finish_non_exhaustive()
    }
}

impl Producer {
    /// Emit one event without waiting.
    ///
    /// # Errors
    ///
    /// [`Closed::Overflow`] when the channel is full: the stream is closed
    /// with `event: overflow` and this producer is cancelled (B-6).
    /// [`Closed::Cancelled`] when the stream is already gone.
    pub fn emit(&self, event: Event) -> Result<(), Closed> {
        if self.shared.is_cancelled() {
            return Err(Closed::Cancelled);
        }
        match self.tx.try_send(event) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.shared.signal(|state| state.overflow = true);
                Err(Closed::Overflow)
            }
            Err(mpsc::error::TrySendError::Closed(_)) => Err(Closed::Cancelled),
        }
    }

    /// Whether the stream has been closed, overflowed, or shut down.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.shared.is_cancelled() || self.tx.is_closed()
    }

    /// Resolves once the stream is cancelled: the cancellation token a
    /// producer task holds (B-7).
    pub async fn cancelled(&self) {
        loop {
            if self.is_cancelled() {
                return;
            }
            let notified = self.shared.producer.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.is_cancelled() {
                return;
            }
            tokio::select! {
                () = &mut notified => {},
                () = self.tx.closed() => {},
            }
        }
    }
}

/// The response's end of a stream: what [`stream`] turns into a body.
pub struct Receiver {
    rx: mpsc::Receiver<Event>,
    shared: Arc<Shared>,
}

impl std::fmt::Debug for Receiver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Receiver").finish_non_exhaustive()
    }
}

/// One stream's channel, `capacity` events deep (B-6), with the default
/// keep-alive.
#[must_use]
pub fn channel(capacity: usize) -> (Producer, Receiver) {
    channel_with(capacity, KEEP_ALIVE)
}

/// [`channel`] with the keep-alive interval named.
#[must_use]
pub fn channel_with(capacity: usize, keep_alive: Duration) -> (Producer, Receiver) {
    let (tx, rx) = mpsc::channel(capacity.max(1));
    let shared = Arc::new(Shared::new(keep_alive));
    (
        Producer {
            tx,
            shared: shared.clone(),
        },
        Receiver { rx, shared },
    )
}

/// The marker a streaming response carries in its extensions, which is how
/// the gate tells a stream from any other body (B-3).
#[derive(Clone)]
struct Marker(Arc<Shared>);

/// B-2: the streaming response over `rx`.
#[must_use]
pub fn stream(rx: Receiver) -> Response {
    let shared = rx.shared.clone();
    let keep_alive = shared.keep_alive;
    let source = Source {
        rx: rx.rx,
        shared: shared.clone(),
        ended: false,
    };
    let mut response = Sse::new(source)
        .keep_alive(KeepAlive::new().interval(keep_alive).text(KEEP_ALIVE_TEXT))
        .into_response();
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(header::CONNECTION, HeaderValue::from_static("keep-alive"));
    headers.insert(
        header::HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    response.extensions_mut().insert(Marker(shared));
    response
}

/// The body: events until the channel ends, an overflow, or a shutdown.
struct Source {
    rx: mpsc::Receiver<Event>,
    shared: Arc<Shared>,
    ended: bool,
}

impl Stream for Source {
    type Item = Result<sse::Event, std::convert::Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.ended {
            return Poll::Ready(None);
        }
        let terminal = {
            let state = self.shared.lock();
            if state.shutdown {
                Some((EVENT_SHUTDOWN, Outcome::Shutdown))
            } else if state.overflow {
                Some((EVENT_OVERFLOW, Outcome::Overflow))
            } else {
                None
            }
        };
        if let Some((name, outcome)) = terminal {
            self.ended = true;
            self.rx.close();
            self.shared.close(outcome);
            return Poll::Ready(Some(Ok(sse::Event::default().event(name))));
        }
        match self.rx.poll_recv(cx) {
            Poll::Ready(Some(event)) => {
                self.shared.events.fetch_add(1, Ordering::Relaxed);
                Poll::Ready(Some(Ok(event.into_sse())))
            }
            Poll::Ready(None) => {
                self.ended = true;
                self.shared.close(Outcome::Complete);
                Poll::Ready(None)
            }
            Poll::Pending => {
                self.shared.lock().waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        self.shared.close(Outcome::ClientGone);
    }
}

/// Every declared streaming route in this process: the route table's
/// streaming column (B-3).
static DECLARED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// Record `path` as a streaming route.
pub fn declare(path: &str) {
    DECLARED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(path.to_owned());
}

/// Every declared route pattern, sorted.
#[must_use]
pub fn declared() -> Vec<String> {
    DECLARED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .cloned()
        .collect()
}

/// `Router::stream_route` (B-3): register a route and declare it streaming.
///
/// The declaration is bound to the route itself, not to its path: the
/// route carries the layer that admits, times, and attaches its stream,
/// so a route with the same path under another mount is not mistaken for
/// it. The path is also recorded in the streaming table for the route
/// table's sake.
pub trait StreamRoutes<S> {
    /// Register `handler` at `path` and record the route as streaming.
    #[must_use]
    fn stream_route(self, path: &str, handler: MethodRouter<S>) -> Self;
}

impl<S> StreamRoutes<S> for Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    fn stream_route(self, path: &str, handler: MethodRouter<S>) -> Self {
        declare(path);
        self.route(
            path,
            handler.layer(axum::middleware::from_fn(declared_route)),
        )
    }
}

/// Resolves the identity a stream counts against (B-5), or `None` to fall
/// back to the principal's subject and then the client address.
///
/// The composer sets one that reads the bearer credential spec 025 leaves
/// on the request, whose `(client_id, sub)` pair this crate cannot name
/// without a dependency spec 020 AC-2 forbids.
pub type StreamIdentityResolver = Arc<dyn Fn(&Request) -> Option<String> + Send + Sync>;

/// Every open stream in the process, the concurrency budget, and the
/// shutdown (B-5, B-7).
#[derive(Clone)]
pub struct StreamHub {
    inner: Arc<HubInner>,
}

struct HubInner {
    options: StreamOptions,
    open: Mutex<HashMap<String, usize>>,
    streams: Mutex<Vec<Weak<Shared>>>,
    drained: Notify,
}

impl std::fmt::Debug for StreamHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamHub")
            .field("options", &self.inner.options)
            .field("open", &self.open_total())
            .finish()
    }
}

impl Default for StreamHub {
    fn default() -> Self {
        Self::new(StreamOptions::default())
    }
}

impl StreamHub {
    /// A hub with `options`.
    #[must_use]
    pub fn new(options: StreamOptions) -> Self {
        Self {
            inner: Arc::new(HubInner {
                options,
                open: Mutex::new(HashMap::new()),
                streams: Mutex::new(Vec::new()),
                drained: Notify::new(),
            }),
        }
    }

    /// The options.
    #[must_use]
    pub fn options(&self) -> &StreamOptions {
        &self.inner.options
    }

    /// A channel with this hub's capacity and keep-alive.
    #[must_use]
    pub fn channel(&self) -> (Producer, Receiver) {
        channel_with(
            self.inner.options.channel_capacity,
            self.inner.options.keep_alive,
        )
    }

    /// Streams `identity` holds open.
    #[must_use]
    pub fn open(&self, identity: &str) -> usize {
        self.inner
            .open
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(identity)
            .copied()
            .unwrap_or(0)
    }

    /// Streams open across every identity.
    #[must_use]
    pub fn open_total(&self) -> usize {
        self.inner
            .open
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .sum()
    }

    fn admits(&self, identity: &str) -> bool {
        self.open(identity) < self.inner.options.max_concurrent_streams
    }

    fn attach(&self, shared: &Arc<Shared>, identity: String, route: String) {
        {
            let mut open = self
                .inner
                .open
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            *open.entry(identity.clone()).or_insert(0) += 1;
        }
        {
            let mut streams = self
                .inner
                .streams
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            streams.retain(|weak| weak.strong_count() > 0);
            streams.push(Arc::downgrade(shared));
        }
        let span = tracing::info_span!(
            "edge.stream",
            route = %route,
            identity = %identity,
            outcome = tracing::field::Empty,
            events = tracing::field::Empty,
        );
        if let Some(obs) = crate::obs::current() {
            obs.metrics().record_stream_open();
        }
        shared.lock().attachment = Some(Attachment {
            hub: self.clone(),
            identity,
            route,
            started: Instant::now(),
            span,
        });
    }

    fn release(&self, identity: &str) {
        let mut open = self
            .inner
            .open
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(count) = open.get_mut(identity) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                open.remove(identity);
            }
        }
        drop(open);
        self.inner.drained.notify_waiters();
    }

    /// B-7: tell every open stream the cell is shutting down, wait up to the
    /// drain timeout for them to close, and report how many were still open.
    pub async fn drain(&self) -> usize {
        let deadline = tokio::time::Instant::now() + self.inner.options.drain_timeout;
        loop {
            // Re-read the live set on every pass: a stream that attached
            // after the previous pass is told too.
            for shared in self.live() {
                shared.signal(|state| state.shutdown = true);
            }
            if self.open_total() == 0 {
                return 0;
            }
            let notified = self.inner.drained.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.open_total() == 0 {
                return 0;
            }
            tokio::select! {
                () = &mut notified => {},
                () = tokio::time::sleep_until(deadline) => {
                    let remaining = self.open_total();
                    for shared in self.live() {
                        shared.close(Outcome::Shutdown);
                    }
                    return remaining;
                }
            }
        }
    }

    fn live(&self) -> Vec<Arc<Shared>> {
        let mut streams = self
            .inner
            .streams
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        streams.retain(|weak| weak.strong_count() > 0);
        streams.iter().filter_map(Weak::upgrade).collect()
    }
}

/// What the builder's gate hands a declared route through the request:
/// the hub, the kernel, and how an identity is resolved.
#[derive(Clone)]
struct GateContext {
    hub: StreamHub,
    kernel: Kernel,
    client: ClientResolver,
    identity: Option<StreamIdentityResolver>,
}

/// Left on a response by a declared route's layer, so the builder's gate
/// knows the stream was admitted where it was declared.
#[derive(Clone, Copy)]
struct Declared;

/// The gate the builder puts over every mount (B-3, B-4, B-5).
#[derive(Clone)]
pub struct StreamGate {
    context: GateContext,
}

impl StreamGate {
    /// A gate over `hub`, ledgering through `kernel`, counting by
    /// `identity` and then by the principal or `client`.
    #[must_use]
    pub fn new(
        hub: StreamHub,
        kernel: Kernel,
        client: ClientResolver,
        identity: Option<StreamIdentityResolver>,
    ) -> Self {
        Self {
            context: GateContext {
                hub,
                kernel,
                client,
                identity,
            },
        }
    }
}

impl GateContext {
    fn identity_of(&self, request: &Request) -> String {
        if let Some(resolver) = &self.identity
            && let Some(identity) = resolver(request)
        {
            return identity;
        }
        if let Some(principal) = request.extensions().get::<Principal>() {
            return principal.sub.as_str().to_owned();
        }
        let (parts, ()) = request_parts(request);
        (self.client)(&parts)
    }
}

/// The request's parts, cloned, for a resolver that wants `Parts`.
fn request_parts(request: &Request) -> (axum::http::request::Parts, ()) {
    let mut builder = axum::http::Request::builder()
        .method(request.method().clone())
        .uri(request.uri().clone())
        .version(request.version());
    if let Some(headers) = builder.headers_mut() {
        *headers = request.headers().clone();
    }
    let mut cloned = builder.body(()).unwrap_or_default();
    *cloned.extensions_mut() = request.extensions().clone();
    cloned.into_parts()
}

fn route_of(request: &Request) -> String {
    request.extensions().get::<MatchedPath>().map_or_else(
        || request.uri().path().to_owned(),
        |m| m.as_str().to_owned(),
    )
}

fn actor_of(request: &Request) -> Sub {
    request
        .extensions()
        .get::<Principal>()
        .map_or_else(|| Sub::new(ANONYMOUS), |p| p.sub.clone())
}

/// The builder's gate: hands the context down, and refuses a stream that
/// came back from a route nobody declared (B-3).
pub async fn enforce(State(gate): State<StreamGate>, mut request: Request, next: Next) -> Response {
    let route = route_of(&request);
    let actor = actor_of(&request);
    request.extensions_mut().insert(gate.context.clone());
    let response = next.run(request).await;
    if response.extensions().get::<Declared>().is_some()
        || response.extensions().get::<Marker>().is_none()
    {
        return response;
    }
    // The body is dropped, which closes the stream, and the refusal is
    // ledgered against whoever asked.
    drop(response);
    let mut payload = serde_json::Map::new();
    payload.insert("route".to_owned(), json!(route));
    let reason =
        format!("route {route} returned a streaming response but was not declared streaming");
    let id =
        gate.context
            .kernel
            .refuse(DECISION_STREAM_UNDECLARED, &actor, reason.clone(), payload);
    error::build(
        StatusCode::INTERNAL_SERVER_ERROR,
        error::body(&Error::Denied(format!("{id}: {reason}"))),
    )
}

/// A declared route's own layer (B-4, B-5): admit against the budget, hold
/// the handler to the first-byte budget, and attach the stream to the hub.
async fn declared_route(request: Request, next: Next) -> Response {
    let Some(context) = request.extensions().get::<GateContext>().cloned() else {
        // No builder gate above this route: the stream is served without a
        // budget, which is what a bare router asked for.
        let mut response = next.run(request).await;
        response.extensions_mut().insert(Declared);
        return response;
    };
    let route = route_of(&request);
    let identity = context.identity_of(&request);
    if !context.hub.admits(&identity) {
        let limit = context.hub.options().max_concurrent_streams;
        let mut payload = serde_json::Map::new();
        payload.insert("route".to_owned(), json!(route));
        payload.insert("identity".to_owned(), json!(identity));
        payload.insert("limit".to_owned(), json!(limit));
        let reason = format!("identity holds {limit} open streams, the ceiling");
        let id = context.kernel.refuse(
            DECISION_STREAM_LIMIT,
            &actor_of(&request),
            reason.clone(),
            payload,
        );
        let mut response = error::build(
            StatusCode::TOO_MANY_REQUESTS,
            error::body(&Error::Denied(format!("{id}: {reason}"))),
        );
        response.extensions_mut().insert(Declared);
        let retry_after = context.hub.options().keep_alive.as_secs().max(1);
        if let Ok(value) = HeaderValue::from_str(&retry_after.to_string()) {
            response.headers_mut().insert(header::RETRY_AFTER, value);
        }
        return response;
    }

    let first_byte = context.hub.options().first_byte_timeout;
    let mut response = match tokio::time::timeout(first_byte, next.run(request)).await {
        Ok(response) => response,
        Err(_) => error::refusal(
            StatusCode::GATEWAY_TIMEOUT,
            "stream_first_byte_timeout",
            "the streaming route did not answer within its first-byte budget",
        ),
    };
    if let Some(Marker(shared)) = response.extensions_mut().remove::<Marker>() {
        context.hub.attach(&shared, identity, route);
    }
    response.extensions_mut().insert(Declared);
    response
}
