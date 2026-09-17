//! The die-together supervisor (spec 031 B-3).
//!
//! One container is two processes and one lifetime. The app spawns rauthy
//! on loopback, waits for its health, runs `serve` in this process, and
//! ends the moment either half ends: rauthy's exit stops serving and the
//! container exits with rauthy's code; serve's failure terminates rauthy
//! (SIGTERM, five seconds, SIGKILL) and the container exits with serve's
//! code; a SIGTERM to the supervisor reaches both. Nothing restarts
//! anything: the orchestrator outside the container owns restarts, and a
//! half-alive container is the failure mode this exists to remove.

use std::future::Future;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use rahi_idp::{Bootstrap, CLIENT_SECRET_FILE, IdpConfig, bootstrap_client};
use rahi_types::{Config, EnvReader, Error, Result};
use tokio::process::{Child, Command};

use crate::KeySet;
use crate::rauthy_api::RauthyApi;
use crate::rauthy_env;

/// Where the rauthy binary is, when the environment does not say.
pub const DEFAULT_RAUTHY_BIN: &str = "/usr/local/bin/rauthy";

/// Overrides the rauthy binary's path.
pub const ENV_RAUTHY_BIN: &str = "RAHI_RAUTHY_BIN";

/// How long rauthy may take to answer its health route (B-3).
pub const HEALTH_BUDGET: Duration = Duration::from_secs(60);

/// How long a terminated child has to exit before it is killed (B-3).
pub const TERM_GRACE: Duration = Duration::from_secs(5);

/// How often the health route is polled while waiting.
pub const HEALTH_INTERVAL: Duration = Duration::from_millis(250);

/// Why the supervisor ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// rauthy exited; serve was stopped.
    RauthyExited,
    /// rauthy never became healthy inside the budget, or the client
    /// bootstrap that follows health failed.
    RauthyUnhealthy,
    /// serve ended; rauthy was terminated.
    ServeEnded,
    /// The supervisor was told to stop; both halves were stopped.
    Shutdown,
}

/// How the supervisor ended: the exit code the container exits with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Exit {
    /// The process exit code.
    pub code: i32,
    /// Which half ended first.
    pub reason: Reason,
}

/// hiqlite's restore variable, which rauthy reads at its node's start.
///
/// The app's own node never sees it (spec 030 [`crate::RESTORE_ENV_VAR`]
/// refuses to start while it is set): this is set on rauthy's child
/// process alone, by the supervisor, for exactly one start, and the
/// restore marker is what makes it exactly one (spec 037 B-3).
pub const RAUTHY_RESTORE_ENV_VAR: &str = crate::RESTORE_ENV_VAR;

/// The rauthy child's command: the binary, its rendered environment, its
/// empty config, and its directory.
///
/// After a restore whose snapshot rauthy has not yet come up on, the
/// command also carries [`RAUTHY_RESTORE_ENV_VAR`] pointing at the placed
/// snapshot, which is how rauthy's own store is restored: the app hands
/// rauthy a file and never opens rauthy's directory (constitution VIII,
/// spec 037 B-3). Every other start passes nothing.
///
/// # Errors
///
/// [`Error::Io`] when the rendered environment cannot be read (run
/// `first-boot`), or when the restore marker cannot be read.
pub fn rauthy_command(config: &Config, env: &dyn EnvReader) -> Result<Command> {
    let bin = env
        .get(ENV_RAUTHY_BIN)
        .map_or_else(|| PathBuf::from(DEFAULT_RAUTHY_BIN), PathBuf::from);
    let env_path = rauthy_env::env_path(config);
    let rendered = std::fs::read_to_string(&env_path).map_err(|err| {
        Error::Io(format!(
            "{} cannot be read; run first-boot first: {err}",
            env_path.display()
        ))
    })?;
    let mut command = Command::new(bin);
    command
        .arg("serve")
        .arg("-c")
        .arg(rauthy_env::config_path(config))
        .current_dir(crate::rauthy_dir(config))
        .env_clear()
        .envs(rauthy_env::parse(&rendered))
        .stdin(Stdio::null())
        .kill_on_drop(true);
    for inherited in ["PATH", "HOME", "TZ"] {
        if let Some(value) = std::env::var_os(inherited) {
            command.env(inherited, value);
        }
    }
    if let Some(snapshot) = crate::restore::pending_rauthy_snapshot(config)? {
        command.env(
            RAUTHY_RESTORE_ENV_VAR,
            format!("file:{}", snapshot.display()),
        );
    }
    Ok(command)
}

