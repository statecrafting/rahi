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
use crate::stop::{Observed, Reason as StopReason};

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
    prepare_rauthy(config, env).map(|(command, _)| command)
}

/// Prepare a child together with the restore source its readiness must certify.
///
/// # Errors
/// As [`rauthy_command`], including a missing pending restore source.
pub fn prepare_rauthy(
    config: &Config,
    env: &dyn EnvReader,
) -> Result<(Command, Option<crate::restore::PendingRauthySnapshot>)> {
    let snapshot = crate::restore::pending_rauthy_snapshot(config)?;
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
    if let Some(snapshot) = &snapshot {
        command.env(
            RAUTHY_RESTORE_ENV_VAR,
            format!("file:{}", snapshot.path().display()),
        );
    }
    Ok((command, snapshot))
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
    supplied: Option<&crate::restore::PendingRauthySnapshot>,
) -> Result<ReadySteps> {
    let restored = crate::restore::record_rauthy_snapshot_applied(config, supplied).await?;
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

/// What the custody step did beyond the cell's own client (spec 038 B-3).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Custodied {
    /// Each declared native client, and whether this boot created it.
    pub native: Vec<rahi_idp::Provisioned>,
    /// Whether this boot had to write the access token lifetime onto the
    /// cell's own client (B-4).
    pub lifetime_applied: bool,
}

impl Custodied {
    /// One clause for the supervisor's line, or nothing to say.
    #[must_use]
    pub fn render(&self) -> String {
        if self.native.is_empty() {
            return if self.lifetime_applied {
                "the manifest's access token lifetime applied".to_owned()
            } else {
                String::new()
            };
        }
        let named: Vec<String> = self
            .native
            .iter()
            .map(|client| {
                if client.created {
                    format!("{} (created)", client.id)
                } else {
                    client.id.clone()
                }
            })
            .collect();
        let lifetime = if self.lifetime_applied {
            "; the manifest's access token lifetime applied"
        } else {
            ""
        };
        format!("native clients {}{lifetime}", named.join(", "))
    }
}

/// Register the cell's OIDC client and custody its secret (spec 021 B-5),
/// then provision every native client the manifest declares (spec 038 B-3),
/// after rauthy is healthy and before serve.
///
/// The order is the one spec 038 B-3 states: the cell's own client first,
/// because a native client is bound to the audience that client's origin
/// defines, and a failure to custody the cell's own secret is a cell that
/// cannot log anybody in at all.
///
/// # Errors
///
/// As `rahi_idp::bootstrap_client`, plus [`Error::Upstream`] when the
/// secret cannot be read back from rauthy or a native client cannot be
/// provisioned.
pub async fn custody_client(
    config: &Config,
    keys: &KeySet,
    manifest: &rahi_kernel::Manifest,
) -> Result<Custodied> {
    let app_name = manifest.app.name.as_str();
    let idp = IdpConfig::derive(config, app_name)?;
    let token = keys.admin_token()?;
    let minted = match bootstrap_client(&idp, &token).await? {
        Bootstrap::Created {
            secret: Some(secret),
        } => Some(secret),
        Bootstrap::Created { secret: None } | Bootstrap::Unchanged => None,
    };
    match minted {
        Some(secret) => write_client_secret(config, &secret)?,
        None if client_secret_path(config).exists() => {}
        None => {
            let secret = read_client_secret(&idp, &token).await?;
            write_client_secret(config, &secret)?;
        }
    }

    // Spec 038 B-4: the manifest's lifetime, on the cell's own client too.
    // A narrow write of one field rather than a `first_mismatch` refusal:
    // spec 021 B-5 refuses to overwrite a client an operator widened, and a
    // lifetime rauthy defaulted is not a decision anybody made.
    let lifetime = manifest.access_token_lifetime_secs();
    let lifetime_applied =
        rahi_idp::native::apply_lifetime(&idp, &token, app_name, lifetime).await?;

    let audience = rahi_idp::Resource::derive(&idp)?.audience().to_owned();
    let native = rahi_idp::provision_native_clients(
        &idp,
        &token,
        manifest.native_clients(),
        &audience,
        lifetime,
    )
    .await?;

    Ok(Custodied {
        native,
        lifetime_applied,
    })
}

