//! The bearer credential: rauthy's, validated here, never minted here
//! (spec 025 B-1, B-3, B-4, B-5, B-6, B-10, B-11, B-12).
//!
//! **This file accepts credentials and creates none.** There is no key here
//! to hash, no token table to compare against, and no code path that returns
//! a secret to a caller. A request either presents a JWT that rauthy signed,
//! for this audience, inside its lifetime, or it presents nothing this cell
//! will act on. That is the whole of B-1, and it is a property of what is
//! absent from this module rather than of anything written in it.
//!
//! Validation is local (D-1). The signature is checked against the JWKS cache
//! of spec 021, which is the same cache the id token path uses and refreshes
//! on rotation, so an API call costs no round-trip to rauthy and the IdP is
//! not a hot dependency of the data path. What that buys in availability it
//! pays for in immediacy: a token stays valid until it expires, and the two
//! things that shorten that window are short lifetimes and the `jti`
//! deny-list of B-5.
//!
//! Three refusals in here are worth naming, because each of them is a place
//! where the obvious alternative is a vulnerability:
//!
//! - **A token without this resource in `aud` is refused** (B-3, D-2), even
//!   when its signature, issuer, and lifetime are perfect. A token minted for
//!   another resource behind the same IdP would otherwise be replayable here
//!   by whoever it was handed to (RFC 8707).
//! - **A token whose subject is a client rather than a person is refused**
//!   unless the route said otherwise (B-4). A client credentials grant is a
//!   machine acting as itself, and a route written for a person's data is not
//!   a route that reviewed that case.
//! - **A request carrying both a cookie and an `Authorization` header is
//!   refused** (B-10). Preferring one over the other is a rule, and a rule
//!   for resolving that ambiguity is a confused deputy waiting for somebody
//!   to find which half of it the audit read.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{Next, from_fn_with_state};
use axum::response::{IntoResponse, Response};
use rahi_kernel::Kernel;
use rahi_store::StoreHandle;
use rahi_types::{Config, CookieScheme, Error, Principal, Result, UnixSeconds};
use serde::{Deserialize, Serialize};

use crate::config::AUTH_PREFIX;
use crate::envelope::{Cookie, cookie_value};
use crate::jwks::Jwks;
use crate::principal::IdpClaims;
use crate::resource::Resource;
use crate::scope;
use crate::session::{
    ALG_RS256, Audience, Clock, JwtHeader, answer, decode_segment, system_clock, verify_rs256,
};

/// The credential scheme, matched case-insensitively per RFC 7235 §2.1.
pub const BEARER_SCHEME: &str = "bearer";
/// How far outside its window a token is still accepted, in seconds (B-3).
pub const LEEWAY_SECONDS: u64 = 60;
/// The revocation lag: how long a deny-listed `jti` is remembered (B-5).
///
/// One access token lifetime. A deny-list entry that outlived the token it
/// names would be holding memory for a credential nobody can present, and one
/// that expired sooner would leave a window where the revocation had lapsed
/// but the token had not.
pub const DEFAULT_REVOCATION_LAG: Duration = Duration::from_secs(900);
/// The cache-key prefix every deny-list entry carries.
pub const DENYLIST_PREFIX: &str = "jti";
/// The rate limit group token-authenticated requests are counted in (B-12).
pub const RATE_LIMIT_GROUP: &str = "bearer";
/// The cache-key prefix the counters carry, shared with spec 020's limiter.
pub const RATE_LIMIT_PREFIX: &str = "rl";
/// The counting window, in seconds. One minute, as at the edge.
pub const RATE_LIMIT_WINDOW_SECONDS: u64 = 60;
/// The ceiling of the bearer group, per `(client_id, sub)` per window (B-12).
pub const DEFAULT_RATE_LIMIT: u32 = 300;
/// The decision kind a service-token refusal is recorded under (B-4).
pub const DECISION_SERVICE_DENIED: &str = "resource.service";
/// The RFC 6750 error code an unusable credential answers with.
pub const ERROR_INVALID_TOKEN: &str = "invalid_token";
/// The RFC 6750 error code a malformed credential answers with.
pub const ERROR_INVALID_REQUEST: &str = "invalid_request";

