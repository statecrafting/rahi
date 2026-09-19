//! The restore verb (B-6): a cluster reset, single-shot by construction.
//!
//! Restore runs only when neither hiqlite node is running, verifies every
//! part of the archive before it writes anything, and then does what
//! hiqlite's own restore path does for the app store: it empties the state
//! machine, the snapshots, the lock, and the Raft log, and places the
//! snapshot as the database so the node boots from it and every peer
//! rejoins by snapshot. rauthy's snapshot is left where spec 031's
//! supervisor hands it to rauthy's own restore on the next start. The
//! marker written last is what makes a second run of the same archive a
//! no-op: there is no variable to unset and nothing that re-applies.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use rahi_types::{Config, Error, Result};

use rahi_store::{Migration, RecordedMigration};

use crate::KeySet;
use crate::archive::{self, APP_DIR, ArchiveManifest, KEYS_DIR, Part, RAUTHY_DIR};

/// The database file hiqlite opens under `state_machine/db`.
pub const APP_DB_FILE: &str = "hiqlite.db";

/// The subdirectories of the app node hiqlite rebuilds from the database
/// on the next start, relative to the hiqlite directory.
pub const APP_RESET_PATHS: [&str; 4] = [
    "state_machine/db",
    "state_machine/snapshots",
    "state_machine/lock",
    "logs",
];

/// What `restore` writes when it is done (B-6).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marker {
    /// The archive's file name.
    pub archive: String,
    /// sha256 of the sealed archive, hex.
    pub sha256: String,
    /// When it was restored, seconds since the epoch.
    pub restored: u64,
    /// Where rauthy's snapshot was placed for rauthy's own restore.
    pub rauthy_snapshot: String,
    /// When the supervisor handed that snapshot to rauthy and rauthy came
    /// up healthy on it, seconds since the epoch (spec 037 B-3). `None` is
    /// "not yet", which is what a restore writes and what every start
    /// before the first healthy one reads. Absent in a marker written
    /// before spec 037, which reads as `None` and is applied on the next
    /// start: the restored rauthy of such a volume is empty, so applying
    /// it is the repair.
    #[serde(default)]
    pub rauthy_snapshot_applied: Option<u64>,
    /// The archive manifest that was applied.
    pub manifest: ArchiveManifest,
}

impl Marker {
    /// Read the marker at `path`, if there is one.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the file exists but cannot be read;
    /// [`Error::Validation`] when it does not parse.
    pub fn read(path: &Path) -> Result<Option<Self>> {
        match std::fs::read(path) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(Error::Io(format!(
                "restore marker {} cannot be read: {err}",
                path.display()
            ))),
            Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|err| {
                Error::Validation(format!(
                    "restore marker {} does not parse: {err}",
                    path.display()
                ))
            }),
        }
    }
}

/// The restore marker captured when preparing this child's restore source.
/// Only a child prepared with this source can certify its application.
#[derive(Clone, Debug)]
pub struct PendingRauthySnapshot {
    marker: Marker,
}

impl PendingRauthySnapshot {
    /// The exact source supplied to the child.
    #[must_use]
    pub fn path(&self) -> &Path {
        Path::new(&self.marker.rauthy_snapshot)
    }
}

/// Capture the pending source for this start (spec 037 B-3).
///
/// `None` means no marker, no named snapshot, or an already applied snapshot.
/// A named pending source that is missing, unreadable, or not a file fails
/// closed before the child starts. Keeping an applied snapshot is optional.
///
/// # Errors
/// [`Error::Io`] for an inaccessible source; [`Error::Validation`] for a
/// malformed marker or a source that is not a regular file.
pub fn pending_rauthy_snapshot(config: &Config) -> Result<Option<PendingRauthySnapshot>> {
    let Some(marker) = Marker::read(&crate::restore_marker(config))? else {
        return Ok(None);
    };
    if marker.rauthy_snapshot_applied.is_some() || marker.rauthy_snapshot.is_empty() {
        return Ok(None);
    }
    let path = Path::new(&marker.rauthy_snapshot);
    let metadata = std::fs::metadata(path).map_err(|err| {
        Error::Io(format!(
            "pending rauthy snapshot {} cannot be inspected: {err}",
            path.display()
        ))
    })?;
    if !metadata.is_file() {
        return Err(Error::Validation(format!(
            "pending rauthy snapshot {} is not a file",
            path.display()
        )));
    }
    // Check readability without consuming or opening rauthy's own store.
    std::fs::File::open(path).map_err(|err| {
        Error::Io(format!(
            "pending rauthy snapshot {} cannot be opened: {err}",
            path.display()
        ))
    })?;
    Ok(Some(PendingRauthySnapshot { marker }))
}