/// Re-apply the rendered API key access to rauthy's key, through the
/// backup admin's session (spec 043 D-24).
///
/// rauthy applies `BOOTSTRAP_API_KEY`'s access only when it initializes a
/// fresh database, not at every start, so a key first rendered before a
/// later spec widened [`rauthy_env::API_KEY_ACCESS`] (038 widened it with
/// `Scopes` and `Sessions`) keeps its old access on an upgraded volume, and
/// the widened calls are refused. The key cannot widen itself (rauthy asks
/// for the `ApiKeys` group, which it is never given), so the passkey-only
/// backup admin (037 B-1) logs in and sets the key's access to exactly the
/// rendered value: nothing broader than 031 D-8 allows.
///
/// # Errors
///
/// [`Error::Unauthorized`] when the key set holds no backup passkey or
/// rauthy refuses the admin; [`Error::Upstream`] when rauthy refuses the
/// update.
pub async fn reapply_api_key_access(api: &RauthyApi, keys: &KeySet) -> Result<()> {
    let token = keys.admin_token()?;
    let name = token
        .split_once('$')
        .map(|(name, _)| name.to_owned())
        .ok_or_else(|| Error::Config("the admin token is not name$secret".to_owned()))?;
    let passkey = keys.backup_passkey()?.ok_or_else(|| {
        Error::Unauthorized(format!(
            "rauthy's API key {name} lacks the access this version renders, and this key set              holds no {} to widen it with",
            crate::BACKUP_PASSKEY_FILE
        ))
    })?;
    let access: serde_json::Value = serde_json::from_str(rauthy_env::API_KEY_ACCESS)
        .map_err(|err| Error::Config(format!("the rendered API key access is not JSON: {err}")))?;
    let mut session = crate::rauthy_session::AdminSession::new(api.base())?;
    session.login(&passkey).await?;
    let body = serde_json::json!({ "name": name, "exp": null, "access": access });
    let path = format!("/auth/v1/api_keys/{name}");
    let (status, text) = session
        .call(reqwest::Method::PUT, &path, Some(&body))
        .await?;
    if !status.is_success() {
        return Err(Error::Upstream(format!(
            "rauthy answered {status} to {path}: {text}"
        )));
    }
    Ok(())
}

/// Give rauthy the refresh token lifetime the manifest names (038 B-4,
/// D-11).
///
/// rauthy takes it as a number of hours in its own configuration rather
/// than as a client field, so it is applied to the child's environment at
/// every start; a manifest whose value changed takes effect at the restart
/// that re-reads it. A manifest that declares no client using the refresh
/// grant leaves rauthy's own default alone.
pub fn apply_device_grant_lifetime(command: &mut Command, manifest: &rahi_kernel::Manifest) {
    if !rahi_idp::uses_refresh(manifest.native_clients()) {
        return;
    }
    let hours = manifest
        .native_refresh_lifetime_secs()
        .saturating_div(SECONDS_PER_HOUR)
        .max(1);
    command.env(
        rauthy_env::ENV_DEVICE_GRANT_REFRESH_HOURS,
        hours.to_string(),
    );
}

/// Seconds in the hour rauthy counts the refresh lifetime in.
const SECONDS_PER_HOUR: u64 = 3_600;

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
    terminate_observed(child, grace).await.code
}

/// How a terminated child ended (spec 043 B-10).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Terminated {
    /// Its exit code (`128 + signal` for a signal death).
    pub code: i32,
    /// Whether this process had to send SIGKILL: the witness of
    /// `rauthy_killed`.
    pub killed: bool,
    /// From SIGTERM to exit.
    pub took: Duration,
}

/// [`terminate`], saying whether the SIGKILL was needed and how long it took.
pub async fn terminate_observed(child: &mut Child, grace: Duration) -> Terminated {
    let started = tokio::time::Instant::now();
    if let Some(pid) = child.id() {
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(pid as i32),
            nix::sys::signal::Signal::SIGTERM,
        );
    }
    let (code, killed) = match tokio::time::timeout(grace, child.wait()).await {
        Ok(Ok(status)) => (exit_code(status), false),
        _ => {
            let _ = child.kill().await;
            (child.wait().await.map_or(137, exit_code), true)
        }
    };
    Terminated {
        code,
        killed,
        took: started.elapsed(),
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

/// How long serve has to finish its own shutdown once told to stop: spec
/// 043 B-9's `S + C + D + H`, forty seconds with the defaults.
pub const SERVE_GRACE: Duration = crate::stop::SERVE_GRACE;

/// How long rauthy has to exit after a propagated SIGTERM (B-3's last
/// sentence, spec 043 B-9's R). rauthy's own graceful stop finishes its open
/// connections and then its Raft node, which takes two to five seconds in
/// practice; the five seconds of [`TERM_GRACE`] belong to the serve-failure
/// path, where nothing is waiting on a clean stop. An orchestrator's
/// termination budget must cover [`SERVE_GRACE`] plus this:
/// [`crate::stop::CONTAINER_GRACE`], fifty seconds (`terminationGracePeriodSeconds:
/// 50`, `docker stop -t 50`).
pub const SHUTDOWN_TERM_GRACE: Duration = crate::stop::RAUTHY_STOP;

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
    rauthy: Command,
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
    supervise_observed(
        rauthy,
        ready,
        |stop| {
            let served = serve(stop);
            async move { (served.await, Observed::default()) }
        },
        shutdown,
        graces,
    )
    .await
    .exit
}

/// How the supervisor ended, with everything its stop observed (spec 043
/// B-10, FR-009).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Supervised {
    /// The exit code and which half ended first.
    pub exit: Exit,
    /// Every phase that ran and every reason the stop is unconfirmed.
    pub observed: Observed,
}

