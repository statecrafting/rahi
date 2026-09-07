//! The one public listener of a cell (spec 020 B-1, B-2).
//!
//! An app mounts its routers on this builder and gets back one `Router`.
//! There is no second listener and no way to reorder the chain: the app
//! chooses what is served, the chassis chooses what every request passes
//! through on its way there.

use std::path::PathBuf;

use axum::Router;
use axum::middleware::{from_fn, from_fn_with_state};

use crate::middleware::csrf::Csrf;
use crate::middleware::rate_limit::{ClientResolver, Clock, RateLimiter, RateLimits};
use crate::middleware::security_headers::SecurityHeaders;
use crate::middleware::{csrf, observation, rate_limit, security_headers};
use crate::state::AppState;
use crate::{probes, static_files};

/// The edge. Its only job is to hand out a [`EdgeBuilder`].
#[derive(Clone, Copy, Debug)]
pub struct Edge;

impl Edge {
    /// Start building the router over `state`.
    #[must_use]
    pub fn builder(state: AppState) -> EdgeBuilder {
        EdgeBuilder::new(state)
    }
}

/// What the app declares before the chassis assembles the router.
pub struct EdgeBuilder {
    state: AppState,
    mounts: Vec<(String, Router)>,
    static_dir: Option<PathBuf>,
    limits: RateLimits,
    resolver: Option<ClientResolver>,
    clock: Option<Clock>,
}

impl std::fmt::Debug for EdgeBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EdgeBuilder")
            .field(
                "mounts",
                &self.mounts.iter().map(|(p, _)| p).collect::<Vec<_>>(),
            )
            .field("static_dir", &self.static_dir)
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl EdgeBuilder {
    /// A builder over `state` with no mounts, no static slot, and the default
    /// ceiling.
    #[must_use]
    pub fn new(state: AppState) -> Self {
        Self {
            state,
            mounts: Vec::new(),
            static_dir: None,
            limits: RateLimits::new(),
            resolver: None,
            clock: None,
        }
    }

    /// Mount `router` under `prefix`.
    ///
    /// `prefix` is a path prefix (`/api`); `/` and the empty string merge at
    /// the root, because axum nests nothing at `/`. The router arrives with
    /// its own state already applied: an app's routes answer to the app's
    /// state, not to the chassis's.
    #[must_use]
    pub fn mount(mut self, prefix: &str, router: Router) -> Self {
        self.mounts.push((prefix.to_owned(), router));
        self
    }

    /// Serve the app's built SPA from `dir` (B-7).
    #[must_use]
    pub fn static_slot(mut self, dir: impl Into<PathBuf>) -> Self {
        self.static_dir = Some(dir.into());
        self
    }

    /// Declare the per-route-group ceilings (B-5).
    #[must_use]
    pub fn rate_limits(mut self, limits: RateLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Resolve the limiter's client identity with `resolver` (spec 024).
    #[must_use]
    pub fn client_resolver(mut self, resolver: ClientResolver) -> Self {
        self.resolver = Some(resolver);
        self
    }

    /// Read the limiter's window ordinal from `clock`.
    #[must_use]
    pub fn clock(mut self, clock: Clock) -> Self {
        self.clock = Some(clock);
        self
    }

    /// Assemble the router.
    ///
    /// The layers go on in one fixed order (B-2). Reading outward from the
    /// app: the rate limiter, the CSRF check, the security headers, and
    /// observation. The probes and `/metrics` (spec 023 B-1) are merged after
    /// the two inner layers are applied, which is what puts them outside
    /// both.
    pub fn build(self) -> Router {
        let mut guarded = Router::new();
        for (prefix, router) in self.mounts {
            guarded = if prefix.is_empty() || prefix == "/" {
                guarded.merge(router)
            } else {
                guarded.nest(&prefix, router)
            };
        }
        if let Some(dir) = &self.static_dir {
            guarded = guarded.merge(static_files::service(dir));
        }

        let mut limiter = RateLimiter::new(self.state.store().clone(), self.limits);
        if let Some(resolver) = self.resolver {
            limiter = limiter.with_resolver(resolver);
        }
        if let Some(clock) = self.clock {
            limiter = limiter.with_clock(clock);
        }

        let guarded = guarded
            .layer(from_fn_with_state(limiter, rate_limit::enforce))
            .layer(from_fn_with_state(
                Csrf::from_config(self.state.config()),
                csrf::enforce,
            ));

        Router::new()
            .merge(probes::router().with_state(self.state.clone()))
            .merge(crate::obs::metrics_router())
            .merge(guarded)
            .layer(from_fn_with_state(
                SecurityHeaders::from_config(self.state.config()),
                security_headers::apply,
            ))
            .layer(from_fn(observation))
    }
}
