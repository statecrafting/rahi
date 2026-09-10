//! Boot a built cell binary (spec 033 B-2, B-4).
//!
//! The sequence mirrors the container's entrypoint (spec 031 B-3):
//! `first-boot` under a throwaway data directory, `migrate`, then either
//! `supervise` with a rauthy binary or `serve` alone with
//! `RAHI_RAUTHY_MODE=none`. Every port is allocated by the OS, the child
//! inherits nothing but `PATH` and `HOME`, and boot returns only after
//! `/readyz` answers `200`.

use std::fs::File;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::{Client, Error, Result};

/// How long `/readyz` may take: ninety seconds for the two elections (B-2).
pub const READY_BUDGET: Duration = Duration::from_secs(90);

/// How often the harness polls while waiting.
pub const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// How long a stopped child gets between SIGTERM and SIGKILL.
pub const STOP_GRACE: Duration = Duration::from_secs(5);

/// The file under the data directory where `first-boot` printed once.
pub const FIRST_BOOT_LOG: &str = "first-boot.log";

/// Whether the instance mounts an identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RauthyMode {
    /// Spawn `supervise` with this rauthy binary (`RAHI_RAUTHY_BIN`).
    External(PathBuf),
    /// Spawn `serve` directly with `RAHI_RAUTHY_MODE=none`; login tests
    /// skip, and [`Instance::login_as`] is [`Error::NoRauthy`].
    None,
}

/// What to boot.
#[derive(Clone, Debug)]
pub struct BootSpec {
    /// The built cell binary.
    pub binary: PathBuf,
    /// The child's working directory, for a cell that resolves files
    /// relative to it. `None` is the data directory. The manifest itself
    /// is compiled into the binary (spec 030), so this is not where it is
    /// read from (D-1).
    pub manifest_dir: Option<PathBuf>,
    /// Whether rauthy runs.
    pub rauthy: RauthyMode,
    /// Extra `RAHI_*` variables, applied after the harness's own, for a
    /// test that needs a knob the harness does not set (an OTLP endpoint,
    /// a deliberately wrong value). Explicit, never inherited (B-4).
    pub env: Vec<(String, String)>,
}

impl BootSpec {
    /// A spec for `binary` with no rauthy and no extra environment.
    #[must_use]
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
            manifest_dir: None,
            rauthy: RauthyMode::None,
            env: Vec::new(),
        }
    }

    /// With rauthy at `binary`.
    #[must_use]
    pub fn with_rauthy(mut self, binary: impl Into<PathBuf>) -> Self {
        self.rauthy = RauthyMode::External(binary.into());
        self
    }

    /// With one more variable.
    #[must_use]
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }
}

/// The entry point.
pub struct Harness;

/// The ports one instance owns.
#[derive(Clone, Copy, Debug)]
pub struct Ports {
    /// The app's listener, which is also the public origin.
    pub app: u16,
    /// The app's hiqlite API.
    pub hiqlite_api: u16,
    /// The app's hiqlite Raft.
    pub hiqlite_raft: u16,
    /// rauthy's HTTP listener on loopback.
    pub rauthy: u16,
    /// rauthy's hiqlite Raft.
    pub rauthy_raft: u16,
    /// rauthy's hiqlite API.
    pub rauthy_api: u16,
}

impl Ports {
    fn allocate() -> Result<Self> {
        Ok(Self {
            app: free_port()?,
            hiqlite_api: free_port()?,
            hiqlite_raft: free_port()?,
            rauthy: free_port()?,
            rauthy_raft: free_port()?,
            rauthy_api: free_port()?,
        })
    }
}

/// A port the OS had free a moment ago.
fn free_port() -> Result<u16> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    Ok(listener.local_addr()?.port())
}

/// A booted cell.
///
/// `Send + Sync`: share one per test file through a `OnceLock` (B-5). The
/// child is stopped when the instance is dropped.
pub struct Instance {
    base_url: String,
    admin_token: String,
    ports: Ports,
    rauthy: RauthyMode,
    dir: tempfile::TempDir,
    child: Mutex<Option<Child>>,
    stderr_path: PathBuf,
    stdout_path: PathBuf,
    /// When `/healthz` first answered, and when `/readyz` first did.
    probe_order: (Duration, Duration),
}