/// [`supervise_with`] whose `serve` reports what its own stop observed, and
/// which answers the whole observation. The exit code carries B-10's
/// outcome on every path: the first half's own non-zero code stands, and a
/// path that would otherwise exit `0` exits `3` when the stop is
/// unconfirmed (a serve overrun, a serve error, a Rauthy kill, a Rauthy
/// non-zero exit).
pub async fn supervise_observed<R, S, F, D>(
    mut rauthy: Command,
    ready: R,
    serve: S,
    shutdown: D,
    graces: Graces,
) -> Supervised
where
    R: Future<Output = Result<()>>,
    S: FnOnce(tokio::sync::oneshot::Receiver<()>) -> F,
    F: Future<Output = (Result<()>, Observed)>,
    D: Future<Output = ()>,
{
    let mut observed = Observed::default();
    let mut child = match rauthy.spawn() {
        Ok(child) => child,
        Err(err) => {
            eprintln!("supervise: rauthy cannot be spawned: {err}");
            return Supervised {
                exit: Exit {
                    code: rahi_types::error::EXIT_INFRA,
                    reason: Reason::RauthyExited,
                },
                observed,
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
            if code != 0 {
                observed.reason(StopReason::RauthyNonzero { code });
            }
            return Supervised { exit: Exit { code, reason: Reason::RauthyExited }, observed };
        }
        readiness = ready => {
            if let Err(err) = readiness {
                eprintln!("supervise: {err}");
                stop_rauthy(&mut child, term_grace, &mut observed).await;
                return Supervised {
                    exit: Exit { code: err.exit_code(), reason: Reason::RauthyUnhealthy },
                    observed,
                };
            }
        }
        () = &mut shutdown => {
            stop_rauthy(&mut child, shutdown_grace, &mut observed).await;
            let code = observed.exit_code();
            return Supervised { exit: Exit { code, reason: Reason::Shutdown }, observed };
        }
    }

    let (stop, stopped) = tokio::sync::oneshot::channel();
    let serve = serve(stopped);
    tokio::pin!(serve);
    tokio::select! {
        status = child.wait() => {
            let code = status.map_or(1, exit_code);
            eprintln!("supervise: rauthy exited with {code}; stopping serve");
            if code != 0 {
                observed.reason(StopReason::RauthyNonzero { code });
            }
            let _ = stop.send(());
            let served = tokio::time::timeout(serve_grace, &mut serve).await;
            let serve_code = absorb_serve(served, &mut observed);
            let code = if code == 0 { serve_code.unwrap_or_else(|| observed.exit_code()) } else { code };
            Supervised { exit: Exit { code, reason: Reason::RauthyExited }, observed }
        }
        served = &mut serve => {
            let serve_code = absorb_serve(Ok(served), &mut observed);
            if let Some(code) = serve_code {
                eprintln!("supervise: serve ended with {code}");
            }
            stop_rauthy(&mut child, term_grace, &mut observed).await;
            let code = serve_code.unwrap_or_else(|| observed.exit_code());
            Supervised { exit: Exit { code, reason: Reason::ServeEnded }, observed }
        }
        () = &mut shutdown => {
            // serve first, then rauthy: serve's proxy holds keep-alive
            // connections to rauthy, and rauthy's server waits for them
            // before it exits, so the other order spends the whole grace
            // and ends in a SIGKILL that leaves rauthy's lock file behind.
            let _ = stop.send(());
            let served = tokio::time::timeout(serve_grace, &mut serve).await;
            let serve_code = absorb_serve(served, &mut observed);
            stop_rauthy(&mut child, shutdown_grace, &mut observed).await;
            let code = serve_code.unwrap_or_else(|| observed.exit_code());
            Supervised { exit: Exit { code, reason: Reason::Shutdown }, observed }
        }
    }
}

/// Fold serve's end into `observed`; its own error's code, if it failed.
fn absorb_serve(
    served: std::result::Result<(Result<()>, Observed), tokio::time::error::Elapsed>,
    observed: &mut Observed,
) -> Option<i32> {
    match served {
        Err(_) => {
            eprintln!("supervise: serve did not stop within its grace");
            observed.reason(StopReason::ServeGraceOverrun);
            None
        }
        Ok((result, from_serve)) => {
            observed.absorb(from_serve);
            match result {
                Ok(()) => None,
                Err(err) => {
                    eprintln!("supervise: serve ended: {err}");
                    if !observed
                        .reasons
                        .iter()
                        .any(|r| matches!(r, StopReason::StorageTerminal))
                    {
                        observed.reason(StopReason::ServeError {
                            error: err.to_string(),
                        });
                    }
                    Some(err.exit_code())
                }
            }
        }
    }
}

/// Terminate Rauthy within `grace`, recording the phase and what it took.
async fn stop_rauthy(child: &mut Child, grace: Duration, observed: &mut Observed) {
    let ended = terminate_observed(child, grace).await;
    observed.phase("rauthy_stop", ended.took, grace);
    if ended.killed {
        eprintln!("supervise: rauthy ignored SIGTERM for {grace:?}; it was killed");
        observed.reason(StopReason::RauthyKilled);
    } else if ended.code != 0 {
        observed.reason(StopReason::RauthyNonzero { code: ended.code });
    }
}
