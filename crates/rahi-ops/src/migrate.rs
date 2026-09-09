//! The migrate verb (B-4): the cell's migrations as a deploy step.
//!
//! Migrations run through `StoreHandle::migrate` (spec 011 B-5), on the
//! leader, and never at boot: `serve` refuses a store that is behind
//! (B-2), so the only way forward is this verb, run deliberately.

use rahi_store::{Migration, MigrationReport, Store};
use rahi_types::{Error, Result};

/// The recorded schema version of `store`, without creating anything.
///
/// A store no migration has ever touched has no `schema_version` table; that
/// reads as version `0`, the baseline spec 011 reserves, rather than as an
/// error, so that `serve` and `preflight` can ask without a deploy step
/// having run.
///
/// # Errors
///
/// A store failure other than the table's absence.
pub async fn schema_version(store: &Store) -> Result<u32> {
    #[derive(serde::Deserialize)]
    struct Row {
        version: u32,
    }
    #[derive(serde::Deserialize)]
    struct Name {
        name: String,
    }
    // A leader-side read fault inside this statement comes back as no rows
    // (spec 016 D-2 records the quirk) and would read as the baseline; the
    // lookup is a fixed probe against sqlite_master, and the migrate verb
    // that acts on the answer reads the table again through the store's own
    // guarded path.
    let tables: Vec<Name> = store
        .query_consistent(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'schema_version'",
            Vec::new(),
        )
        .await?;
    if !tables.iter().any(|t| t.name == "schema_version") {
        return Ok(rahi_store::migrate::BASELINE_VERSION);
    }
    let rows: Vec<Row> = store
        .query_consistent(
            "SELECT version FROM schema_version ORDER BY version DESC LIMIT 1",
            Vec::new(),
        )
        .await?;
    Ok(rows
        .first()
        .map_or(rahi_store::migrate::BASELINE_VERSION, |r| r.version))
}

/// The version the cell expects: its newest migration, or the baseline when
/// it declares none.
#[must_use]
pub fn expected_version(migrations: &[Migration]) -> u32 {
    migrations
        .iter()
        .map(|m| m.version)
        .max()
        .unwrap_or(rahi_store::migrate::BASELINE_VERSION)
}

/// The store is at least as new as the cell's migrations.
///
/// # Errors
///
/// [`Error::Stale`] naming both versions and the command that closes the
/// gap (B-2); a store failure as itself.
pub async fn check_current(store: &Store, migrations: &[Migration]) -> Result<()> {
    let current = schema_version(store).await?;
    let expected = expected_version(migrations);
    if current < expected {
        return Err(Error::Stale(format!(
            "schema_version is {current}, the cell expects {expected}; run: rahi migrate"
        )));
    }
    Ok(())
}

/// Run `migrations` on `store` (B-4).
///
/// # Errors
///
/// [`Error::Stale`] on a follower, naming where the leader must be looked
/// for; whatever `StoreHandle::migrate` returns otherwise.
pub async fn run(store: &Store, migrations: &[Migration]) -> Result<MigrationReport> {
    if !store.is_leader().await {
        let peers: Vec<String> = store
            .config()
            .nodes
            .iter()
            .filter(|p| p.id != store.config().node_id)
            .map(|p| format!("node {} at {}", p.id, p.api_addr))
            .collect();
        return Err(Error::Stale(format!(
            "this node ({}) is a follower; run migrate on the leader, one of: {}",
            store.config().node_id,
            if peers.is_empty() {
                "(no peers declared)".to_owned()
            } else {
                peers.join(", ")
            }
        )));
    }
    store.migrate(migrations).await
}

/// Render a report the way the verb prints it.
#[must_use]
pub fn render(report: &MigrationReport) -> String {
    let applied = if report.applied.is_empty() {
        "none".to_owned()
    } else {
        report
            .applied
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        "migrate: schema_version {} -> {}, applied: {applied}",
        report.previous, report.current
    )
}
