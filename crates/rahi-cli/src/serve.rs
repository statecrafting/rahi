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
use std::time::Duration;

use axum::Router;
use axum::http::HeaderMap;
use rahi_edge::obs::DECISION_TARGET;
use rahi_edge::{
    AppState, Edge, Route, RouteClass, StreamHub, StreamIdentityResolver, StreamOptions,
};
use rahi_idp::{
    AUTH_PREFIX, Discovery, IdpConfig, Jwks, Proxy, Resource, SessionKey, Sessions, proxy_router,
    resource_router, session_router, with_sessions,
};
use rahi_kernel::{Kernel, KernelOptions, Manifest};
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

/// Where the static slot serves from, when the deployment names it
/// (spec 039 B-5). It wins over the cell's own `static_dir()`, which names a
/// path in the source tree that an image does not carry.
pub const ENV_STATIC_DIR: &str = "RAHI_STATIC_DIR";

/// The directory the static slot serves, or `None` when neither the
/// environment nor the cell names one (spec 039 B-5).
///
/// A named directory that does not exist is a startup error rather than a
/// slot that answers `404` for every page: the cell was deployed with a page
/// it cannot serve, and the operator finds that out at boot instead of from
/// the first visitor.
///
/// # Errors
///
/// [`Error::Config`] naming the directory and who named it.
pub fn static_dir<C: Cell>(env: &dyn EnvReader) -> Result<Option<PathBuf>> {
    let (dir, named_by) = match env.get(ENV_STATIC_DIR) {
        Some(raw) if !raw.trim().is_empty() => (PathBuf::from(raw.trim()), ENV_STATIC_DIR),
        _ => match C::static_dir() {
            Some(dir) => (dir, "the cell's static_dir()"),
            None => return Ok(None),
        },
    };
    if !dir.is_dir() {
        return Err(Error::Config(format!(
            "{named_by} names the static directory {}, which does not exist; the static slot \
             would answer 404 for every page it was deployed to serve",
            dir.display()
        )));
    }
    Ok(Some(dir))
}

/// Seconds a stop gives the kernel's denial queue to drain before it shuts
/// the store (spec 035 B-1); the default is [`DEFAULT_DENIAL_DRAIN_TIMEOUT`].
pub const ENV_DENIAL_DRAIN_TIMEOUT: &str = "RAHI_DENIAL_DRAIN_TIMEOUT_SECS";

/// The denial drain bound when the environment names none (spec 035 D-1).
pub const DEFAULT_DENIAL_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Read [`ENV_DENIAL_DRAIN_TIMEOUT`].
///
/// # Errors
///
/// [`Error::Config`] when the value is not a number of seconds.
pub fn denial_drain_timeout(env: &dyn EnvReader) -> Result<Duration> {
    let Some(raw) = env.get(ENV_DENIAL_DRAIN_TIMEOUT) else {
        return Ok(DEFAULT_DENIAL_DRAIN_TIMEOUT);
    };
    raw.parse::<u64>().map(Duration::from_secs).map_err(|_| {
        Error::Config(format!(
            "{ENV_DENIAL_DRAIN_TIMEOUT} {raw:?} is not a number of seconds"
        ))
    })
}

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

/// A composed cell: the router `serve` listens with, and the kernel whose
/// denial queue a stop drains before the store goes away (spec 035 B-1).
pub struct Composed {
    /// The cell's router, identity layer included.
    pub router: Router,
    /// The kernel every route of the router adjudicates through.
    pub kernel: Kernel,
}

/// Compose the cell's router over a booted cell (B-2, without the listener).
///
/// # Errors
///
/// As [`compose_parts`].
pub async fn compose<C: Cell>(
    booted: &Booted,
    env: &dyn EnvReader,
    streams: &StreamHub,
) -> Result<Router> {
    compose_parts::<C>(booted, env, streams)
        .await
        .map(|composed| composed.router)
}