/// What a validated access token said, carried beside the [`Principal`].
///
/// The principal is what spec 022 built for a cookie session, transcribed
/// from the same claims (B-4), so a handler that reads `Authenticated` cannot
/// tell which credential resolved it and does not need to. What is here
/// instead is what only a token has: the client that presented it, the scopes
/// it was granted, and the id the deny-list names it by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bearer {
    /// The client rauthy issued this token to, from `azp`.
    pub client_id: String,
    /// The principal the token describes.
    pub principal: Principal,
    /// The scopes it was granted (B-9).
    pub scopes: BTreeSet<String>,
    /// The token id, when rauthy minted one (B-5).
    pub jti: Option<String>,
    /// Whether the subject names the client itself rather than a person
    /// (the client credentials grant, B-4).
    pub service: bool,
}

impl Bearer {
    /// Whether the token carries `scope`, matched exactly (B-9).
    #[must_use]
    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.contains(scope)
    }

    /// The rate limit identity: the `(client_id, sub)` pair (B-12).
    ///
    /// Not the client address of spec 024: two agent runtimes behind one
    /// egress address are two budgets, and one user's runtime exhausting the
    /// other's would be a denial of service with no attacker in it. Spec 026
    /// keys its concurrency budget on the same pair.
    #[must_use]
    pub fn identity(&self) -> String {
        format!("{}:{}", self.client_id, self.principal.sub.as_str())
    }
}

/// A deny-list entry: the moment the token it names expires anyway (B-5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Denied {
    expires: UnixSeconds,
}

/// The claims of a rauthy access token.
///
/// Deliberately partial, like [`IdpClaims`]: `azp` is the client, `scope` is
/// the grant, `jti` names the token, and the rest is the registered set every
/// check below reads. rauthy sends more and is free to send more still.
#[derive(Clone, Debug, Deserialize)]
struct AccessClaims {
    iss: String,
    aud: Audience,
    exp: u64,
    #[serde(default)]
    nbf: Option<u64>,
    #[serde(default)]
    jti: Option<String>,
    #[serde(default)]
    azp: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(flatten)]
    claims: IdpClaims,
}

/// What counting a token-authenticated request decided (B-12).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    /// Inside the ceiling.
    Admitted,
    /// The window is spent; this many seconds remain in it.
    Spent {
        /// Seconds until the window rolls.
        retry_after: u64,
    },
}

/// The resource server: what validates a bearer credential (B-3).
#[derive(Clone)]
pub struct ResourceServer {
    resource: Arc<Resource>,
    jwks: Jwks,
    store: StoreHandle,
    kernel: Kernel,
    scheme: CookieScheme,
    lag: Duration,
    rate_limit: u32,
    clock: Clock,
}

impl std::fmt::Debug for ResourceServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResourceServer")
            .field("resource", &self.resource.as_str())
            .field("lag", &self.lag)
            .field("rate_limit", &self.rate_limit)
            .finish_non_exhaustive()
    }
}

impl ResourceServer {
    /// Build the resource server over the cell's parts.
    ///
    /// `jwks` is the cache of spec 021 and is cloned, not rebuilt: it holds
    /// its key sets behind a shared handle, so the session path and this one
    /// see the same rotation at the same moment.
    #[must_use]
    pub fn new(
        config: &Config,
        resource: Resource,
        jwks: Jwks,
        store: StoreHandle,
        kernel: Kernel,
    ) -> Self {
        Self {
            resource: Arc::new(resource),
            jwks,
            store,
            kernel,
            scheme: config.cookie_scheme,
            lag: DEFAULT_REVOCATION_LAG,
            rate_limit: DEFAULT_RATE_LIMIT,
            clock: system_clock(),
        }
    }

    /// Read the clock from `clock` rather than from the system.
    #[must_use]
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    /// Remember a deny-listed `jti` for `lag` (B-5).
    #[must_use]
    pub const fn with_revocation_lag(mut self, lag: Duration) -> Self {
        self.lag = lag;
        self
    }

