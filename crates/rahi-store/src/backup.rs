//! The leader-only backup surface (spec 011 B-5, constitution XII).
//!
//! `backup` issues hiqlite's `VACUUM main INTO` on the writer thread and,
//! with an S3 target configured, encrypts and pushes the file. Restore is
//! not here: it is a cluster reset and a boot-time concern of spec 030.

use rahi_types::Error;
use serde::{Deserialize, Serialize};

use crate::error::map;
use crate::store::StoreHandle;

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
    /// Take a backup. Leader-only.
    ///
    /// # Errors
    ///
    /// [`Error::Stale`] on a follower: the caller retries against the
    /// leader; [`Error::Upstream`] when hiqlite fails to write it.
    pub async fn backup(&self) -> Result<BackupId, Error> {
        if !self.is_leader().await {
            return Err(Error::Stale(
                "backup is leader-only; this node is a follower".to_owned(),
            ));
        }
        let before: Vec<BackupListing> = self.backup_list_local().await?;
        self.client().backup().await.map_err(map)?;
        let after = self.backup_list_local().await?;
        after
            .into_iter()
            .filter(|l| !before.iter().any(|b| b.id == l.id))
            .map(|l| l.id)
            .max()
            .ok_or_else(|| {
                Error::Upstream("backup completed but no new file was listed".to_owned())
            })
    }

    /// The snapshots under the local backup directory.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the directory cannot be read.
    pub async fn backup_list_local(&self) -> Result<Vec<BackupListing>, Error> {
        let mut list: Vec<BackupListing> = self
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
            .collect();
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