/// [`compose`], keeping the kernel it booted.
///
/// The kernel names this replica's node in every decision id
/// (spec 035 B-6), read from the store configuration the node was opened
/// with.
///
/// # Errors
///
/// [`Error::Stale`] when the store is behind the cell's migrations;
/// [`Error::Integrity`] when the chain does not verify; [`Error::Config`]
/// when identity cannot be derived or a route is unclassified;
/// [`Error::Upstream`] when rauthy is required and does not answer.
pub async fn compose_parts<C: Cell>(
    booted: &Booted,
    env: &dyn EnvReader,
    streams: &StreamHub,
) -> Result<Composed> {
    rahi_ops::migrate::check_current_sets(&booted.store, C::migrations(), &C::migration_sets())
        .await?;
    let ledger = booted.ledger().await?;
    let kernel = Kernel::boot_with(
        booted.manifest.clone(),
        booted.store.handle(),
        ledger.clone(),
        KernelOptions {
            node_id: booted.store.config().node_id,
            ..KernelOptions::default()
        },
    )
    .await?;
    let obs = rahi_edge::ObsOptions::from_env(&booted.config, env)?
        .with_service_name(booted.manifest.app.name.as_str());
    rahi_edge::obs::init(obs)?;

    let state = AppState::new(
        kernel.clone(),
        booted.store.handle(),
        ledger,
        booted.config.clone(),
    )
    .with_extension(streams.clone());

    // The app's routes are built first because building them is what
    // publishes the scopes its gates require (spec 025 B-2), and spec 038
    // B-2 holds the manifest's native clients to that published set. A
    // client declared a scope no route asks for is a ceiling nobody
    // enforces, and the cell refuses to serve rather than provision it.
    let app_routes = C::routes(state.clone());
    let operator_routes = C::operator_routes(state.clone());
    let declared_bearer = C::bearer_routes();
    booted
        .manifest
        .validate_native_scopes(&rahi_idp::scope::supported().into_iter().collect())?;

    let mut resolver: Option<Sessions> = None;
    let mut bearer: Option<rahi_idp::RequireBearer> = None;
    let mut operator_routes = operator_routes;
    let mut identity: Option<Identity> = None;
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
        let jwks_cache = jwks.clone();
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

        // Spec 025's resource server, with spec 038's two additions: the
        // deny-list remembers a revocation for as long as a token of the
        // manifest's lifetime can still validate (038 D-7), and the
        // revocation routes are what write it (038 B-5).
        let lifetime = Duration::from_secs(booted.manifest.access_token_lifetime_secs());
        let server = rahi_idp::ResourceServer::new(
            &booted.config,
            resource.clone(),
            jwks_cache.clone(),
            booted.store.handle(),
            kernel.clone(),
        )
        .with_lifetime(lifetime);
        let mut revoker = rahi_idp::Revoker::new(server.clone());
        if let Ok(admin_token) = booted.keys.admin_token() {
            // Spec 038 D-8: revoking a subject ends the grant at rauthy as
            // well as deny-listing the access tokens. A cell whose key set
            // holds no admin token still revokes the tokens and says in its
            // answer that the grant is untouched.
            revoker = revoker.ending_grants(&idp, &admin_token)?;
        }
        operator_routes = operator_routes.merge(rahi_idp::operator_revoke_router(revoker.clone()));

        // The route a bearer client revokes its own token on is itself a
        // bearer route: it reads the credential the gate resolved, and
        // nothing a caller writes decides what is revoked (038 B-5).
        let declared = declared_bearer.clone().route(rahi_idp::SESSION_REVOKE_PATH);
        bearer = Some(rahi_idp::RequireBearer::new(server, declared));

        // The proxy and the resource metadata carry their full paths, so they
        // merge at the root and are named in the exposure table by prefix;
        // the session routes are relative and nest under their prefix.
        identity = Some(Identity {
            proxy: proxy_router(Proxy::new(&idp)?),
            sessions: session_router(sessions.clone()),
            resource: resource_router(resource),
            revoke: rahi_idp::revoke_router(revoker),
        });
        resolver = Some(sessions);
    }

    let mut edge = Edge::builder(state.clone())
        .stream_identity(stream_identity())
        .mount("/", app_routes)
        .mount_operator(OPERATOR_PREFIX, operator_routes);
    for route in C::exposed() {
        edge = edge.expose(route);
    }
    if let Some(dir) = static_dir::<C>(env)? {
        edge = edge.static_slot(dir);
    }
    if let Some(identity) = identity {
        edge = edge
            .mount("/", identity.proxy)
            .expose(Route::new(AUTH_PREFIX, RouteClass::Proxy))
            .mount_public(SESSION_PREFIX, identity.sessions)
            .mount("/", identity.revoke)
            .mount("/", identity.resource)
            .expose(Route::new(rahi_idp::METADATA_PATH, RouteClass::Public));
    }
    // Spec 038 B-1: the CSRF check does not apply to a bearer write. The
    // predicate is passed from here because the facts it reads are the
    // composer's: which routes were declared bearer, and what this cell's
    // session cookie is called. A request carrying both credentials is not
    // exempt and never reaches the check anyway, because the bearer layer
    // refuses it first (025 B-10).
    let scheme = booted.config.cookie_scheme;
    edge = edge.csrf_exemption(std::sync::Arc::new(
        move |path: &str, headers: &HeaderMap| {
            rahi_idp::is_bearer_route(path)
                && headers.contains_key(axum::http::header::AUTHORIZATION)
                && !rahi_idp::bearer::both_credentials(headers, scheme)
        },
    ));

    let router = edge.try_build().map_err(|err| err.0)?;
    // Spec 022's layer, outermost: it opens the session cookie, renews the
    // assertion, and leaves the `Principal` in the request extensions that
    // `Authenticated` and the operator gate read. Outside it every
    // authenticated route answers 401 (022 D-3), which is what an app with
    // a login would have met here before the first app existed (034 D-2).
    let router = match resolver {
        Some(sessions) => with_sessions(sessions, router),
        None => router,
    };
    // Spec 025 B-11: outside the session layer, where the paths are whole.
    // It resolves a token, refuses a request carrying two credentials, and
    // strips any cookie a bearer-authenticated answer tried to set.
    let router = match bearer {
        Some(gate) => rahi_idp::with_bearer(gate, router),
        None => router,
    };
    Ok(Composed { router, kernel })
}

