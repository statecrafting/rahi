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
    /// The archive was applied and the marker written.
    Restored(Marker),
    /// The marker already named this archive; nothing was touched.
    AlreadyRestored(Marker),
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
/// from opening the archive, before anything is written; [`Error::Io`] when
/// the volume cannot be written.
pub async fn run(config: &Config, archive_path: &Path, key: &KeySource) -> Result<Outcome> {
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
    Ok(Outcome::Restored(marker))
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
