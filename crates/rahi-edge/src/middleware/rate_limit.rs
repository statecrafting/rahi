//! The fixed-window rate limiter (spec 020 B-5).
//!
//! One counter per client identity per route group per window, kept in the
//! store's cache group. **The ceiling is per node.** hiqlite's cache group is
//! memory-resident and this crate never coordinates across replicas, so a
//! cell running N replicas admits N times the declared ceiling; a limit that
//! must hold for the cluster is not this.
//!
//! The counter's window is the key, not a TTL: the store's counters take no
//! expiry (spec 011), so the window ordinal is part of the counter's name and
//! a spent window is simply never named again. Nothing is durable here, which
//! is the point: constitution IX puts rate limits in the derived group
//! precisely because losing one changes no decision.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{ConnectInfo, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;
use rahi_store::StoreHandle;

use crate::error;

/// The ceiling of the group every request falls into unless the app declares
/// a narrower one.
pub const DEFAULT_LIMIT: u32 = 300;
/// The window, in seconds. One minute, and not configurable: the ceilings are.
pub const WINDOW_SECONDS: u64 = 60;
/// The name of the group a path matches no declared prefix in.
pub const DEFAULT_GROUP: &str = "default";
/// The cache-key prefix every counter this module writes carries.
pub const KEY_PREFIX: &str = "rl";
/// The identity a request with no resolvable client address is counted under.
pub const UNKNOWN_IDENTITY: &str = "unknown";

/// How a request's client identity is resolved.
///
/// Spec 024 supplies the one the router uses:
/// [`client_identity::from_config`](crate::client_identity::from_config),
/// which reads `X-Forwarded-For` only as far as the operator's declared
/// trusted hops. [`peer_address_resolver`] remains the answer for a cell with
/// no proxy in front of it, because a header a client can set is not an
/// identity.
pub type ClientResolver = Arc<dyn Fn(&Parts) -> String + Send + Sync>;

/// The clock the window ordinal is read from. Injectable so a test can hold
/// a window still rather than race its boundary.
pub type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

/// The peer address of the connection, when axum was given one.
///
/// A router served with `into_make_service_with_connect_info` carries a
/// [`ConnectInfo`]; one driven directly by a test or by a unix socket does
/// not, and every such request shares [`UNKNOWN_IDENTITY`].
#[must_use]
pub fn peer_address(parts: &Parts) -> String {
    parts
        .extensions
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map_or_else(
            || UNKNOWN_IDENTITY.to_owned(),
            |ConnectInfo(addr)| addr.ip().to_string(),
        )
}

/// The default resolver: [`peer_address`].
#[must_use]
pub fn peer_address_resolver() -> ClientResolver {
    Arc::new(peer_address)
}

/// The clock the limiter uses unless one is injected: the system clock,
/// truncated to whole seconds.
#[must_use]
pub fn system_clock() -> Clock {
    Arc::new(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_secs())
    })
}

/// The ceilings the app declares, per route group.
///
/// A group is a path prefix; the longest declared prefix that a request's
/// path starts with wins, and a path that matches none falls into
/// [`DEFAULT_GROUP`] at [`DEFAULT_LIMIT`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RateLimits {
    default: u32,
    groups: Vec<(String, u32)>,
}

impl RateLimits {
    /// The default ceiling and no declared groups.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            default: DEFAULT_LIMIT,
            groups: Vec::new(),
        }
    }

    /// Set the ceiling of the default group.
    #[must_use]
    pub const fn default_limit(mut self, per_minute: u32) -> Self {
        self.default = per_minute;
        self
    }

    /// Declare `per_minute` for every path under `prefix`.
    #[must_use]
    pub fn group(mut self, prefix: &str, per_minute: u32) -> Self {
        let prefix = prefix.to_owned();
        self.groups.retain(|(declared, _)| declared != &prefix);
        self.groups.push((prefix, per_minute));
        self
    }

    /// The group `path` falls into and the ceiling that group carries.
    #[must_use]
    pub fn limit_for<'s>(&'s self, path: &str) -> (&'s str, u32) {
        self.groups
            .iter()
            .filter(|(prefix, _)| path.starts_with(prefix.as_str()))
            .max_by_key(|(prefix, _)| prefix.len())
            .map_or((DEFAULT_GROUP, self.default), |(prefix, limit)| {
                (prefix.as_str(), *limit)
            })
    }
}

impl Default for RateLimits {
    fn default() -> Self {
        Self::new()
    }
}

