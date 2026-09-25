//! Revocation: how a bearer token stops working before it expires
//! (spec 038 B-5, D-7, D-8).
//!
//! Spec 025 chose local validation (025 D-1): a token is checked against a
//! cached key set and never introspected on the request path. That buys a
//! data path with no hot dependency on the IdP, and it costs immediacy,
//! because nothing asks rauthy whether the credential is still good. The
//! deny-list is what buys a bounded amount of that immediacy back, and
//! `ResourceServer::deny` has existed since 025 with nobody calling it.
//!
//! Two lists, both in the store's cache group, which is derived state a
//! restart forgets (and a restart also drops every cached assertion, so the
//! window a restart opens is the same window it closes):
//!
//! - **By `jti`**, written by a client revoking its own token
//!   ([`SESSION_REVOKE_PATH`]) and by an operator revoking one it was handed
//!   ([`OPERATOR_REVOKE_PATH`]).
//! - **By subject and instant**, written by an operator and by the browser
//!   logout when the manifest asks for it. Every access token for that
//!   subject *issued before the instant* is refused; one issued after is
//!   admitted, so revoking a person does not lock them out of logging in
//!   again.
//!
//! ## The TTL is the accepted validity, not the lifetime (D-7)
//!
//! A deny-list entry has to outlive the token it denies, or the token comes
//! back to life when the entry lapses. The resource server accepts `exp` and
//! `nbf` with [`LEEWAY_SECONDS`] of leeway on either side (025 B-3), so the
//! longest a token minted at the moment of revocation can still validate is
//! its lifetime plus that leeway, and a token whose clock ran ahead by the
//! leeway extends that by the leeway again. [`denylist_ttl`] is therefore
//! `lifetime + 2 * LEEWAY_SECONDS`: ten minutes of lifetime is twelve
//! minutes of memory.
//!
//! ## Revoking access is not ending the grant (D-8)
//!
//! Deny-listing a subject bounds *access* tokens. It does nothing to the
//! refresh token the client holds: a refresh presented a second later yields
//! an access token issued *after* the instant, which the deny-list then
//! admits, and the revocation has bought nothing. So subject revocation also
//! ends the grant at rauthy, through
//! `DELETE /auth/v1/sessions/{user_id}`, which invalidates that user's
//! sessions and refresh tokens (rauthy 0.36.2,
//! `src/api/src/sessions.rs::delete_sessions_for_user`). A cell that was
//! given no admin token revokes the access tokens and says so in the answer
//! rather than implying the grant is over.
//!
//! ## Durable, and retained (spec 043 B-6, D-10)
//!
//! Since 043 both lists are SQL tables in the app store, not cache entries:
//! `rahi_revocation_jti` and `rahi_revocation_sub`, each row carrying
//! `revoked_at`. A restart, a lost cache directory or the cache boundary's
//! transition removes nothing a check relies on, and the bearer check reads
//! them on every token. No code path deletes a row (043 D-10's retention):
//! a revoked `jti` is refused for the life of the volume, whatever the clock
//! does afterwards. [`denylist_ttl`] remains the accepted validity V, which
//! preflight reports and the revocation answer states; no row expires by it.

use std::time::Duration;

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use rahi_store::StoreHandle;
use rahi_types::{Error, Result, UnixSeconds};
use serde::{Deserialize, Serialize};

use crate::bearer::{Bearer, LEEWAY_SECONDS, ResourceServer};
use crate::config::{API_KEY_SCHEME, IdpConfig};
use crate::session::answer;

/// Where a bearer client revokes the token it is holding (B-5).
///
/// Under the session prefix, which is the app's own subtree: `/auth/*` is
/// rauthy's and is forwarded raw (021 B-2). The whole path, not a relative
/// one, because this router merges at the root: the session prefix is
/// already mounted as a public subtree (022) and this route is not public,
/// so it cannot be nested inside that mount.
pub const SESSION_REVOKE_PATH: &str = "/session/token/revoke";

