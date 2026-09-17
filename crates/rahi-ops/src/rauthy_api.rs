//! rauthy's admin API on loopback, the two calls the verbs make (B-3, B-5).
//!
//! rauthy's store is captured through rauthy (constitution VIII): a backup
//! asks rauthy to snapshot itself and then fetches the file rauthy lists,
//! never touching rauthy's directory.
//!
//! # Two credentials, because rauthy demands two (spec 037 B-1)
//!
//! The admin API key spec 031 custodies is what the health probe and the
//! client bootstrap carry (spec 021 B-5), and it is refused on the backup
//! routes: all four of them call `validate_admin_session`, which takes a
//! session with MFA satisfied and nothing else (030 D-3, confirmed against
//! rauthy 0.36.2). So a backup logs in as the dedicated passkey-only backup
//! admin instead ([`crate::rauthy_session`]), and a deployment whose key set
//! predates spec 037 and holds no passkey is told so by name rather than by
//! a `401`.
//!
//! # What "the snapshot this backup took" means
//!
//! rauthy's store is hiqlite, so rauthy's backup route inherits hiqlite's
//! suppression: a request within sixty seconds of the last one is ignored
//! and answered `204` anyway. The rule here is the one
//! [`rahi_store::backup`] states and for the same reason: the file name
//! carries the second its request was issued at, the window is waited out
//! before triggering, only a snapshot at or after this call's own trigger
//! second is accepted, and the whole sequence is bounded by one deadline
//! (spec 037 B-1, B-2).

use std::time::{Duration, Instant};

use rahi_store::backup::{SUPPRESSION_WINDOW, snapshot_ts};
use rahi_types::{Error, Result};

use crate::rauthy_session::{AdminSession, Passkey};

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

/// How long the whole of [`RauthyApi::backup`] may take: the login, the
/// wait for hiqlite's suppression window, the trigger, and the wait for the
/// file (spec 037 B-1).
pub const BACKUP_DEADLINE: Duration = Duration::from_secs(120);

/// How long one trigger is given to produce its file before the sequence
/// assumes the request was suppressed and tries again inside the deadline.
pub const FILE_WAIT: Duration = Duration::from_secs(30);

/// How often rauthy's listing is re-read while waiting.
pub const POLL_INTERVAL: Duration = Duration::from_millis(250);

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