/// The readiness step of a supervised start (spec 037 B-1, B-3): record a
/// restored snapshot that rauthy has now come up on, and make sure rauthy
/// holds the dedicated backup admin this key set's passkey belongs to.
///
/// Called after [`wait_healthy`] and before the client bootstrap. Each half
/// is a no-op on every start but the one that needs it, and each returns
/// what it did so the supervisor can say so.
///
/// The two halves fail differently on purpose. A restore marker that cannot
/// be written is fatal: the next start would hand rauthy the same snapshot
/// again, and a volume that silently re-restores is the crash loop spec
/// 030 exists to remove. A backup admin that cannot be provisioned is
/// **not** fatal, it is reported: the cell serves without it, and a cell
/// that refuses to start takes rauthy's own admin interface down with it,
/// which is the one place an operator could repair the account. `rahi
/// backup` is where the consequence lands, and it names the credential it
/// wanted.
///
/// # Errors
///
/// [`Error::Io`] when the restore marker cannot be written.
pub async fn ready_after_health(
    config: &Config,
    keys: &KeySet,
    api: &RauthyApi,
) -> Result<ReadySteps> {
    let restored = crate::restore::record_rauthy_snapshot_applied(config).await?;
    let backup_admin = match keys.backup_passkey() {
        Ok(Some(passkey)) => {
            match crate::rauthy_session::ensure_backup_admin(api.base(), api.token(), &passkey)
                .await
            {
                Ok(outcome) => Ok(Some(outcome)),
                Err(err) => Err(err.to_string()),
            }
        }
        Ok(None) => Ok(None),
        Err(err) => Err(err.to_string()),
    };
    Ok(ReadySteps {
        rauthy_snapshot_applied: restored,
        backup_admin,
    })
}

/// What [`ready_after_health`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadySteps {
    /// The restored rauthy snapshot this start applied, if any.
    pub rauthy_snapshot_applied: Option<PathBuf>,
    /// What became of the backup admin: `Ok(None)` when this key set holds
    /// no passkey (one minted before spec 037), `Err` when rauthy refused,
    /// which is reported rather than fatal.
    pub backup_admin: std::result::Result<Option<crate::rauthy_session::Provisioned>, String>,
}

impl ReadySteps {
    /// Whether this start left the cell able to back rauthy up.
    #[must_use]
    pub fn can_back_rauthy_up(&self) -> bool {
        matches!(self.backup_admin, Ok(Some(_)))
    }

    /// One line for the supervisor's own output, or nothing to say.
    #[must_use]
    pub fn render(&self) -> String {
        let mut said = Vec::new();
        if let Some(path) = &self.rauthy_snapshot_applied {
            said.push(format!("rauthy restored from {}", path.display()));
        }
        match &self.backup_admin {
            Ok(Some(crate::rauthy_session::Provisioned::Created)) => {
                said.push("backup admin provisioned".to_owned());
            }
            Ok(Some(crate::rauthy_session::Provisioned::AlreadyPresent)) => {}
            Ok(None) => said.push(format!(
                "no {}: this key set predates spec 037, and `rahi backup` cannot take \
                 rauthy's half until the deployment is given one",
                crate::BACKUP_PASSKEY_FILE
            )),
            Err(err) => said.push(format!(
                "the backup admin is NOT usable and `rahi backup` will refuse rauthy's \
                 half until it is: {err}"
            )),
        }
        said.join("; ")
    }
}

/// Poll rauthy's health until it answers or `budget` runs out.
///
/// # Errors
///
/// [`Error::Upstream`] when the budget runs out first.
pub async fn wait_healthy(api: &RauthyApi, budget: Duration) -> Result<()> {
    let deadline = tokio::time::Instant::now() + budget;
    loop {
        if api.health().await.is_ok() {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(Error::Upstream(format!(
                "rauthy did not answer {} within {} seconds",
                api.base(),
                budget.as_secs()
            )));
        }
        tokio::time::sleep(HEALTH_INTERVAL).await;
    }
}

/// Where the OIDC client secret is custodied: under the key set when it is
/// writable or already holds one, else beside rauthy's rendered
/// environment on the volume, for a key set that is a read-only Secret
/// mount (spec 032 B-2). The secret is rauthy's to mint and lives in
/// rauthy's replicated store, so a backup carries it either way.
#[must_use]
pub fn client_secret_path(config: &Config) -> PathBuf {
    let keys = KeySet::of(config);
    let in_keys = keys.path(CLIENT_SECRET_FILE);
    if in_keys.exists() || crate::dir_is_writable(keys.dir()) {
        return in_keys;
    }
    crate::rauthy_dir(config).join(CLIENT_SECRET_FILE)
}

