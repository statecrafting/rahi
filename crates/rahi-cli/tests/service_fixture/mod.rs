//! The cells spec 047's managed-service tests run (FR-001 to FR-007).
//!
//! As in spec 043's stop suite, the test binary is the fixture: a child
//! started through `stop_fixture::Node` with [`MODE_VAR`] set composes
//! [`ServiceCell`] (or, for [`MODE_PLAIN`], [`PlainCell`]) and runs the verb
//! `stop_fixture::FIXTURE_VERB` names. The mode decides what the cell's
//! services do; every service writes what it did to `service_log` through
//! the `AppState` it was handed, and [`MODE_REPORT`] reads that log back on
//! the next boot and prints it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    dead_code
)]

use std::time::Duration;

use axum::Router;
use rahi_cli::{Cell, ManagedService, ServiceShutdown};
use rahi_edge::AppState;
use rahi_store::{Migration, StoreHandle, Value};
use rahi_types::{Error, Result};

/// Set on a child to the services mode; the fixture runs only when it is.
pub const MODE_VAR: &str = "RAHI_TEST_SERVICES";

/// Two services that log `started`, wait for the stop, log `final`, and end.
pub const MODE_NORMAL: &str = "normal";
/// `alpha` returns `Ok(())` before any stop; `beta` is normal.
pub const MODE_EARLY: &str = "early";
/// `alpha` returns a named error; `beta` is normal.
pub const MODE_ERROR: &str = "error";
/// `alpha` panics; `beta` is normal.
pub const MODE_PANIC: &str = "panic";
/// `alpha` ignores the stop; `beta` is normal.
pub const MODE_HANG: &str = "hang";
/// A service with an empty name.
pub const MODE_EMPTY_NAME: &str = "empty-name";
/// Two services named `alpha`.
pub const MODE_DUPLICATE: &str = "duplicate";
/// The declaration itself returns an error.
pub const MODE_DECLARE_ERROR: &str = "declare-error";
/// One service that prints the log and waits for the stop.
pub const MODE_REPORT: &str = "report";
/// [`PlainCell`]: a cell written before spec 047, with no `services`.
pub const MODE_PLAIN: &str = "plain";

/// What `alpha`'s error says, so a test can find it in the stop record.
pub const ALPHA_ERROR: &str = "alpha could not reach its upstream";
/// What `alpha`'s panic says.
pub const ALPHA_PANIC: &str = "alpha panicked on purpose";
/// The line [`MODE_REPORT`] prints the log on.
pub const REPORT_PREFIX: &str = "SERVICE_LOG ";

/// How long a failing `alpha` waits before it fails, so `beta` has started.
const FAIL_AFTER: Duration = Duration::from_secs(1);

const MANIFEST: &str = r#"# The services cell: no capabilities; its services write their own log
# through the store handle the chassis hands them (spec 047).

schema_version = "1.0.0"

[app]
name = "services-cell"
org = "rahi-tests"

[ledger]
schema_version = "1.0.0"
max_record_bytes = 65536

[observability]
metrics_path = "/metrics"
otel = false

[auth]
operator_role = "services-cell-operator"

[contract]
version = "1.0.0"
"#;

static MIGRATIONS: std::sync::LazyLock<Vec<Migration>> = std::sync::LazyLock::new(|| {
    vec![Migration::new(
        1,
        "service_log",
        "CREATE TABLE IF NOT EXISTS service_log (\
           seq INTEGER PRIMARY KEY AUTOINCREMENT, \
           service TEXT NOT NULL, \
           event TEXT NOT NULL)",
    )]
});

/// The cell with managed services, behaving as [`MODE_VAR`] says.
pub struct ServiceCell;

