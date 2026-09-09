//! The `Cell` trait (B-1): what an app declares, and nothing more.
//!
//! An app is a manifest, its migrations, its routes, and optionally an
//! operator surface and a static page. The composer turns that into a
//! binary with `serve` and every verb; the app never sees argv, a runtime,
//! or the order the chassis boots in.

use std::path::PathBuf;

use axum::Router;
use rahi_edge::{AppState, Route};
use rahi_store::Migration;

/// The prefix a cell's operator routes are mounted under (spec 024 B-2).
pub const OPERATOR_PREFIX: &str = "/operator";

/// One application on the chassis.
///
/// Every method is an associated function: a cell is a declaration, not a
/// value, and `rahi_cli::run(cell)` takes one only to name the type.
pub trait Cell: Send + Sync + 'static {
    /// The manifest, as TOML text (spec 015 B-1). Parsed and hashed at
    /// every boot; the hash is the chain's genesis parent.
    fn manifest() -> &'static str;

    /// The migrations, in version order (spec 011 B-5). `migrate` applies
    /// them; `serve` refuses a store that is behind them.
    fn migrations() -> &'static [Migration];

    /// The app's routes, merged at the root and classified authenticated
    /// unless [`Cell::exposed`] says otherwise (spec 024 B-4).
    fn routes(state: AppState) -> Router;

    /// The operator surface, mounted under [`OPERATOR_PREFIX`] behind the
    /// role gate (spec 024 B-2). Empty by default.
    fn operator_routes(state: AppState) -> Router {
        let _ = state;
        Router::new()
    }

    /// Routes named individually in the exposure table (spec 024 B-3), for
    /// an app that serves a public path inside its root merge.
    fn exposed() -> Vec<Route> {
        Vec::new()
    }

    /// The directory the static slot serves (spec 020 B-7), if any.
    fn static_dir() -> Option<PathBuf> {
        None
    }
}

/// The chassis with no app: probes, metrics, identity, and the verbs.
///
/// The `rahi` binary is this cell, so the composer can be exercised end to
/// end (AC-2, FR-005) before any app exists, and so a deployment can be
/// preflighted with the same binary that will serve it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EmptyCell;

impl EmptyCell {
    /// The smallest manifest `Manifest::parse` accepts.
    pub const MANIFEST: &'static str = r#"# The empty cell: the chassis, no app (spec 030).

[app]
name = "rahi"
org = "statecrafting"

[ledger]
schema_version = "1.0.0"
max_record_bytes = 65536

[observability]
metrics_path = "/metrics"
otel = false

[auth]
operator_role = "rahi-operator"

[contract]
version = "1.0.0"
"#;
}

static EMPTY_MIGRATIONS: std::sync::LazyLock<Vec<Migration>> = std::sync::LazyLock::new(|| {
    vec![Migration::new(
        1,
        "cell_meta",
        "CREATE TABLE IF NOT EXISTS cell_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
    )]
});

impl Cell for EmptyCell {
    fn manifest() -> &'static str {
        Self::MANIFEST
    }

    fn migrations() -> &'static [Migration] {
        &EMPTY_MIGRATIONS
    }

    fn routes(_state: AppState) -> Router {
        Router::new()
    }
}
