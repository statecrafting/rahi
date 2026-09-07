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
//! What is not here: the login flow, the session, and the principal (spec
//! 022, which adds modules inside this crate); rauthy's own configuration
//! file and its supervision (spec 031).
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
pub mod jwks;
pub mod proxy;

pub use bootstrap::{Bootstrap, ClientSettings, bootstrap_client};
pub use config::{
    API_KEY_SCHEME, AUTH_PREFIX, CALLBACK_PATH, CLIENT_SECRET_FILE, DISCOVERY_PATH, ISSUER_PATH,
    IdpConfig,
};
pub use discovery::Discovery;
pub use jwks::{DEFAULT_REFRESH_INTERVAL, Jwk, Jwks};
pub use proxy::{Proxy, proxy_router};
