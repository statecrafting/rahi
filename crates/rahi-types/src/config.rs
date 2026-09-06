//! The configuration tree, derived from one public URL (spec 010 B-7,
//! thesis §3).
//!
//! One variable, `RAHI_PUBLIC_URL`, is required; everything else has a
//! default that fits the single-container deployment unit. The environment
//! is read through [`EnvReader`] so this crate never touches the process
//! environment itself and tests inject a map.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// The required variable: the one public origin of the cell.
pub const ENV_PUBLIC_URL: &str = "RAHI_PUBLIC_URL";
/// Override for the data volume root. Default `/data`.
pub const ENV_DATA_DIR: &str = "RAHI_DATA_DIR";
/// Override for the app hiqlite API bind address. Default `127.0.0.1:8300`.
pub const ENV_HIQLITE_API_ADDR: &str = "RAHI_HIQLITE_API_ADDR";
/// Override for the app hiqlite Raft bind address. Default `127.0.0.1:8400`.
pub const ENV_HIQLITE_RAFT_ADDR: &str = "RAHI_HIQLITE_RAFT_ADDR";
/// Override for rauthy's loopback address. Default `127.0.0.1:8080`.
pub const ENV_RAUTHY_ADDR: &str = "RAHI_RAUTHY_ADDR";
/// Override for the number of trusted reverse-proxy hops. Default `0`.
pub const ENV_TRUSTED_PROXY_HOPS: &str = "RAHI_TRUSTED_PROXY_HOPS";
/// Override for the OTLP exporter endpoint. Default: none.
pub const ENV_OTLP_ENDPOINT: &str = "RAHI_OTLP_ENDPOINT";

/// Default data volume root.
pub const DEFAULT_DATA_DIR: &str = "/data";
/// Default app hiqlite API address (`8100` belongs to rauthy's hiqlite).
pub const DEFAULT_HIQLITE_API_ADDR: &str = "127.0.0.1:8300";
/// Default app hiqlite Raft address (`8200` belongs to rauthy's hiqlite).
pub const DEFAULT_HIQLITE_RAFT_ADDR: &str = "127.0.0.1:8400";
/// Default rauthy loopback address.
pub const DEFAULT_RAUTHY_ADDR: &str = "127.0.0.1:8080";

/// A source of environment variables.
///
/// The binary implements this over the process environment; tests implement
/// it over a map. An empty value is treated as absent.
pub trait EnvReader {
    /// The value of `key`, if set.
    fn get(&self, key: &str) -> Option<String>;
}

impl EnvReader for BTreeMap<String, String> {
    fn get(&self, key: &str) -> Option<String> {
        BTreeMap::get(self, key).cloned()
    }
}

impl EnvReader for BTreeMap<&str, &str> {
    fn get(&self, key: &str) -> Option<String> {
        BTreeMap::get(self, key).map(|v| (*v).to_owned())
    }
}

/// The cell's public URL: an `http` or `https` origin with an optional path.
///
/// Validated on construction, stored without a trailing slash, and never
/// carrying a query or a fragment.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PublicUrl(String);

impl PublicUrl {
    /// Parse and normalise a public URL.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when the scheme is not `http` or `https`, the host
    /// is empty, or a query or fragment is present.
    pub fn parse(raw: &str) -> Result<Self> {
        let trimmed = raw.trim();
        let rest = trimmed
            .strip_prefix("https://")
            .or_else(|| trimmed.strip_prefix("http://"))
            .ok_or_else(|| {
                Error::Config(format!(
                    "{ENV_PUBLIC_URL} must start with http:// or https://"
                ))
            })?;
        if rest.contains('?') || rest.contains('#') {
            return Err(Error::Config(format!(
                "{ENV_PUBLIC_URL} must not carry a query or fragment"
            )));
        }
        let authority = rest.split('/').next().unwrap_or_default();
        if authority.is_empty() || authority.starts_with(':') || authority.contains('@') {
            return Err(Error::Config(format!("{ENV_PUBLIC_URL} must name a host")));
        }
        Ok(Self(trimmed.trim_end_matches('/').to_owned()))
    }

    /// The normalised URL.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// `true` when the scheme is `https`.
    #[must_use]
    pub fn is_https(&self) -> bool {
        self.0.starts_with("https://")
    }

    /// The scheme, `http` or `https`.
    #[must_use]
    pub fn scheme(&self) -> &str {
        if self.is_https() { "https" } else { "http" }
    }

    /// The host and optional port.
    #[must_use]
    pub fn authority(&self) -> &str {
        let rest = self.0.split("://").nth(1).unwrap_or_default();
        rest.split('/').next().unwrap_or_default()
    }