impl std::fmt::Debug for Instance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Instance")
            .field("base_url", &self.base_url)
            .field("data_dir", &self.data_dir())
            .field("rauthy", &self.rauthy)
            .finish_non_exhaustive()
    }
}

/// How a stopped child ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stopped {
    /// It exited on SIGTERM with this code (`None` when a signal ended it).
    Exited(Option<i32>),
    /// It ignored SIGTERM for [`STOP_GRACE`] and was killed.
    Killed,
    /// It had already exited before `stop` was called.
    AlreadyGone(Option<i32>),
}

impl Harness {
    /// Boot `spec` and wait for `/readyz` (B-2).
    ///
    /// Synchronous on purpose: a `OnceLock` initialiser is. Callable from
    /// inside a tokio runtime too; the readiness poll runs on its own
    /// thread with its own runtime.
    ///
    /// # Errors
    ///
    /// [`Error::Boot`] when the binary cannot run or `first-boot` or
    /// `migrate` fails, with that verb's stderr; [`Error::NotReady`] when
    /// the child exits or `/readyz` does not answer `200` inside
    /// [`READY_BUDGET`], with the child's stderr tail.
    pub fn boot(spec: BootSpec) -> Result<Instance> {
        let dir = tempfile::Builder::new().prefix("rahi-harness-").tempdir()?;
        let data_dir = dir.path().join("data");
        std::fs::create_dir_all(&data_dir)?;
        let ports = Ports::allocate()?;
        // `localhost`, not the address: rauthy derives its WebAuthn relying
        // party id from the public host and refuses an IP literal (D-3).
        let base_url = format!("http://localhost:{}", ports.app);
        let env = environment(&spec, &data_dir, ports, &base_url);
        let cwd = spec
            .manifest_dir
            .clone()
            .unwrap_or_else(|| data_dir.clone());

        // first-boot, then migrate, as the entrypoint does (D-2).
        let first_boot = run_verb(&spec.binary, "first-boot", &env, &cwd)?;
        std::fs::write(data_dir.join(FIRST_BOOT_LOG), &first_boot)?;
        run_verb(&spec.binary, "migrate", &env, &cwd)?;

        let admin_token = std::fs::read_to_string(data_dir.join("keys").join("rauthy_admin_token"))
            .map(|t| t.trim().to_owned())
            .map_err(|err| Error::Boot(format!("the admin token was not written: {err}")))?;

        let verb = match spec.rauthy {
            RauthyMode::External(_) => "supervise",
            RauthyMode::None => "serve",
        };
        let stdout_path = dir.path().join(format!("{verb}.stdout"));
        let stderr_path = dir.path().join(format!("{verb}.stderr"));
        let mut command = Command::new(&spec.binary);
        command
            .arg(verb)
            .current_dir(&cwd)
            .env_clear()
            .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(Stdio::null())
            .stdout(File::create(&stdout_path)?)
            .stderr(File::create(&stderr_path)?);
        let child = command.spawn().map_err(|err| {
            Error::Boot(format!(
                "{} {verb} cannot be spawned: {err}",
                spec.binary.display()
            ))
        })?;

        let mut instance = Instance {
            base_url,
            admin_token,
            ports,
            rauthy: spec.rauthy,
            dir,
            child: Mutex::new(Some(child)),
            stderr_path,
            stdout_path,
            probe_order: (Duration::ZERO, Duration::ZERO),
        };
        match wait_ready(&instance) {
            Ok(order) => {
                instance.probe_order = order;
                Ok(instance)
            }
            Err(err) => {
                let _ = instance.stop();
                Err(err)
            }
        }
    }
}

