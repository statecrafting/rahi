//! The one public listener of a cell (spec 020 B-1, B-2).
//!
//! An app mounts its routers on this builder and gets back one `Router`.
//! There is no second listener and no way to reorder the chain: the app
//! chooses what is served, the chassis chooses what every request passes
//! through on its way there.
//!
//! Spec 024 adds the other half of that choice: what the app mounts is also
//! *classified*. Every mount carries a [`RouteClass`], the build records the
//! whole set in the exposure table, and a route that arrives without a class
//! fails the build rather than quietly becoming reachable.

use std::path::PathBuf;

use axum::Router;
use axum::middleware::{from_fn, from_fn_with_state};
use rahi_types::Error;

use crate::client_identity;
use crate::error::{EdgeError, EdgeResult};
use crate::exposure::{self, Exposure, Route, RouteClass, STATIC_SLOT_PATH};
use crate::middleware::csrf::Csrf;
use crate::middleware::rate_limit::{ClientResolver, Clock, RateLimiter, RateLimits};
use crate::middleware::security_headers::SecurityHeaders;
use crate::middleware::{csrf, observation, rate_limit, security_headers};
use crate::operator::{self, RequireOperator};
use crate::state::AppState;
use crate::{error, probes, static_files};

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

/// One thing the app mounted, and what stands in front of it.
///
/// `class` is `None` when the app did not name one, which is not the same as
/// unreviewed: the default rule (spec 024 B-4) resolves it at build time.
struct Mount {
    prefix: String,
    router: Router,
    class: Option<RouteClass>,
}

