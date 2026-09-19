//! The migrate verb (B-4): the cell's migrations as a deploy step, and the
//! step that adopts the cell's manifest (spec 036 B-3).
//!
//! Migrations run through `StoreHandle::migrate` (spec 011 B-5), on the
//! leader, and never at boot: `serve` refuses a store that is behind
//! (B-2), so the only way forward is this verb, run deliberately.
//!
//! Spec 036 puts the manifest on the same footing. `--adopt-manifest` makes
//! this one verb the whole of a deploy's schema and ceiling movement: the
//! migrations, then a transition record when the booted manifest differs
//! from the chain's current one. The order inside [`adopt`] is the part
//! worth reading, because it is what D-6 guarantees: the transition's size
//! is measured **before** any migration is applied, so a manifest too large
//! to be retained leaves the store and the chain exactly as they were.

use std::collections::BTreeSet;

use rahi_kernel::Manifest;
use rahi_ledger::{BinaryVersions, Hash, Ledger, ManifestTransition, SYSTEM_DEPLOY};
use rahi_store::{Migration, MigrationReport, RecordedMigration, Store, check_checksums};
use rahi_types::{Error, Result, Sub};

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

/// The store is one this binary may serve (B-2, spec 036 B-7 and B-8).
///
/// Three questions. A recorded version whose SQL differs from this binary's
/// is asked first and is [`Error::Integrity`], fatal, because what ran and
/// what this build declares are not the same migration (036 B-7), and that
/// is true whether or not the store is also behind (036 D-13). Behind the
/// binary is then [`Error::Stale`] naming the command that closes the gap,
/// as B-2 has always had it. Ahead of the
/// binary is admitted only when every version above the binary's last
/// declared itself additive, and refused with [`Error::Stale`] naming the
/// first that did not (036 B-8): before this spec it was admitted in
/// silence, which is how an incompatible rollback came to serve a schema it
/// could not read.
///
/// # Errors
///
/// [`Error::Stale`] (exit 2) when the store is behind, or ahead across a
/// migration that is not recorded additive; [`Error::Integrity`] (exit 1) on
/// a checksum mismatch; a store failure as itself.
pub async fn check_current(store: &Store, migrations: &[Migration]) -> Result<()> {
    let history = store.handle().recorded_migrations().await?;
    // Spec 036 D-13: the checksum check runs first, whether or not the store
    // is also behind. It is a statement about what already ran, and a store
    // that is behind as well would otherwise be told only to run `migrate`,
    // which would apply the pending version on top of a history this binary
    // cannot vouch for.
    check_checksums(&history, migrations)?;
    let current = history
        .last()
        .map_or(rahi_store::migrate::BASELINE_VERSION, |row| row.version);
    let expected = expected_version(migrations);
    if current < expected {
        return Err(Error::Stale(format!(
            "schema_version is {current}, the cell expects {expected}; run: rahi migrate"
        )));
    }
    check_ahead(&history, expected)
}

/// Every applied version above `expected` declared itself additive
/// (spec 036 B-8).
///
/// # Errors
///
/// [`Error::Stale`] naming the first version above `expected` that is not
/// recorded additive.
pub fn check_ahead(history: &[RecordedMigration], expected: u32) -> Result<()> {
    let Some(blocker) = history
        .iter()
        .filter(|row| row.version > expected)
        .find(|row| !row.is_additive())
    else {
        return Ok(());
    };
    let current = history.last().map_or(expected, |row| row.version);
    Err(Error::Stale(format!(
        "schema_version is {current} and this binary's last migration is {expected}: version {} \
         ({}) is not recorded additive, so this binary cannot serve the schema above it; the \
         repair is forward, never a down migration",
        blocker.version, blocker.name
    )))
}

/// Run `migrations` on `store` (B-4).
///
/// # Errors
///
/// [`Error::Stale`] on a follower, naming where the leader must be looked
/// for; whatever `StoreHandle::migrate` returns otherwise.
pub async fn run(store: &Store, migrations: &[Migration]) -> Result<MigrationReport> {
    refuse_follower(store).await?;
    store.migrate(migrations).await
}

/// This node is the leader, or the deploy step says where the leader is.
///
/// # Errors
///
/// [`Error::Stale`] (exit 2) naming this node and the peers to try instead.
pub async fn refuse_follower(store: &Store) -> Result<()> {
    if store.is_leader().await {
        return Ok(());
    }
    let peers: Vec<String> = store
        .config()
        .nodes
        .iter()
        .filter(|p| p.id != store.config().node_id)
        .map(|p| format!("node {} at {}", p.id, p.api_addr))
        .collect();
    Err(Error::Stale(format!(
        "this node ({}) is a follower; run migrate on the leader, one of: {}",
        store.config().node_id,
        if peers.is_empty() {
            "(no peers declared)".to_owned()
        } else {
            peers.join(", ")
        }
    )))
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

/// What an adoption run did (spec 036 B-3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Adoption {
    /// The chain's current manifest before this run.
    pub from: Hash,
    /// The booted manifest's hash.
    pub to: Hash,
    /// The record the transition landed under, or `None` when the chain
    /// already named the booted manifest and nothing was appended.
    pub appended: Option<Hash>,
    /// The grants this manifest has that the previous one did not.
    pub added: Vec<String>,
    /// The grants the previous manifest had that this one does not.
    pub removed: Vec<String>,
    /// Whether the previous ceiling's text was recoverable, so the two lists
    /// above are a diff rather than an absence (D-9).
    pub diffed: bool,
}

impl Adoption {
    /// Whether this run appended a transition.
    #[must_use]
    pub fn adopted(&self) -> bool {
        self.appended.is_some()
    }
}

