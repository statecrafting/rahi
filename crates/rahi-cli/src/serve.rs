//! Boot, in the one order every cell boots in (B-2), and the listener.
//!
//! The sequence is fixed: config, the key set, the store, the schema check,
//! the chain (fatal on integrity), the kernel with the manifest's hash,
//! observability, identity, the edge, the listener. Every verb that needs a
//! booted cell shares the front of this sequence through [`Booted`], so a
//! backup and a serve agree about which store and which manifest they mean.

use std::future::{Future, IntoFuture as _};
use std::net::SocketAddr;
use std::path::PathBuf;

use axum::Router;
use rahi_edge::{
    AppState, Edge, Route, RouteClass, StreamHub, StreamIdentityResolver, StreamOptions,
};
use rahi_idp::{
    AUTH_PREFIX, Discovery, IdpConfig, Jwks, Proxy, Resource, SessionKey, Sessions, proxy_router,
    resource_router, session_router,
};
use rahi_kernel::{Kernel, Manifest};
use rahi_ledger::{FsArchive, Hash, Ledger};
use rahi_ops::KeySet;
use rahi_store::Store;
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

/// Open streams one identity may hold (spec 026 B-5); the default is four.
pub const ENV_MAX_CONCURRENT_STREAMS: &str = "RAHI_MAX_CONCURRENT_STREAMS";

/// Seconds a shutdown waits for open streams to close (spec 026 B-7); the
/// default is ten.
pub const ENV_STREAM_DRAIN_TIMEOUT: &str = "RAHI_STREAM_DRAIN_TIMEOUT_SECS";

/// The stream options the environment describes (spec 026).
///
/// # Errors
///
/// [`Error::Config`] when a value is not a number.
pub fn stream_options(env: &dyn EnvReader) -> Result<StreamOptions> {
    let mut options = StreamOptions::default();
    if let Some(raw) = env.get(ENV_MAX_CONCURRENT_STREAMS) {
        options.max_concurrent_streams = raw.parse().map_err(|_| {
            Error::Config(format!(
                "{ENV_MAX_CONCURRENT_STREAMS} {raw:?} is not a count"
            ))
        })?;
    }
    if let Some(raw) = env.get(ENV_STREAM_DRAIN_TIMEOUT) {
        let secs: u64 = raw.parse().map_err(|_| {
            Error::Config(format!(
                "{ENV_STREAM_DRAIN_TIMEOUT} {raw:?} is not a number of seconds"
            ))
        })?;
        options.drain_timeout = std::time::Duration::from_secs(secs);
    }
    Ok(options)
}