/// Record health only for the pending source supplied to this child.
///
/// Normal health has no restore token and cannot advance a marker. If the
/// marker changed while the child started, fail rather than certify another
/// source. A failed child never calls this and leaves the restore retryable.
///
/// # Errors
/// [`Error::Io`] when the marker cannot be read or written;
/// [`Error::Validation`] when malformed; [`Error::Conflict`] if it changed.
pub async fn record_rauthy_snapshot_applied(
    config: &Config,
    supplied: Option<&PendingRauthySnapshot>,
) -> Result<Option<PathBuf>> {
    let Some(supplied) = supplied else {
        return Ok(None);
    };
    let path = crate::restore_marker(config);
    let current = Marker::read(&path)?;
    if current.as_ref() != Some(&supplied.marker) {
        return Err(Error::Conflict(
            "restore marker changed after preparing rauthy's source".to_owned(),
        ));
    }
    let mut marker = supplied.marker.clone();
    marker.rauthy_snapshot_applied = Some(crate::unix_now());
    let bytes = serde_json::to_vec_pretty(&marker)
        .map_err(|err| Error::Io(format!("the restore marker cannot be serialised: {err}")))?;
    let temporary = path.with_extension("marker.pending");
    tokio::fs::write(&temporary, bytes).await.map_err(|err| {
        Error::Io(format!(
            "restore marker {} cannot be written: {err}",
            temporary.display()
        ))
    })?;
    tokio::fs::rename(&temporary, &path).await.map_err(|err| {
        Error::Io(format!(
            "restore marker {} cannot be replaced: {err}",
            path.display()
        ))
    })?;
    Ok(Some(supplied.path().to_path_buf()))
}

/// How a restore ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The archive was applied and the marker written, on the schema
    /// evidence named (spec 036 D-12).
    Restored(Marker, SchemaEvidence),
    /// The marker already named this archive; nothing was touched.
    AlreadyRestored(Marker),
}

/// What the running cell is, for the checks spec 036 B-9 makes before a
/// restore writes anything.
#[derive(Clone, Copy, Debug)]
pub struct Compatibility<'a> {
    /// The booted manifest's hash, as text.
    pub manifest_hash: &'a str,
    /// The cell's migrations.
    pub migrations: &'a [Migration],
    /// Whether `--adopt` was given: the operator's statement that the
    /// manifest difference is intended and the next deploy step will
    /// ledger it.
    pub adopt: bool,
}

/// Where the schema evidence a restore was judged on came from (spec 036
/// D-12).
///
/// There is no third variant, because there is no restore that was not
/// judged: an archive whose compatibility cannot be established is refused
/// before the destination is touched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchemaEvidence {
    /// The archive's `manifest.json` recorded its migration history.
    Recorded,
    /// The manifest recorded none, so the history was read out of the
    /// archived database itself.
    ArchivedDatabase,
}

impl SchemaEvidence {
    /// One line naming the evidence, for the verb to print.
    #[must_use]
    pub fn describe(self) -> &'static str {
        match self {
            Self::Recorded => "the archive's recorded migration history",
            Self::ArchivedDatabase => {
                "the schema_version table read out of the archived database (the archive \
                 predates spec 036 and its manifest records no history)"
            }
        }
    }
}