    /// Set the ceiling of the bearer rate limit group (B-12).
    #[must_use]
    pub const fn with_rate_limit(mut self, per_minute: u32) -> Self {
        self.rate_limit = per_minute;
        self
    }

    /// What this cell is, and what every token must be addressed to.
    #[must_use]
    pub fn resource(&self) -> &Resource {
        &self.resource
    }

    /// The revocation lag an operator is owed a number for (B-5).
    ///
    /// `preflight` reports it and the deployment documentation states it: a
    /// token that is not introspected is a token that stays good for this
    /// long after it is revoked, and an operator who does not know the number
    /// cannot reason about an incident.
    #[must_use]
    pub const fn revocation_lag(&self) -> Duration {
        self.lag
    }

    /// How this cell scopes its cookies, which is how B-10 recognises one.
    #[must_use]
    pub const fn cookie_scheme(&self) -> CookieScheme {
        self.scheme
    }

    /// The current time, from the injected clock.
    #[must_use]
    pub fn now(&self) -> u64 {
        (self.clock)()
    }

    /// The cache key one deny-listed token id lives under.
    #[must_use]
    pub fn denylist_key(jti: &str) -> String {
        format!("{DENYLIST_PREFIX}:{jti}")
    }

    /// Deny-list `jti` for the revocation lag (B-5).
    ///
    /// The writers are the logout path of spec 022 and the operator verb of
    /// spec 030. The entry lives in the store's cache group, which is
    /// derived state: a restart forgets it, and a restart also drops every
    /// cached assertion and re-reads the roles, so the window this bounds is
    /// bounded by the restart too.
    ///
    /// # Errors
    ///
    /// The store's error when the cache group refuses the write.
    pub async fn deny(&self, jti: &str) -> Result<()> {
        let lag = self.lag.as_secs();
        let entry = Denied {
            expires: UnixSeconds::new(self.now().saturating_add(lag)),
        };
        let ttl = u32::try_from(lag).unwrap_or(u32::MAX);
        self.store
            .kv_put(&Self::denylist_key(jti), &entry, Some(ttl))
            .await
    }

    /// Whether `jti` is deny-listed and not yet past its own expiry (B-5).
    ///
    /// The entry carries the moment it stops mattering as well as a TTL,
    /// because the cache group's expiry is the store's business and the bound
    /// this crate promises is its own.
    ///
    /// # Errors
    ///
    /// The store's error when the cache group cannot be reached. A deny-list
    /// this cell cannot read is a deny-list it must not act as if were empty.
    pub async fn is_denied(&self, jti: &str) -> Result<bool> {
        let entry: Option<Denied> = self.store.kv_get(&Self::denylist_key(jti)).await?;
        Ok(entry.is_some_and(|denied| self.now() < denied.expires.get()))
    }

