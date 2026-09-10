//! The cell (spec 034 B-2): what hello-cell declares, and nothing more.

use std::path::PathBuf;

use axum::Router;
use axum::routing::get;
use rahi_cli::Cell;
use rahi_edge::AppState;
use rahi_store::Migration;

use crate::migrations::MIGRATIONS;
use crate::notes;

/// The reference cell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HelloCell;

impl HelloCell {
    /// The manifest, embedded at build (B-1).
    pub const MANIFEST: &'static str = include_str!("../manifest.toml");
}

impl Cell for HelloCell {
    fn manifest() -> &'static str {
        Self::MANIFEST
    }

    fn migrations() -> &'static [Migration] {
        &MIGRATIONS
    }

    fn routes(state: AppState) -> Router {
        notes::router(&state)
    }

    /// `GET /operator/traces`: the in-process trace ring (spec 023 B-3),
    /// and `GET /operator/exposure`: the route table as the edge published
    /// it (spec 024 B-3, this spec's B-6), both behind the `hello_operator`
    /// role gate the chassis mounts them under.
    fn operator_routes(_state: AppState) -> Router {
        Router::new()
            .route("/traces", get(traces))
            .route("/exposure", get(exposure))
    }

    /// The page (B-4), served from the static slot.
    fn static_dir() -> Option<PathBuf> {
        Some(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web"))
    }
}

/// The exposure table, rendered: what `exposure::report()` says about
/// this cell, which is what the README shows.
async fn exposure() -> String {
    rahi_edge::exposure::report()
}

/// The trace ring as JSON: each trace's id, its root span, and how many
/// children closed inside it.
async fn traces() -> axum::Json<serde_json::Value> {
    let traces: Vec<serde_json::Value> = rahi_edge::obs::list_traces()
        .into_iter()
        .map(|trace| {
            serde_json::json!({
                "id": trace.id,
                "root": trace.root.name,
                "duration_ms": trace.root.duration_ms,
                "spans": trace.children.len(),
            })
        })
        .collect();
    axum::Json(serde_json::json!({ "traces": traces }))
}
