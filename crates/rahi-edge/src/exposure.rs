//! The exposure table: every route, with what authenticates it (spec 024 B-3).
//!
//! enrahitu's exposure review found an unauthenticated data surface on a
//! container bound to all interfaces, and it found it by hand
//! (enrahitu://025). Doing that review by hand again is the failure mode, so
//! the review became a data structure: [`EdgeBuilder::build`] records every
//! route it mounts with its [`RouteClass`], and a route that reaches the
//! table without one is a build failure rather than a line nobody read.
//!
//! [`report`] renders what this process built. A cell builds one router, so
//! the table is the cell's answer to "what is reachable, and by whom"; the
//! reference app (spec 034) prints it in its README and asserts on it in its
//! end-to-end test.
//!
//! [`EdgeBuilder::build`]: crate::router::EdgeBuilder::build

use std::fmt;
use std::sync::{Mutex, PoisonError};

use rahi_types::{Error, Result};

use crate::middleware::csrf;
use crate::obs::METRICS_PATH;
use crate::probes::{HEALTHZ_PATH, READYZ_PATH};

/// The path the static slot's fallback is recorded under.
///
/// The slot answers every path no route matched, which is precisely why it is
/// the one entry a reviewer must see spelled out.
pub const STATIC_SLOT_PATH: &str = "/*";

/// What stands between a request and a route.
///
/// The order of the variants is the order [`Exposure::render`] sorts by, and
/// it is the order a reviewer wants: the surfaces that answer to anybody come
/// first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RouteClass {
    /// Anyone. The app named this explicitly; it is never a default.
    Public,
    /// A session. The default for anything an app mounts (B-4).
    Authenticated,
    /// A session carrying the manifest's `auth.operator_role`, and no rate
    /// limiter (B-2).
    Operator,
    /// A probe or the metrics scrape: outside CSRF and outside the limiter.
    Probe,
    /// rauthy's raw proxy subtree, which brings its own everything.
    Proxy,
}

impl RouteClass {
    /// The label [`Exposure::render`] prints.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Public => "Public",
            Self::Authenticated => "Authenticated",
            Self::Operator => "Operator",
            Self::Probe => "Probe",
            Self::Proxy => "Proxy",
        }
    }
}

impl fmt::Display for RouteClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// One row of the table: a path, and what stands in front of it.
///
/// The class is optional because "nobody has said" is a state the table has
/// to be able to hold; it is the state [`Exposure::check`] refuses.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Route {
    path: String,
    class: Option<RouteClass>,
}

impl Route {
    /// A reviewed route.
    #[must_use]
    pub fn new(path: impl Into<String>, class: RouteClass) -> Self {
        Self {
            path: path.into(),
            class: Some(class),
        }
    }

    /// A route nobody has classified.
    ///
    /// The only reason to build one is to assert that the build refuses it.
    #[must_use]
    pub fn unclassified(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            class: None,
        }
    }

    /// The path.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The class, if this route has one.
    #[must_use]
    pub const fn class(&self) -> Option<RouteClass> {
        self.class
    }
}

/// The table itself.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Exposure {
    routes: Vec<Route>,
}

impl Exposure {
    /// An empty table.
    #[must_use]
    pub const fn new() -> Self {
        Self { routes: Vec::new() }
    }

    /// Add `route`, unless an identical row is already present.
    pub fn record(&mut self, route: Route) {
        if !self.routes.contains(&route) {
            self.routes.push(route);
        }
    }

    /// [`Exposure::record`], for a builder chain.
    #[must_use]
    pub fn with(mut self, route: Route) -> Self {
        self.record(route);
        self
    }

    /// Every row, in the order it was recorded.
    #[must_use]
    pub fn routes(&self) -> &[Route] {
        &self.routes
    }

    /// The paths nobody classified.
    #[must_use]
    pub fn unclassified(&self) -> Vec<&str> {
        self.routes
            .iter()
            .filter(|route| route.class.is_none())
            .map(Route::path)
            .collect()
    }

    /// The class recorded for `path`, if the table names it.
    #[must_use]
    pub fn class_of(&self, path: &str) -> Option<RouteClass> {
        self.routes
            .iter()
            .find(|route| route.path == path)
            .and_then(Route::class)
    }

    /// Refuse a table holding a route nobody classified (B-3).
    ///
    /// # Errors
    ///
    /// [`Error::Config`] naming every unclassified path. An unreviewed route
    /// is a configuration fault, not a request-time one: it is wrong before
    /// anybody has asked for it.
    pub fn check(&self) -> Result<()> {
        let unreviewed = self.unclassified();
        if unreviewed.is_empty() {
            return Ok(());
        }
        Err(Error::Config(format!(
            "these routes are mounted with no exposure class, so nobody has \
             reviewed who can reach them: {}",
            unreviewed.join(", ")
        )))
    }