    /// The origin: scheme and authority, no path.
    #[must_use]
    pub fn origin(&self) -> String {
        format!("{}://{}", self.scheme(), self.authority())
    }
}

/// How the session cookie is scoped, decided by the public URL's scheme.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CookieScheme {
    /// `Secure`; the public URL is `https`.
    Secure,
    /// No `Secure` attribute; the public URL is plain `http` (development).
    Plain,
}

impl CookieScheme {
    /// Whether the cookie carries the `Secure` attribute.
    #[must_use]
    pub const fn is_secure(self) -> bool {
        matches!(self, Self::Secure)
    }
}

/// Bind addresses of the app's own hiqlite cluster.
///
/// rauthy's hiqlite is a separate cluster on separate ports and is never
/// configured here (constitution VIII).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HiqliteConfig {
    /// The SQL and cache API listener.
    pub api_addr: SocketAddr,
    /// The Raft listener.
    pub raft_addr: SocketAddr,
}

/// The whole configuration tree of a cell.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    /// The one public origin every issuer, redirect, and cookie derives from.
    pub public_url: PublicUrl,
    /// The data volume root: `hiqlite/`, `rauthy/`, and `keys/` live under it.
    pub data_dir: PathBuf,
    /// The app's hiqlite listeners.
    pub hiqlite: HiqliteConfig,
    /// Where rauthy listens on loopback.
    pub rauthy_addr: SocketAddr,
    /// The session cookie scheme, `Secure` iff the public URL is `https`.
    pub cookie_scheme: CookieScheme,
    /// How many reverse-proxy hops are trusted to set client-address headers.
    pub trusted_proxy_hops: u8,
    /// The OTLP exporter endpoint, if any. Tracing runs in-process regardless.
    pub otlp_endpoint: Option<String>,
}

impl Config {
    /// Build the whole tree from the environment.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when `RAHI_PUBLIC_URL` is missing or invalid, or
    /// when an override does not parse.
    pub fn from_env(reader: &dyn EnvReader) -> Result<Self> {
        let public_url = PublicUrl::parse(
            &read(reader, ENV_PUBLIC_URL)
                .ok_or_else(|| Error::Config(format!("{ENV_PUBLIC_URL} is required")))?,
        )?;
        let cookie_scheme = if public_url.is_https() {
            CookieScheme::Secure
        } else {
            CookieScheme::Plain
        };
        Ok(Self {
            public_url,
            data_dir: PathBuf::from(
                read(reader, ENV_DATA_DIR).unwrap_or_else(|| DEFAULT_DATA_DIR.to_owned()),
            ),
            hiqlite: HiqliteConfig {
                api_addr: read_addr(reader, ENV_HIQLITE_API_ADDR, DEFAULT_HIQLITE_API_ADDR)?,
                raft_addr: read_addr(reader, ENV_HIQLITE_RAFT_ADDR, DEFAULT_HIQLITE_RAFT_ADDR)?,
            },
            rauthy_addr: read_addr(reader, ENV_RAUTHY_ADDR, DEFAULT_RAUTHY_ADDR)?,
            cookie_scheme,
            trusted_proxy_hops: match read(reader, ENV_TRUSTED_PROXY_HOPS) {
                None => 0,
                Some(raw) => raw.parse().map_err(|_| {
                    Error::Config(format!(
                        "{ENV_TRUSTED_PROXY_HOPS} must be an integer 0..=255"
                    ))
                })?,
            },
            otlp_endpoint: read(reader, ENV_OTLP_ENDPOINT),
        })
    }

    /// The loopback base URL the `/auth/*` proxy forwards to.
    #[must_use]
    pub fn rauthy_base_url(&self) -> String {
        format!("http://{}", self.rauthy_addr)
    }

    /// The directory of the app's own hiqlite state.
    #[must_use]
    pub fn hiqlite_dir(&self) -> PathBuf {
        self.data_dir.join("hiqlite")
    }

    /// The directory of the deployment's key set.
    #[must_use]
    pub fn keys_dir(&self) -> PathBuf {
        self.data_dir.join("keys")
    }
}

fn read(reader: &dyn EnvReader, key: &str) -> Option<String> {
    reader
        .get(key)
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

fn read_addr(reader: &dyn EnvReader, key: &str, default: &str) -> Result<SocketAddr> {
    let raw = read(reader, key).unwrap_or_else(|| default.to_owned());
    raw.parse()
        .map_err(|_| Error::Config(format!("{key} must be a socket address, got {raw:?}")))
}