/// The archive is one this binary can serve (spec 036 B-9, D-12).
///
/// Two refusals, both [`Error::Stale`] (exit 2), both before a byte is
/// written. The schema one is the same question `serve` asks of a live store
/// (B-8), because a version above this binary's last is one this binary
/// knows nothing about. The manifest one refuses an archive whose chain
/// names a ceiling this binary does not, unless the operator said `--adopt`.
///
/// Schema compatibility is **established** or the restore is refused; there
/// is no third outcome and no flag that buys one, and `--adopt` authorizes a
/// manifest difference only (D-12). An archive written before spec 036
/// records no history in its manifest, and the evidence is then taken from
/// the archive's own payload: the app part is the destination's database
/// byte for byte ([`reset_app_node`]), so its `schema_version` table is the
/// same recorded history a live store answers from. An archive whose payload
/// yields no such evidence is refused.
///
/// # Errors
///
/// [`Error::Stale`] naming what is incompatible, or what evidence could not
/// be obtained, and the flag or the forward fix that resolves it. Nothing is
/// written on any path here.
pub fn check_compatible(
    archive: &ArchiveManifest,
    parts: &[Part],
    cell: &Compatibility<'_>,
) -> Result<SchemaEvidence> {
    let expected = crate::migrate::expected_version(cell.migrations);
    let (history, evidence) = match &archive.schema {
        Some(schema) => (schema.migrations.clone(), SchemaEvidence::Recorded),
        None => (archived_history(parts)?, SchemaEvidence::ArchivedDatabase),
    };
    crate::migrate::check_ahead(&history, expected).map_err(|err| {
        Error::Stale(format!(
            "the archive cannot be restored into this binary: {} (judged on {})",
            err.message(),
            evidence.describe()
        ))
    })?;
    if archive.manifest_hash != cell.manifest_hash && !cell.adopt {
        return Err(Error::Stale(format!(
            "the archive's chain names manifest {} and this binary's manifest hashes to {}: \
             restoring it would put a chain under a ceiling it has never named; pass --adopt to \
             restore anyway, and the next rahi migrate --adopt-manifest will ledger the change",
            archive.manifest_hash, cell.manifest_hash
        )));
    }
    Ok(evidence)
}

/// The migration history an archive carries in its payload (spec 036 D-12).
///
/// The app part is written to the destination as `hiqlite.db` unchanged, so
/// it is an ordinary SQLite database and this reads the same
/// `schema_version` table `StoreHandle::recorded_migrations` reads from a
/// live store. It is opened read-only, from a temporary copy, and nothing
/// about the destination is touched.
///
/// An archived database with no `schema_version` table has provably applied
/// no migration, which is the baseline: that is evidence, not silence. A
/// payload that cannot be opened or read is [`Error::Stale`], because
/// compatibility was not established.
///
/// # Errors
///
/// [`Error::Stale`] naming the evidence that could not be obtained.
fn archived_history(parts: &[Part]) -> Result<Vec<RecordedMigration>> {
    let app = parts.iter().find(|p| p.dir() == APP_DIR).ok_or_else(|| {
        Error::Stale(
            "the archive records no migration history and holds no app snapshot to read one \
             from, so its schema compatibility cannot be established; nothing was restored"
                .to_owned(),
        )
    })?;
    let stale = |what: &str| {
        Error::Stale(format!(
            "the archive records no migration history and {what}, so its schema compatibility \
             cannot be established; nothing was restored"
        ))
    };
    let dir = tempfile::tempdir().map_err(|err| {
        stale(&format!(
            "a workspace for reading it cannot be made ({err})"
        ))
    })?;
    let path = dir.path().join("archived.db");
    std::fs::write(&path, &app.bytes)
        .map_err(|err| stale(&format!("its app snapshot cannot be staged ({err})")))?;
    let db = rusqlite::Connection::open_with_flags(
        &path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|err| {
        stale(&format!(
            "its app snapshot is not a readable database ({err})"
        ))
    })?;
    read_schema_version(&db).map_err(|err| {
        stale(&format!(
            "its archived schema_version table cannot be read ({err})"
        ))
    })
}