    /// Validate one bearer credential (B-3, B-4, B-5).
    ///
    /// Six checks and no shortcut through any of them: RS256 under a key
    /// rauthy published, the issuer, the lifetime with
    /// [`LEEWAY_SECONDS`] of leeway, the audience, a subject, and the
    /// deny-list.
    ///
    /// # Errors
    ///
    /// [`Error::Unauthorized`] for every way a token can fail to be one this
    /// cell will act on; [`Error::Upstream`] when the key set or the cache
    /// group cannot be reached, which is this cell failing rather than the
    /// credential.
    pub async fn validate(&self, token: &str) -> Result<Bearer> {
        let (signing_input, signature) = token
            .rsplit_once('.')
            .ok_or_else(|| Error::Unauthorized("the token is not a three-part JWT".to_owned()))?;
        let (header, payload) = signing_input
            .rsplit_once('.')
            .ok_or_else(|| Error::Unauthorized("the token is not a three-part JWT".to_owned()))?;

        let header: JwtHeader = decode_segment(header, "header")?;
        if header.alg != ALG_RS256 {
            return Err(Error::Unauthorized(format!(
                "the token is signed with {:?} and this chassis accepts {ALG_RS256} only",
                header.alg
            )));
        }
        let kid = header
            .kid
            .ok_or_else(|| Error::Unauthorized("the token's header names no key id".to_owned()))?;
        let jwk = self.jwks.key(&kid).await?;
        verify_rs256(&jwk.document, signing_input, signature)?;

        let claims: AccessClaims = decode_segment(payload, "payload")?;
        let issuer = self.resource.authorization_server();
        if claims.iss != issuer {
            return Err(Error::Unauthorized(format!(
                "the token is issued by {:?} and this cell's issuer is {issuer:?}",
                claims.iss
            )));
        }
        let now = self.now();
        if claims.exp.saturating_add(LEEWAY_SECONDS) <= now {
            return Err(Error::Unauthorized("the token has expired".to_owned()));
        }
        if claims
            .nbf
            .is_some_and(|nbf| nbf > now.saturating_add(LEEWAY_SECONDS))
        {
            return Err(Error::Unauthorized("the token is not valid yet".to_owned()));
        }
        // RFC 8707, and the reason this whole module can be trusted beside
        // another resource server on the same IdP: a token addressed
        // somewhere else is not admitted here however well formed it is.
        if !claims.aud.contains(self.resource.audience()) {
            return Err(Error::Unauthorized(format!(
                "the token's audience does not include this resource {:?}",
                self.resource.audience()
            )));
        }
        let sub = claims.claims.subject()?;
        let client_id = claims
            .azp
            .as_deref()
            .map(str::trim)
            .filter(|azp| !azp.is_empty())
            .ok_or_else(|| {
                Error::Unauthorized(
                    "the token names no authorised party, so no client presented it".to_owned(),
                )
            })?
            .to_owned();
        if let Some(jti) = claims.jti.as_deref()
            && self.is_denied(jti).await?
        {
            return Err(Error::Unauthorized(
                "this token has been revoked".to_owned(),
            ));
        }

        Ok(Bearer {
            service: client_id == sub.as_str(),
            client_id,
            principal: crate::principal::principal(sub, &claims.claims, UnixSeconds::new(now)),
            scopes: claims
                .scope
                .as_deref()
                .map(scope::parse)
                .unwrap_or_default(),
            jti: claims.jti,
        })
    }

    /// Count one token-authenticated request in the bearer group (B-12).
    ///
    /// # Errors
    ///
    /// The store's error when the counters cannot be reached. A limiter that
    /// cannot count does not admit (spec 024 B-5).
    pub async fn count(&self, bearer: &Bearer) -> Result<Admission> {
        let now = self.now();
        let window = now / RATE_LIMIT_WINDOW_SECONDS;
        let key = format!(
            "{RATE_LIMIT_PREFIX}:{RATE_LIMIT_GROUP}:{}:{window}",
            bearer.identity()
        );
        let count = self.store.counter_add(&key, 1).await?;
        if count > i64::from(self.rate_limit) {
            return Ok(Admission::Spent {
                retry_after: RATE_LIMIT_WINDOW_SECONDS - (now % RATE_LIMIT_WINDOW_SECONDS),
            });
        }
        Ok(Admission::Admitted)
    }
}

/// One declared bearer route: a prefix, and whether a service may call it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BearerRoute {
    prefix: String,
    service_callable: bool,
}

impl BearerRoute {
    /// The path prefix this declaration covers.
    #[must_use]
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// Whether a client credentials token reaches it (B-4).
    #[must_use]
    pub const fn is_service_callable(&self) -> bool {
        self.service_callable
    }
}

/// Where a bearer credential is accepted, declared by the app (B-11, B-4).
///
/// The declaration is the point. A route's credential kind is a fact the app
/// states when it mounts the route, not something a layer infers from the
/// headers of the request in front of it: inferring it means a request
/// decides which rules apply to it, and the rule this one would be choosing
/// is whether CSRF applies.
///
/// ```no_run
/// # use rahi_idp::BearerRoutes;
/// let routes = BearerRoutes::new().route("/api").service_route("/api/ingest");
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BearerRoutes {
    entries: Vec<BearerRoute>,
}

impl BearerRoutes {
    /// No declarations.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Accept a person's token on everything under `prefix`.
    #[must_use]
    pub fn route(self, prefix: &str) -> Self {
        self.declared(prefix, false)
    }