/// The child's whole environment (B-4): `PATH` and `HOME` from the runner,
/// every `RAHI_*` the cell needs set explicitly, then the spec's own.
fn environment(
    spec: &BootSpec,
    data_dir: &Path,
    ports: Ports,
    base_url: &str,
) -> Vec<(String, String)> {
    let lo = |port: u16| SocketAddr::from((Ipv4Addr::LOCALHOST, port)).to_string();
    let mut env: Vec<(String, String)> = ["PATH", "HOME"]
        .iter()
        .filter_map(|k| std::env::var(k).ok().map(|v| ((*k).to_owned(), v)))
        .collect();
    env.extend(
        [
            ("RAHI_PUBLIC_URL", base_url.to_owned()),
            ("RAHI_LISTEN_ADDR", lo(ports.app)),
            ("RAHI_DATA_DIR", data_dir.display().to_string()),
            ("RAHI_HIQLITE_API_ADDR", lo(ports.hiqlite_api)),
            ("RAHI_HIQLITE_RAFT_ADDR", lo(ports.hiqlite_raft)),
            ("RAHI_RAUTHY_ADDR", lo(ports.rauthy)),
            ("RAHI_RAUTHY_HQL_RAFT_PORT", ports.rauthy_raft.to_string()),
            ("RAHI_RAUTHY_HQL_API_PORT", ports.rauthy_api.to_string()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v)),
    );
    match &spec.rauthy {
        RauthyMode::External(bin) => {
            env.push(("RAHI_RAUTHY_MODE".to_owned(), "required".to_owned()));
            env.push(("RAHI_RAUTHY_BIN".to_owned(), bin.display().to_string()));
        }
        RauthyMode::None => env.push(("RAHI_RAUTHY_MODE".to_owned(), "none".to_owned())),
    }
    for (k, v) in &spec.env {
        env.retain(|(key, _)| key != k);
        env.push((k.clone(), v.clone()));
    }
    env
}

