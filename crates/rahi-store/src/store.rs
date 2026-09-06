//! Opening the node and the handle every other crate holds (spec 011 B-1,
//! B-2, B-3).
//!
//! Spec 016 adds two properties to the same node: the connection is never
//! handed a SQLite extension, which [`Store::open`] proves against its own
//! engine at boot, and a write refuses a parameter above the handle's
//! [`crate::MAX_VALUE_BYTES`] ceiling before the statement is submitted.

use std::borrow::Cow;

use hiqlite::{CacheVariants, Client, Node, NodeConfig};
use rahi_types::Error;
use serde::de::DeserializeOwned;

use crate::blob;
use crate::config::StoreConfig;
use crate::error::map;
use crate::query::{Value, owned_row_to, params};
use crate::txn::{ExecuteResult, Statement};

/// The cache group's indices. Nothing durable lives here (constitution IX):
/// every cache is rebuilt on demand after a restart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cache {
    /// The general key-value cache.
    Kv,
}

impl CacheVariants for Cache {
    fn hiqlite_cache_index(&self) -> usize {
        match self {
            Self::Kv => 0,
        }
    }

    fn hiqlite_cache_variants() -> &'static [(usize, &'static str)] {
        &[(0, "Kv")]
    }
}

/// The running hiqlite node. Owns the lifecycle; hands out [`StoreHandle`]s.
pub struct Store {
    handle: StoreHandle,
    cfg: StoreConfig,
}

/// A cheap clone of the store's client, held by every other crate.
#[derive(Clone)]
pub struct StoreHandle {
    client: Client,
    has_s3: bool,
    /// The size a single parameter is refused above (spec 016 B-2). Set from
    /// [`crate::MAX_VALUE_BYTES`] at open and lowered, never raised, by
    /// [`StoreHandle::with_max_value_bytes`].
    pub(crate) max_value_bytes: usize,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store")
            .field("cfg", &self.cfg)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for StoreHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StoreHandle")
    }
}

impl Store {
    /// Start the app's hiqlite node and await the Raft election.
    ///
    /// Starts the `Sqlite` and `Cache` groups under `cfg.data_dir`, bound
    /// to `cfg.raft_addr` and `cfg.api_addr`, as a single voter unless
    /// `cfg.nodes` lists peers, with the keys from `cfg.secrets`.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when `data_dir` is, or lies inside, rauthy's
    /// directory, when the secrets are malformed, or when the engine does not
    /// refuse to load an extension (spec 016 B-4); [`Error::Io`] or
    /// [`Error::Upstream`] when the node fails to start.
    pub async fn open(cfg: &StoreConfig) -> Result<Self, Error> {
        cfg.check_data_dir()?;
        let node_config = node_config(cfg)?;
        let client = hiqlite::start_node_with_cache::<Cache>(node_config)
            .await
            .map_err(map)?;
        client.wait_until_healthy_db().await;
        client.wait_until_healthy_cache().await;
        let handle = StoreHandle {
            client,
            has_s3: cfg.s3.is_some(),
            max_value_bytes: blob::MAX_VALUE_BYTES,
        };
        if let Err(refused) = blob::assert_extensions_refused(&handle).await {
            // The node is already up and this process must not run: stop the
            // Raft it just started rather than leaving it serving.
            let _ = handle.client.shutdown().await;
            return Err(refused);
        }
        Ok(Self {
            handle,
            cfg: cfg.clone(),
        })
    }

    /// A clone of the client for another crate.
    #[must_use]
    pub fn handle(&self) -> StoreHandle {
        self.handle.clone()
    }

    /// The configuration this store was opened with.
    #[must_use]
    pub fn config(&self) -> &StoreConfig {
        &self.cfg
    }

    /// Stop the node. Idempotent from hiqlite's side.
    ///
    /// # Errors
    ///
    /// [`Error::Upstream`] when hiqlite reports a shutdown failure.
    pub async fn shutdown(&self) -> Result<(), Error> {
        self.handle.client.shutdown().await.map_err(map)
    }
}

impl std::ops::Deref for Store {
    type Target = StoreHandle;

    fn deref(&self) -> &Self::Target {
        &self.handle
    }
}

impl StoreHandle {
    pub(crate) fn client(&self) -> &Client {
        &self.client
    }

    pub(crate) fn has_s3(&self) -> bool {
        self.has_s3
    }

    /// Both Raft groups report healthy.
    ///
    /// # Errors
    ///
    /// The mapped hiqlite error of whichever group is unhealthy.
    pub async fn health(&self) -> Result<(), Error> {
        self.client.is_healthy_db().await.map_err(map)?;
        self.client.is_healthy_cache().await.map_err(map)
    }

    /// Whether this node currently leads the SQL group.
    pub async fn is_leader(&self) -> bool {
        self.client.is_leader_db().await
    }

