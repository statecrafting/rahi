//! rauthy's signing keys, cached with room for a rotation (spec 021 B-4).
//!
//! Two sets are held: the current one, and the one it replaced. A key that
//! rauthy has rotated away still verifies for one interval, because tokens
//! signed a minute before a rotation are legitimate tokens and rejecting them
//! would log every user out on a key roll. After that interval the old set is
//! gone, which is what makes a rotation a rotation rather than an ever-growing
//! list of keys that were once trusted.
//!
//! A `kid` that is in neither set buys exactly one refresh. That is what
//! makes a fresh key usable before its timer comes round, and the bound is
//! what keeps an attacker from turning unknown key ids into a request
//! amplifier against the IdP.
//!
//! The refresh is lazy: it happens on the access that finds the interval
//! elapsed, not on a task of its own (spec 021 D-4). A cache that spawns its
//! own timer outlives the value that owns it, and this one is held by the app
//! for exactly as long as the app is serving.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rahi_types::{Error, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::discovery::Discovery;

/// How long a cached set is used before the next access refreshes it.
pub const DEFAULT_REFRESH_INTERVAL: Duration = Duration::from_secs(3600);

/// The clock the cache reads, in unix seconds. Injectable so a test can move
/// an hour without waiting one.
pub type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

/// The system clock, truncated to whole seconds.
#[must_use]
pub fn system_clock() -> Clock {
    Arc::new(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_secs())
    })
}

/// One signing key, kept whole.
///
/// The `kid` is lifted out because that is what a token names; everything
/// else stays as rauthy sent it, so the crate that verifies a signature (spec
/// 022) reads the key material it needs without this one having to model
/// every key type first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Jwk {
    /// The key id a token's header names.
    pub kid: String,
    /// The whole JWK as rauthy published it.
    pub document: serde_json::Value,
}

/// How the cache is built.
#[derive(Clone)]
pub struct JwksOptions {
    /// How long a set is current.
    pub interval: Duration,
    /// The clock the interval is measured against.
    pub clock: Clock,
}

impl std::fmt::Debug for JwksOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JwksOptions")
            .field("interval", &self.interval)
            .finish_non_exhaustive()
    }
}

impl Default for JwksOptions {
    fn default() -> Self {
        Self {
            interval: DEFAULT_REFRESH_INTERVAL,
            clock: system_clock(),
        }
    }
}

/// The cached key sets.
#[derive(Debug)]
struct Sets {
    current: BTreeMap<String, Jwk>,
    previous: BTreeMap<String, Jwk>,
    refreshed_at: u64,
}

impl Sets {
    fn get(&self, kid: &str, now: u64, interval: u64) -> Option<Jwk> {
        if let Some(key) = self.current.get(kid) {
            return Some(key.clone());
        }
        let previous_expires_at = self.refreshed_at.saturating_add(interval);
        if now < previous_expires_at {
            return self.previous.get(kid).cloned();
        }
        None
    }
}

/// rauthy's key set, cached.
#[derive(Clone)]
pub struct Jwks {
    client: reqwest::Client,
    uri: String,
    interval: Duration,
    clock: Clock,
    sets: Arc<Mutex<Sets>>,
}

impl std::fmt::Debug for Jwks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Jwks")
            .field("uri", &self.uri)
            .field("interval", &self.interval)
            .finish_non_exhaustive()
    }
}

impl Jwks {
    /// Fetch the key set named by `discovery` and start the interval.
    ///
    /// # Errors
    ///
    /// [`Error::Upstream`] when the JWKS endpoint cannot be reached;
    /// [`Error::Validation`] when what it returns is not a JWK set.
    pub async fn load(discovery: &Discovery) -> Result<Self> {
        Self::load_with(discovery, JwksOptions::default()).await
    }

    /// [`Jwks::load`] with the interval and the clock named.
    ///
    /// # Errors
    ///
    /// As [`Jwks::load`].
    pub async fn load_with(discovery: &Discovery, options: JwksOptions) -> Result<Self> {
        let client = reqwest::Client::builder()
            .build()
            .map_err(|err| Error::Config(format!("the loopback client cannot be built: {err}")))?;
        let uri = discovery.jwks_uri.clone();
        let current = fetch(&client, &uri).await?;
        let now = (options.clock)();
        Ok(Self {
            client,
            uri,
            interval: options.interval,
            clock: options.clock,
            sets: Arc::new(Mutex::new(Sets {
                current,
                previous: BTreeMap::new(),
                refreshed_at: now,
            })),
        })
    }

    /// The key `kid` names.
    ///
    /// The interval is honoured first: an access that finds the set stale
    /// refreshes before it answers. A `kid` in neither set then buys one
    /// refresh, and is rejected if it is still absent.
    ///
    /// # Errors
    ///
    /// [`Error::Unauthorized`] when no key with that id is published;
    /// [`Error::Upstream`] when the refresh could not reach the endpoint.
    pub async fn key(&self, kid: &str) -> Result<Jwk> {
        let interval = self.interval.as_secs();
        let mut sets = self.sets.lock().await;
        let now = (self.clock)();

        if now.saturating_sub(sets.refreshed_at) >= interval {
            self.rotate(&mut sets, now).await?;
        }
        if let Some(key) = sets.get(kid, now, interval) {
            return Ok(key);
        }

        self.rotate(&mut sets, now).await?;
        sets.get(kid, now, interval).ok_or_else(|| {
            Error::Unauthorized(format!(
                "no signing key with id {kid:?} is published at {}",
                self.uri
            ))
        })
    }

    /// Refresh now, whatever the interval says.
    ///
    /// # Errors
    ///
    /// As [`Jwks::key`].
    pub async fn refresh(&self) -> Result<()> {
        let mut sets = self.sets.lock().await;
        let now = (self.clock)();
        self.rotate(&mut sets, now).await
    }

    /// The key ids currently published, for a probe or a log line.
    pub async fn key_ids(&self) -> Vec<String> {
        self.sets.lock().await.current.keys().cloned().collect()
    }

    /// Fetch, and move the set that was current into the previous slot.
    async fn rotate(&self, sets: &mut Sets, now: u64) -> Result<()> {
        let fetched = fetch(&self.client, &self.uri).await?;
        let replaced = std::mem::replace(&mut sets.current, fetched);
        sets.previous = replaced;
        sets.refreshed_at = now;
        Ok(())
    }
}

/// One JWKS fetch, keyed by `kid`.
async fn fetch(client: &reqwest::Client, uri: &str) -> Result<BTreeMap<String, Jwk>> {
    let response =
        client.get(uri).send().await.map_err(|err| {
            Error::Upstream(format!("the key set at {uri} is unreachable: {err}"))
        })?;
    let status = response.status();
    if !status.is_success() {
        return Err(Error::Upstream(format!(
            "the key set at {uri} answered {status}"
        )));
    }
    let document: serde_json::Value = response
        .json()
        .await
        .map_err(|err| Error::Validation(format!("the key set at {uri} is not JSON: {err}")))?;

    let keys = document
        .get("keys")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| Error::Validation(format!("the key set at {uri} has no \"keys\" array")))?;

    let mut set = BTreeMap::new();
    for key in keys {
        let Some(kid) = key.get("kid").and_then(serde_json::Value::as_str) else {
            return Err(Error::Validation(format!(
                "a key at {uri} carries no \"kid\", so no token could name it"
            )));
        };
        set.insert(
            kid.to_owned(),
            Jwk {
                kid: kid.to_owned(),
                document: key.clone(),
            },
        );
    }
    Ok(set)
}