/// `schema_version` out of an already opened archived database.
fn read_schema_version(db: &rusqlite::Connection) -> rusqlite::Result<Vec<RecordedMigration>> {
    let present: i64 = db.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'schema_version'",
        [],
        |row| row.get(0),
    )?;
    if present == 0 {
        // Provably no migration has ever run in this database, which is the
        // baseline spec 011 reserves. Read out of the file's own catalogue,
        // so this is evidence rather than an absent answer.
        return Ok(Vec::new());
    }
    let mut statement = db
        .prepare("SELECT version, name, checksum, additive FROM schema_version ORDER BY version")?;
    let rows = statement.query_map([], |row| {
        let version: i64 = row.get(0)?;
        let name: String = row.get(1)?;
        let checksum: Option<String> = row.get(2)?;
        let additive: Option<i64> = row.get(3)?;
        Ok(RecordedMigration {
            version: u32::try_from(version).unwrap_or(u32::MAX),
            name,
            checksum: checksum.filter(|text| !text.is_empty()),
            additive: additive.map(|flag| flag != 0),
        })
    })?;
    rows.collect()
}

/// Where the backup identity comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeySource {
    /// A file holding one age identity.
    File(PathBuf),
    /// The deployment's own key set, when it still has one.
    KeySet,
}

/// Restore `archive_path` into `config`'s volume (B-6).
///
/// # Errors
///
/// [`Error::Conflict`] when a node is running or its lock file is present;
/// [`Error::Unauthorized`], [`Error::Integrity`], or [`Error::Validation`]
/// from opening the archive, before anything is written; [`Error::Stale`]
/// (exit 2) when the archive is not one this binary can serve (spec 036
/// B-9), also before anything is written; [`Error::Io`] when the volume
/// cannot be written.
pub async fn run(
    config: &Config,
    archive_path: &Path,
    key: &KeySource,
    cell: &Compatibility<'_>,
) -> Result<Outcome> {
    let name = archive_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| Error::Validation(format!("{} names no file", archive_path.display())))?;

    let marker_path = crate::restore_marker(config);
    if let Some(marker) = Marker::read(&marker_path)?
        && marker.archive == name
    {
        return Ok(Outcome::AlreadyRestored(marker));
    }

    refuse_running(config).await?;

    let sealed = tokio::fs::read(archive_path).await.map_err(|err| {
        Error::Io(format!(
            "archive {} cannot be read: {err}",
            archive_path.display()
        ))
    })?;
    let identity = match key {
        KeySource::File(path) => {
            let text = std::fs::read_to_string(path).map_err(|err| {
                Error::Io(format!(
                    "backup key {} cannot be read: {err}",
                    path.display()
                ))
            })?;
            crate::parse_backup_identity(&text).map_err(|err| {
                Error::Config(format!("{} is not an age identity: {err}", path.display()))
            })?
        }
        KeySource::KeySet => KeySet::of(config).backup_identity()?,
    };
    let (manifest, parts) = archive::open(&sealed, &identity)?;
    // Spec 036 B-9: the compatibility questions are asked here, after the
    // archive has proved itself intact and before the first byte of the
    // volume is touched.
    let evidence = check_compatible(&manifest, &parts, cell)?;

    let keys = KeySet::of(config);
    for part in parts.iter().filter(|p| p.dir() == KEYS_DIR) {
        keys.write(part.name(), &part.bytes)?;
    }

    let app = parts
        .iter()
        .find(|p| p.dir() == APP_DIR)
        .ok_or_else(|| Error::Integrity("the archive holds no app snapshot".to_owned()))?;
    reset_app_node(config, app).await?;

    let rauthy = parts
        .iter()
        .find(|p| p.dir() == RAUTHY_DIR)
        .ok_or_else(|| Error::Integrity("the archive holds no rauthy snapshot".to_owned()))?;
    let rauthy_snapshot = place_rauthy_snapshot(config, rauthy).await?;

    let marker = Marker {
        archive: name,
        sha256: archive::sha256_hex(&sealed),
        restored: crate::unix_now(),
        rauthy_snapshot: rauthy_snapshot.display().to_string(),
        rauthy_snapshot_applied: None,
        manifest,
    };
    let bytes = serde_json::to_vec_pretty(&marker)
        .map_err(|err| Error::Io(format!("the restore marker cannot be serialised: {err}")))?;
    tokio::fs::write(&marker_path, bytes).await.map_err(|err| {
        Error::Io(format!(
            "restore marker {} cannot be written: {err}",
            marker_path.display()
        ))
    })?;
    Ok(Outcome::Restored(marker, evidence))
}

