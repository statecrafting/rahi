//! The notes resource (spec 034 B-3): create, list, and delete behind
//! login, every write one `txn` with its revision stamp and its outbox row,
//! and one route that reaches for a capability the manifest never granted.
//!
//! Every store call goes through a `Governed` facade whose triple
//! (service, kind, resource) is literal, so `rahi_kernel::verify!` can read
//! the ceiling this crate observes off its source and hold the manifest to
//! it at build (FR-002).

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use rahi_edge::AppState;
use rahi_idp::Authenticated;
use rahi_kernel::{CapabilityKind, Governed};
use rahi_store::{Envelope, Outbox, Statement, StoreHandle, TxnBuilder, Value, Watermark};
use rahi_types::{Error, Revision};
use serde::{Deserialize, Serialize};

/// The table.
pub const TABLE: &str = "notes";

/// The outbox kind a note's envelope carries.
pub const ENVELOPE_KIND: &str = "note";

/// One note, as listed.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Note {
    /// The id, minted at creation.
    pub id: String,
    /// The body.
    pub body: String,
    /// The watermark the write was stamped with.
    pub revision: i64,
    /// Seconds since the epoch.
    pub created_at: i64,
}

/// `POST /api/notes`'s body.
#[derive(Clone, Debug, Deserialize)]
pub struct NewNote {
    /// The body, non-empty.
    pub body: String,
}

/// The facades, one per capability the service uses.
#[derive(Clone)]
struct Notes {
    read: Governed<StoreHandle>,
    write: Governed<StoreHandle>,
    txn: Governed<StoreHandle>,
    migrate: Governed<StoreHandle>,
}

/// The notes router over `state` (B-3). Facades are built once; a facade
/// whose service the manifest does not declare would refuse here, at
/// compose, rather than at the first request.
///
/// # Panics
///
/// When the manifest declares no `notes` service, which the embedded
/// manifest does; `verify!` and the manifest tests hold that invariant.
pub fn router(state: &AppState) -> Router {
    let kernel = state.kernel();
    let store = state.store().clone();
    let facades = Notes {
        read: Governed::new(
            kernel,
            "notes",
            CapabilityKind::DbRead,
            "notes",
            store.clone(),
        )
        .unwrap_or_else(|err| panic!("the notes service is declared: {err}")),
        write: Governed::new(
            kernel,
            "notes",
            CapabilityKind::DbWrite,
            "notes",
            store.clone(),
        )
        .unwrap_or_else(|err| panic!("the notes service is declared: {err}")),
        txn: Governed::new(
            kernel,
            "notes",
            CapabilityKind::DbTxn,
            "notes",
            store.clone(),
        )
        .unwrap_or_else(|err| panic!("the notes service is declared: {err}")),
        // Declared to the kernel, never granted by the manifest: the call
        // through it is the denial the end-to-end test reads (B-1, B-5).
        migrate: Governed::new(kernel, "notes", CapabilityKind::DbMigrate, "notes", store)
            .unwrap_or_else(|err| panic!("the notes service is declared: {err}")),
    };
    Router::new()
        .route("/api/notes", get(list).post(create))
        .route("/api/notes/migrate", post(migrate))
        .route("/api/notes/{id}", axum::routing::delete(delete))
        .with_state(facades)
}

/// `GET /api/notes`: the principal's notes, newest last.
async fn list(
    State(notes): State<Notes>,
    Authenticated(principal): Authenticated,
) -> Result<Json<Vec<Note>>, Response> {
    let rows: Vec<Note> = notes
        .read
        .query(
            &principal.sub,
            "SELECT id, body, revision, created_at FROM notes WHERE sub = $1 ORDER BY created_at, id",
            vec![Value::from(principal.sub.as_str())],
        )
        .await
        .map_err(answer)?;
    Ok(Json(rows))
}

/// `POST /api/notes`: one `txn` holding the insert, its revision stamp,
/// and its outbox envelope (constitution IX).
async fn create(
    State(notes): State<Notes>,
    Authenticated(principal): Authenticated,
    Json(new): Json<NewNote>,
) -> Result<Response, Response> {
    let body = new.body.trim();
    if body.is_empty() {
        return Err(answer(Error::Validation("a note needs a body".to_owned())));
    }
    let id = note_id();
    let created_at = now();
    // The envelope names the revision the stamp will assign: one more than
    // the table's current high mark, read consistently through the leader.
    let high: Vec<HighMark> = notes
        .read
        .query_consistent(
            &principal.sub,
            "SELECT COALESCE(MAX(revision), 0) AS revision FROM notes",
            vec![],
        )
        .await
        .map_err(answer)?;
    let next = high.first().map_or(1, |h| h.revision.saturating_add(1));
    let mut txn = TxnBuilder::new();
    txn.push(Statement::with_params(
        "INSERT INTO notes (id, sub, body, revision, fence, created_at) VALUES ($1, $2, $3, 0, 0, $4)",
        vec![
            Value::from(id.as_str()),
            Value::from(principal.sub.as_str()),
            Value::from(body),
            Value::from(created_at),
        ],
    ));
    Watermark::next(&mut txn, TABLE).map_err(answer)?;
    Outbox::stage(
        &mut txn,
        &Envelope::new(
            ENVELOPE_KIND,
            None,
            id.clone(),
            Revision::new(u64::try_from(next).unwrap_or_default()),
        ),
    );
    notes
        .txn
        .txn(&principal.sub, txn.into_statements())
        .await
        .map_err(answer)?;
    let note = Note {
        id,
        body: body.to_owned(),
        revision: next,
        created_at,
    };
    Ok((StatusCode::CREATED, Json(note)).into_response())
}

/// `DELETE /api/notes/{id}`: only the caller's own; a note that is not
/// theirs is not found, which says nothing about whether it exists.
async fn delete(
    State(notes): State<Notes>,
    Authenticated(principal): Authenticated,
    Path(id): Path<String>,
) -> Result<StatusCode, Response> {
    let result = notes
        .write
        .execute(
            &principal.sub,
            "DELETE FROM notes WHERE id = $1 AND sub = $2",
            vec![Value::from(id), Value::from(principal.sub.as_str())],
        )
        .await
        .map_err(answer)?;
    if result.rows_affected == 0 {
        return Err(answer(Error::NotFound("no such note of yours".to_owned())));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/notes/migrate`: the demonstration (B-3). The service reaches
/// for `db.migrate` on `notes`, the manifest never granted it, the kernel
/// refuses, and the 403 carries the decision id of the record that says so.
async fn migrate(
    State(notes): State<Notes>,
    Authenticated(principal): Authenticated,
) -> Result<Json<serde_json::Value>, Response> {
    let report = notes
        .migrate
        .migrate(&principal.sub, crate::migrations::MIGRATIONS.clone())
        .await
        .map_err(answer)?;
    Ok(Json(serde_json::json!({ "applied": report.applied.len() })))
}

#[derive(Debug, Deserialize)]
struct HighMark {
    revision: i64,
}

/// A chassis error as the edge renders it (spec 020 B-3): a denial is a
/// 403 whose body names the decision.
fn answer(error: Error) -> Response {
    rahi_edge::error::response(&error)
}

/// A note id: the creation instant in nanoseconds, then the process id,
/// so notes sort by creation and two cells on one clock still differ.
fn note_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("n-{nanos:x}-{:x}", std::process::id())
}

fn now() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default(),
    )
    .unwrap_or_default()
}