impl Cell for ServiceCell {
    fn manifest() -> &'static str {
        MANIFEST
    }

    fn migrations() -> &'static [Migration] {
        &MIGRATIONS
    }

    fn routes(_state: AppState) -> Router {
        Router::new()
    }

    fn services(state: AppState, shutdown: ServiceShutdown) -> Result<Vec<ManagedService>> {
        let mode = std::env::var(MODE_VAR).unwrap_or_default();
        let store = state.store().clone();
        let beta = || cooperative("beta", store.clone(), shutdown.clone());
        let services = match mode.as_str() {
            MODE_NORMAL => vec![
                cooperative("alpha", store.clone(), shutdown.clone()),
                beta(),
            ],
            MODE_EARLY => vec![
                ManagedService::new("alpha", {
                    let store = store.clone();
                    async move {
                        log(&store, "alpha", "started").await?;
                        tokio::time::sleep(FAIL_AFTER).await;
                        Ok(())
                    }
                }),
                beta(),
            ],
            MODE_ERROR => vec![
                ManagedService::new("alpha", {
                    let store = store.clone();
                    async move {
                        log(&store, "alpha", "started").await?;
                        tokio::time::sleep(FAIL_AFTER).await;
                        Err(Error::Upstream(ALPHA_ERROR.to_owned()))
                    }
                }),
                beta(),
            ],
            MODE_PANIC => vec![
                ManagedService::new("alpha", {
                    let store = store.clone();
                    async move {
                        log(&store, "alpha", "started").await?;
                        tokio::time::sleep(FAIL_AFTER).await;
                        panic!("{ALPHA_PANIC}");
                    }
                }),
                beta(),
            ],
            MODE_HANG => vec![
                ManagedService::new("alpha", {
                    let store = store.clone();
                    async move {
                        log(&store, "alpha", "started").await?;
                        std::future::pending::<()>().await;
                        Ok(())
                    }
                }),
                beta(),
            ],
            MODE_EMPTY_NAME => vec![ManagedService::new("", async { Ok(()) })],
            MODE_DUPLICATE => vec![
                cooperative("alpha", store.clone(), shutdown.clone()),
                cooperative("alpha", store.clone(), shutdown.clone()),
            ],
            MODE_DECLARE_ERROR => {
                return Err(Error::Config(
                    "the services cell refuses to declare its services".to_owned(),
                ));
            }
            MODE_REPORT => vec![ManagedService::new("reporter", async move {
                let rows: Vec<(String, String)> = store
                    .query(
                        "SELECT service, event FROM service_log ORDER BY seq",
                        vec![],
                    )
                    .await?;
                println!(
                    "{REPORT_PREFIX}{}",
                    serde_json::to_string(&rows).expect("rows encode")
                );
                shutdown.cancelled().await;
                Ok(())
            })],
            other => panic!("unknown {MODE_VAR} {other:?}"),
        };
        Ok(services)
    }
}

/// A service that does what a well-behaved worker does: it logs that it
/// started, waits for the stop, finishes its last write, and ends.
fn cooperative(
    name: &'static str,
    store: StoreHandle,
    shutdown: ServiceShutdown,
) -> ManagedService {
    ManagedService::new(name, async move {
        log(&store, name, "started").await?;
        shutdown.cancelled().await;
        assert!(shutdown.is_cancelled(), "the broadcast is observable");
        log(&store, name, "final").await
    })
}

async fn log(store: &StoreHandle, service: &str, event: &str) -> Result<()> {
    store
        .execute(
            "INSERT INTO service_log (service, event) VALUES ($1, $2)",
            vec![Value::from(service), Value::from(event)],
        )
        .await
        .map(|_| ())
}

/// A cell written against the `Cell` of before spec 047: it overrides
/// nothing it did not know about (FR-007).
pub struct PlainCell;

impl Cell for PlainCell {
    fn manifest() -> &'static str {
        MANIFEST
    }

    fn migrations() -> &'static [Migration] {
        &MIGRATIONS
    }

    fn routes(_state: AppState) -> Router {
        Router::new()
    }
}

/// Whether this process was started as a services fixture.
pub fn requested() -> bool {
    std::env::var(MODE_VAR).is_ok()
}

/// The fixture's process entry: run the verb against the mode's cell.
pub fn fixture_main() {
    let Ok(verb) = std::env::var(crate::stop_fixture::FIXTURE_VERB) else {
        return;
    };
    let args: Vec<String> = verb.split_whitespace().map(str::to_owned).collect();
    let env = rahi_cli::process_env();
    let code = if std::env::var(MODE_VAR).as_deref() == Ok(MODE_PLAIN) {
        rahi_cli::run_with::<PlainCell>(&args, &env)
    } else {
        rahi_cli::run_with::<ServiceCell>(&args, &env)
    };
    std::process::exit(code);
}

/// The log a [`MODE_REPORT`] boot printed, as `(service, event)` rows.
pub fn report(stdout: &str) -> Vec<(String, String)> {
    let line = stdout
        .lines()
        .find_map(|line| line.strip_prefix(REPORT_PREFIX))
        .unwrap_or_else(|| panic!("no {REPORT_PREFIX:?} line in\n{stdout}"));
    serde_json::from_str(line).unwrap()
}

/// Every `hiqlite-owner.lock` under `dir`.
pub fn owner_locks(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    fn walk(dir: &std::path::Path, into: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, into);
            } else if path.file_name().is_some_and(|n| n == "hiqlite-owner.lock") {
                into.push(path);
            }
        }
    }
    let mut found = Vec::new();
    walk(dir, &mut found);
    found
}

/// Whether every owner lock under `dir` can be taken: no process, and no
/// task of an exited one, still holds the store.
pub fn locks_released(dir: &std::path::Path) -> bool {
    let locks = owner_locks(dir);
    !locks.is_empty()
        && locks.iter().all(|path| {
            std::fs::File::open(path).is_ok_and(|file| {
                let free = file.try_lock().is_ok();
                let _ = file.unlock();
                free
            })
        })
}