/// Neither node is running: no lock file on either side, and nothing
/// answering on rauthy's loopback address.
///
/// # Errors
///
/// [`Error::Conflict`] naming what is in the way.
pub async fn refuse_running(config: &Config) -> Result<()> {
    let app_lock = crate::app_lock_file(config);
    if app_lock.exists() {
        return Err(Error::Conflict(format!(
            "the app node holds {}; stop it (or clear an unclean shutdown) before restoring",
            app_lock.display()
        )));
    }
    let rauthy_lock = crate::rauthy_lock_file(config);
    if rauthy_lock.exists() {
        return Err(Error::Conflict(format!(
            "rauthy's node holds {}; stop it before restoring",
            rauthy_lock.display()
        )));
    }
    if tokio::time::timeout(
        std::time::Duration::from_millis(500),
        tokio::net::TcpStream::connect(config.rauthy_addr),
    )
    .await
    .is_ok_and(|r| r.is_ok())
    {
        return Err(Error::Conflict(format!(
            "something answers on rauthy's address {}; stop it before restoring",
            config.rauthy_addr
        )));
    }
    Ok(())
}

async fn reset_app_node(config: &Config, app: &Part) -> Result<()> {
    let hiqlite = config.hiqlite_dir();
    for rel in APP_RESET_PATHS {
        let path = hiqlite.join(rel);
        remove_any(&path).await?;
    }
    let db_dir = hiqlite.join("state_machine").join("db");
    tokio::fs::create_dir_all(&db_dir)
        .await
        .map_err(|err| Error::Io(format!("{} cannot be created: {err}", db_dir.display())))?;
    crate::set_mode(&hiqlite.join("state_machine"), 0o700)?;
    crate::set_mode(&db_dir, 0o700)?;
    let db = db_dir.join(APP_DB_FILE);
    tokio::fs::write(&db, &app.bytes)
        .await
        .map_err(|err| Error::Io(format!("{} cannot be written: {err}", db.display())))?;
    crate::set_mode(&db, 0o600)
}

async fn place_rauthy_snapshot(config: &Config, rauthy: &Part) -> Result<PathBuf> {
    let dir = config.data_dir.join(crate::RAUTHY_RESTORE_DIR);
    remove_any(&dir).await?;
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|err| Error::Io(format!("{} cannot be created: {err}", dir.display())))?;
    crate::set_mode(&dir, 0o700)?;
    let path = dir.join(rauthy.name());
    tokio::fs::write(&path, &rauthy.bytes)
        .await
        .map_err(|err| Error::Io(format!("{} cannot be written: {err}", path.display())))?;
    crate::set_mode(&path, 0o600)?;
    Ok(path)
}

async fn remove_any(path: &Path) -> Result<()> {
    match tokio::fs::metadata(path).await {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(Error::Io(format!(
            "{} cannot be inspected: {err}",
            path.display()
        ))),
        Ok(meta) if meta.is_dir() => tokio::fs::remove_dir_all(path)
            .await
            .map_err(|err| Error::Io(format!("{} cannot be removed: {err}", path.display()))),
        Ok(_) => tokio::fs::remove_file(path)
            .await
            .map_err(|err| Error::Io(format!("{} cannot be removed: {err}", path.display()))),
    }
}