    /// Accept a person's token *or* a client credentials token under `prefix`
    /// (B-4).
    ///
    /// The one way a machine acting as itself reaches a route: somebody wrote
    /// this call, in the spec that owns the route, having thought about what
    /// a principal with no person behind it means there.
    #[must_use]
    pub fn service_route(self, prefix: &str) -> Self {
        self.declared(prefix, true)
    }

    fn declared(mut self, prefix: &str, service_callable: bool) -> Self {
        self.entries.retain(|entry| entry.prefix != prefix);
        self.entries.push(BearerRoute {
            prefix: prefix.to_owned(),
            service_callable,
        });
        self
    }

    /// The declaration covering `path`: the longest matching prefix.
    #[must_use]
    pub fn covers(&self, path: &str) -> Option<&BearerRoute> {
        self.entries
            .iter()
            .filter(|entry| path.starts_with(entry.prefix.as_str()))
            .max_by_key(|entry| entry.prefix.len())
    }

    /// Every declaration, in the order it was made.
    #[must_use]
    pub fn entries(&self) -> &[BearerRoute] {
        &self.entries
    }
}

/// The process-wide union of every declaration this process applied.
static DECLARED: Mutex<Vec<BearerRoute>> = Mutex::new(Vec::new());

/// Publish `routes` into the process-wide declaration.
///
/// [`RequireBearer::new`] calls this, so applying the layer is what publishes.
pub fn publish(routes: &BearerRoutes) {
    let mut declared = DECLARED.lock().unwrap_or_else(PoisonError::into_inner);
    for entry in &routes.entries {
        if !declared.contains(entry) {
            declared.push(entry.clone());
        }
    }
}

/// The process-wide declaration, as a value.
#[must_use]
pub fn declared() -> BearerRoutes {
    BearerRoutes {
        entries: DECLARED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone(),
    }
}

/// Whether `path` was declared a bearer route (B-11).
///
/// The CSRF check of spec 020 defends ambient credentials, and a bearer token
/// is not ambient: it is attached by the client that holds it, so a
/// cross-origin page cannot cause it to be sent. This predicate is how the
/// composer states that exemption from the declaration rather than from the
/// shape of a request. The identity crate is a peer of the edge and never a
/// dependency of it (spec 021 D-6), so the fact lives here and the composer
/// applies it, exactly as spec 022 B-8's rate limit does.
#[must_use]
pub fn is_bearer_route(path: &str) -> bool {
    declared().covers(path).is_some()
}

/// The bearer gate: a resource server, and where its credential is accepted.
#[derive(Clone, Debug)]
pub struct RequireBearer {
    server: ResourceServer,
    routes: Arc<BearerRoutes>,
}

impl RequireBearer {
    /// Accept `server`'s credential on `routes`, and publish the declaration.
    #[must_use]
    pub fn new(server: ResourceServer, routes: BearerRoutes) -> Self {
        publish(&routes);
        Self {
            server,
            routes: Arc::new(routes),
        }
    }

    /// The resource server this gate validates against.
    #[must_use]
    pub const fn server(&self) -> &ResourceServer {
        &self.server
    }

    /// The routes this gate accepts a credential on.
    #[must_use]
    pub fn routes(&self) -> &BearerRoutes {
        &self.routes
    }
}

/// Wrap `router` in the bearer gate (B-3, B-6, B-10, B-11).
///
/// **Apply it where the paths are whole**, outside every `nest`, because the
/// declaration selects by path prefix and axum hands a nested router the path
/// with its prefix already removed. That is the same placement spec 020's
/// rate limiter takes, and for the same reason.
///
/// A scope gate goes inside this one: it reads the credential this layer
/// resolved. Spec 022's session layer may be inside or outside; a request
/// carrying both credentials is refused either way (B-10), and the challenge
/// of B-6 is attached to a 401 that comes back from within.
///
/// ```no_run
/// # use rahi_idp::{BearerRoutes, RequireBearer, ResourceServer, with_bearer};
/// # fn compose(server: ResourceServer, app: axum::Router) -> axum::Router {
/// let routes = BearerRoutes::new().route("/api");
/// with_bearer(RequireBearer::new(server, routes), app)
/// # }
/// ```
pub fn with_bearer<S>(require: RequireBearer, router: Router<S>) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    router.layer(from_fn_with_state(require, authenticate))
}

