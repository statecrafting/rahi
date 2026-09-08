//! The access assertion and the context every session operation runs in
//! (spec 022 B-3).
//!
//! Two things live here. The first is [`Session`], the short-lived assertion:
//! the principal as the IdP described it, and the moment that description
//! stops being current. It is cached server-side in the store's cache group,
//! keyed by the random session id the envelope carries, with a time to live
//! equal to the access token's lifetime. **The cache group is not durable,
//! and that is correct** (constitution IX): a restart loses every assertion,
//! which forces a renewal round-trip, which re-reads the roles from rauthy.
//! An assertion that survived a restart would be an answer the cell kept
//! believing without asking.
//!
//! The second is [`Sessions`], the context: the configuration, the discovery
//! document, the key set, the HTTP client, the store, and the sealing key.
//! Every round-trip this crate makes to rauthy is a method on it, so there is
//! one place where the loopback rewrite happens and one place where the client
//! secret is presented.
//!
//! **Front channel and back channel are not the same URL.** The browser is
//! sent to the endpoints rauthy published, which are on the cell's public
//! origin and reached through the raw proxy (spec 021 B-2). The cell's own
//! calls (the token exchange, userinfo, revocation) go to the loopback
//! listener, because rauthy is in the same deployment unit and a cell that
//! dialled its own public URL to talk to a process beside it would depend on
//! its own ingress being up to renew a session.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rahi_store::StoreHandle;
use rahi_types::{Config, CookieScheme, Error, Principal, Result, UnixSeconds};
use ring::signature::{RSA_PKCS1_2048_8192_SHA256, RsaPublicKeyComponents};
use serde::{Deserialize, Serialize};

use crate::config::{ISSUER_PATH, IdpConfig};
use crate::discovery::Discovery;
use crate::envelope::{Cookie, SessionId, SessionKey};
use crate::jwks::Jwks;
use crate::principal::IdpClaims;

/// The prefix the app mounts this crate's session routes under (B-1).
///
/// Not `/auth`: that subtree is rauthy's own, forwarded raw (spec 021 B-2),
/// and a route the app answered inside it would be a route the proxy could not
/// forward.
pub const SESSION_PREFIX: &str = "/session";
/// Where a login starts, under [`SESSION_PREFIX`].
pub const LOGIN_PATH: &str = "/login";
/// Where the authorization code comes back, under [`SESSION_PREFIX`].
pub const CALLBACK_PATH: &str = "/callback";
/// Where a logout is posted, under [`SESSION_PREFIX`].
pub const LOGOUT_PATH: &str = "/logout";

/// The rate limit group's ceiling, per minute per client identity (B-8).
///
/// The app declares it on the edge's limiter:
/// `RateLimits::new().group(SESSION_PREFIX, SESSION_RATE_LIMIT)`. The identity
/// crate does not depend on the edge (spec 021 D-6), so the number lives here
/// and the composer applies it.
pub const SESSION_RATE_LIMIT: u32 = 30;

/// How long an assertion is current unless the IdP says otherwise (B-3).
pub const DEFAULT_ACCESS_TTL: Duration = Duration::from_secs(900);
/// How long a login may sit half-finished before its state cookie is stale.
pub const DEFAULT_LOGIN_TTL: Duration = Duration::from_secs(600);
/// The cache-key prefix every assertion this module writes carries.
pub const CACHE_PREFIX: &str = "session";
/// The only id token algorithm this chassis accepts (spec 021 B-5).
pub const ALG_RS256: &str = "RS256";
/// The scopes a login asks for. `groups` is rauthy's, and carries the roles.
pub const SCOPES: &str = "openid profile email groups";

/// The clock this crate reads, in unix seconds. Injectable so a test can
/// expire an assertion without waiting a quarter of an hour.
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