    /// One statement, one write.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] for a bad statement or a parameter above
    /// [`Self::max_value_bytes`], [`Error::Conflict`] for a constraint
    /// violation, [`Error::Upstream`] for a Raft failure.
    pub async fn execute(
        &self,
        sql: impl Into<Cow<'static, str>>,
        values: Vec<Value>,
    ) -> Result<ExecuteResult, Error> {
        blob::check_values(&values, self.max_value_bytes)?;
        self.client
            .execute(sql, params(values))
            .await
            .map(ExecuteResult::from_rows)
            .map_err(map)
    }

    /// A batch as one Raft operation inside one SQLite transaction.
    ///
    /// Either every statement lands or none does: a failing statement rolls
    /// the batch back and surfaces as the error. This is the write path for
    /// anything with an invariant.
    ///
    /// # Errors
    ///
    /// The first failing statement's error, mapped; the batch is rolled
    /// back. An empty batch, or a parameter above [`Self::max_value_bytes`]
    /// in any statement, is [`Error::Validation`] and submits nothing.
    pub async fn txn(&self, statements: Vec<Statement>) -> Result<Vec<ExecuteResult>, Error> {
        if statements.is_empty() {
            return Err(Error::Validation(
                "txn needs at least one statement".to_owned(),
            ));
        }
        blob::check_statements(&statements, self.max_value_bytes)?;
        let queries: Vec<(String, hiqlite::Params)> = statements
            .into_iter()
            .map(Statement::into_hiqlite)
            .collect();
        let results = self.client.txn(queries).await.map_err(map)?;
        results
            .into_iter()
            .map(|r| r.map(ExecuteResult::from_rows).map_err(map))
            .collect()
    }

    /// Read the local replica.
    ///
    /// Right for lists, details, and controller scans. Not for admission or
    /// the chain head: those state their need with [`Self::query_consistent`].
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] for a bad statement or a row that does not fit
    /// `T`.
    pub async fn query<T>(
        &self,
        sql: impl Into<Cow<'static, str>>,
        values: Vec<Value>,
    ) -> Result<Vec<T>, Error>
    where
        T: DeserializeOwned + Send + 'static,
    {
        self.client.query_as(sql, params(values)).await.map_err(map)
    }

    /// Read through the leader, after a quorum has applied every current log.
    ///
    /// Expensive: a network round-trip and a Raft pause. Right for admission
    /// and the chain head, where a stale read would be a wrong decision.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] for a bad statement or a row that does not fit
    /// `T`; [`Error::Upstream`] when the leader cannot be reached.
    pub async fn query_consistent<T>(
        &self,
        sql: impl Into<Cow<'static, str>>,
        values: Vec<Value>,
    ) -> Result<Vec<T>, Error>
    where
        T: DeserializeOwned,
    {
        let rows = self
            .client
            .query_consistent(sql, params(values))
            .await
            .map_err(map)?;
        rows.into_iter().map(owned_row_to).collect()
    }
}

fn node_config(cfg: &StoreConfig) -> Result<NodeConfig, Error> {
    let nodes = cfg
        .effective_nodes()
        .into_iter()
        .map(|p| Node {
            id: p.id,
            addr_raft: p.raft_addr,
            addr_api: p.api_addr,
        })
        .collect();
    let data_dir = cfg
        .data_dir
        .to_str()
        .ok_or_else(|| Error::Config("store data_dir is not valid UTF-8".to_owned()))?
        .to_owned();
    let secrets = &cfg.secrets;
    if secrets.enc_keys.keys.is_empty() {
        return Err(Error::Config(
            "store enc_keys must hold at least one key".to_owned(),
        ));
    }
    if !secrets
        .enc_keys
        .keys
        .iter()
        .any(|k| k.id == secrets.enc_keys.active)
    {
        return Err(Error::Config(format!(
            "store enc_keys.active {:?} names no key in the set",
            secrets.enc_keys.active
        )));
    }
    if let Some(bad) = secrets.enc_keys.keys.iter().find(|k| k.key.len() != 32) {
        return Err(Error::Config(format!(
            "store enc key {:?} is not 32 bytes",
            bad.id
        )));
    }

    let mut node_config = NodeConfig {
        node_id: cfg.node_id,
        nodes,
        listen_addr_api: Cow::Owned(cfg.api_addr.ip().to_string()),
        listen_addr_raft: Cow::Owned(cfg.raft_addr.ip().to_string()),
        data_dir: Cow::Owned(data_dir),
        secret_raft: secrets.secret_raft.clone(),
        secret_api: secrets.secret_api.clone(),
        backup_keep_days_local: cfg.backup_keep_days,
        ..NodeConfig::default()
    };
    node_config.enc_keys.enc_key_active = secrets.enc_keys.active.clone();
    node_config.enc_keys.enc_keys = secrets
        .enc_keys
        .keys
        .iter()
        .map(|k| (k.id.clone(), k.key.clone()))
        .collect();
    if let Some(s3) = &cfg.s3 {
        node_config.s3_config = Some(
            hiqlite::s3::S3Config::new(
                &s3.endpoint,
                s3.bucket.clone(),
                s3.region.clone(),
                s3.access_key.clone(),
                s3.secret_key.clone(),
                s3.path_style,
            )
            .map_err(map)?,
        );
    }
    node_config.is_valid().map_err(map)?;
    Ok(node_config)
}