/// Where an operator revokes by `jti` or by subject (B-5).
///
/// Relative to the operator prefix spec 024 mounts behind the role gate, so
/// the route this spells is `/operator/tokens/revoke`.
pub const OPERATOR_REVOKE_PATH: &str = "/tokens/revoke";

/// The cache-key prefix a deny-listed subject lives under.
pub const DENYLIST_SUBJECT_PREFIX: &str = "sub";

/// The path on rauthy's admin API that ends a user's sessions and refresh
/// tokens (D-8).
pub const SESSIONS_PATH: &str = "/auth/v1/sessions";

/// How long a deny-list entry is remembered for a token of `lifetime` (D-7).
///
/// The accepted validity, not the lifetime: a token minted at the moment of
/// revocation validates until `exp + LEEWAY_SECONDS`, and a clock running a
/// leeway ahead moves that by the leeway again.
#[must_use]
pub const fn denylist_ttl(lifetime: Duration) -> Duration {
    Duration::from_secs(
        lifetime
            .as_secs()
            .saturating_add(LEEWAY_SECONDS.saturating_mul(2)),
    )
}

/// The cache key one deny-listed subject lives under.
#[must_use]
pub fn subject_key(sub: &str) -> String {
    format!("{DENYLIST_SUBJECT_PREFIX}:{sub}")
}

/// A deny-listed subject: the instant before which its tokens are refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevokedSubject {
    /// Tokens for this subject issued at or before this second are refused.
    pub before: UnixSeconds,
}

/// Deny-list every token `sub` holds now (spec 043 B-6): a durable subject
/// row whose `revoked_at` only rises, so a later revocation covers every
/// token an earlier one did. The row is retained (043 D-10); `_ttl` is the
/// accepted validity 038 named, kept in the signature for its callers.
///
/// # Errors
///
/// The store's error when the write is refused.
pub async fn deny_subject(store: &StoreHandle, sub: &str, now: u64, _ttl: Duration) -> Result<()> {
    record_subject(store, sub, now).await
}

/// Record a subject revocation at `revoked_at` (spec 043 B-6).
///
/// # Errors
///
/// The store's error when the write is refused.
pub async fn record_subject(store: &StoreHandle, sub: &str, revoked_at: u64) -> Result<()> {
    store.record_subject_revocation(sub, revoked_at).await
}

/// Record a `jti` revocation at `revoked_at` (spec 043 B-6).
///
/// # Errors
///
/// The store's error when the write is refused.
pub async fn record_jti(store: &StoreHandle, jti: &str, revoked_at: u64) -> Result<()> {
    store.record_jti_revocation(jti, revoked_at).await
}

/// Whether `jti` has a revocation row (spec 043 B-6).
///
/// # Errors
///
/// The store's error when the table cannot be read.
pub async fn is_jti_revoked(store: &StoreHandle, jti: &str) -> Result<bool> {
    Ok(store.jti_revoked_at(jti).await?.is_some())
}

/// Whether a token for `sub` issued at `issued_at` is deny-listed.
///
/// `issued_at` is the token's `iat`. A token that carries none is refused
/// while its subject is deny-listed: the question the entry asks is "was
/// this minted before the revocation", and a token that will not say cannot
/// be given the benefit of the doubt without making the revocation
/// optional.
///
/// # Errors
///
/// The store's error when the cache group cannot be reached. A deny-list
/// that cannot be read is not one to act as if were empty.
pub async fn is_subject_denied(
    store: &StoreHandle,
    sub: &str,
    issued_at: Option<u64>,
) -> Result<bool> {
    let Some(before) = store.subject_revoked_at(sub).await? else {
        return Ok(false);
    };
    Ok(issued_at.is_none_or(|iat| iat <= before))
}

/// What an operator asked to revoke: a token, or everything a person holds.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct RevokeRequest {
    /// One token id.
    #[serde(default)]
    pub jti: Option<String>,
    /// One subject, as rauthy names it.
    #[serde(default)]
    pub sub: Option<String>,
}