/// The access assertion: who this is, and until when that is still true.
///
/// Nothing here is authority. It is a cached answer, and when it expires the
/// cell asks again rather than extending it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    /// The principal as the IdP described it when this assertion was minted.
    pub principal: Principal,
    /// When the description stops being current.
    pub expires: UnixSeconds,
}

impl Session {
    /// The assertion for `principal`, current until `expires`.
    #[must_use]
    pub const fn new(principal: Principal, expires: UnixSeconds) -> Self {
        Self { principal, expires }
    }

    /// Whether `now` is at or past the expiry.
    #[must_use]
    pub const fn is_expired(&self, now: UnixSeconds) -> bool {
        now.get() >= self.expires.get()
    }
}

/// What rauthy's token endpoint answers with.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct TokenResponse {
    /// The bearer token userinfo is read with.
    pub access_token: String,
    /// The id token, present on a code exchange.
    #[serde(default)]
    pub id_token: Option<String>,
    /// The refresh token the next envelope carries.
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// How long the access token is good for, in seconds.
    #[serde(default)]
    pub expires_in: Option<u64>,
}

/// An `aud` claim, which is one string or a list of them.
///
/// Read by the id token path here and by the access token path of spec 025,
/// which checks the same claim against the resource identifier rather than
/// against the client id.
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum Audience {
    One(String),
    Many(Vec<String>),
}

impl Audience {
    pub(crate) fn contains(&self, wanted: &str) -> bool {
        match self {
            Self::One(one) => one == wanted,
            Self::Many(many) => many.iter().any(|aud| aud == wanted),
        }
    }
}

/// The id token's claims: the four this cell checks, plus the ones it reads.
#[derive(Clone, Debug, Deserialize)]
struct IdTokenPayload {
    iss: String,
    aud: Audience,
    exp: u64,
    #[serde(default)]
    nonce: Option<String>,
    #[serde(flatten)]
    claims: IdpClaims,
}

/// A JWT header, of which two fields matter.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct JwtHeader {
    pub(crate) alg: String,
    #[serde(default)]
    pub(crate) kid: Option<String>,
}

/// Everything a session operation needs, cloned per request.
#[derive(Clone)]
pub struct Sessions {
    idp: Arc<IdpConfig>,
    discovery: Arc<Discovery>,
    jwks: Jwks,
    client: reqwest::Client,
    store: StoreHandle,
    key: Arc<SessionKey>,
    client_secret: Arc<String>,
    scheme: CookieScheme,
    origin: Arc<String>,
    redirect_uri: Arc<String>,
    access_ttl: Duration,
    login_ttl: Duration,
    clock: Clock,
}

impl std::fmt::Debug for Sessions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sessions")
            .field("issuer", &self.idp.issuer)
            .field("redirect_uri", &self.redirect_uri)
            .field("access_ttl", &self.access_ttl)
            .finish_non_exhaustive()
    }
}

