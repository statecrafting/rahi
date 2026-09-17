//! The leader-only backup surface (spec 011 B-5, constitution XII).
//!
//! `backup` issues hiqlite's `VACUUM main INTO` on the writer thread and,
//! with an S3 target configured, encrypts and pushes the file. Restore is
//! not here: it is a cluster reset and a boot-time concern of spec 030.
//!
//! # What a backup returns (spec 037 B-2)
//!
//! hiqlite's `Client::backup` returns before the file exists, and it
//! *silently ignores* a request made within [`SUPPRESSION_WINDOW`] of the
//! last one while acknowledging it as success (hiqlite 0.14,
//! `state_machine/sqlite/writer.rs`). Reporting the newest file on disk
//! after such a call reports somebody else's snapshot as this call's.
//!
//! What makes freshness decidable is the name: hiqlite writes
//! `backup_node_<id>_<ts>.sqlite`, where `<ts>` is the epoch second at
//! which the request was issued, and the writer applies the log in order,
//! so the `VACUUM` behind a snapshot whose `<ts>` is at or after the
//! second this process triggered in ran after that trigger, whoever issued
//! it. Age on disk proves nothing of the kind and is not used.
//!
//! So [`StoreHandle::backup`] serialises rahi's own requests, waits out
//! the remainder of the suppression window before it triggers, accepts
//! only a snapshot at or after its own trigger second, triggers again when
//! an outside request wins the window, and bounds the whole sequence by
//! one deadline. It never reports a stale file as a success.

use std::time::{Duration, Instant};

use rahi_types::Error;
use serde::{Deserialize, Serialize};

use crate::error::map;
use crate::store::StoreHandle;

/// How long the whole of [`StoreHandle::backup`] may take: the wait for
/// hiqlite's suppression window, the trigger, and the wait for the file
/// (spec 037 B-2).
pub const BACKUP_DEADLINE: Duration = Duration::from_secs(120);

/// hiqlite ignores a backup request made within this long of the one
/// before and acknowledges it anyway. Its timer is process-local, so the
/// first request after a restart is never suppressed.
pub const SUPPRESSION_WINDOW: Duration = Duration::from_secs(60);

/// How long one trigger is given to produce its file before the sequence
/// assumes the request was suppressed and tries again inside the deadline.
pub const FILE_WAIT: Duration = Duration::from_secs(30);

/// How often the local listing is re-read while waiting.
pub const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// One process takes one backup at a time, so that two callers of this
/// chassis never race each other into hiqlite's suppression window. It
/// says nothing about a request from outside this process, which is what
/// the trigger-second rule is for.
static ONE_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The name of one backup file, `backup_node_<id>_<ts>.sqlite`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BackupId(String);

impl BackupId {
    /// The file name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The epoch second hiqlite named this snapshot for: the second the
    /// request that produced it was issued at.
    #[must_use]
    pub fn taken_at(&self) -> Option<i64> {
        snapshot_ts(&self.0)
    }
}

/// The `<ts>` of `backup_node_<id>_<ts>.sqlite`.
#[must_use]
pub fn snapshot_ts(name: &str) -> Option<i64> {
    name.strip_prefix("backup_node_")?
        .strip_suffix(".sqlite")?
        .rsplit_once('_')
        .and_then(|(_, ts)| ts.parse().ok())
}

/// Seconds since the epoch.
fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// One snapshot as listed locally or in S3.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupListing {
    /// The file or object name.
    pub id: BackupId,
    /// Last modification, seconds since the Unix epoch, as the lister reported.
    pub last_modified: i64,
    /// Size in bytes, when the lister knows it.
    pub size: Option<u64>,
}

impl StoreHandle {
    /// Take a backup and return the snapshot it produced. Leader-only.
    ///
    /// Bounded by [`BACKUP_DEADLINE`]; see the module documentation for
    /// what makes the returned snapshot this call's and not an older one.
    ///
    /// # Errors
    ///
    /// [`Error::Stale`] on a follower: the caller retries against the
    /// leader; [`Error::Upstream`] when hiqlite fails to write it, or when
    /// the deadline runs out before a snapshot of this call's own exists.
    pub async fn backup(&self) -> Result<BackupId, Error> {
        self.backup_within(BACKUP_DEADLINE).await
    }