/// Resolve a bearer credential, or answer the challenge that bootstraps one.
pub async fn authenticate(
    State(require): State<RequireBearer>,
    request: Request,
    next: Next,
) -> Response {
    let server = &require.server;
    // rauthy's own subtree is forwarded raw (spec 021 B-2) and nothing in it
    // is addressed to this resource server: its registration endpoint takes a
    // bearer token rauthy issued for itself (B-7), its device grant is a flow
    // this chassis has no leg of (B-8), and its authorization endpoint sees a
    // browser carrying rauthy's own cookies. This layer reads none of it.
    if is_proxy_path(request.uri().path()) {
        return next.run(request).await;
    }
    if both_credentials(request.headers(), server.cookie_scheme()) {
        return answer(&ambiguous());
    }

    let declared = require.routes.covers(request.uri().path()).cloned();
    let presented = presented(request.headers());

    let (token, route) = match (presented, declared) {
        // Not a declared bearer route and no credential of ours: this layer
        // authenticates, it does not authorize, so the request goes on.
        (Presented::Absent, None) => return next.run(request).await,
        (Presented::Absent, Some(_)) => {
            // A declared route can accept either credential, one at a time,
            // and the session layer of spec 022 may be inside this one: what
            // resolves the request is not this layer's to know. So the
            // request goes on and the challenge is attached to whatever 401
            // comes back, which is the answer B-6 requires however the app
            // composed the two layers.
            let response = next.run(request).await;
            return challenged(server, response);
        }
        // A credential on a route nobody declared bearer. Refused rather than
        // ignored: silently dropping it would leave the caller believing it
        // had authenticated (B-11).
        (Presented::Bearer(_) | Presented::Other, None) => {
            return answer(&Error::Validation(
                "this route does not accept a bearer credential; no spec declared it one"
                    .to_owned(),
            ));
        }
        (Presented::Other, Some(_)) => {
            return challenge(
                server,
                &Error::Unauthorized(
                    "the Authorization header does not carry a Bearer credential".to_owned(),
                ),
                ERROR_INVALID_REQUEST,
            );
        }
        (Presented::Bearer(token), Some(route)) => (token, route),
    };

    let bearer = match server.validate(&token).await {
        Ok(bearer) => bearer,
        Err(err @ Error::Unauthorized(_)) => {
            return challenge(server, &err, ERROR_INVALID_TOKEN);
        }
        // The key set or the cache group is unreachable. That is this cell
        // failing, not the credential, and saying 401 would send a client to
        // fetch a new token that would fail in exactly the same way.
        Err(err) => return answer(&err),
    };

    if bearer.service && !route.service_callable {
        return service_refused(server, &bearer, &request);
    }

    match server.count(&bearer).await {
        Ok(Admission::Admitted) => {}
        Ok(Admission::Spent { retry_after }) => return spent(retry_after),
        Err(_) => return unavailable(),
    }

    let mut request = request;
    request.extensions_mut().insert(bearer.principal.clone());
    request.extensions_mut().insert(bearer);

    let mut response = next.run(request).await;
    // B-11: a bearer-authenticated route sets no cookie. A cookie set on a
    // request that authenticated without one would be an ambient credential
    // nobody asked for, handed to a client that has no cookie jar to keep it
    // safe in. A loop rather than one call: `HeaderMap::remove` drops the
    // whole entry today, and a strip that leans on that is a strip that
    // breaks quietly on the day it drops one value instead.
    while response.headers_mut().remove(header::SET_COOKIE).is_some() {}
    response
}

/// Whether `path` belongs to rauthy's raw proxy subtree (spec 021 B-2).
///
/// The same rule spec 020's CSRF check applies, and for the same reason: a
/// subtree that is forwarded byte for byte is not a subtree this chassis has
/// an opinion about.
#[must_use]
pub fn is_proxy_path(path: &str) -> bool {
    path == AUTH_PREFIX || path.starts_with("/auth/")
}

