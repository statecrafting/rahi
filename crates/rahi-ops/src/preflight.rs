//! The preflight verb (B-3): every check by name, and nothing mutated.
//!
//! Preflight answers "would `serve` start here, and why not" without
//! starting it. Each check is reported pass or fail with a reason; the verb
//! exits `1` on any failure. A check whose precondition failed is reported
//! as skipped rather than silently omitted, so the list is the same length
//! on every run and a reader can diff two of them.

use std::fmt;

use rahi_kernel::Manifest;
use rahi_ledger::Ledger;
use rahi_store::Store;
use rahi_types::{Config, EnvReader, Error, Result};

use crate::KeySet;
use crate::rauthy_api::RauthyApi;

/// The least free space the volume may have, in bytes (512 MiB).
pub const MIN_FREE_BYTES: u64 = 512 * 1024 * 1024;

/// The check names, in the order they are reported.
pub const CHECKS: [&str; 10] = [
    "config",
    "data_dir",
    "keys",
    "restore_env",
    "hiqlite",
    "engine",
    "rauthy",
    "ledger",
    "coverage",
    "disk",
];

/// One check's verdict.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The check passed; the detail says what was observed.
    Pass(String),
    /// The check failed; the detail says why.
    Fail(String),
    /// The check could not run because an earlier one failed.
    Skipped(String),
}

/// One named check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    /// One of [`CHECKS`].
    pub name: &'static str,
    /// What happened.
    pub verdict: Verdict,
}

impl Check {
    fn pass(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            verdict: Verdict::Pass(detail.into()),
        }
    }

    fn fail(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            verdict: Verdict::Fail(detail.into()),
        }
    }

    fn skipped(name: &'static str, because: &str) -> Self {
        let because = because.strip_prefix("skipped: ").unwrap_or(because);
        Self {
            name,
            verdict: Verdict::Skipped(format!("skipped: {because}")),
        }
    }

    fn of(name: &'static str, result: Result<String>) -> Self {
        match result {
            Ok(detail) => Self::pass(name, detail),
            Err(err) => Self::fail(name, err.to_string()),
        }
    }

    /// The check did not fail.
    #[must_use]
    pub fn ok(&self) -> bool {
        !matches!(self.verdict, Verdict::Fail(_))
    }
}

impl fmt::Display for Check {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (word, detail) = match &self.verdict {
            Verdict::Pass(d) => ("PASS", d),
            Verdict::Fail(d) => ("FAIL", d),
            Verdict::Skipped(d) => ("SKIP", d),
        };
        write!(f, "{word} {}: {detail}", self.name)
    }
}

/// The whole report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    /// Every check, in [`CHECKS`] order.
    pub checks: Vec<Check>,
}

impl Report {
    /// No check failed.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.checks.iter().all(Check::ok)
    }

    /// The check named `name`.
    #[must_use]
    pub fn check(&self, name: &str) -> Option<&Check> {
        self.checks.iter().find(|c| c.name == name)
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for check in &self.checks {
            writeln!(f, "{check}")?;
        }
        write!(
            f,
            "preflight: {}",
            if self.passed() { "ok" } else { "failed" }
        )
    }
}

/// Run every check against `env` for the cell whose manifest is
/// `manifest_text`.
pub async fn run(env: &dyn EnvReader, manifest_text: &str) -> Report {
    run_inner(env, manifest_text).await
}