    /// [`StoreHandle::backup`] under a deadline of your own. A deadline
    /// shorter than the remaining suppression window is a refusal, not a
    /// stale file.
    ///
    /// # Errors
    ///
    /// As [`StoreHandle::backup`].
    pub async fn backup_within(&self, deadline: Duration) -> Result<BackupId, Error> {
        if !self.is_leader().await {
            return Err(Error::Stale(
                "backup is leader-only; this node is a follower".to_owned(),
            ));
        }
        // Two callers of this process never race each other into hiqlite's
        // suppression window; the trigger-second rule below covers anyone
        // else (spec 037 B-2).
        let _serialised = ONE_AT_A_TIME.lock().await;
        let started = Instant::now();
        let expired = |waited: Duration| -> Error {
            Error::Upstream(format!(
                "no snapshot of this backup's own within the {} second deadline (waited {}): \
                 hiqlite ignores a backup request within {} seconds of the one before, and \
                 the window did not clear",
                deadline.as_secs(),
                waited.as_secs(),
                SUPPRESSION_WINDOW.as_secs(),
            ))
        };
        loop {
            // hiqlite would ignore a trigger inside the window of the
            // newest snapshot, and answer success anyway. Wait it out.
            if let Some(wait) = self.suppressed_for().await? {
                if started.elapsed() + wait > deadline {
                    return Err(expired(started.elapsed()));
                }
                tokio::time::sleep(wait).await;
            }
            if started.elapsed() >= deadline {
                return Err(expired(started.elapsed()));
            }
            let trigger = unix_now();
            self.client().backup().await.map_err(map)?;
            let wait_until = started + FILE_WAIT.min(deadline.saturating_sub(started.elapsed()));
            loop {
                if let Some(id) = self.snapshot_since(trigger).await? {
                    return Ok(id);
                }
                if Instant::now() >= wait_until {
                    // Either the trigger was suppressed by a request from
                    // outside this process, or the writer is slow. Both are
                    // answered the same way: go round, wait the window out
                    // again, and trigger again inside the deadline.
                    break;
                }
                tokio::time::sleep(POLL_INTERVAL).await;
            }
            if started.elapsed() >= deadline {
                return Err(expired(started.elapsed()));
            }
        }
    }

    /// How long until hiqlite would stop ignoring a backup request, given
    /// the newest snapshot already on disk. `None` when it would take one
    /// now.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the backup directory cannot be read.
    pub async fn suppressed_for(&self) -> Result<Option<Duration>, Error> {
        let newest = self
            .backup_list_local()
            .await?
            .iter()
            .filter_map(|l| l.id.taken_at())
            .max();
        let Some(newest) = newest else {
            return Ok(None);
        };
        let age = unix_now().saturating_sub(newest);
        let window = i64::try_from(SUPPRESSION_WINDOW.as_secs()).unwrap_or(60);
        if (0..window).contains(&age) {
            // One second past the window: hiqlite compares against a
            // timestamp of its own taking, which may be a fraction later
            // than the second its file name carries.
            let remaining = u64::try_from(window - age).unwrap_or(0).saturating_add(1);
            return Ok(Some(Duration::from_secs(remaining)));
        }
        Ok(None)
    }

    /// The snapshot this call's trigger produced: one whose name carries a
    /// second at or after `trigger`. Nothing older is this call's.
    async fn snapshot_since(&self, trigger: i64) -> Result<Option<BackupId>, Error> {
        Ok(self
            .backup_list_local()
            .await?
            .into_iter()
            .filter(|l| l.id.taken_at().is_some_and(|ts| ts >= trigger))
            .map(|l| l.id)
            .max())
    }

    /// The snapshots under the local backup directory.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the directory cannot be read.
    pub async fn backup_list_local(&self) -> Result<Vec<BackupListing>, Error> {
        let mut list: Vec<BackupListing> = match self.attached_backup_dir() {
            Some(Some(dir)) => list_backup_dir(dir)?,
            Some(None) => {
                return Err(Error::Config(
                    "a client of the cluster has no local backup directory; run backup inside the leader replica".to_owned(),
                ));
            }
            None => self
                .client()
                .backup_list_local()
                .await
                .map_err(map)?
                .into_iter()
                .map(|l| BackupListing {
                    id: BackupId(l.name),
                    last_modified: l.last_modified,
                    size: l.size,
                })
                .collect(),
        };
        list.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(list)
    }

    /// The snapshots in the S3 target.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when no S3 target is configured; [`Error::Upstream`]
    /// when the bucket cannot be listed.
    pub async fn backup_list_s3(&self) -> Result<Vec<BackupListing>, Error> {
        if !self.has_s3() {
            return Err(Error::Config(
                "no S3 backup target is configured".to_owned(),
            ));
        }
        let mut list: Vec<BackupListing> = self
            .client()
            .backup_list_s3()
            .await
            .map_err(map)?
            .into_iter()
            .map(|l| BackupListing {
                id: BackupId(l.name),
                last_modified: l.last_modified,
                size: l.size,
            })
            .collect();
        list.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(list)
    }
}

/// The backup files under `dir`, read from the filesystem the way hiqlite
/// reads its own, for a handle attached from outside the node (spec 032
/// B-5). A missing directory is an empty list.
fn list_backup_dir(dir: &std::path::Path) -> Result<Vec<BackupListing>, Error> {
    let mut list = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(list),
        Err(err) => {
            return Err(Error::Io(format!(
                "backup directory {} cannot be read: {err}",
                dir.display()
            )));
        }
    };
    for entry in entries {
        let entry =
            entry.map_err(|err| Error::Io(format!("backup directory {}: {err}", dir.display())))?;
        let meta = entry
            .metadata()
            .map_err(|err| Error::Io(format!("{}: {err}", entry.path().display())))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if meta.is_dir() || !name.starts_with("backup_node_") {
            continue;
        }
        let last_modified = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|d| i64::try_from(d.as_secs()).ok())
            .unwrap_or_default();
        list.push(BackupListing {
            id: BackupId(name),
            last_modified,
            size: Some(meta.len()),
        });
    }
    Ok(list)
}
