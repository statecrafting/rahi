//! The identity crate of the rahi chassis (spec 021).
//!
//! rauthy owns authentication, and this crate is how a cell reaches it
//! without giving up its one origin. Four things live here, and none of them
//! is a second opinion about a question rauthy has already answered:
//!
//! - **The proxy** ([`proxy`]) forwards `/auth/*` to the loopback listener
//!   raw, so there is one exposed port and no CORS between the app and its
//!   IdP (constitution VI).
//! - **Discovery** ([`discovery`]) reads rauthy's own document and refuses a
//!   document whose issuer is not the one this cell was configured for. Every
//!   endpoint the app uses comes from that document rather than from an
//!   assumption about rauthy's paths.
//! - **JWKS** ([`jwks`]) caches the signing keys, refreshes on a timer, and
//!   keeps the previous set for one interval so a rotation does not reject
//!   tokens that were legitimately signed.
//! - **Bootstrap** ([`bootstrap`]) registers the app's own OIDC client on
//!   first boot, and is idempotent: a client that already matches is left
//!   alone and a client that differs is a conflict, never an overwrite.
//!
//! Spec 022 adds the other half: the session and the principal. The app keeps
//! the shell and gives up the authority.
//!
//! - **Login** ([`login`]) is the authorization-code flow with PKCE, mounted
//!   at [`SESSION_PREFIX`] because `/auth/*` belongs to the proxy.
//! - **The envelope** ([`envelope`]) is the session cookie: rauthy's refresh
//!   token and the pinned subject, sealed for integrity, httpOnly, and
//!   granting nothing on presentation.
//! - **The assertion** ([`session`]) is the short-lived cached answer about
//!   who this is, held in the store's non-durable cache group.
//! - **Renewal** ([`refresh`]) forwards the refresh token to rauthy and
//!   re-reads the roles and `email_verified` from userinfo every time, so a
//!   role removed at the IdP takes effect within one access lifetime and a
//!   refused grant ends the session.
//! - **The principal** ([`principal`]) is rauthy's `sub` and nothing else.
//!   No local account row is ever written (constitution VII).
//! - **The extractor** ([`extractor`]) makes all of that invisible to a
//!   handler, and [`RequireRole`] refuses without one into the decision chain.
//!
//! What is not here: rauthy's own configuration file and its supervision
//! (spec 031); the operator gate and the exposure table (spec 024).
//!
//! ```no_run
//! # use rahi_idp::{Discovery, IdpConfig, Jwks, Proxy, proxy_router};
//! # async fn compose(config: &rahi_types::Config) -> Result<axum::Router, rahi_types::Error> {
//! let idp = IdpConfig::derive(config, "hello-cell")?;
//! let discovery = Discovery::fetch(&idp).await?;
//! let _keys = Jwks::load(&discovery).await?;
//! Ok(proxy_router(Proxy::new(&idp)?))
//! # }
//! ```

#![forbid(unsafe_code)]

pub mod bootstrap;
pub mod config;
pub mod discovery;
pub mod envelope;
pub mod extractor;
pub mod jwks;
pub mod login;
pub mod principal;
pub mod proxy;
pub mod refresh;
pub mod session;

pub use bootstrap::{Bootstrap, ClientSettings, bootstrap_client};
pub use config::{
    API_KEY_SCHEME, AUTH_PREFIX, CALLBACK_PATH, CLIENT_SECRET_FILE, DISCOVERY_PATH, ISSUER_PATH,
    IdpConfig,
};
pub use discovery::Discovery;
pub use envelope::{Cookie, Envelope, Secret, SessionId, SessionKey, cookie_value, open, seal};
pub use extractor::{Authenticated, RequireRole, with_role, with_sessions};
pub use jwks::{DEFAULT_REFRESH_INTERVAL, Jwk, Jwks};
pub use login::{LoginState, login_cookie, login_cookie_name, session_router};
pub use principal::{IdpClaims, pinned, principal};
pub use proxy::{Proxy, proxy_router};
pub use refresh::{Renewed, ends_the_session, renew};
pub use session::{
    DEFAULT_ACCESS_TTL, SESSION_PREFIX, SESSION_RATE_LIMIT, Session, Sessions, answer,
};