/// Run one verb to completion; its stdout on success, its stderr in the
/// error otherwise.
fn run_verb(binary: &Path, verb: &str, env: &[(String, String)], cwd: &Path) -> Result<String> {
    let output = Command::new(binary)
        .arg(verb)
        .current_dir(cwd)
        .env_clear()
        .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .stdin(Stdio::null())
        .output()
        .map_err(|err| Error::Boot(format!("{} {verb} cannot run: {err}", binary.display())))?;
    if !output.status.success() {
        return Err(Error::Boot(format!(
            "{verb} exited {}: {}",
            output
                .status
                .code()
                .map_or("by signal".to_owned(), |c| c.to_string()),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Poll `/healthz` and `/readyz` until ready or out of budget. Returns
/// when each first answered, measured from the start of the wait.
///
/// Runs on a thread of its own with its own runtime, so `boot` is callable
/// from a plain test and from inside a tokio runtime alike.
fn wait_ready(instance: &Instance) -> Result<(Duration, Duration)> {
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?;
                runtime.block_on(poll_ready(instance))
            })
            .join()
            .map_err(|_| Error::Io("the readiness thread panicked".to_owned()))?
    })
}

async fn poll_ready(instance: &Instance) -> Result<(Duration, Duration)> {
    let base = instance.base_url.clone();
    let healthz = format!("{base}/healthz");
    let readyz = format!("{base}/readyz");
    let start = Instant::now();
    let deadline = start + READY_BUDGET;
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()?;
    let mut healthz_at: Option<Duration> = None;
    loop {
        if let Some(status) = instance.try_wait()? {
            return Err(Error::NotReady(format!(
                "the cell exited {} before /readyz answered; stderr:\n{}",
                status
                    .code()
                    .map_or("by signal".to_owned(), |c| c.to_string()),
                instance.stderr_tail(40)
            )));
        }
        if Instant::now() >= deadline {
            return Err(Error::NotReady(format!(
                "/readyz did not answer 200 within {}s; stderr:\n{}",
                READY_BUDGET.as_secs(),
                instance.stderr_tail(40)
            )));
        }
        if healthz_at.is_none() && probe(&http, &healthz).await == Some(200) {
            healthz_at = Some(start.elapsed());
        }
        if let Some(at) = healthz_at
            && probe(&http, &readyz).await == Some(200)
        {
            return Ok((at, start.elapsed()));
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

async fn probe(http: &reqwest::Client, url: &str) -> Option<u16> {
    http.get(url).send().await.ok().map(|r| r.status().as_u16())
}

impl Instance {
    /// The public origin, `http://localhost:<port>`; the listener is on
    /// `127.0.0.1` at that port.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// rauthy's bootstrap API token, from the key set `first-boot` wrote.
    #[must_use]
    pub fn admin_token(&self) -> &str {
        &self.admin_token
    }

    /// The throwaway data directory (`/data` of this cell).
    #[must_use]
    pub fn data_dir(&self) -> PathBuf {
        self.dir.path().join("data")
    }

    /// The ports this instance owns.
    #[must_use]
    pub fn ports(&self) -> Ports {
        self.ports
    }

    /// Whether rauthy is part of this instance.
    #[must_use]
    pub fn has_rauthy(&self) -> bool {
        matches!(self.rauthy, RauthyMode::External(_))
    }

    /// rauthy's loopback base, `http://127.0.0.1:<port>`, whether or not
    /// it runs.
    #[must_use]
    pub fn rauthy_loopback(&self) -> String {
        format!("http://127.0.0.1:{}", self.ports.rauthy)
    }

    /// When `/healthz` first answered `200` and when `/readyz` did, from
    /// the start of the wait. Liveness is never later than readiness.
    #[must_use]
    pub fn probe_order(&self) -> (Duration, Duration) {
        self.probe_order
    }

    /// What `first-boot` printed: the admin credentials, once.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the log cannot be read.
    pub fn first_boot_output(&self) -> Result<String> {
        Ok(std::fs::read_to_string(
            self.data_dir().join(FIRST_BOOT_LOG),
        )?)
    }

    /// The child's process id, while it runs.
    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        self.child
            .lock()
            .ok()
            .and_then(|guard| guard.as_ref().map(Child::id))
    }

    /// The last `lines` of the child's stderr.
    #[must_use]
    pub fn stderr_tail(&self, lines: usize) -> String {
        tail(&self.stderr_path, lines)
    }

    /// The last `lines` of the child's stdout.
    #[must_use]
    pub fn stdout_tail(&self, lines: usize) -> String {
        tail(&self.stdout_path, lines)
    }

    /// A fresh client with an empty cookie jar (B-3).
    #[must_use]
    pub fn client(&self) -> Client {
        Client::new(&self.base_url)
    }

    /// Create `user` in rauthy and log the client in as them through the
    /// cell's own `/login` (B-3).
    ///
    /// # Errors
    ///
    /// [`Error::NoRauthy`] without an identity; [`Error::Rauthy`] when the
    /// flow does not complete.
    pub async fn login_as(&self, client: &Client, user: &crate::User) -> Result<()> {
        if !self.has_rauthy() {
            return Err(Error::NoRauthy);
        }
        let rauthy = crate::Rauthy::new(&self.rauthy_loopback(), &self.admin_token);
        rauthy.ensure_user(user).await?;
        crate::rauthy::login(client, &self.base_url, user).await
    }

    fn try_wait(&self) -> Result<Option<std::process::ExitStatus>> {
        let mut guard = self
            .child
            .lock()
            .map_err(|_| Error::Io("the child lock is poisoned".to_owned()))?;
        match guard.as_mut() {
            Some(child) => Ok(child.try_wait()?),
            None => Ok(None),
        }
    }

    /// Stop the cell: SIGTERM, [`STOP_GRACE`], then SIGKILL. Idempotent.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the signal cannot be sent or the wait fails.
    pub fn stop(&self) -> Result<Stopped> {
        let mut guard = self
            .child
            .lock()
            .map_err(|_| Error::Io("the child lock is poisoned".to_owned()))?;
        let Some(mut child) = guard.take() else {
            return Ok(Stopped::AlreadyGone(None));
        };
        if let Some(status) = child.try_wait()? {
            return Ok(Stopped::AlreadyGone(status.code()));
        }
        terminate(child.id())?;
        let deadline = Instant::now() + STOP_GRACE;
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok(Stopped::Exited(status.code()));
            }
            if Instant::now() >= deadline {
                child.kill()?;
                child.wait()?;
                return Ok(Stopped::Killed);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// SIGTERM `pid` through the `kill` utility, so the crate needs no libc.
fn terminate(pid: u32) -> Result<()> {
    let status = Command::new("kill")
        .arg("-TERM")
        .arg(pid.to_string())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::Io(format!("kill -TERM {pid} exited {status}")))
    }
}

fn tail(path: &Path, lines: usize) -> String {
    let Ok(mut file) = File::open(path) else {
        return String::new();
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let window = 64 * 1024;
    let start = len.saturating_sub(window);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    // Raw bytes, decoded leniently: the window may open inside a multi-byte
    // character, and a tail that vanished on that account would be worse
    // than one with a replacement character at its head.
    let mut bytes = Vec::new();
    let _ = file.read_to_end(&mut bytes);
    let buf = String::from_utf8_lossy(&bytes);
    let all: Vec<&str> = buf.lines().collect();
    let from = all.len().saturating_sub(lines);
    all.get(from..).unwrap_or(&[]).join("\n")
}