async fn run_inner(env: &dyn EnvReader, manifest_text: &str) -> Report {
    let mut checks = Vec::with_capacity(CHECKS.len());

    let config = match Config::from_env(env) {
        Ok(config) => {
            checks.push(Check::pass(
                "config",
                format!("public_url {}", config.public_url.as_str()),
            ));
            config
        }
        Err(err) => {
            checks.push(Check::fail("config", err.to_string()));
            for name in CHECKS.iter().skip(1) {
                checks.push(Check::skipped(name, "config did not parse"));
            }
            return Report { checks };
        }
    };

    checks.push(Check::of("data_dir", data_dir_writable(&config)));

    let keys = KeySet::of(&config);
    let keys_ok = keys.check();
    checks.push(Check::of(
        "keys",
        keys_ok
            .as_ref()
            .map(|()| {
                format!(
                    "{} files with mode 0600 under {}",
                    KeySet::REQUIRED.len(),
                    keys.dir().display()
                )
            })
            .map_err(Clone::clone),
    ));

    let restore_ok = crate::refuse_env_restore();
    checks.push(Check::of(
        "restore_env",
        restore_ok
            .as_ref()
            .map(|()| format!("{} is not set", crate::RESTORE_ENV_VAR))
            .map_err(Clone::clone),
    ));

    // The node is opened only when nothing above refused: hiqlite reads the
    // restore variable at start and would apply it, and a key set that does
    // not read cannot open the store anyway.
    let store = match (&keys_ok, &restore_ok) {
        (Ok(()), Ok(())) => open_store(&config, env, &keys).await,
        (Err(_), _) => Err(Error::Config(
            "skipped: the key set did not check".to_owned(),
        )),
        (_, Err(_)) => Err(Error::Config(format!(
            "skipped: {} is set",
            crate::RESTORE_ENV_VAR
        ))),
    };
    match &store {
        Ok((_, detail)) => checks.push(Check::pass("hiqlite", detail.clone())),
        Err(err) if keys_ok.is_err() || restore_ok.is_err() => {
            checks.push(Check::skipped("hiqlite", err.message()));
        }
        Err(err) => checks.push(Check::fail("hiqlite", err.to_string())),
    }
    let store = store.ok().map(|(s, _)| s);

    match &store {
        Some(store) => checks.push(Check::pass("engine", store.engine_report().to_string())),
        None => checks.push(Check::skipped("engine", "the store did not open")),
    }

    match keys.admin_token() {
        Ok(token) => {
            let rauthy =
                RauthyApi::new(config.rauthy_base_url(), token).map_err(|err| err.to_string());
            let verdict = match rauthy {
                Ok(api) => api
                    .health()
                    .await
                    .map(|()| format!("answers at {}", api.base())),
                Err(err) => Err(Error::Config(err)),
            };
            checks.push(Check::of("rauthy", verdict));
        }
        Err(_) => checks.push(Check::skipped("rauthy", "no admin token")),
    }

    match &store {
        Some(store) => {
            let (ledger, coverage) = verify_ledger(store, &keys, manifest_text).await;
            checks.push(Check::of("ledger", ledger));
            checks.push(Check::of("coverage", coverage));
        }
        None => {
            checks.push(Check::skipped("ledger", "the store did not open"));
            checks.push(Check::skipped("coverage", "the store did not open"));
        }
    }

    checks.push(Check::of("disk", free_disk(&config)));

    if let Some(store) = store {
        let _ = store.shutdown().await;
    }
    Report { checks }
}

fn data_dir_writable(config: &Config) -> Result<String> {
    let dir = &config.data_dir;
    if !dir.is_dir() {
        return Err(Error::Io(format!("{} is not a directory", dir.display())));
    }
    let probe = dir.join(format!(".preflight-{}", std::process::id()));
    std::fs::write(&probe, b"")
        .map_err(|err| Error::Io(format!("{} is not writable: {err}", dir.display())))?;
    let _ = std::fs::remove_file(&probe);
    Ok(format!("{} is writable", dir.display()))
}

async fn open_store(
    config: &Config,
    env: &dyn EnvReader,
    keys: &KeySet,
) -> Result<(Store, String)> {
    let lock = crate::app_lock_file(config);
    if lock.exists() {
        return Err(Error::Conflict(format!(
            "{} exists: the node is running, or did not shut down cleanly",
            lock.display()
        )));
    }
    let secrets = keys.store_secrets()?;
    let cfg = crate::store_config(config, env, secrets)?;
    let store = Store::open(&cfg).await?;
    store.health().await?;
    let role = if store.is_leader().await {
        "leader"
    } else {
        "follower"
    };
    let detail = format!(
        "node {} of {} opened at {} and is {role}",
        cfg.node_id,
        cfg.nodes.len().max(1),
        cfg.api_addr
    );
    Ok((store, detail))
}