/// What the `Authorization` header presented, if anything.
enum Presented {
    /// No header.
    Absent,
    /// A `Bearer` credential.
    Bearer(String),
    /// A header this cell has no scheme for.
    Other,
}

/// Read the `Authorization` header, case-insensitively in its scheme.
fn presented(headers: &HeaderMap) -> Presented {
    let Some(value) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    else {
        return Presented::Absent;
    };
    match value.split_once(' ') {
        Some((scheme, token)) if scheme.eq_ignore_ascii_case(BEARER_SCHEME) => {
            let token = token.trim();
            if token.is_empty() {
                Presented::Other
            } else {
                Presented::Bearer(token.to_owned())
            }
        }
        _ => Presented::Other,
    }
}

/// Whether a request presents a session cookie *and* an `Authorization`
/// header (B-10).
///
/// Read by this layer and by spec 022's session layer, so the refusal does
/// not depend on which of the two the app put outermost.
#[must_use]
pub fn both_credentials(headers: &HeaderMap, scheme: CookieScheme) -> bool {
    let has_authorization = headers.contains_key(header::AUTHORIZATION);
    let has_session = headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|header| cookie_value(header, Cookie::session(scheme).name()))
        .is_some();
    has_authorization && has_session
}

/// The refusal a request carrying two credentials gets (B-10).
#[must_use]
pub fn ambiguous() -> Error {
    Error::Validation(
        "this request presents both a session cookie and an Authorization header; \
         a resolution rule between them is a confused deputy, so neither is read"
            .to_owned(),
    )
}

/// Attach the challenge to a 401 that came back from inside (B-6).
///
/// A route that answered anything else is a route that authenticated the
/// request some other way, and a challenge on it would be telling a client
/// that succeeded to go and get a credential.
fn challenged(server: &ResourceServer, mut response: Response) -> Response {
    if response.status() != StatusCode::UNAUTHORIZED
        || response.headers().contains_key(header::WWW_AUTHENTICATE)
    {
        return response;
    }
    if let Ok(value) = HeaderValue::from_str(&server.resource().challenge()) {
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, value);
    }
    response
}

/// 401 with the challenge that bootstraps discovery (B-6).
fn challenge(server: &ResourceServer, error: &Error, code: &str) -> Response {
    let mut response = answer(error);
    if let Ok(value) = HeaderValue::from_str(&server.resource().challenge_with(code)) {
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, value);
    }
    response
}

/// 403 with a ledgered decision: a service token on a route for a person
/// (B-4).
fn service_refused(server: &ResourceServer, bearer: &Bearer, request: &Request) -> Response {
    let reason = format!(
        "the client {:?} presented a service credential and this route is not \
         declared service-callable",
        bearer.client_id
    );
    let mut payload = serde_json::Map::new();
    payload.insert(
        "client_id".to_owned(),
        serde_json::Value::from(bearer.client_id.as_str()),
    );
    payload.insert(
        "path".to_owned(),
        serde_json::Value::from(request.uri().path()),
    );
    let id = server.kernel.refuse(
        DECISION_SERVICE_DENIED,
        &bearer.principal.sub,
        reason.clone(),
        payload,
    );
    answer(&Error::Denied(format!("{id}: {reason}")))
}

/// 429 for a spent window, carrying the seconds left in it (B-12).
fn spent(retry_after: u64) -> Response {
    let mut response = answer(&Error::Denied(format!(
        "the request rate for this credential exceeds the ceiling of the \
         {RATE_LIMIT_GROUP} group"
    )));
    *response.status_mut() = StatusCode::TOO_MANY_REQUESTS;
    if let Ok(value) = HeaderValue::from_str(&retry_after.to_string()) {
        response.headers_mut().insert(header::RETRY_AFTER, value);
    }
    response
}