    /// The table as text, sorted by class and then by path.
    #[must_use]
    pub fn render(&self) -> String {
        let mut rows: Vec<&Route> = self.routes.iter().collect();
        rows.sort_by(|a, b| (a.class, &a.path).cmp(&(b.class, &b.path)));

        let width = rows
            .iter()
            .map(|route| Self::class_label(route).len())
            .chain(std::iter::once("class".len()))
            .max()
            .unwrap_or_default();

        let mut out = format!("{:<width$}  path\n", "class");
        out.push_str(&format!("{}  {}\n", "-".repeat(width), "-".repeat(24)));
        for route in rows {
            out.push_str(&format!(
                "{:<width$}  {}\n",
                Self::class_label(route),
                route.path
            ));
        }
        out
    }

    /// What [`Exposure::render`] prints in the class column.
    fn class_label(route: &Route) -> &'static str {
        route.class.map_or("UNCLASSIFIED", RouteClass::label)
    }
}

/// The class a mount carries when the app named none (B-4).
///
/// Probes and `/metrics` are `Probe`, rauthy's subtree is `Proxy`, and
/// everything else an app mounts is `Authenticated`. `Public` is never a
/// default: a surface that answers to anybody is a decision somebody makes on
/// purpose.
#[must_use]
pub fn default_class(prefix: &str) -> RouteClass {
    if is_probe(prefix) {
        RouteClass::Probe
    } else if csrf::is_exempt(prefix) {
        RouteClass::Proxy
    } else {
        RouteClass::Authenticated
    }
}

/// Whether `path` is one of the three surfaces mounted outside the guards.
#[must_use]
pub fn is_probe(path: &str) -> bool {
    [HEALTHZ_PATH, READYZ_PATH, METRICS_PATH].contains(&path)
}

/// The process-wide table, the union of every router built in this process.
///
/// A cell builds one router, so in a cell this is that router's table. A test
/// binary builds many, and a union rather than a replacement is what keeps
/// [`report`] meaningful there: a row that was true when it was recorded does
/// not stop being true because another router was built afterwards.
static TABLE: Mutex<Vec<Route>> = Mutex::new(Vec::new());

/// Publish `table` into the process-wide table.
pub fn publish(table: &Exposure) {
    let mut published = TABLE.lock().unwrap_or_else(PoisonError::into_inner);
    for route in &table.routes {
        if !published.contains(route) {
            published.push(route.clone());
        }
    }
}

/// The process-wide table, as a value.
#[must_use]
pub fn current() -> Exposure {
    let published = TABLE.lock().unwrap_or_else(PoisonError::into_inner);
    Exposure {
        routes: published.clone(),
    }
}

/// The process-wide table, rendered (B-3).
#[must_use]
pub fn report() -> String {
    current().render()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_the_ones_b4_fixes() {
        assert_eq!(default_class(HEALTHZ_PATH), RouteClass::Probe);
        assert_eq!(default_class(READYZ_PATH), RouteClass::Probe);
        assert_eq!(default_class(METRICS_PATH), RouteClass::Probe);
        assert_eq!(default_class("/auth"), RouteClass::Proxy);
        assert_eq!(default_class("/auth/v1/token"), RouteClass::Proxy);
        assert_eq!(default_class("/api"), RouteClass::Authenticated);
        assert_eq!(default_class("/"), RouteClass::Authenticated);
        assert_eq!(
            default_class("/authors"),
            RouteClass::Authenticated,
            "a lookalike is not the proxy subtree"
        );
    }

    #[test]
    fn a_classified_table_passes_and_an_unclassified_one_does_not() {
        let reviewed = Exposure::new()
            .with(Route::new("/api", RouteClass::Authenticated))
            .with(Route::new(HEALTHZ_PATH, RouteClass::Probe));
        assert!(reviewed.check().is_ok());
        assert!(reviewed.unclassified().is_empty());

        let unreviewed = reviewed.with(Route::unclassified("/internal/dump"));
        let err = unreviewed
            .check()
            .err()
            .map_or_else(String::new, |err| err.message().to_owned());
        assert!(err.contains("/internal/dump"), "{err}");
        assert_eq!(unreviewed.unclassified(), vec!["/internal/dump"]);
    }

    #[test]
    fn an_identical_row_is_recorded_once() {
        let table = Exposure::new()
            .with(Route::new("/api", RouteClass::Authenticated))
            .with(Route::new("/api", RouteClass::Authenticated));
        assert_eq!(table.routes().len(), 1);
    }

    #[test]
    fn the_render_leads_with_what_answers_to_anybody() {
        let rendered = Exposure::new()
            .with(Route::new(HEALTHZ_PATH, RouteClass::Probe))
            .with(Route::new("/api", RouteClass::Authenticated))
            .with(Route::new(STATIC_SLOT_PATH, RouteClass::Public))
            .with(Route::unclassified("/internal/dump"))
            .render();
        let order: Vec<&str> = rendered.lines().skip(2).collect();
        assert_eq!(
            order.first().map(|line| line.contains("/internal/dump")),
            Some(true),
            "an unreviewed route sorts to the top:\n{rendered}"
        );
        let public = rendered.find("Public").unwrap_or(usize::MAX);
        let probe = rendered.find("Probe").unwrap_or(0);
        assert!(public < probe, "{rendered}");
    }
}