impl Sessions {
    /// Build the context from a booted cell's parts.
    ///
    /// `client_secret` is the value rauthy minted at bootstrap (spec 021 B-5)
    /// and spec 031 custodied under the key set; this crate never reads it
    /// from a file, because which file it is in is the packaging's decision.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when the HTTP client cannot be built, when the
    /// configured issuer does not end in the issuer path every URL in this
    /// crate is derived from, or when the registered redirect URI is not the
    /// callback route this crate mounts.
    pub fn new(
        idp: &IdpConfig,
        config: &Config,
        discovery: Discovery,
        jwks: Jwks,
        store: StoreHandle,
        key: SessionKey,
        client_secret: String,
    ) -> Result<Self> {
        let origin = idp.issuer.strip_suffix(ISSUER_PATH).ok_or_else(|| {
            Error::Config(format!(
                "the issuer {:?} does not end in {ISSUER_PATH}, so no session URL can be \
                 derived from it",
                idp.issuer
            ))
        })?;
        let mounted = format!("{origin}{SESSION_PREFIX}{CALLBACK_PATH}");
        if idp.redirect_uri != mounted {
            return Err(Error::Config(format!(
                "the registered redirect URI {:?} is not the callback route this crate \
                 mounts ({mounted:?}); rauthy matches a redirect URI literally, so a \
                 login sent against a client registered with the other value is refused",
                idp.redirect_uri
            )));
        }
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|err| Error::Config(format!("the loopback client cannot be built: {err}")))?;
        Ok(Self {
            idp: Arc::new(idp.clone()),
            discovery: Arc::new(discovery),
            jwks,
            client,
            store,
            key: Arc::new(key),
            client_secret: Arc::new(client_secret),
            scheme: config.cookie_scheme,
            origin: Arc::new(origin.to_owned()),
            redirect_uri: Arc::new(idp.redirect_uri.clone()),
            access_ttl: DEFAULT_ACCESS_TTL,
            login_ttl: DEFAULT_LOGIN_TTL,
            clock: system_clock(),
        })
    }

    /// Set how long an assertion stays current.
    #[must_use]
    pub const fn with_access_ttl(mut self, ttl: Duration) -> Self {
        self.access_ttl = ttl;
        self
    }

    /// Set how long a half-finished login stays valid.
    #[must_use]
    pub const fn with_login_ttl(mut self, ttl: Duration) -> Self {
        self.login_ttl = ttl;
        self
    }

    /// Read the clock from `clock` rather than from the system.
    #[must_use]
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    /// The identity configuration this context was built from.
    #[must_use]
    pub fn idp(&self) -> &IdpConfig {
        &self.idp
    }

    /// rauthy's discovery document.
    #[must_use]
    pub fn discovery(&self) -> &Discovery {
        &self.discovery
    }

    /// Where rauthy sends the authorization code back to.
    ///
    /// `<public_url>/session/callback`: the app's own route, outside the raw
    /// proxy prefix (B-1). This is [`IdpConfig::redirect_uri`] verbatim, the
    /// string `bootstrap_client` registers with rauthy (spec 021 B-5, D-9),
    /// and [`Sessions::new`] refuses to build unless it is the route this
    /// crate mounts. Sending anything else is a login rauthy refuses.
    #[must_use]
    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// The cell's public origin.
    #[must_use]
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// The session cookie's scoping (B-2).
    #[must_use]
    pub const fn cookie(&self) -> Cookie {
        Cookie::session(self.scheme)
    }

    /// How this cell scopes the cookies it sets.
    #[must_use]
    pub const fn cookie_scheme(&self) -> CookieScheme {
        self.scheme
    }

    /// The sealing key both cookies are written under.
    #[must_use]
    pub fn key(&self) -> &SessionKey {
        &self.key
    }

    /// The store this context caches assertions in.
    ///
    /// The resource server of spec 025 keeps its deny-list and its rate limit
    /// counters in the same cache group, and a cell that has built this
    /// context has already resolved the handle.
    #[must_use]
    pub fn store(&self) -> &StoreHandle {
        &self.store
    }

    /// How long a half-finished login stays valid.
    #[must_use]
    pub const fn login_ttl(&self) -> Duration {
        self.login_ttl
    }

    /// The current time, from the injected clock.
    #[must_use]
    pub fn now(&self) -> UnixSeconds {
        UnixSeconds::new((self.clock)())
    }

    /// When an assertion minted now stops being current.
    ///
    /// `expires_in` is the IdP's own answer when it sends one, capped at the
    /// configured ceiling: an access lifetime the cell did not choose is still
    /// a lifetime the cell has to be able to bound.
    #[must_use]
    pub fn expiry(&self, expires_in: Option<u64>) -> UnixSeconds {
        let ttl = expires_in
            .unwrap_or(self.access_ttl.as_secs())
            .min(self.access_ttl.as_secs());
        UnixSeconds::new((self.clock)().saturating_add(ttl))
    }

    /// The cache key one assertion lives under.
    #[must_use]
    pub fn cache_key(sid: &SessionId) -> String {
        format!("{CACHE_PREFIX}:{}", sid.as_str())
    }

    /// Cache `session` under `sid` for the rest of its lifetime (B-3).
    ///
    /// # Errors
    ///
    /// The store's error when the cache group refuses the write.
    pub async fn store_assertion(&self, sid: &SessionId, session: &Session) -> Result<()> {
        let ttl = session.expires.get().saturating_sub((self.clock)());
        let ttl = u32::try_from(ttl).unwrap_or(u32::MAX);
        self.store
            .kv_put(&Self::cache_key(sid), session, Some(ttl))
            .await
    }

    /// Read the assertion under `sid`, if one is still current.
    ///
    /// `None` means "mint one", never "there is no such session": the group is
    /// memory-resident and a miss is the ordinary case after a restart.
    ///
    /// # Errors
    ///
    /// The store's error when the cache group cannot be reached.
    pub async fn load_assertion(&self, sid: &SessionId) -> Result<Option<Session>> {
        let cached: Option<Session> = self.store.kv_get(&Self::cache_key(sid)).await?;
        Ok(cached.filter(|session| !session.is_expired(self.now())))
    }

    /// Forget the assertion under `sid`.
    ///
    /// # Errors
    ///
    /// The store's error when the cache group refuses the delete.
    pub async fn drop_assertion(&self, sid: &SessionId) -> Result<()> {
        self.store.kv_del(&Self::cache_key(sid)).await
    }

    /// Mint an assertion for `sub` from `claims` and cache it (B-3).
    ///
    /// The one place a [`Session`] is built, used by both the login that
    /// establishes one and the renewal that replaces one, so the two cannot
    /// disagree about how claims become a principal.
    ///
    /// # Errors
    ///
    /// The store's error when the cache group refuses the write.
    pub async fn mint(
        &self,
        sid: &SessionId,
        sub: rahi_types::Sub,
        claims: &IdpClaims,
        expires_in: Option<u64>,
    ) -> Result<Session> {
        let session = Session::new(
            crate::principal::principal(sub, claims, self.now()),
            self.expiry(expires_in),
        );
        self.store_assertion(sid, &session).await?;
        Ok(session)
    }

    /// Rewrite a published endpoint onto the loopback base.
    ///
    /// rauthy builds its discovery document from the cell's public URL, which
    /// is the correct answer for a browser and the wrong one for the process
    /// beside it. An endpoint that is not under this cell's origin is left
    /// alone, so a document that names somewhere else fails at the request
    /// rather than being silently redirected at loopback.
    #[must_use]
    pub fn back_channel(&self, endpoint: &str) -> String {
        endpoint.strip_prefix(self.origin.as_str()).map_or_else(
            || endpoint.to_owned(),
            |path| format!("{}{path}", self.idp.loopback_base),
        )
    }

    /// One call to rauthy's token endpoint, through the loopback base.
    ///
    /// The client authenticates with `client_secret_post`, which rauthy
    /// supports and which keeps the secret out of a URL and out of a header a
    /// proxy might log.
    ///
    /// # Errors
    ///
    /// [`Error::Upstream`] when rauthy cannot be reached;
    /// [`Error::Unauthorized`] when it refuses the grant, which is the answer
    /// a caller acts on by ending the session.
    pub async fn token(&self, form: &[(&str, &str)]) -> Result<TokenResponse> {
        let url = self.back_channel(&self.discovery.token_endpoint);
        let mut body = form.to_vec();
        body.push(("client_id", self.idp.client_id.as_str()));
        body.push(("client_secret", self.client_secret.as_str()));

        let response = self
            .client
            .post(&url)
            .form(&body)
            .send()
            .await
            .map_err(|err| {
                Error::Upstream(format!("rauthy's token endpoint is unreachable: {err}"))
            })?;
        let status = response.status();
        if !status.is_success() {
            let detail = response.text().await.unwrap_or_default();
            return Err(Error::Unauthorized(format!(
                "rauthy refused the grant with {status}: {}",
                detail.trim()
            )));
        }
        response.json().await.map_err(|err| {
            Error::Upstream(format!("rauthy's token response does not parse: {err}"))
        })
    }

    /// Re-read the claims for `access_token` from userinfo (B-5).
    ///
    /// # Errors
    ///
    /// [`Error::Upstream`] when rauthy cannot be reached;
    /// [`Error::Unauthorized`] when it refuses the token.
    pub async fn userinfo(&self, access_token: &str) -> Result<IdpClaims> {
        let url = self.back_channel(&self.discovery.userinfo_endpoint);
        let response = self
            .client
            .get(&url)
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|err| Error::Upstream(format!("rauthy's userinfo is unreachable: {err}")))?;
        let status = response.status();
        if !status.is_success() {
            return Err(Error::Unauthorized(format!(
                "rauthy refused the userinfo read with {status}"
            )));
        }
        response
            .json()
            .await
            .map_err(|err| Error::Upstream(format!("rauthy's userinfo does not parse: {err}")))
    }

    /// Revoke `refresh_token` at rauthy's revocation endpoint (B-7).
    ///
    /// A logout whose revocation fails still clears the cookies: the session
    /// ends here either way, and a token the cell has thrown away is a token
    /// the cell will not present again.
    ///
    /// # Errors
    ///
    /// [`Error::Upstream`] when rauthy publishes no revocation endpoint or
    /// cannot be reached, or when it answers with a failure status.
    pub async fn revoke(&self, refresh_token: &str) -> Result<()> {
        let endpoint = self
            .discovery
            .revocation_endpoint
            .as_deref()
            .ok_or_else(|| {
                Error::Upstream(
                    "rauthy's discovery document publishes no revocation endpoint".to_owned(),
                )
            })?;
        let url = self.back_channel(endpoint);
        let response = self
            .client
            .post(&url)
            .form(&[
                ("token", refresh_token),
                ("token_type_hint", "refresh_token"),
                ("client_id", self.idp.client_id.as_str()),
                ("client_secret", self.client_secret.as_str()),
            ])
            .send()
            .await
            .map_err(|err| {
                Error::Upstream(format!(
                    "rauthy's revocation endpoint is unreachable: {err}"
                ))
            })?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(Error::Upstream(format!(
                "rauthy answered {} to the revocation",
                response.status()
            )))
        }
    }

    /// Verify an id token against the key set and this cell's expectations
    /// (B-1).
    ///
    /// Four checks, and none of them is optional: the signature under a key
    /// rauthy published, the issuer, the audience, and the expiry. The nonce
    /// is the fifth, and it binds the token to the login this browser started.
    ///
    /// # Errors
    ///
    /// [`Error::Unauthorized`] for every failure. A token that does not verify
    /// and a token that verifies for somebody else are the same event to the
    /// caller: this is not a token this cell will act on.
    pub async fn verify_id_token(&self, token: &str, nonce: &str) -> Result<IdpClaims> {
        let (signing_input, signature) = token.rsplit_once('.').ok_or_else(|| {
            Error::Unauthorized("the id token is not a three-part JWT".to_owned())
        })?;
        let (header, payload) = signing_input.rsplit_once('.').ok_or_else(|| {
            Error::Unauthorized("the id token is not a three-part JWT".to_owned())
        })?;

        let header: JwtHeader = decode_segment(header, "header")?;
        if header.alg != ALG_RS256 {
            return Err(Error::Unauthorized(format!(
                "the id token is signed with {:?} and this chassis accepts {ALG_RS256} only",
                header.alg
            )));
        }
        let kid = header.kid.ok_or_else(|| {
            Error::Unauthorized("the id token's header names no key id".to_owned())
        })?;
        let jwk = self.jwks.key(&kid).await?;
        verify_rs256(&jwk.document, signing_input, signature)?;

        let claims: IdTokenPayload = decode_segment(payload, "payload")?;
        if claims.iss != self.idp.issuer {
            return Err(Error::Unauthorized(format!(
                "the id token is issued by {:?} and this cell's issuer is {:?}",
                claims.iss, self.idp.issuer
            )));
        }
        if !claims.aud.contains(&self.idp.client_id) {
            return Err(Error::Unauthorized(format!(
                "the id token's audience does not include this cell's client {:?}",
                self.idp.client_id
            )));
        }
        if claims.exp <= (self.clock)() {
            return Err(Error::Unauthorized("the id token has expired".to_owned()));
        }
        match claims.nonce.as_deref() {
            Some(sent) if sent == nonce => Ok(claims.claims),
            _ => Err(Error::Unauthorized(
                "the id token's nonce is not the one this login sent".to_owned(),
            )),
        }
    }
}

