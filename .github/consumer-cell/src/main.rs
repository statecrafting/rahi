//! A cell outside the chassis workspace (spec 039 B-7).
//!
//! It composes the published crates the way a consumer does: one governed
//! write the manifest grants, one the manifest never granted, and nothing
//! else. The release workflow boots it, writes, is denied, and verifies the
//! chain, which is what a consumer's first hour with the chassis looks like.

use std::sync::LazyLock;

use axum::Router;
use axum::extract::State;
use axum::response::{IntoResponse as _, Response};
use axum::routing::{get, post};
use rahi_cli::Cell;
use rahi_edge::AppState;
use rahi_edge::exposure::{Route, RouteClass};
use rahi_kernel::{CapabilityKind, Governed};
use rahi_store::{Migration, StoreHandle};
use rahi_types::Sub;

struct ConsumerCell;

static MIGRATIONS: LazyLock<Vec<Migration>> = LazyLock::new(|| {
    vec![Migration::new(
        1,
        "items",
        "CREATE TABLE IF NOT EXISTS items (id INTEGER PRIMARY KEY, body TEXT NOT NULL)",
    )]
});

impl Cell for ConsumerCell {
    fn manifest() -> &'static str {
        include_str!("../manifest.toml")
    }

    fn migrations() -> &'static [Migration] {
        &MIGRATIONS
    }

    fn routes(state: AppState) -> Router {
        let granted = Governed::new(
            state.kernel(),
            "items",
            CapabilityKind::DbWrite,
            "items",
            state.store().clone(),
        )
        .expect("the manifest grants items a write");
        let ungranted = Governed::new(
            state.kernel(),
            "items",
            CapabilityKind::DbWrite,
            "audit",
            state.store().clone(),
        )
        .expect("the facade builds; the grant is what is missing");
        Router::new()
            .route("/api/write", post(write))
            .with_state(granted)
            .merge(
                Router::new()
                    .route("/api/deny", get(deny))
                    .with_state(ungranted),
            )
    }

    fn exposed() -> Vec<Route> {
        vec![
            Route::new("/api/write", RouteClass::Public),
            Route::new("/api/deny", RouteClass::Public),
        ]
    }
}

async fn write(State(items): State<Governed<StoreHandle>>) -> Response {
    match items
        .execute(
            &Sub::new("consumer"),
            "INSERT INTO items (body) VALUES ('x')",
            vec![],
        )
        .await
    {
        Ok(result) => format!("wrote {}", result.rows_affected).into_response(),
        Err(err) => rahi_edge::error::response(&err),
    }
}

async fn deny(State(audit): State<Governed<StoreHandle>>) -> Response {
    match audit
        .execute(
            &Sub::new("consumer"),
            "INSERT INTO audit (body) VALUES ('x')",
            vec![],
        )
        .await
    {
        Ok(_) => "unexpectedly allowed".into_response(),
        Err(err) => rahi_edge::error::response(&err),
    }
}

fn main() {
    rahi_cli::run(ConsumerCell)
}