fn write_client_secret(config: &Config, secret: &str) -> Result<()> {
    let path = client_secret_path(config);
    if path.starts_with(config.keys_dir()) {
        return KeySet::of(config).write(CLIENT_SECRET_FILE, secret.as_bytes());
    }
    std::fs::write(&path, secret.as_bytes())
        .map_err(|err| Error::Io(format!("{} cannot be written: {err}", path.display())))?;
    crate::set_mode(&path, crate::KEY_FILE_MODE)
}

/// Register the cell's OIDC client and custody its secret (spec 021 B-5),
/// after rauthy is healthy and before serve.
///
/// # Errors
///
/// As `rahi_idp::bootstrap_client`, plus [`Error::Upstream`] when the
/// secret cannot be read back from rauthy.
pub async fn custody_client(config: &Config, keys: &KeySet, app_name: &str) -> Result<()> {
    let idp = IdpConfig::derive(config, app_name)?;
    let token = keys.admin_token()?;
    let minted = match bootstrap_client(&idp, &token).await? {
        Bootstrap::Created {
            secret: Some(secret),
        } => Some(secret),
        Bootstrap::Created { secret: None } | Bootstrap::Unchanged => None,
    };
    match minted {
        Some(secret) => write_client_secret(config, &secret),
        None if client_secret_path(config).exists() => Ok(()),
        None => {
            let secret = read_client_secret(&idp, &token).await?;
            write_client_secret(config, &secret)
        }
    }
}

async fn read_client_secret(idp: &IdpConfig, token: &str) -> Result<String> {
    #[derive(serde::Deserialize)]
    struct SecretResponse {
        secret: Option<String>,
    }
    let url = format!("{}/secret", idp.client_url());
    let client = reqwest::Client::builder()
        .build()
        .map_err(|err| Error::Config(format!("the loopback client cannot be built: {err}")))?;
    let response = client
        .post(&url)
        .header(
            reqwest::header::AUTHORIZATION,
            format!("{} {token}", rahi_idp::API_KEY_SCHEME),
        )
        .send()
        .await
        .map_err(|err| Error::Upstream(format!("rauthy is unreachable at {url}: {err}")))?;
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(Error::Unauthorized(format!(
            "rauthy refused the admin token at {url} ({status})"
        )));
    }
    if !status.is_success() {
        return Err(Error::Upstream(format!(
            "rauthy answered {status} at {url}"
        )));
    }
    let body: SecretResponse = response.json().await.map_err(|err| {
        Error::Upstream(format!("rauthy's secret document does not parse: {err}"))
    })?;
    body.secret.ok_or_else(|| {
        Error::Upstream(format!(
            "rauthy holds no secret for client {}; the cell is a confidential client",
            idp.client_id
        ))
    })
}

/// Resolves on SIGTERM or Ctrl-C: the supervisor's own stop signal (B-3).
pub async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
        Ok(mut term) => {
            tokio::select! {
                _ = ctrl_c => {},
                _ = term.recv() => {},
            }
        }
        Err(_) => {
            let _ = ctrl_c.await;
        }
    }
}

/// SIGTERM `child`, wait `grace`, then SIGKILL. Returns the exit code.
pub async fn terminate(child: &mut Child, grace: Duration) -> i32 {
    if let Some(pid) = child.id() {
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(pid as i32),
            nix::sys::signal::Signal::SIGTERM,
        );
    }
    match tokio::time::timeout(grace, child.wait()).await {
        Ok(Ok(status)) => exit_code(status),
        _ => {
            let _ = child.kill().await;
            child.wait().await.map_or(137, exit_code)
        }
    }
}

/// The code a child exited with; a signal death is `128 + signal`.
#[must_use]
pub fn exit_code(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt as _;
    status
        .code()
        .or_else(|| status.signal().map(|s| 128 + s))
        .unwrap_or(1)
}

/// How long serve has to finish its own shutdown once told to stop.
pub const SERVE_GRACE: Duration = Duration::from_secs(15);

/// How long rauthy has to exit after a propagated SIGTERM (B-3's last
/// sentence). rauthy's own graceful stop finishes its open connections and
/// then its Raft node, which takes two to five seconds in practice; the
/// five seconds of [`TERM_GRACE`] belong to the serve-failure path, where
/// nothing is waiting on a clean stop. An orchestrator's termination budget
/// must cover [`SERVE_GRACE`] plus this (Kubernetes gives thirty seconds by
/// default; `docker stop -t 30`).
pub const SHUTDOWN_TERM_GRACE: Duration = Duration::from_secs(10);