/// What the app declares before the chassis assembles the router.
pub struct EdgeBuilder {
    state: AppState,
    mounts: Vec<Mount>,
    declared: Vec<Route>,
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
                &self
                    .mounts
                    .iter()
                    .map(|mount| (&mount.prefix, mount.class))
                    .collect::<Vec<_>>(),
            )
            .field("declared", &self.declared)
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
            declared: Vec::new(),
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
    ///
    /// The exposure class is the one [`exposure::default_class`] gives the
    /// prefix, which for anything an app mounts is
    /// [`RouteClass::Authenticated`] (spec 024 B-4).
    #[must_use]
    pub fn mount(self, prefix: &str, router: Router) -> Self {
        self.mounted(prefix, router, None)
    }

    /// Mount `router` under `prefix` in `class`.
    ///
    /// [`RouteClass::Operator`] here is [`EdgeBuilder::mount_operator`]: the
    /// class is what puts a mount behind the role gate and outside the
    /// limiter, not the method that named it.
    #[must_use]
    pub fn mount_as(self, prefix: &str, router: Router, class: RouteClass) -> Self {
        self.mounted(prefix, router, Some(class))
    }

    /// Mount `router` under `prefix` as a surface that answers to anybody.
    ///
    /// The one class an app has to say out loud (spec 024 B-4).
    #[must_use]
    pub fn mount_public(self, prefix: &str, router: Router) -> Self {
        self.mount_as(prefix, router, RouteClass::Public)
    }

    /// Mount `router` under `prefix` behind the operator role gate
    /// (spec 024 B-2).
    ///
    /// The mount goes inside the CSRF check and the security header floor and
    /// *outside* the rate limiter: the surface is already gated by a role the
    /// manifest declares, and a ceiling on the operator is a ceiling on the
    /// person diagnosing why the ceiling was reached.
    #[must_use]
    pub fn mount_operator(self, prefix: &str, router: Router) -> Self {
        self.mount_as(prefix, router, RouteClass::Operator)
    }

    /// Name one route in the exposure table without mounting anything.
    ///
    /// An app that serves individual paths inside a mount lists them here so
    /// that the review names routes rather than prefixes. A [`Route`] built
    /// with [`Route::unclassified`] is what makes the build fail: it is a
    /// route somebody added and nobody reviewed.
    #[must_use]
    pub fn expose(mut self, route: Route) -> Self {
        self.declared.push(route);
        self
    }

    fn mounted(mut self, prefix: &str, router: Router, class: Option<RouteClass>) -> Self {
        self.mounts.push(Mount {
            prefix: prefix.to_owned(),
            router,
            class,
        });
        self
    }

    /// Serve the app's built SPA from `dir` (B-7).
    ///
    /// The slot answers every path no route matched, so it is
    /// [`RouteClass::Public`] in the exposure table.
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
    ///
    /// Unset, the limiter keys on
    /// [`client_identity::from_config`], which reads the trusted proxy hops
    /// the operator declared.
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

    /// The exposure table this builder will publish (spec 024 B-3).
    ///
    /// Pure: it reads the declarations and nothing else, so an app can print
    /// it or assert on it without building a router.
    #[must_use]
    pub fn exposure(&self) -> Exposure {
        let mut table = Exposure::new();
        for path in [
            probes::HEALTHZ_PATH,
            probes::READYZ_PATH,
            crate::obs::METRICS_PATH,
        ] {
            table.record(Route::new(path, RouteClass::Probe));
        }
        for mount in &self.mounts {
            let class = mount
                .class
                .unwrap_or_else(|| exposure::default_class(&mount.prefix));
            table.record(Route::new(exposed_path(&mount.prefix), class));
        }
        if self.static_dir.is_some() {
            table.record(Route::new(STATIC_SLOT_PATH, RouteClass::Public));
        }
        for route in &self.declared {
            table.record(route.clone());
        }
        table
    }

    /// Assemble the router, or refuse a route nobody classified (024 B-3).
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when the exposure table holds a route with no class.
    ///
    /// [`Error::Config`]: rahi_types::Error::Config
    pub fn try_build(self) -> EdgeResult<Router> {
        let table = self.exposure();
        table.check()?;
        exposure::publish(&table);
        Ok(self.assemble())
    }

    /// Assemble the router.
    ///
    /// The layers go on in one fixed order (B-2). Reading outward from the
    /// app: the rate limiter, the CSRF check, the security headers, and
    /// observation. The probes and `/metrics` (spec 023 B-1) are merged after
    /// the two inner layers are applied, which is what puts them outside
    /// both. Operator mounts (spec 024 B-2) are merged after the limiter and
    /// before the CSRF check, which is what puts them inside one and outside
    /// the other.
    ///
    /// # Panics
    ///
    /// In a debug build, when the exposure table holds a route with no class
    /// (spec 024 B-3). A release build cannot panic here and cannot answer
    /// with a `Result` either, so it returns a router that refuses every
    /// request with that `Error::Config`: an unreviewed route is not served
    /// while the configuration that produced it stands. Use
    /// [`EdgeBuilder::try_build`] to handle it.
    pub fn build(self) -> Router {
        match self.try_build() {
            Ok(router) => router,
            Err(EdgeError(err)) => {
                if cfg!(debug_assertions) {
                    panic!("the router cannot be built: {err}");
                }
                refusing(err)
            }
        }
    }

    /// Everything after the exposure table has been checked.
    fn assemble(self) -> Router {
        let (operator_mounts, app_mounts): (Vec<Mount>, Vec<Mount>) = self
            .mounts
            .into_iter()
            .partition(|mount| mount.class == Some(RouteClass::Operator));

        let mut guarded = Router::new();
        for mount in app_mounts {
            guarded = attach(guarded, &mount.prefix, mount.router);
        }
        if let Some(dir) = &self.static_dir {
            guarded = guarded.merge(static_files::service(dir));
        }

        let mut limiter = RateLimiter::new(self.state.store().clone(), self.limits);
        limiter = limiter.with_resolver(
            self.resolver
                .unwrap_or_else(|| client_identity::from_config(self.state.config())),
        );
        if let Some(clock) = self.clock {
            limiter = limiter.with_clock(clock);
        }
        let guarded = guarded.layer(from_fn_with_state(limiter, rate_limit::enforce));

        // Merged after the limiter went on and before the CSRF check does:
        // that is the whole of B-2's placement. Skipped entirely when the app
        // mounted none, because merging an empty router would hand its own
        // fallback to the one that already has a good answer.
        let guarded = if operator_mounts.is_empty() {
            guarded
        } else {
            let mut ops = Router::new();
            for mount in operator_mounts {
                ops = attach(ops, &mount.prefix, mount.router);
            }
            let gate = RequireOperator::new(self.state.kernel().clone());
            guarded.merge(operator::with_operator(gate, ops))
        };

        let guarded = guarded.layer(from_fn_with_state(
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

/// Nest `router` under `prefix`, or merge it when the prefix is the root.
fn attach(into: Router, prefix: &str, router: Router) -> Router {
    if prefix.is_empty() || prefix == "/" {
        into.merge(router)
    } else {
        into.nest(prefix, router)
    }
}

/// How a mount's prefix is named in the exposure table.
fn exposed_path(prefix: &str) -> &str {
    if prefix.is_empty() { "/" } else { prefix }
}

/// The router a release build serves when the exposure table did not check.
fn refusing(err: Error) -> Router {
    Router::new().fallback(move || {
        let err = err.clone();
        async move { error::response(&err) }
    })
}
