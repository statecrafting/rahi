//! The edge of the rahi chassis (spec 020).
//!
//! One listener, one router, one order. An app builds its own routers, mounts
//! them here, and gets back an `axum::Router` in which every request has
//! already passed observation, the security header floor, the CSRF check, and
//! the rate limiter before it reaches a handler. Nothing about that order is
//! negotiable, because a middleware chain an app can reorder is a middleware
//! chain that will eventually be reordered wrongly.
//!
//! ```no_run
//! # use rahi_edge::{AppState, Edge, RateLimits};
//! # use rahi_kernel::Kernel;
//! # use rahi_ledger::Ledger;
//! # use rahi_store::StoreHandle;
//! # use rahi_types::Config;
//! # fn compose(kernel: Kernel, store: StoreHandle, ledger: Ledger, config: Config) -> axum::Router {
//! let state = AppState::new(kernel, store, ledger, config);
//! Edge::builder(state)
//!     .mount("/api", axum::Router::new())
//!     .rate_limits(RateLimits::new().group("/api/export", 10))
//!     .static_slot("/app/web")
//!     .build()
//! # }
//! ```
//!
//! What lives here: the router builder ([`router`]), the state every handler
//! is given ([`state`]), the three middleware ([`middleware`]), the two
//! probes ([`probes`]), the static slot ([`static_files`]), and the one
//! mapping from a workspace error to an HTTP answer ([`error`]).
//!
//! What does not: metrics and tracing (spec 023), trusted proxy hops and the
//! operator gate (spec 024), rauthy's proxy route (spec 021), and sessions
//! (spec 022). The identity crate is a peer of this one, never a dependency
//! of it: an app composes both.

#![forbid(unsafe_code)]

pub mod error;
pub mod middleware;
pub mod probes;
pub mod router;
pub mod state;
pub mod static_files;

pub use error::{EdgeError, EdgeResult, status_of};
pub use middleware::csrf::Csrf;
pub use middleware::rate_limit::{ClientResolver, Clock, RateLimiter, RateLimits};
pub use middleware::security_headers::SecurityHeaders;
pub use probes::{HEALTHZ_PATH, READYZ_PATH};
pub use router::{Edge, EdgeBuilder};
pub use state::AppState;