/// Run `rauthy` and `serve` as one lifetime (B-3).
///
/// `ready` runs after the child is spawned and before `serve`: the health
/// wait and the client bootstrap. `serve` is built from a stop signal the
/// supervisor fires when rauthy exits or when `shutdown` resolves, so the
/// node under serve is shut down cleanly rather than dropped mid-flight
/// (a dropped serve leaves hiqlite's lock file, and the next start refuses
/// to open the node). `shutdown` resolving is the supervisor's own SIGTERM,
/// which reaches both halves.
pub async fn supervise<R, S, F, D>(rauthy: Command, ready: R, serve: S, shutdown: D) -> Exit
where
    R: Future<Output = Result<()>>,
    S: FnOnce(tokio::sync::oneshot::Receiver<()>) -> F,
    F: Future<Output = Result<()>>,
    D: Future<Output = ()>,
{
    supervise_with(
        rauthy,
        ready,
        serve,
        shutdown,
        Graces {
            term: TERM_GRACE,
            serve: SERVE_GRACE,
            shutdown_term: SHUTDOWN_TERM_GRACE,
        },
    )
    .await
}

/// The three grace periods [`supervise_with`] honours.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Graces {
    /// SIGTERM to SIGKILL for rauthy when serve failed ([`TERM_GRACE`]).
    pub term: Duration,
    /// How long serve may take to stop ([`SERVE_GRACE`]).
    pub serve: Duration,
    /// SIGTERM to SIGKILL for rauthy on a propagated SIGTERM
    /// ([`SHUTDOWN_TERM_GRACE`]).
    pub shutdown_term: Duration,
}

/// [`supervise`] with the grace periods named.
pub async fn supervise_with<R, S, F, D>(
    mut rauthy: Command,
    ready: R,
    serve: S,
    shutdown: D,
    graces: Graces,
) -> Exit
where
    R: Future<Output = Result<()>>,
    S: FnOnce(tokio::sync::oneshot::Receiver<()>) -> F,
    F: Future<Output = Result<()>>,
    D: Future<Output = ()>,
{
    let mut child = match rauthy.spawn() {
        Ok(child) => child,
        Err(err) => {
            eprintln!("supervise: rauthy cannot be spawned: {err}");
            return Exit {
                code: rahi_types::error::EXIT_INFRA,
                reason: Reason::RauthyExited,
            };
        }
    };
    let Graces {
        term: term_grace,
        serve: serve_grace,
        shutdown_term: shutdown_grace,
    } = graces;
    tokio::pin!(shutdown);

    tokio::select! {
        status = child.wait() => {
            let code = status.map_or(1, exit_code);
            eprintln!("supervise: rauthy exited with {code} before it was healthy");
            return Exit { code, reason: Reason::RauthyExited };
        }
        readiness = ready => {
            if let Err(err) = readiness {
                eprintln!("supervise: {err}");
                let _ = terminate(&mut child, term_grace).await;
                return Exit { code: err.exit_code(), reason: Reason::RauthyUnhealthy };
            }
        }
        () = &mut shutdown => {
            let _ = terminate(&mut child, shutdown_grace).await;
            return Exit { code: 0, reason: Reason::Shutdown };
        }
    }

    let (stop, stopped) = tokio::sync::oneshot::channel();
    let serve = serve(stopped);
    tokio::pin!(serve);
    tokio::select! {
        status = child.wait() => {
            let code = status.map_or(1, exit_code);
            eprintln!("supervise: rauthy exited with {code}; stopping serve");
            let _ = stop.send(());
            let _ = tokio::time::timeout(serve_grace, &mut serve).await;
            Exit { code, reason: Reason::RauthyExited }
        }
        served = &mut serve => {
            let code = match &served {
                Ok(()) => 0,
                Err(err) => {
                    eprintln!("supervise: serve ended: {err}");
                    err.exit_code()
                }
            };
            let _ = terminate(&mut child, term_grace).await;
            Exit { code, reason: Reason::ServeEnded }
        }
        () = &mut shutdown => {
            // serve first, then rauthy: serve's proxy holds keep-alive
            // connections to rauthy, and rauthy's server waits for them
            // before it exits, so the other order spends the whole grace
            // and ends in a SIGKILL that leaves rauthy's lock file behind.
            let _ = stop.send(());
            let served = tokio::time::timeout(serve_grace, &mut serve).await;
            let _ = terminate(&mut child, shutdown_grace).await;
            let code = match served {
                Ok(Err(err)) => err.exit_code(),
                _ => 0,
            };
            Exit { code, reason: Reason::Shutdown }
        }
    }
}
