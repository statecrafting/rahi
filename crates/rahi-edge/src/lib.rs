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
//! Spec 023 adds [`obs`]: the process-wide metrics registry `/metrics` serves,
//! the in-process tracer, and the bounded ring of recent traces a cell keeps
//! whether or not a collector is listening. The observation seam spec 020 left
//! outermost is where it attaches.
//!
//! Spec 024 adds the three that make exposure a property rather than a
//! review: [`client_identity`] turns `X-Forwarded-For` into an identity only
//! as far as the operator's declared hops, [`operator`] is the role gate the
//! internal surfaces sit behind with no limiter on top, and [`exposure`] is
//! the table every mounted route appears in with its class, checked at build
//! so that an unreviewed public route cannot land.
//!
//! What lives here: the router builder ([`router`]), the state every handler
//! is given ([`state`]), the three middleware ([`middleware`]), the two
//! probes ([`probes`]), the static slot ([`static_files`]), and the one
//! mapping from a workspace error to an HTTP answer ([`error`]).
//!
//! What does not: rauthy's proxy route (spec 021) and sessions (spec 022).
//! The identity crate is a peer of this one, never a dependency of it: an app
//! composes both. That is why [`operator`] reads the
//! [`Principal`](rahi_types::Principal) spec 022 leaves in the request
//! extensions rather than calling into it.

#![forbid(unsafe_code)]

pub mod client_identity;
pub mod error;
pub mod exposure;
pub mod middleware;
pub mod obs;
pub mod operator;
pub mod probes;
pub mod router;
pub mod state;
pub mod static_files;

pub use client_identity::ClientIdentity;
pub use error::{EdgeError, EdgeResult, status_of};
pub use exposure::{Exposure, Route, RouteClass};
pub use middleware::csrf::Csrf;
pub use middleware::rate_limit::{ClientResolver, Clock, RateLimiter, RateLimits};
pub use middleware::security_headers::SecurityHeaders;
pub use obs::{Metrics, Obs, ObsOptions, Ring, Trace, get_trace, list_traces, subscribe};
pub use operator::RequireOperator;
pub use probes::{HEALTHZ_PATH, READYZ_PATH};
pub use router::{Edge, EdgeBuilder};
pub use state::AppState;