/// This crate's own answer for a workspace error.
///
/// Spec 020 B-8 owns the mapping from an [`Error`] to a status, and the
/// layering forbids the dependency that would let this crate call it: identity
/// and the edge are peers and neither may depend on the other (spec 021 D-6).
/// So the statuses the session flows raise are named here, drawn from that
/// table rather than invented beside it, in the envelope spec 020 emits so a
/// client parses one shape. A denial carries the id of the decision the chain
/// holds, which is what the kernel put in front of the message (spec 015 B-6).
#[must_use]
pub fn answer(error: &Error) -> axum::response::Response {
    use axum::http::{StatusCode, header};
    use axum::response::IntoResponse as _;

    let status = match error {
        Error::Validation(_) => StatusCode::BAD_REQUEST,
        Error::Unauthorized(_) => StatusCode::UNAUTHORIZED,
        Error::Denied(_) => StatusCode::FORBIDDEN,
        Error::NotFound(_) => StatusCode::NOT_FOUND,
        Error::Conflict(_) => StatusCode::CONFLICT,
        Error::Stale(_) => StatusCode::SERVICE_UNAVAILABLE,
        Error::Integrity(_) | Error::Io(_) | Error::Config(_) => StatusCode::INTERNAL_SERVER_ERROR,
        Error::Upstream(_) => StatusCode::BAD_GATEWAY,
    };
    let mut body = serde_json::Map::new();
    body.insert("error".to_owned(), error.kind().into());
    body.insert("message".to_owned(), error.message().into());
    if let Some(id) = decision_id(error) {
        body.insert("decision".to_owned(), id.into());
    }
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::Value::Object(body).to_string(),
    )
        .into_response()
}

