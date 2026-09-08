//! rauthy's own discovery document, trusted and verified (spec 021 B-3).
//!
//! Every endpoint the app uses is read from here rather than assumed, so a
//! rauthy that moves a path moves the app with it. One thing is checked
//! rather than trusted: the issuer. A document whose issuer is not this
//! cell's issuer is a document from somewhere else, and tokens minted under
//! it would not be tokens for this cell.
//!
//! The fetch retries, because rauthy starts in the same container and a cell
//! that gave up on the first refused connection would race its own IdP
//! (constitution VI).

use std::time::Duration;

use rahi_types::{Error, Result};
use serde::{Deserialize, Serialize};

use crate::config::IdpConfig;

/// How long the boot fetch keeps trying before it gives up (B-3).
pub const BOOT_BUDGET: Duration = Duration::from_secs(60);
/// The first pause between attempts. It doubles up to [`MAX_BACKOFF`].
pub const FIRST_BACKOFF: Duration = Duration::from_millis(100);
/// The longest pause between attempts.
pub const MAX_BACKOFF: Duration = Duration::from_secs(5);

/// The endpoints a cell needs, from rauthy's `openid-configuration`.
///
/// Unknown fields are kept out deliberately: this is the subset spec 021 and
/// spec 022 use, and a field nobody reads is a field nobody has to keep true.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Discovery {
    /// The issuer, which must equal the cell's configured issuer.
    pub issuer: String,
    /// Where the browser is sent to authorize.
    pub authorization_endpoint: String,
    /// Where a code is exchanged and a refresh token is renewed.
    pub token_endpoint: String,
    /// Where claims are re-read on renewal (constitution VII).
    pub userinfo_endpoint: String,
    /// Where a logout is completed.
    pub end_session_endpoint: String,
    /// Where the signing keys live.
    pub jwks_uri: String,
    /// Where a refresh token is revoked (spec 022 B-7).
    ///
    /// Optional because RFC 8414 makes it optional and an authorization server
    /// that publishes none is still a legal one; rauthy publishes it. Spec 021
    /// D-7 keeps this list to the endpoints the chassis actually calls, and a
    /// logout that revokes is a call, so it is modelled here rather than
    /// guessed from the issuer's path.
    #[serde(default)]
    pub revocation_endpoint: Option<String>,
}

impl Discovery {
    /// Parse a document and hold it to `expected_issuer`.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when the document is not JSON, when a field this
    /// cell needs is missing (`jwks_uri` among them), or when the issuer is
    /// not the configured one.
    pub fn parse(document: &str, expected_issuer: &str) -> Result<Self> {
        let discovery: Self = serde_json::from_str(document).map_err(|err| {
            Error::Validation(format!("rauthy's discovery document does not parse: {err}"))
        })?;
        if discovery.issuer != expected_issuer {
            return Err(Error::Validation(format!(
                "the discovery document is issued by {} and this cell's issuer is \
                 {expected_issuer}: the document belongs to another deployment",
                discovery.issuer
            )));
        }
        Ok(discovery)
    }

    /// Fetch the document from the loopback listener, retrying for
    /// [`BOOT_BUDGET`].
    ///
    /// # Errors
    ///
    /// [`Error::Upstream`] when the budget runs out before rauthy answers;
    /// [`Error::Validation`] from [`Discovery::parse`] as soon as a document
    /// arrives, because a wrong document is not a transient failure and
    /// retrying it would only delay the truth.
    pub async fn fetch(config: &IdpConfig) -> Result<Self> {
        Self::fetch_within(config, BOOT_BUDGET).await
    }

    /// [`Discovery::fetch`] with the budget named. A test uses a small one.
    ///
    /// # Errors
    ///
    /// As [`Discovery::fetch`].
    pub async fn fetch_within(config: &IdpConfig, budget: Duration) -> Result<Self> {
        let client = reqwest::Client::builder()
            .build()
            .map_err(|err| Error::Config(format!("the loopback client cannot be built: {err}")))?;
        let url = config.discovery_url();
        let deadline = tokio::time::Instant::now() + budget;
        let mut backoff = FIRST_BACKOFF;
        let mut last;

        loop {
            match attempt(&client, &url).await {
                Ok(document) => return Self::parse(&document, &config.issuer),
                Err(err) => last = err,
            }
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Err(Error::Upstream(format!(
                    "rauthy's discovery document did not arrive from {url} within \
                     {}s: {last}",
                    budget.as_secs()
                )));
            }
            let pause = backoff.min(deadline.saturating_duration_since(now));
            tokio::time::sleep(pause).await;
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
    }
}

/// One attempt, with a transport or status failure rendered as a string.
async fn attempt(client: &reqwest::Client, url: &str) -> std::result::Result<String, String> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|err| err.to_string())?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("rauthy answered {status}"));
    }
    response.text().await.map_err(|err| err.to_string())
}