/// The identity a stream counts against (spec 026 B-5): the bearer
/// credential's `(client_id, sub)` pair when spec 025's layer left one on
/// the request, else nothing, so the edge falls back to the principal and
/// then the client address.
fn stream_identity() -> StreamIdentityResolver {
    std::sync::Arc::new(|request: &axum::extract::Request| {
        request
            .extensions()
            .get::<rahi_idp::Bearer>()
            .map(rahi_idp::Bearer::identity)
    })
}

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
        Self::boot::<C>(env, false).await
    }

    /// As [`Self::open`], for a verb that may run beside a running cluster
    /// (spec 032 B-5 and B-6). With [`rahi_ops::ENV_STORE_CLIENT`] set, the
    /// process is a pure client of the peers in [`rahi_ops::ENV_HIQ_NODES`]
    /// and starts no node: the migration Job. When this data directory's
    /// node is already running in another process (its lock file exists),
    /// it attaches to that node instead of starting a second one: `rahi
    /// backup` inside a replica whose `serve` holds the node. Otherwise it
    /// opens the node as [`Self::open`] does.
    ///
    /// # Errors
    ///
    /// As [`Self::open`]; [`Error::Upstream`] when the running node does
    /// not answer.
    pub async fn open_or_attach<C: Cell>(env: &dyn EnvReader) -> Result<Self> {
        Self::boot::<C>(env, true).await
    }

    async fn boot<C: Cell>(env: &dyn EnvReader, may_attach: bool) -> Result<Self> {
        let config = Config::from_env(env)?;
        rahi_ops::refuse_env_restore()?;
        let keys = KeySet::of(&config);
        keys.check()?;
        let manifest = Manifest::parse(C::manifest())?;
        let hash = manifest.hash()?;
        let store_cfg = rahi_ops::store_config(&config, env, keys.store_secrets()?)?;
        let store = if may_attach && rahi_ops::store_client_requested(env) {
            Store::connect(&store_cfg).await?
        } else if may_attach && rahi_ops::app_lock_file(&config).exists() {
            Store::attach(&store_cfg).await?
        } else {
            Store::open(&store_cfg).await?
        };
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
pub async fn compose<C: Cell>(
    booted: &Booted,
    env: &dyn EnvReader,
    streams: &StreamHub,
) -> Result<Router> {
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

    let state = AppState::new(kernel, booted.store.handle(), ledger, booted.config.clone())
        .with_extension(streams.clone());
    let mut edge = Edge::builder(state.clone())
        .stream_identity(stream_identity())
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
        // rauthy publishes its endpoints on the public origin, which is not
        // reachable from inside the container; the key set is fetched over
        // the same loopback base every other back-channel call uses (spec
        // 021 B-1, spec 031 D-3).
        let mut on_loopback = discovery.clone();
        on_loopback.jwks_uri = back_channel(&idp, &discovery.jwks_uri);
        let jwks = Jwks::load(&on_loopback).await?;
        let key = SessionKey::load(&booted.keys.path(rahi_ops::SESSION_KEY_FILE))?;
        let secret_path = rahi_ops::supervise::client_secret_path(&booted.config);
        let client_secret = std::fs::read_to_string(&secret_path)
            .map(|text| text.trim().to_owned())
            .map_err(|err| {
                Error::Config(format!(
                    "the OIDC client secret at {} is not custodied yet; run the client bootstrap (spec 021 B-5) first: {err}",
                    secret_path.display()
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
        // The proxy and the resource metadata carry their full paths, so they
        // merge at the root and are named in the exposure table by prefix;
        // the session routes are relative and nest under their prefix.
        edge = edge
            .mount("/", proxy_router(Proxy::new(&idp)?))
            .expose(Route::new(AUTH_PREFIX, RouteClass::Proxy))
            .mount_public(SESSION_PREFIX, session_router(sessions))
            .mount("/", resource_router(resource))
            .expose(Route::new(rahi_idp::METADATA_PATH, RouteClass::Public));
    }

    edge.try_build().map_err(|err| err.0)
}

/// How long in-flight connections have to finish once serve is told to
/// stop. Past this the listener is dropped, the connections with it, and
/// the node is shut down regardless: a lock file must never outlive the
/// process (spec 031 D-4).
pub const DRAIN_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);

/// B-2: boot, compose, listen until SIGTERM or Ctrl-C.
///
/// # Errors
///
/// As [`Booted::open`] and [`compose`], plus [`Error::Io`] when the address
/// cannot be bound.
pub async fn serve<C: Cell>(env: &dyn EnvReader) -> Result<()> {
    serve_until::<C>(env, shutdown_signal()).await
}

/// [`serve`] that stops when `stop` resolves, and only then: under the
/// supervisor (spec 031 B-3) `stop` is the supervisor's hand, so that one
/// SIGTERM has one listener and one ordered shutdown. The node is shut
/// down on every exit, within [`DRAIN_BUDGET`] of the stop.
///
/// # Errors
///
/// As [`serve`].
pub async fn serve_until<C: Cell>(
    env: &dyn EnvReader,
    stop: impl Future<Output = ()> + Send + 'static,
) -> Result<()> {
    let addr = listen_addr(env)?;
    let streams = StreamHub::new(stream_options(env)?);
    let booted = Booted::open::<C>(env).await?;
    let router = match compose::<C>(&booted, env, &streams).await {
        Ok(router) => router,
        Err(err) => {
            booted.shutdown().await;
            return Err(err);
        }
    };
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => listener,
        Err(err) => {
            booted.shutdown().await;
            return Err(Error::Io(format!("cannot listen on {addr}: {err}")));
        }
    };
    let bound = listener
        .local_addr()
        .map_err(|err| Error::Io(format!("bound address unknown: {err}")))?;
    println!(
        "serve: {} listening on {bound}, public origin {}",
        booted.manifest.app.name.as_str(),
        booted.config.public_url.origin()
    );
    let stopped = std::sync::Arc::new(tokio::sync::Notify::new());
    let until = {
        let stopped = stopped.clone();
        let streams = streams.clone();
        async move {
            stop.await;
            // Spec 026 B-7: every open stream hears the shutdown and has the
            // drain timeout to close before the listener stops.
            let remaining = streams.drain().await;
            if remaining > 0 {
                eprintln!(
                    "serve: {remaining} stream(s) still open after the drain timeout; closing them"
                );
            }
            stopped.notify_one();
        }
    };
    let mut server = Box::pin(
        axum::serve(listener, router)
            .with_graceful_shutdown(until)
            .into_future(),
    );
    let served = tokio::select! {
        result = &mut server => {
            result.map_err(|err| Error::Io(format!("the listener failed: {err}")))
        }
        () = async {
            stopped.notified().await;
            tokio::time::sleep(DRAIN_BUDGET).await;
        } => {
            eprintln!("serve: connections still open after the drain budget; closing them");
            Ok(())
        }
    };
    drop(server);
    booted.shutdown().await;
    served
}

/// `endpoint` on the loopback base when it is on the public origin, as
/// `rahi_idp::Sessions::back_channel` rewrites it.
fn back_channel(idp: &IdpConfig, endpoint: &str) -> String {
    let origin = idp
        .issuer
        .strip_suffix(rahi_idp::ISSUER_PATH)
        .unwrap_or(&idp.issuer);
    endpoint.strip_prefix(origin).map_or_else(
        || endpoint.to_owned(),
        |path| format!("{}{path}", idp.loopback_base),
    )
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
