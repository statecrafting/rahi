//! The cache group, which is derived state (spec 012 B-6, constitution IX).
//!
//! Rate limits, session lookups, and memoised reads belong here; nothing
//! whose loss changes a decision does. Every value in this module lives in
//! hiqlite's cache Raft group, whose state machine is memory-resident and
//! whose entries expire on their own TTL, vanish when the cache is cleared,
//! and are not restored by a restore. hiqlite 0.14 does replay the cache log
//! at startup, so a value can outlive the process (spec 012 D-6), but that is
//! an implementation detail of the dependency and never a guarantee to build
//! on: the application rebuilds what it needs on demand. Lock state lives in
//! the same group, which is why a fencing token is minted from the SQL group
//! instead (see [`crate::lock`]).

use rahi_types::Error;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::map;
use crate::store::{Cache, StoreHandle};

impl StoreHandle {
    /// Put a value in the KV cache, optionally expiring in `ttl_secs`.
    ///
    /// Not durable: the value is gone after a restart and may be gone before
    /// it, because the group is memory-resident.
    ///
    /// # Errors
    ///
    /// [`Error::Upstream`] when the cache group refuses the write;
    /// [`Error::Integrity`] when the value cannot be serialised.
    pub async fn kv_put<V>(&self, key: &str, value: &V, ttl_secs: Option<u32>) -> Result<(), Error>
    where
        V: Serialize,
    {
        self.client()
            .put(Cache::Kv, key.to_owned(), value, ttl_secs.map(i64::from))
            .await
            .map_err(map)
    }

    /// Read a value from the KV cache. `None` means absent or expired, which
    /// a caller treats as "rebuild it", never as "it does not exist".
    ///
    /// # Errors
    ///
    /// [`Error::Integrity`] when the stored bytes do not fit `V`;
    /// [`Error::Upstream`] when the cache group cannot be reached.
    pub async fn kv_get<V>(&self, key: &str) -> Result<Option<V>, Error>
    where
        V: DeserializeOwned,
    {
        self.client()
            .get::<_, _, V>(Cache::Kv, key.to_owned())
            .await
            .map_err(map)
    }

    /// Delete a value from the KV cache. Deleting an absent key is not an
    /// error.
    ///
    /// # Errors
    ///
    /// [`Error::Upstream`] when the cache group refuses the write.
    pub async fn kv_del(&self, key: &str) -> Result<(), Error> {
        self.client()
            .delete(Cache::Kv, key.to_owned())
            .await
            .map_err(map)
    }

    /// Add to a counter and return its new value. Atomic in the cache group,
    /// which is what makes it usable for a rate limit.
    ///
    /// Not durable: a restart resets the counter to absent, so a window in
    /// progress restarts with it.
    ///
    /// # Errors
    ///
    /// [`Error::Upstream`] when the cache group refuses the write.
    pub async fn counter_add(&self, key: &str, by: i64) -> Result<i64, Error> {
        self.client()
            .counter_add(Cache::Kv, key.to_owned(), by)
            .await
            .map_err(map)
    }

    /// Read a counter. `None` means it has never been added to since the last
    /// restart.
    ///
    /// # Errors
    ///
    /// [`Error::Upstream`] when the cache group cannot be reached.
    pub async fn counter_get(&self, key: &str) -> Result<Option<i64>, Error> {
        self.client()
            .counter_get(Cache::Kv, key.to_owned())
            .await
            .map_err(map)
    }
}
