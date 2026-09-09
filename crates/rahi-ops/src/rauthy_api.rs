//! rauthy's admin API on loopback, the two calls the verbs make (B-3, B-5).
//!
//! rauthy's store is captured through rauthy (constitution VIII): a backup
//! asks rauthy to snapshot itself and then fetches the file rauthy lists,
//! never touching rauthy's directory. The credential is the admin token
//! spec 031 custodies, carried the way the client bootstrap carries it
//! (spec 021 B-5). See D-3 in the spec for what a real rauthy demands of
//! these two routes.

use std::time::Duration;

use rahi_idp::API_KEY_SCHEME;
use rahi_types::{Error, Result};

/// rauthy's health route, relative to the loopback base.
pub const HEALTH_PATH: &str = "/auth/v1/health";

/// rauthy's backup route: `POST` triggers one, `GET` lists them.
pub const BACKUP_PATH: &str = "/auth/v1/backup";

/// rauthy's local backup download route, followed by the file name.
pub const BACKUP_LOCAL_PATH: &str = "/auth/v1/backup/local/";

/// How long one call may take before rauthy is declared unreachable.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a health probe may take.
pub const HEALTH_TIMEOUT: Duration = Duration::from_secs(3);

/// One rauthy backup, as rauthy lists it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub struct Listing {
    /// The file name.
    pub name: String,
    /// Seconds since the epoch.
    #[serde(default)]
    pub last_modified: i64,
    /// Bytes, when rauthy knows.
    #[serde(default)]
    pub size: Option<u64>,
}

#[derive(Debug, serde::Deserialize)]
struct Listings {
    #[serde(default)]
    local: Vec<Listing>,
}

/// rauthy on loopback, with the admin token.
#[derive(Clone)]
pub struct RauthyApi {
    base: String,
    token: String,
    client: reqwest::Client,
}

impl std::fmt::Debug for RauthyApi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RauthyApi")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

impl RauthyApi {
    /// rauthy at `base` (`http://127.0.0.1:8080`), authenticated as `token`.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when the HTTP client cannot be built.
    pub fn new(base: impl Into<String>, token: impl Into<String>) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(CALL_TIMEOUT)
            .build()
            .map_err(|err| Error::Config(format!("the loopback client cannot be built: {err}")))?;
        Ok(Self {
            base: base.into().trim_end_matches('/').to_owned(),
            token: token.into(),
            client,
        })
    }

    /// The loopback base this API talks to.
    #[must_use]
    pub fn base(&self) -> &str {
        &self.base
    }

    /// rauthy answers its health route.
    ///
    /// # Errors
    ///
    /// [`Error::Upstream`] when nothing answers within [`HEALTH_TIMEOUT`] or
    /// the answer is a server error.
    pub async fn health(&self) -> Result<()> {
        let url = format!("{}{HEALTH_PATH}", self.base);
        let response = self
            .client
            .get(&url)
            .timeout(HEALTH_TIMEOUT)
            .send()
            .await
            .map_err(|err| Error::Upstream(format!("rauthy does not answer at {url}: {err}")))?;
        if response.status().is_server_error() {
            return Err(Error::Upstream(format!(
                "rauthy answers {} at {url}",
                response.status()
            )));
        }
        Ok(())
    }

    /// Take one backup through rauthy: trigger it, find the file that
    /// appeared, and fetch it. Returns the file name and its bytes.
    ///
    /// # Errors
    ///
    /// [`Error::Unauthorized`] when rauthy refuses the token;
    /// [`Error::Upstream`] when rauthy is unreachable, answers an error, or
    /// lists no new file after the trigger.
    pub async fn backup(&self) -> Result<(String, Vec<u8>)> {
        let before = self.list().await?;
        self.trigger().await?;
        let after = self.list().await?;
        let newest = after
            .into_iter()
            .filter(|l| !before.iter().any(|b| b.name == l.name))
            .max_by(|a, b| {
                a.last_modified
                    .cmp(&b.last_modified)
                    .then(a.name.cmp(&b.name))
            })
            .ok_or_else(|| {
                Error::Upstream("rauthy accepted the backup but lists no new file".to_owned())
            })?;
        let bytes = self.fetch(&newest.name).await?;
        Ok((newest.name, bytes))
    }

    async fn trigger(&self) -> Result<()> {
        let url = format!("{}{BACKUP_PATH}", self.base);
        let response = self.send(self.client.post(&url)).await?;
        Self::check(&url, response).await.map(|_| ())
    }

    async fn list(&self) -> Result<Vec<Listing>> {
        let url = format!("{}{BACKUP_PATH}", self.base);
        let response = self.send(self.client.get(&url)).await?;
        let response = Self::check(&url, response).await?;
        let listings: Listings = response.json().await.map_err(|err| {
            Error::Upstream(format!("rauthy's backup listing does not parse: {err}"))
        })?;
        Ok(listings.local)
    }

    async fn fetch(&self, name: &str) -> Result<Vec<u8>> {
        let url = format!("{}{BACKUP_LOCAL_PATH}{name}", self.base);
        let response = self.send(self.client.get(&url)).await?;
        let response = Self::check(&url, response).await?;
        let bytes = response.bytes().await.map_err(|err| {
            Error::Upstream(format!("rauthy's backup body cannot be read: {err}"))
        })?;
        if bytes.is_empty() {
            return Err(Error::Upstream(format!(
                "rauthy served an empty backup at {url}"
            )));
        }
        Ok(bytes.to_vec())
    }

    async fn send(&self, request: reqwest::RequestBuilder) -> Result<reqwest::Response> {
        request
            .header(
                reqwest::header::AUTHORIZATION,
                format!("{API_KEY_SCHEME} {}", self.token),
            )
            .send()
            .await
            .map_err(|err| Error::Upstream(format!("rauthy is unreachable: {err}")))
    }

    async fn check(url: &str, response: reqwest::Response) -> Result<reqwest::Response> {
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(Error::Unauthorized(format!(
                "rauthy refused the admin token at {url} ({status})"
            )));
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(Error::Upstream(format!(
                "rauthy answered {status} at {url}: {}",
                body.chars().take(200).collect::<String>()
            )));
        }
        Ok(response)
    }
}