/// Every grant a manifest declares, as `service:capability` (spec 036 B-3).
#[must_use]
pub fn grants(manifest: &Manifest) -> BTreeSet<String> {
    manifest
        .services
        .iter()
        .flat_map(|(service, declared)| {
            declared
                .capabilities
                .iter()
                .map(move |capability| format!("{service}:{capability}"))
        })
        .collect()
}

/// The transition this deploy would append, for the size preflight (D-6).
///
/// `schema_version` is the value the store's will hold once this run has
/// applied its migrations: the larger of what it already records and the
/// cell's own last migration, because `migrate` never lowers it. That makes
/// the preflight's measure the appended record's measure rather than an
/// approximation of it.
fn candidate(
    manifest: &Manifest,
    from: Hash,
    to: Hash,
    schema_version: u32,
) -> Result<ManifestTransition> {
    Ok(ManifestTransition::new(
        from,
        to,
        manifest.canonical_model()?,
        schema_version,
        BinaryVersions {
            rahi: env!("CARGO_PKG_VERSION").to_owned(),
            contract: manifest.contract.version.clone(),
        },
        Sub::new(SYSTEM_DEPLOY),
    ))
}

/// Apply the cell's migrations and adopt its manifest (spec 036 B-3).
///
/// The order is the guarantee. Leadership first, as `migrate` has always
/// checked it. Then, when the booted manifest differs from the chain's
/// current one, the transition's size is measured against
/// `ledger.max_record_bytes` **before** a migration is applied, so an
/// oversized manifest exits with the store and the chain untouched (D-6).
/// Only then do the migrations run, and only then is the transition
/// appended, which measures again as its own backstop.
///
/// When the two manifests are equal nothing is appended and the report says
/// so.
///
/// # Errors
///
/// [`Error::Stale`] on a follower, and from the checks `migrate` already
/// makes; [`Error::Validation`] when the transition does not fit
/// `ledger.max_record_bytes`, with nothing applied and nothing appended
/// (D-6); [`Error::Integrity`] when the chain does not verify or a recorded
/// migration's checksum differs from this binary's.
pub async fn adopt(
    store: &Store,
    ledger: &Ledger,
    manifest: &Manifest,
    migrations: &[Migration],
) -> Result<(MigrationReport, Adoption)> {
    refuse_follower(store).await?;

    let to = manifest.hash()?;
    let from = ledger.current_manifest().await?;
    let predicted = expected_version(migrations).max(schema_version(store).await?);

    if from != to {
        // D-6's preflight: everything the record carries is known here, so a
        // manifest that cannot be retained stops the deploy before it has
        // changed anything.
        let candidate = candidate(manifest, from.clone(), to.clone(), predicted)?;
        let measured = ledger.measure_transition(&candidate)?;
        rahi_ledger::check_fits(&candidate, measured, manifest.ledger.max_record_bytes)?;
    }

    let report = run(store, migrations).await?;

    // The chain is re-read rather than assumed: another deploy step may have
    // adopted this very manifest while the migrations ran, and B-3 answers
    // that by appending nothing.
    let from = ledger.current_manifest().await?;
    let (appended, added, removed, diffed) = if from == to {
        (None, Vec::new(), Vec::new(), true)
    } else {
        let previous = previous_model(ledger).await?;
        let (added, removed, diffed) = diff_grants(previous.as_ref(), manifest);
        let transition = candidate(manifest, from.clone(), to.clone(), report.current)?;
        let hash = ledger
            .append_transition(&transition, manifest.ledger.max_record_bytes)
            .await?;
        (Some(hash), added, removed, diffed)
    };

    Ok((
        report,
        Adoption {
            from,
            to,
            appended,
            added,
            removed,
            diffed,
        },
    ))
}

/// The manifest the chain currently names, parsed back from the newest
/// resident transition record (D-9, D-14).
///
/// `None` is a real absence and only that: the chain has never transitioned,
/// or the transition that set the current manifest has been sealed away and
/// the model is in the archive, which a deploy step does not fetch. A read
/// that did not answer comes back as [`rahi_types::Error::Integrity`] from
/// the guarded read rather than as a `None` the caller would print as an
/// unavailable diff (D-11, D-14).
async fn previous_model(ledger: &Ledger) -> Result<Option<Manifest>> {
    ledger
        .current_manifest_model()
        .await?
        .map(|model| Manifest::parse_model(&model))
        .transpose()
}

/// The grants added and removed, and whether the two lists are a diff.
fn diff_grants(
    previous: Option<&Manifest>,
    adopted: &Manifest,
) -> (Vec<String>, Vec<String>, bool) {
    let Some(previous) = previous else {
        return (Vec::new(), Vec::new(), false);
    };
    let before = grants(previous);
    let after = grants(adopted);
    (
        after.difference(&before).cloned().collect(),
        before.difference(&after).cloned().collect(),
        true,
    )
}

/// Render an adoption the way the verb prints it (spec 036 B-3).
#[must_use]
pub fn render_adoption(adoption: &Adoption) -> String {
    if !adoption.adopted() {
        return format!(
            "adopt-manifest: the chain already names {}; nothing appended",
            adoption.to
        );
    }
    let list = |label: &str, items: &[String]| {
        if items.is_empty() {
            format!("{label}: none")
        } else {
            format!("{label}: {}", items.join(", "))
        }
    };
    let grants = if adoption.diffed {
        format!(
            "{}; {}",
            list("grants added", &adoption.added),
            list("grants removed", &adoption.removed)
        )
    } else {
        "grants added and removed: not shown; the chain holds no earlier manifest to diff against"
            .to_owned()
    };
    format!(
        "adopt-manifest: {} -> {} appended as {}; {grants}",
        adoption.from,
        adoption.to,
        adoption
            .appended
            .as_ref()
            .map_or_else(String::new, ToString::to_string)
    )
}
