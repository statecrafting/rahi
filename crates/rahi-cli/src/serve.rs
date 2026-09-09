//! Boot, in the one order every cell boots in (B-2), and the listener.
//!
//! The sequence is fixed: config, the key set, the store, the schema check,
//! the chain (fatal on integrity), the kernel with the manifest's hash,
//! observability, identity, the edge, the listener. Every verb that needs a
//! booted cell shares the front of this sequence through [`Booted`], so a
//! backup and a serve agree about which store and which manifest they mean.

use std::net::SocketAddr;
use std::path::PathBuf;

use axum::Router;
use rahi_edge::{AppState, Edge, Route, RouteClass};
use rahi_idp::{
    AUTH_PREFIX, CLIENT_SECRET_FILE, Discovery, IdpConfig, Jwks, Proxy, Resource, SessionKey,
    Sessions, proxy_router, resource_router, session_router,
};
use rahi_kernel::{Kernel, Manifest};
use rahi_ledger::{FsArchive, Hash, Ledger};
use rahi_ops::KeySet;
use rahi_store::{Store, StoreConfig};
use rahi_types::{Config, EnvReader, Error, Result};

use crate::cell::{Cell, OPERATOR_PREFIX};

/// The address `serve` listens on.
pub const ENV_LISTEN_ADDR: &str = "RAHI_LISTEN_ADDR";

/// The default listen address: every interface, the port spec 031 exposes.
pub const DEFAULT_LISTEN_ADDR: &str = "0.0.0.0:8443";

/// Whether rauthy is expected: `required` (the default) mounts identity and
/// refuses to serve without discovery; `none` serves without identity, the
/// mode spec 033's harness boots when no rauthy is available.
pub const ENV_RAUTHY_MODE: &str = "RAHI_RAUTHY_MODE";

/// Where sealed ledger segments are archived on disk, for `ledger verify
/// --full` (spec 014 B-3's filesystem archive).
pub const ENV_LEDGER_ARCHIVE_DIR: &str = "RAHI_LEDGER_ARCHIVE_DIR";

/// The default ledger archive, relative to the data directory.
pub const DEFAULT_LEDGER_ARCHIVE_DIR: &str = "ledger-archive";

/// The session prefix identity mounts under (spec 022).
pub const SESSION_PREFIX: &str = rahi_idp::session::SESSION_PREFIX;

/// How rauthy is treated at boot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RauthyMode {
    /// Identity is mounted and discovery must succeed.
    Required,
    /// No identity is mounted.
    None,
}

impl RauthyMode {
    /// Read [`ENV_RAUTHY_MODE`].
    ///
    /// # Errors
    ///
    /// [`Error::Config`] on a value that is neither `required` nor `none`.
    pub fn from_env(env: &dyn EnvReader) -> Result<Self> {
        match env.get(ENV_RAUTHY_MODE).as_deref() {
            None | Some("") | Some("required") => Ok(Self::Required),
            Some("none") => Ok(Self::None),
            Some(other) => Err(Error::Config(format!(
                "{ENV_RAUTHY_MODE} must be required or none, not {other:?}"
            ))),
        }
    }
}

/// The front of the boot sequence, shared by every verb that opens the
/// store.
pub struct Booted {
    /// The configuration tree.
    pub config: Config,
    /// The key set.
    pub keys: KeySet,
    /// The app node.
    pub store: Store,
    /// The cell's manifest, parsed.
    pub manifest: Manifest,
    /// Its hash: the chain's genesis parent.
    pub hash: Hash,
}

impl Booted {
    /// Config, keys, store, manifest: everything before the chain.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when the environment, the keys, or the manifest are
    /// wrong, or when [`rahi_ops::RESTORE_ENV_VAR`] is set; a store failure
    /// as itself.
    pub async fn open<C: Cell>(env: &dyn EnvReader) -> Result<Self> {
        let config = Config::from_env(env)?;
        rahi_ops::refuse_env_restore()?;
        let keys = KeySet::of(&config);
        keys.check()?;
        let manifest = Manifest::parse(C::manifest())?;
        let hash = manifest.hash()?;
        let store = Store::open(&StoreConfig::from_config(&config, keys.store_secrets()?)).await?;
        Ok(Self {
            config,
            keys,
            store,
            manifest,
            hash,
        })
    }

    /// Open the chain and verify it at resident depth (spec 013 B-6).
    ///
    /// # Errors
    ///
    /// [`Error::Integrity`] when the chain does not verify; the caller lets
    /// it end the process.
    pub async fn ledger(&self) -> Result<Ledger> {
        Ledger::open(
            self.store.handle(),
            self.keys.ledger_signer()?,
            self.hash.clone(),
        )
        .await
    }