/// What a revocation did, which is not always everything that was asked.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Revoked {
    /// The token id that was deny-listed, if one was named.
    pub jti: Option<String>,
    /// The subject that was deny-listed, if one was named.
    pub sub: Option<String>,
    /// How long the entries are remembered, in seconds.
    pub remembered_for_secs: u64,
    /// Whether the grant itself was ended at the IdP (D-8).
    ///
    /// `false` means the access tokens are refused and the refresh token the
    /// client holds is not: the cell was given no admin credential, so it
    /// could not ask rauthy to end the session. It is said here rather than
    /// implied by silence.
    pub grant_ended: bool,
}

/// The revoker: the deny-lists, and the admin credential that ends a grant.
#[derive(Clone)]
pub struct Revoker {
    server: ResourceServer,
    admin: Option<Admin>,
}

/// rauthy's admin API as this crate reaches it: the loopback base, and the
/// API key spec 031 custodies.
#[derive(Clone)]
struct Admin {
    base: String,
    token: String,
    client: reqwest::Client,
}

impl std::fmt::Debug for Revoker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Revoker")
            .field("ends_grants", &self.admin.is_some())
            .finish_non_exhaustive()
    }
}

impl Revoker {
    /// Revoke into `server`'s deny-lists, ending no grant at the IdP.
    #[must_use]
    pub const fn new(server: ResourceServer) -> Self {
        Self {
            server,
            admin: None,
        }
    }

    /// Also end the grant at rauthy, with the admin key spec 031 custodies
    /// (D-8).
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when the loopback client cannot be built.
    pub fn ending_grants(mut self, config: &IdpConfig, admin_token: &str) -> Result<Self> {
        let client = reqwest::Client::builder()
            .build()
            .map_err(|err| Error::Config(format!("the loopback client cannot be built: {err}")))?;
        self.admin = Some(Admin {
            base: config.loopback_base.clone(),
            token: admin_token.to_owned(),
            client,
        });
        Ok(self)
    }

    /// The resource server whose deny-lists this writes.
    #[must_use]
    pub const fn server(&self) -> &ResourceServer {
        &self.server
    }

    /// Whether this revoker can end a grant at the IdP.
    #[must_use]
    pub const fn ends_grants(&self) -> bool {
        self.admin.is_some()
    }

    /// Deny-list `jti`, and nothing else.
    ///
    /// # Errors
    ///
    /// The store's error when the cache group refuses the write.
    pub async fn revoke_token(&self, jti: &str) -> Result<Revoked> {
        self.server.deny(jti).await?;
        Ok(Revoked {
            jti: Some(jti.to_owned()),
            sub: None,
            remembered_for_secs: self.server.revocation_lag().as_secs(),
            grant_ended: false,
        })
    }

    /// Deny-list every token `sub` holds, and end the grant when this cell
    /// can (B-5, D-8).
    ///
    /// # Errors
    ///
    /// The store's error when the cache group refuses the write;
    /// [`Error::Upstream`] when rauthy was asked to end the session and did
    /// not. A grant that could not be ended is an error rather than a
    /// quieter `grant_ended: false`: the operator asked for a sign-out and
    /// did not get one.
    pub async fn revoke_subject(&self, sub: &str) -> Result<Revoked> {
        let lag = self.server.revocation_lag();
        deny_subject(self.server.store(), sub, self.server.now(), lag).await?;
        if let Some(admin) = &self.admin {
            admin.end_sessions(sub).await?;
        }
        Ok(Revoked {
            jti: None,
            sub: Some(sub.to_owned()),
            remembered_for_secs: lag.as_secs(),
            grant_ended: self.admin.is_some(),
        })
    }
}