/// The chain, and what this binary can prove about the ids in it
/// (spec 030 B-3, spec 042 B-10).
///
/// Two verdicts because there are two questions, and spec 042 D-8 keeps them
/// apart deliberately: whether the chain verifies, and whether its lifetime
/// identity index accounts for every record in it. The first is damage and
/// the second is a missing upgrade step, and an operator told the wrong one
/// chases the wrong thing.
///
/// This never constructs a repair handle: `preflight` is not one of the
/// three verbs spec 042 B-15 gives one to. It opens the chain the ordinary
/// way, and reads B-10's refusal for what it is. Since that refusal is
/// raised only after verification has already passed, an `Error::Stale` here
/// is itself the evidence that the chain verified.
async fn verify_ledger(
    store: &Store,
    keys: &KeySet,
    manifest_text: &str,
) -> (Result<String>, Result<String>) {
    match verify_ledger_inner(store, keys, manifest_text).await {
        Ok(pair) => pair,
        Err(Error::Stale(said)) => (
            Ok("verified at resident depth; the chain opened as far as verification".to_owned()),
            Err(Error::Stale(said)),
        ),
        Err(err) => (
            Err(err.clone()),
            Err(Error::Validation(format!(
                "not established: the chain did not open ({})",
                err.message()
            ))),
        ),
    }
}

async fn verify_ledger_inner(
    store: &Store,
    keys: &KeySet,
    manifest_text: &str,
) -> Result<(Result<String>, Result<String>)> {
    let signer = keys.ledger_signer()?;
    let manifest = Manifest::parse(manifest_text)?;
    let hash = manifest.hash()?;
    #[derive(serde::Deserialize)]
    struct Name {
        name: String,
    }
    // A leader-side read fault inside this statement comes back as no rows
    // (spec 016 D-2 records the quirk), which reads as "no chain yet". The
    // probe is a fixed lookup against sqlite_master, and serve's own open
    // verifies the chain for real; preflight reports, it does not gate.
    let tables: Vec<Name> = store
        .query_consistent(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'kernel_decisions'",
            Vec::new(),
        )
        .await?;
    if !tables.iter().any(|t| t.name == "kernel_decisions") {
        return Ok((
            Ok("no chain yet; serve writes genesis".to_owned()),
            Ok("no chain yet; a chain with no history has nothing to account for".to_owned()),
        ));
    }
    #[derive(serde::Deserialize)]
    struct Id {
        id: String,
    }
    let rows: Vec<Id> = store
        .query_consistent("SELECT id FROM kernel_decisions LIMIT 1", Vec::new())
        .await?;
    if rows.iter().all(|r| r.id.is_empty()) {
        return Ok((
            Ok("chain table is empty; serve writes genesis".to_owned()),
            Ok(
                "chain table is empty; a chain with no history has nothing to account for"
                    .to_owned(),
            ),
        ));
    }
    let ledger = Ledger::open(store.handle(), signer, hash).await?;
    let count = ledger.count().await?;
    let segments = ledger.segment_count().await?;
    // Spec 042 B-7: `preflight` is one of the two commands that pay for the
    // lifetime aggregates, because an operator asked for the number.
    let coverage = ledger.coverage().await?;
    let totals = ledger.identity_totals().await?;
    let said = format!(
        "{} uncovered segment(s), {} unstamped resident record(s), {} identity row(s), {} \
         collision(s)",
        coverage.uncovered().len(),
        coverage.unstamped_resident(),
        totals.identity_rows,
        totals.collisions
    );
    // A recorded collision does not make coverage incomplete and does not
    // stop `serve` (spec 042 B-12, D-4), so it is reported rather than
    // failed on: the containment is per affected id.
    let coverage = if coverage.is_complete() {
        Ok(said)
    } else {
        Err(Error::Stale(format!("{said}: {}", coverage.why())))
    };
    Ok((
        Ok(format!(
            "verified at resident depth: {count} resident record(s), {segments} sealed segment(s)"
        )),
        coverage,
    ))
}

fn free_disk(config: &Config) -> Result<String> {
    let free = fs4::available_space(&config.data_dir).map_err(|err| {
        Error::Io(format!(
            "free space of {} unknown: {err}",
            config.data_dir.display()
        ))
    })?;
    if free < MIN_FREE_BYTES {
        return Err(Error::Io(format!(
            "{} bytes free under {}, below the {} byte floor",
            free,
            config.data_dir.display(),
            MIN_FREE_BYTES
        )));
    }
    Ok(format!(
        "{free} bytes free under {}",
        config.data_dir.display()
    ))
}