    /// The filesystem archive sealed segments are kept in.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the directory cannot be created.
    pub fn ledger_archive(&self, env: &dyn EnvReader) -> Result<FsArchive> {
        let dir = env.get(ENV_LEDGER_ARCHIVE_DIR).map_or_else(
            || self.config.data_dir.join(DEFAULT_LEDGER_ARCHIVE_DIR),
            PathBuf::from,
        );
        FsArchive::open(dir)
    }

    /// Shut the node down.
    pub async fn shutdown(&self) {
        let _ = self.store.shutdown().await;
    }
}

/// Read [`ENV_LISTEN_ADDR`].
///
/// # Errors
///
/// [`Error::Config`] when the value is not a socket address.
pub fn listen_addr(env: &dyn EnvReader) -> Result<SocketAddr> {
    let raw = env
        .get(ENV_LISTEN_ADDR)
        .unwrap_or_else(|| DEFAULT_LISTEN_ADDR.to_owned());
    raw.parse()
        .map_err(|_| Error::Config(format!("{ENV_LISTEN_ADDR} {raw:?} is not host:port")))
}

/// Compose the cell's router over a booted cell (B-2, without the listener).
///
/// # Errors
///
/// [`Error::Stale`] when the store is behind the cell's migrations;
/// [`Error::Integrity`] when the chain does not verify; [`Error::Config`]
/// when identity cannot be derived or a route is unclassified;
/// [`Error::Upstream`] when rauthy is required and does not answer.
pub async fn compose<C: Cell>(booted: &Booted, env: &dyn EnvReader) -> Result<Router> {
    rahi_ops::migrate::check_current(&booted.store, C::migrations()).await?;
    let ledger = booted.ledger().await?;
    let kernel = Kernel::boot(
        booted.manifest.clone(),
        booted.store.handle(),
        ledger.clone(),
    )
    .await?;
    let obs = rahi_edge::ObsOptions::from_env(&booted.config, env)?
        .with_service_name(booted.manifest.app.name.as_str());
    rahi_edge::obs::init(obs)?;

    let state = AppState::new(kernel, booted.store.handle(), ledger, booted.config.clone());
    let mut edge = Edge::builder(state.clone())
        .mount("/", C::routes(state.clone()))
        .mount_operator(OPERATOR_PREFIX, C::operator_routes(state.clone()));
    for route in C::exposed() {
        edge = edge.expose(route);
    }
    if let Some(dir) = C::static_dir() {
        edge = edge.static_slot(dir);
    }

    if RauthyMode::from_env(env)? == RauthyMode::Required {
        let idp = IdpConfig::derive(&booted.config, booted.manifest.app.name.as_str())?;
        let discovery = Discovery::fetch(&idp).await?;
        let jwks = Jwks::load(&discovery).await?;
        let key = SessionKey::load(&booted.keys.path(rahi_ops::SESSION_KEY_FILE))?;
        let client_secret = booted.keys.read_text(CLIENT_SECRET_FILE).map_err(|err| {
            Error::Config(format!(
                "the OIDC client secret is not custodied yet; run the client bootstrap (spec 021 B-5) first: {err}"
            ))
        })?;
        let sessions = Sessions::new(
            &idp,
            &booted.config,
            discovery,
            jwks,
            booted.store.handle(),
            key,
            client_secret,
        )?;
        let resource = Resource::derive(&idp)?;
        edge = edge
            .mount(AUTH_PREFIX, proxy_router(Proxy::new(&idp)?))
            .mount_public(SESSION_PREFIX, session_router(sessions))
            .mount("/", resource_router(resource))
            .expose(Route::new(rahi_idp::METADATA_PATH, RouteClass::Public));
    }

    edge.try_build().map_err(|err| err.0)
}

/// B-2: boot, compose, listen until SIGTERM or Ctrl-C.
///
/// # Errors
///
/// As [`Booted::open`] and [`compose`], plus [`Error::Io`] when the address
/// cannot be bound.
pub async fn serve<C: Cell>(env: &dyn EnvReader) -> Result<()> {
    let addr = listen_addr(env)?;
    let booted = Booted::open::<C>(env).await?;
    let router = match compose::<C>(&booted, env).await {
        Ok(router) => router,
        Err(err) => {
            booted.shutdown().await;
            return Err(err);
        }
    };
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|err| Error::Io(format!("cannot listen on {addr}: {err}")))?;
    let bound = listener
        .local_addr()
        .map_err(|err| Error::Io(format!("bound address unknown: {err}")))?;
    println!(
        "serve: {} listening on {bound}, public origin {}",
        booted.manifest.app.name.as_str(),
        booted.config.public_url.origin()
    );
    let served = axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|err| Error::Io(format!("the listener failed: {err}")));
    booted.shutdown().await;
    served
}

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut term =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(term) => term,
                Err(_) => {
                    let _ = ctrl_c.await;
                    return;
                }
            };
        tokio::select! {
            _ = ctrl_c => {},
            _ = term.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = ctrl_c.await;
    }
}