/// What the identity block built, mounted once the edge builder exists.
struct Identity {
    proxy: Router,
    sessions: Router,
    resource: Router,
    revoke: Router,
}

/// B-7 of spec 035: the line naming the nonce and the node this boot's
/// kernel mints decision ids under, so an auditor can pair any id with the
/// boot that minted it and see two boots of one replica on one nonce.
#[must_use]
pub fn boot_line(kernel: &Kernel) -> String {
    format!(
        "INFO {DECISION_TARGET}: decision ids of this boot are kernel:{nonce}:{node}:<counter> \
         (nonce {nonce}, node {node})",
        nonce = kernel.nonce(),
        node = kernel.node_id(),
    )
}

/// B-1 and B-2 of spec 035: give the denial queue its bound, and say so in
/// one warning line when the bound expired. Each abandoned id has already
/// been counted and written as its own error line by the failure observer.
async fn drain_denials(kernel: &Kernel, bound: Duration) {
    let drained = kernel.drain(bound).await;
    if !drained.is_complete() {
        eprintln!(
            "WARN {DECISION_TARGET}: the denial drain bound of {bound:?} expired with {} \
             decision(s) abandoned",
            drained.abandoned.len()
        );
    }
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
/// down on every exit, within [`DRAIN_BUDGET`] of the stop plus the denial
/// drain's bound: in-flight requests finish first, then the kernel's denial
/// queue drains within [`ENV_DENIAL_DRAIN_TIMEOUT`], then the store is shut
/// (spec 035 B-1).
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
    let denial_bound = denial_drain_timeout(env)?;
    // Spec 039 B-5: a page the cell cannot serve is a boot failure, and it is
    // one before the store is opened. `compose` resolves it again for its own
    // callers; both calls are one `is_dir`.
    static_dir::<C>(env)?;
    let booted = Booted::open::<C>(env).await?;
    let Composed { router, kernel } = match compose_parts::<C>(&booted, env, &streams).await {
        Ok(composed) => composed,
        Err(err) => {
            booted.shutdown().await;
            return Err(err);
        }
    };
    println!("{}", boot_line(&kernel));
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => listener,
        Err(err) => {
            drain_denials(&kernel, denial_bound).await;
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
    // Spec 035 B-1: the requests are done; the records of their denials get
    // their bound before the store they are written through goes away.
    drain_denials(&kernel, denial_bound).await;
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

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::OnceLock;

    use axum::Router;
    use rahi_edge::AppState;
    use rahi_store::Migration;

    use super::*;
    use crate::cell::EmptyCell;

    /// Where [`PagedCell`] says its page lives.
    fn cell_page() -> &'static PathBuf {
        static DIR: OnceLock<PathBuf> = OnceLock::new();
        DIR.get_or_init(|| {
            let dir = std::env::temp_dir().join("rahi-serve-cell-page");
            std::fs::create_dir_all(&dir).expect("a directory for the cell's own page");
            dir
        })
    }

    /// A cell that names a page of its own, the way hello-cell does.
    struct PagedCell;

    impl Cell for PagedCell {
        fn manifest() -> &'static str {
            EmptyCell::MANIFEST
        }

        fn migrations() -> &'static [Migration] {
            &[]
        }

        fn routes(_state: AppState) -> Router {
            Router::new()
        }

        fn static_dir() -> Option<PathBuf> {
            Some(cell_page().clone())
        }
    }

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn the_environment_names_the_static_directory_over_the_cells_own() {
        let deployed = tempfile::tempdir().expect("a temp dir");
        let chosen = static_dir::<PagedCell>(&env(&[(
            ENV_STATIC_DIR,
            &deployed.path().display().to_string(),
        )]))
        .expect("the directory exists")
        .expect("one is named");
        assert_eq!(chosen, deployed.path(), "spec 039 B-5: the deployment wins");

        let fallback = static_dir::<PagedCell>(&env(&[]))
            .expect("the cell's own directory exists")
            .expect("one is named");
        assert_eq!(&fallback, cell_page(), "with nothing set, the cell's own");

        assert!(
            static_dir::<EmptyCell>(&env(&[]))
                .expect("no error")
                .is_none(),
            "a cell with no page and no environment names no directory"
        );
    }

    #[test]
    fn a_static_directory_that_does_not_exist_is_a_startup_error_naming_it() {
        let missing = std::env::temp_dir().join("rahi-no-such-page-dir");
        let _ = std::fs::remove_dir_all(&missing);
        let err =
            static_dir::<EmptyCell>(&env(&[(ENV_STATIC_DIR, &missing.display().to_string())]))
                .expect_err("a named directory that is absent is refused");
        assert_eq!(err.exit_code(), rahi_types::error::EXIT_INFRA);
        assert!(err.message().contains(ENV_STATIC_DIR), "{err}");
        assert!(
            err.message().contains(&missing.display().to_string()),
            "{err}"
        );
        assert!(
            err.message().contains("404"),
            "it says what the slot would do: {err}"
        );
    }
}