/// The decision id a denial's message starts with, when it carries one.
fn decision_id(error: &Error) -> Option<&str> {
    let Error::Denied(message) = error else {
        return None;
    };
    let (id, _reason) = message.split_once(": ")?;
    (!id.is_empty() && id.contains(':') && !id.contains(char::is_whitespace)).then_some(id)
}

/// Decode one base64url JWT segment into `T`.
///
/// Shared with the access token path of spec 025: a second decoder would be
/// a second place for a padding rule to drift.
pub(crate) fn decode_segment<T: serde::de::DeserializeOwned>(
    segment: &str,
    what: &str,
) -> Result<T> {
    let bytes = URL_SAFE_NO_PAD
        .decode(segment)
        .map_err(|_| Error::Unauthorized(format!("the id token's {what} is not base64url")))?;
    serde_json::from_slice(&bytes)
        .map_err(|err| Error::Unauthorized(format!("the id token's {what} does not parse: {err}")))
}

/// Verify `signature` over `signing_input` under the RSA key `jwk` publishes.
///
/// The JWK carries the modulus and the public exponent as base64url integers,
/// which is exactly the form the verifier takes, so no key is ever reassembled
/// into a DER document on the way.
///
/// Shared with the access token path of spec 025. One signature check, in one
/// place: two would be two chances to accept a signature that does not verify.
pub(crate) fn verify_rs256(
    jwk: &serde_json::Value,
    signing_input: &str,
    signature: &str,
) -> Result<()> {
    let component = |name: &str| -> Result<Vec<u8>> {
        let raw = jwk
            .get(name)
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                Error::Unauthorized(format!("the signing key publishes no {name:?} component"))
            })?;
        URL_SAFE_NO_PAD.decode(raw).map_err(|_| {
            Error::Unauthorized(format!("the signing key's {name:?} is not base64url"))
        })
    };
    let n = component("n")?;
    let e = component("e")?;
    let signature = URL_SAFE_NO_PAD
        .decode(signature)
        .map_err(|_| Error::Unauthorized("the id token's signature is not base64url".to_owned()))?;

    RsaPublicKeyComponents { n: &n, e: &e }
        .verify(
            &RSA_PKCS1_2048_8192_SHA256,
            signing_input.as_bytes(),
            &signature,
        )
        .map_err(|_| {
            Error::Unauthorized(
                "the id token's signature does not verify under rauthy's key".to_owned(),
            )
        })
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use rahi_types::{Principal, Sub};

    fn assertion(expires: u64) -> Session {
        Session::new(
            Principal {
                sub: Sub::new("s-1"),
                email: None,
                email_verified: false,
                roles: std::collections::BTreeSet::new(),
                issued_at: UnixSeconds::new(0),
            },
            UnixSeconds::new(expires),
        )
    }

    #[test]
    fn the_route_this_crate_mounts_is_the_one_the_bootstrap_registers() {
        // `config::CALLBACK_PATH` is what `IdpConfig::redirect_uri` is built
        // from and what `bootstrap_client` registers with rauthy (spec 021
        // D-9). `SESSION_PREFIX` + `CALLBACK_PATH` is the route `login_router`
        // mounts. rauthy matches a redirect URI literally, so the day these
        // two disagree is the day every login is refused, and the only other
        // thing that catches it is a browser (AC-2, verified in 034).
        assert_eq!(
            crate::config::CALLBACK_PATH,
            format!("{SESSION_PREFIX}{CALLBACK_PATH}"),
        );
    }

    #[test]
    fn an_assertion_expires_at_its_expiry_not_after_it() {
        let session = assertion(100);
        assert!(!session.is_expired(UnixSeconds::new(99)));
        assert!(session.is_expired(UnixSeconds::new(100)));
        assert!(session.is_expired(UnixSeconds::new(101)));
    }

    #[test]
    fn an_audience_is_one_string_or_a_list() {
        let one: Audience =
            serde_json::from_value(serde_json::json!("hello-cell")).expect("parses");
        assert!(one.contains("hello-cell"));
        assert!(!one.contains("other"));

        let many: Audience =
            serde_json::from_value(serde_json::json!(["other", "hello-cell"])).expect("parses");
        assert!(many.contains("hello-cell"));
        assert!(!many.contains("third"));
    }

    #[test]
    fn a_cache_key_is_namespaced_by_the_session_id() {
        assert_eq!(
            Sessions::cache_key(&SessionId::new("abc")),
            "session:abc".to_owned()
        );
    }

    #[test]
    fn an_id_token_payload_reads_the_claims_beside_the_registered_ones() {
        let payload: IdTokenPayload = serde_json::from_value(serde_json::json!({
            "iss": "https://cell.example.com/auth/v1",
            "aud": "hello-cell",
            "exp": 100,
            "nonce": "n-1",
            "sub": "s-1",
            "email": "a@example.com",
            "roles": ["admin"],
        }))
        .expect("the payload parses");
        assert_eq!(payload.claims.sub.as_deref(), Some("s-1"));
        assert_eq!(payload.claims.roles, vec!["admin".to_owned()]);
        assert!(!payload.claims.is_email_verified());
    }
}