/// rauthy on loopback, with the admin token and, for the backup routes,
/// the backup admin's passkey.
pub struct RauthyApi {
    base: String,
    token: String,
    client: reqwest::Client,
    passkey: Option<Passkey>,
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
            passkey: None,
        })
    }

    /// With the backup admin's passkey, which is the only credential
    /// rauthy's backup routes accept (spec 037 B-1).
    #[must_use]
    pub fn with_passkey(mut self, passkey: Option<Passkey>) -> Self {
        self.passkey = passkey;
        self
    }

    /// The loopback base this API talks to.
    #[must_use]
    pub fn base(&self) -> &str {
        &self.base
    }

    /// The admin API key this deployment custodies: the credential that
    /// provisions the backup admin, and the one rauthy refuses on the
    /// backup routes (spec 037 B-1).
    #[must_use]
    pub fn token(&self) -> &str {
        &self.token
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

    /// Take one backup through rauthy and return the snapshot it produced:
    /// its name and its bytes (B-5, spec 037 B-1).
    ///
    /// Bounded by [`BACKUP_DEADLINE`]. The snapshot returned is this call's
    /// own: see the module documentation for why a newer file on disk is
    /// not evidence of that and what is.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when this deployment's key set holds no backup
    /// passkey; [`Error::Unauthorized`] when rauthy refuses the login or
    /// the credential; [`Error::Upstream`] when rauthy is unreachable,
    /// answers an error, or the deadline runs out before a snapshot of this
    /// call's own exists.
    pub async fn backup(&self) -> Result<(String, Vec<u8>)> {
        self.backup_within(BACKUP_DEADLINE).await
    }

    /// [`RauthyApi::backup`] under a deadline of your own.
    ///
    /// # Errors
    ///
    /// As [`RauthyApi::backup`].
    pub async fn backup_within(&self, deadline: Duration) -> Result<(String, Vec<u8>)> {
        let Some(passkey) = self.passkey.as_ref() else {
            return Err(Error::Config(format!(
                "this key set holds no {}, so nothing can complete rauthy's admin MFA and \
                 take its backup; a key set minted before spec 037 has none, and a new one \
                 is minted only with a new key set (`rahi first-boot` on an empty volume)",
                crate::BACKUP_PASSKEY_FILE
            )));
        };
        let mut session = AdminSession::new(&self.base)?;
        session.login(passkey).await?;

        let started = Instant::now();
        let expired = |waited: Duration| -> Error {
            Error::Upstream(format!(
                "rauthy produced no snapshot of this backup's own within the {} second \
                 deadline (waited {}): rauthy's store is hiqlite, which ignores a backup \
                 request within {} seconds of the one before while answering it as success",
                deadline.as_secs(),
                waited.as_secs(),
                SUPPRESSION_WINDOW.as_secs(),
            ))
        };
        let name = loop {
            if let Some(wait) = self.suppressed_for(&mut session).await? {
                if started.elapsed() + wait > deadline {
                    return Err(expired(started.elapsed()));
                }
                tokio::time::sleep(wait).await;
            }
            if started.elapsed() >= deadline {
                return Err(expired(started.elapsed()));
            }
            let trigger = i64::try_from(crate::unix_now()).unwrap_or(i64::MAX);
            self.trigger(&mut session).await?;
            let wait_until = started + FILE_WAIT.min(deadline.saturating_sub(started.elapsed()));
            let mut taken = None;
            loop {
                if let Some(name) = self.snapshot_since(&mut session, trigger).await? {
                    taken = Some(name);
                    break;
                }
                if Instant::now() >= wait_until {
                    // Suppressed by a request from outside this process, or
                    // rauthy's writer is slow. Both are answered by waiting
                    // the window out and triggering again inside the deadline.
                    break;
                }
                tokio::time::sleep(POLL_INTERVAL).await;
            }
            if let Some(name) = taken {
                break name;
            }
            if started.elapsed() >= deadline {
                return Err(expired(started.elapsed()));
            }
        };
        let bytes = self.fetch(&mut session, &name).await?;
        Ok((name, bytes))
    }

    /// How long until rauthy's hiqlite would stop ignoring a backup
    /// request, given the newest snapshot it lists.
    async fn suppressed_for(&self, session: &mut AdminSession) -> Result<Option<Duration>> {
        let newest = self
            .list(session)
            .await?
            .iter()
            .filter_map(|l| snapshot_ts(&l.name))
            .max();
        let Some(newest) = newest else {
            return Ok(None);
        };
        let age = i64::try_from(crate::unix_now())
            .unwrap_or(i64::MAX)
            .saturating_sub(newest);
        let window = i64::try_from(SUPPRESSION_WINDOW.as_secs()).unwrap_or(60);
        if (0..window).contains(&age) {
            let remaining = u64::try_from(window - age).unwrap_or(0).saturating_add(1);
            return Ok(Some(Duration::from_secs(remaining)));
        }
        Ok(None)
    }

    /// The snapshot this call's trigger produced: one rauthy named for a
    /// second at or after `trigger`.
    async fn snapshot_since(
        &self,
        session: &mut AdminSession,
        trigger: i64,
    ) -> Result<Option<String>> {
        Ok(self
            .list(session)
            .await?
            .into_iter()
            .filter(|l| snapshot_ts(&l.name).is_some_and(|ts| ts >= trigger))
            .map(|l| l.name)
            .max())
    }

    async fn trigger(&self, session: &mut AdminSession) -> Result<()> {
        let (status, body) = session
            .call(reqwest::Method::POST, BACKUP_PATH, None)
            .await?;
        Self::checked(BACKUP_PATH, status, &body)
    }

    async fn list(&self, session: &mut AdminSession) -> Result<Vec<Listing>> {
        let (status, body) = session
            .call(reqwest::Method::GET, BACKUP_PATH, None)
            .await?;
        Self::checked(BACKUP_PATH, status, &body)?;
        let listings: Listings = serde_json::from_str(&body).map_err(|err| {
            Error::Upstream(format!("rauthy's backup listing does not parse: {err}"))
        })?;
        Ok(listings.local)
    }

    async fn fetch(&self, session: &mut AdminSession, name: &str) -> Result<Vec<u8>> {
        let path = format!("{BACKUP_LOCAL_PATH}{name}");
        let (status, body) = session.call_bytes(reqwest::Method::GET, &path).await?;
        if !status.is_success() {
            return Self::checked(&path, status, &String::from_utf8_lossy(&body))
                .map(|()| Vec::new());
        }
        if body.is_empty() {
            return Err(Error::Upstream(format!(
                "rauthy served an empty backup at {}{path}",
                self.base
            )));
        }
        Ok(body)
    }

    /// rauthy's answer on a backup route, as an error when it is not a
    /// success.
    fn checked(path: &str, status: reqwest::StatusCode, body: &str) -> Result<()> {
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(Error::Unauthorized(format!(
                "rauthy refused the backup admin's session at {path} ({status}): {}",
                body.chars().take(200).collect::<String>()
            )));
        }
        if status == reqwest::StatusCode::NOT_ACCEPTABLE {
            return Err(Error::Unauthorized(format!(
                "rauthy answered {status} at {path}: {}. The backup admin's session is not \
                 MFA-satisfied; rauthy demands one while ADMIN_FORCE_MFA is on, which this \
                 chassis never turns off (spec 037 D-1)",
                body.chars().take(200).collect::<String>()
            )));
        }
        if !status.is_success() {
            return Err(Error::Upstream(format!(
                "rauthy answered {status} at {path}: {}",
                body.chars().take(200).collect::<String>()
            )));
        }
        Ok(())
    }
}