/// 503 for a limiter that cannot reach its counters (spec 024 B-5).
fn unavailable() -> Response {
    let mut response = answer(&Error::Stale(
        "the rate limiter cannot reach its counters, so this request is not admitted".to_owned(),
    ));
    *response.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
    response.into_response()
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::collections::BTreeSet;

    use rahi_types::{Role, Sub};

    use super::*;

    fn bearer(client_id: &str, sub: &str, scopes: &[&str]) -> Bearer {
        Bearer {
            client_id: client_id.to_owned(),
            principal: Principal {
                sub: Sub::new(sub),
                email: None,
                email_verified: false,
                roles: BTreeSet::<Role>::new(),
                issued_at: UnixSeconds::new(0),
            },
            scopes: scopes.iter().map(|s| (*s).to_owned()).collect(),
            jti: None,
            service: client_id == sub,
        }
    }

    #[test]
    fn the_rate_limit_identity_is_the_client_and_the_subject() {
        assert_eq!(bearer("cli", "s-1", &[]).identity(), "cli:s-1");
        assert_ne!(
            bearer("cli", "s-1", &[]).identity(),
            bearer("cli", "s-2", &[]).identity(),
            "two people behind one client are two budgets (B-12)"
        );
    }

    #[test]
    fn a_scope_is_carried_and_matched_exactly() {
        let bearer = bearer("cli", "s-1", &["notes.read", "notes.write"]);
        assert!(bearer.has_scope("notes.write"));
        assert!(!bearer.has_scope("notes"));
    }

    #[test]
    fn the_longest_declared_prefix_decides_the_route() {
        let routes = BearerRoutes::new()
            .route("/api")
            .service_route("/api/ingest");
        assert_eq!(
            routes
                .covers("/api/notes")
                .map(BearerRoute::is_service_callable),
            Some(false)
        );
        assert_eq!(
            routes
                .covers("/api/ingest/batch")
                .map(BearerRoute::is_service_callable),
            Some(true),
            "service-callable is declared per route, never inherited upward (B-4)"
        );
        assert!(routes.covers("/session/login").is_none());
    }

    #[test]
    fn declaring_a_prefix_twice_keeps_the_last_declaration() {
        let routes = BearerRoutes::new().route("/api").service_route("/api");
        assert_eq!(routes.entries().len(), 1);
        assert_eq!(
            routes.covers("/api").map(BearerRoute::is_service_callable),
            Some(true)
        );
    }

    #[test]
    fn both_credentials_is_a_cookie_and_a_header_together() {
        let scheme = CookieScheme::Plain;
        let mut headers = HeaderMap::new();
        assert!(!both_credentials(&headers, scheme));

        headers.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer t"));
        assert!(!both_credentials(&headers, scheme), "a token alone is fine");

        headers.insert(header::COOKIE, HeaderValue::from_static("session=abc"));
        assert!(both_credentials(&headers, scheme));

        headers.insert(header::COOKIE, HeaderValue::from_static("other=abc"));
        assert!(
            !both_credentials(&headers, scheme),
            "another cookie is not a session"
        );
    }

    #[test]
    fn the_scheme_is_read_case_insensitively_and_an_empty_token_is_not_one() {
        let with = |value: &'static str| {
            let mut headers = HeaderMap::new();
            headers.insert(header::AUTHORIZATION, HeaderValue::from_static(value));
            presented(&headers)
        };
        assert!(matches!(with("bearer abc"), Presented::Bearer(token) if token == "abc"));
        assert!(matches!(with("BEARER abc"), Presented::Bearer(token) if token == "abc"));
        assert!(matches!(with("Basic abc"), Presented::Other));
        assert!(matches!(with("Bearer   "), Presented::Other));
        assert!(matches!(presented(&HeaderMap::new()), Presented::Absent));
    }

    #[test]
    fn the_proxy_subtree_is_left_alone_and_its_lookalikes_are_not() {
        assert!(is_proxy_path("/auth"));
        assert!(is_proxy_path("/auth/v1/clients_dyn"));
        assert!(!is_proxy_path("/authors"));
        assert!(!is_proxy_path("/api/auth"));
    }

    #[test]
    fn a_denylist_key_is_namespaced_by_the_token_id() {
        assert_eq!(ResourceServer::denylist_key("t-1"), "jti:t-1");
    }

    #[test]
    fn the_revocation_lag_is_the_quarter_hour_b5_states() {
        assert_eq!(DEFAULT_REVOCATION_LAG, Duration::from_secs(900));
        assert_eq!(LEEWAY_SECONDS, 60);
    }
}
