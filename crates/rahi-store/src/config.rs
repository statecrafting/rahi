//! The store's configuration, derived from the chassis [`Config`] by one
//! pure function (spec 011 FR-004).
//!
//! There is no field for rauthy's data directory and no way to derive one
//! (B-7): the app's hiqlite and rauthy's are two clusters that never share a
//! path.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use rahi_types::{Config, Error};
use serde::{Deserialize, Serialize};

/// The path component that marks rauthy's territory on the volume.
const RAUTHY_DIR_NAME: &str = "rauthy";

/// One encryption key of the at-rest key set.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncKey {
    /// The key id, `^[a-zA-Z0-9:_-]{2,20}$`.
    pub id: String,
    /// Exactly 32 raw key bytes.
    pub key: Vec<u8>,
}

impl std::fmt::Debug for EncKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EncKey")
            .field("id", &self.id)
            .field("key", &"<redacted>")
            .finish()
    }
}

/// The at-rest key set: every key the store may need to read, and the one
/// it writes with. Custodied once per deployment (constitution XII).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncKeys {
    /// The id of the key new writes are encrypted with.
    pub active: String,
    /// Every key, active and retired.
    pub keys: Vec<EncKey>,
}

/// The secrets the store needs that the chassis [`Config`] does not carry.
///
/// Spec 030 reads them from the keys directory; tests build them inline.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreSecrets {
    /// The Raft-internal shared secret, at least 16 bytes.
    pub secret_raft: String,
    /// The API shared secret, at least 16 bytes.
    pub secret_api: String,
    /// The at-rest key set.
    pub enc_keys: EncKeys,
}

impl std::fmt::Debug for StoreSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoreSecrets")
            .field("secret_raft", &"<redacted>")
            .field("secret_api", &"<redacted>")
            .field("enc_keys", &self.enc_keys)
            .finish()
    }
}

/// An S3 target for encrypted backups.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct S3Backup {
    /// The endpoint URL.
    pub endpoint: String,
    /// The bucket name.
    pub bucket: String,
    /// The region.
    pub region: String,
    /// The access key id.
    pub access_key: String,
    /// The secret access key.
    pub secret_key: String,
    /// Use path-style addressing (MinIO and most self-hosted targets).
    pub path_style: bool,
}

impl std::fmt::Debug for S3Backup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Backup")
            .field("endpoint", &self.endpoint)
            .field("bucket", &self.bucket)
            .field("region", &self.region)
            .field("access_key", &"<redacted>")
            .field("secret_key", &"<redacted>")
            .field("path_style", &self.path_style)
            .finish()
    }
}

/// A peer in the app's Raft cluster (spec 032 fills these in for N=3).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Peer {
    /// The node id, `1..=n`; node `1` bootstraps the cluster.
    pub id: u64,
    /// The Raft address peers dial.
    pub raft_addr: String,
    /// The API address peers dial.
    pub api_addr: String,
}

/// Everything [`crate::Store::open`] needs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreConfig {
    /// This node's id; `1` for a single voter.
    pub node_id: u64,
    /// The cluster's peers, including this node. Empty means a single voter
    /// at `raft_addr` and `api_addr`.
    pub nodes: Vec<Peer>,
    /// The app hiqlite directory. Never equal to or inside rauthy's.
    pub data_dir: PathBuf,
    /// The Raft listener.
    pub raft_addr: SocketAddr,
    /// The SQL and cache API listener.
    pub api_addr: SocketAddr,
    /// Shared secrets and the at-rest key set.
    pub secrets: StoreSecrets,
    /// Days to keep local backups.
    pub backup_keep_days: u16,
    /// The S3 backup target, if any.
    pub s3: Option<S3Backup>,
}

impl StoreConfig {
    /// Derive the store configuration from the chassis [`Config`].
    ///
    /// Pure: the data directory is `cfg.hiqlite_dir()`, the listeners are the
    /// hiqlite addresses of the chassis config, and the node is a single
    /// voter. Callers that run N=3 set `nodes` afterwards (spec 032).
    #[must_use]
    pub fn from_config(cfg: &Config, secrets: StoreSecrets) -> Self {
        Self {
            node_id: 1,
            nodes: Vec::new(),
            data_dir: cfg.hiqlite_dir(),
            raft_addr: cfg.hiqlite.raft_addr,
            api_addr: cfg.hiqlite.api_addr,
            secrets,
            backup_keep_days: 30,
            s3: None,
        }
    }

    /// Refuse a data directory that is, or lies inside, rauthy's (B-1).
    ///
    /// Rauthy's directory is the `rauthy` sibling on the same volume, so any
    /// `rauthy` path component marks its territory.
    pub(crate) fn check_data_dir(&self) -> Result<(), Error> {
        if path_has_component(&self.data_dir, RAUTHY_DIR_NAME) {
            return Err(Error::Config(format!(
                "store data_dir {} is rauthy's territory; the app's hiqlite never opens it",
                self.data_dir.display()
            )));
        }
        if self.data_dir.as_os_str().is_empty() {
            return Err(Error::Config("store data_dir must not be empty".to_owned()));
        }
        Ok(())
    }

    /// The nodes hiqlite is told about: `nodes`, or this node alone.
    pub(crate) fn effective_nodes(&self) -> Vec<Peer> {
        if self.nodes.is_empty() {
            vec![Peer {
                id: self.node_id,
                raft_addr: self.raft_addr.to_string(),
                api_addr: self.api_addr.to_string(),
            }]
        } else {
            self.nodes.clone()
        }
    }

    /// Where hiqlite writes local backups under this configuration.
    #[must_use]
    pub fn backup_dir(&self) -> PathBuf {
        self.data_dir.join("state_machine").join("backups")
    }
}

fn path_has_component(path: &Path, name: &str) -> bool {
    path.components().any(|c| c.as_os_str() == name)
}