impl Admin {
    /// `DELETE /auth/v1/sessions/{sub}`: rauthy invalidates that user's
    /// sessions and refresh tokens (D-8).
    async fn end_sessions(&self, sub: &str) -> Result<()> {
        let url = format!("{}{SESSIONS_PATH}/{sub}", self.base);
        let response = self
            .client
            .delete(&url)
            .header(
                reqwest::header::AUTHORIZATION,
                format!("{API_KEY_SCHEME} {}", self.token),
            )
            .send()
            .await
            .map_err(|err| Error::Upstream(format!("rauthy is unreachable at {url}: {err}")))?;
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(Error::Unauthorized(format!(
                "rauthy refused the admin API key when asked to end {sub}'s sessions; the \
                 access tokens are deny-listed and the refresh grant is not"
            )));
        }
        Err(Error::Upstream(format!(
            "rauthy answered {status} when asked to end {sub}'s sessions; the access tokens \
             are deny-listed and the refresh grant is not"
        )))
    }
}

/// `POST /session/token/revoke`: a client revokes the token it presented.
///
/// Mounted at the root, because [`SESSION_REVOKE_PATH`] is a whole path, and
/// inside the bearer gate, so the credential is already resolved and the
/// only thing this route can revoke is the caller's own: there is no body,
/// and nothing a caller writes decides what is revoked.
pub fn revoke_router(revoker: Revoker) -> Router {
    Router::new()
        .route(SESSION_REVOKE_PATH, post(revoke_own))
        .with_state(revoker)
}

/// `POST /operator/tokens/revoke`: an operator revokes by `jti` or by `sub`.
pub fn operator_revoke_router(revoker: Revoker) -> Router {
    Router::new()
        .route(OPERATOR_REVOKE_PATH, post(revoke_named))
        .with_state(revoker)
}

/// Revoke the credential this request presented.
async fn revoke_own(
    State(revoker): State<Revoker>,
    bearer: Option<axum::Extension<Bearer>>,
) -> Response {
    let Some(axum::Extension(bearer)) = bearer else {
        return answer(&Error::Unauthorized(
            "this route revokes the bearer credential the request presented, and this request \
             presented none"
                .to_owned(),
        ));
    };
    let Some(jti) = bearer.jti.as_deref() else {
        return answer(&Error::Validation(
            "the token carries no jti, so there is no id to deny-list; a token this cell \
             cannot name is one it cannot revoke before it expires"
                .to_owned(),
        ));
    };
    match revoker.revoke_token(jti).await {
        Ok(revoked) => Json(revoked).into_response(),
        Err(err) => answer(&err),
    }
}

/// Revoke what the operator named.
async fn revoke_named(
    State(revoker): State<Revoker>,
    Json(request): Json<RevokeRequest>,
) -> Response {
    let jti = request
        .jti
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let sub = request
        .sub
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());
    match (jti, sub) {
        (None, None) => answer(&Error::Validation(
            "name the jti of one token or the sub of one person to revoke".to_owned(),
        )),
        (Some(_), Some(_)) => answer(&Error::Validation(
            "name a jti or a sub, not both: one revokes a token and the other revokes a \
             person, and a request that means both says which it meant by being two requests"
                .to_owned(),
        )),
        (Some(jti), None) => match revoker.revoke_token(jti).await {
            Ok(revoked) => Json(revoked).into_response(),
            Err(err) => answer(&err),
        },
        (None, Some(sub)) => match revoker.revoke_subject(sub).await {
            Ok(revoked) => Json(revoked).into_response(),
            Err(err) => answer(&err),
        },
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn the_ttl_covers_the_leeway_on_both_ends() {
        // D-7: ten minutes of lifetime is twelve minutes of memory.
        assert_eq!(
            denylist_ttl(Duration::from_secs(600)),
            Duration::from_secs(720)
        );
        assert_eq!(
            denylist_ttl(Duration::from_secs(0)),
            Duration::from_secs(2 * LEEWAY_SECONDS)
        );
    }

    #[test]
    fn a_subject_key_is_namespaced_away_from_a_token_id() {
        assert_eq!(subject_key("s-1"), "sub:s-1");
        assert_ne!(subject_key("s-1"), ResourceServer::denylist_key("s-1"));
    }
}