/// The layer's state: where the counters live, what the ceilings are, who the
/// client is, and what time it is.
#[derive(Clone)]
pub struct RateLimiter {
    store: StoreHandle,
    limits: Arc<RateLimits>,
    resolver: ClientResolver,
    clock: Clock,
}

impl std::fmt::Debug for RateLimiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RateLimiter")
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl RateLimiter {
    /// A limiter over `store` with `limits`, the peer-address resolver, and
    /// the system clock.
    #[must_use]
    pub fn new(store: StoreHandle, limits: RateLimits) -> Self {
        Self {
            store,
            limits: Arc::new(limits),
            resolver: peer_address_resolver(),
            clock: system_clock(),
        }
    }

    /// Resolve the client identity with `resolver` (spec 024).
    #[must_use]
    pub fn with_resolver(mut self, resolver: ClientResolver) -> Self {
        self.resolver = resolver;
        self
    }

    /// Read the window ordinal from `clock`.
    #[must_use]
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    /// The cache key of one client's window in one group.
    #[must_use]
    pub fn key(group: &str, identity: &str, window: u64) -> String {
        format!("{KEY_PREFIX}:{group}:{identity}:{window}")
    }
}

/// Count the request, and refuse it when its window is spent.
///
/// **A limiter that cannot count does not admit** (spec 024 B-5). A store
/// failure answers 503, not 200. The counters are derived state whose loss
/// changes no decision (constitution IX), and the earlier reading of that was
/// that losing them should cost nothing; but a ceiling that disappears the
/// moment its store blinks is a ceiling an attacker can remove by making the
/// store blink, which is exactly the silent fail-open enrahitu's exposure
/// review found (enrahitu://025). The answer says the cell is temporarily
/// unable to serve, which is true, and it is the honest cost of the ceiling
/// being real.
pub async fn enforce(State(limiter): State<RateLimiter>, request: Request, next: Next) -> Response {
    let (parts, body) = request.into_parts();
    let now = (limiter.clock)();
    let window = now / WINDOW_SECONDS;
    let (group, limit) = limiter.limits.limit_for(parts.uri.path());
    let identity = (limiter.resolver)(&parts);
    let key = RateLimiter::key(group, &identity, window);
    let request = Request::from_parts(parts, body);

    let Ok(count) = limiter.store.counter_add(&key, 1).await else {
        return unavailable();
    };
    if count > i64::from(limit) {
        return spent(now);
    }
    next.run(request).await
}

/// The 503 for a limiter that cannot reach its counters (spec 024 B-5).
///
/// No `Retry-After`: nothing here knows when the store comes back, and a
/// number invented for the header would be a worse answer than none.
fn unavailable() -> Response {
    error::refusal(
        StatusCode::SERVICE_UNAVAILABLE,
        "rate_limit_unavailable",
        "the rate limiter cannot reach its counters, so this request is not admitted",
    )
}

/// The 429 for a spent window, carrying the seconds left in it.
fn spent(now: u64) -> Response {
    let retry_after = WINDOW_SECONDS - (now % WINDOW_SECONDS);
    let mut response = error::refusal(
        StatusCode::TOO_MANY_REQUESTS,
        "rate_limited",
        "the request rate for this client exceeds the ceiling of its route group",
    );
    if let Ok(value) = HeaderValue::from_str(&retry_after.to_string()) {
        response.headers_mut().insert(header::RETRY_AFTER, value);
    }
    response
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn an_undeclared_path_falls_into_the_default_group() {
        let limits = RateLimits::new();
        assert_eq!(limits.limit_for("/api/notes"), (DEFAULT_GROUP, 300));
    }

    #[test]
    fn the_longest_declared_prefix_wins() {
        let limits = RateLimits::new()
            .group("/api", 100)
            .group("/api/expensive", 5);
        assert_eq!(limits.limit_for("/api/notes"), ("/api", 100));
        assert_eq!(limits.limit_for("/api/expensive/x"), ("/api/expensive", 5));
        assert_eq!(limits.limit_for("/other"), (DEFAULT_GROUP, 300));
    }

    #[test]
    fn declaring_a_prefix_twice_keeps_the_last_ceiling() {
        let limits = RateLimits::new().group("/api", 100).group("/api", 7);
        assert_eq!(limits.limit_for("/api"), ("/api", 7));
    }

    #[test]
    fn the_key_names_the_group_the_client_and_the_window() {
        assert_eq!(
            RateLimiter::key("/api", "10.0.0.1", 29),
            "rl:/api:10.0.0.1:29"
        );
    }
}
