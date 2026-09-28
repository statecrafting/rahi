//! Spec 048 FR-001 to FR-004: a program that opens the node through
//! `Store::run` leaves a data directory the next start opens, however the
//! program ends, and the pattern it replaces does not.
//!
//! Every program is a real process: the test binary is its own child. A
//! child started on [`CHILD`] with [`PHASE`] set runs one program against
//! the directory and ports the parent hands it, and exits with its code. The
//! parent then inspects the directory and starts a reader, a separate
//! process again, because the claim is about the next start.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod common;

use std::io::Read as _;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rahi_store::{STOP_GRACE, Stopping, Store, StoreConfig, StoreHandle};
use rahi_types::Error;
use serde::Deserialize;

/// The libtest name of the child's entry.
const CHILD: &str = "child";
/// Set on a child to the program it runs.
const PHASE: &str = "RAHI_TEST_LIFECYCLE_PHASE";
const DIR: &str = "RAHI_TEST_LIFECYCLE_DIR";
const RAFT: &str = "RAHI_TEST_LIFECYCLE_RAFT";
const API: &str = "RAHI_TEST_LIFECYCLE_API";

/// Printed by a child once its body is running.
const READY: &str = "@@ READY";

/// What a start that lost its port to another process on the machine says.
/// Such a start opened nothing and wrote nothing, so it is started again
/// rather than failing a test about shutdowns (spec 048 D-9).
const PORT_TAKEN: &str = "Address already in use";
const START_ATTEMPTS: usize = 5;

/// Far above a start, the grace, and a shutdown, so a hang fails here.
const EXIT_BUDGET: Duration = Duration::from_secs(90);

// ------------------------------------------------------------------ child

#[test]
fn child() {
    let Ok(phase) = std::env::var(PHASE) else {
        return;
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let code = runtime.block_on(program(&phase, &child_config()));
    std::process::exit(code);
}

/// The parent's directory and ports. Built here rather than by
/// `common::config`, which would claim two more ports for nothing.
fn child_config() -> StoreConfig {
    StoreConfig {
        node_id: 1,
        nodes: Vec::new(),
        data_dir: PathBuf::from(std::env::var(DIR).unwrap()),
        raft_addr: std::env::var(RAFT).unwrap().parse().unwrap(),
        api_addr: std::env::var(API).unwrap().parse().unwrap(),
        secrets: common::secrets(),
        backup_keep_days: 1,
        s3: None,
    }
}

async fn mark(store: &StoreHandle, name: &str) -> Result<(), Error> {
    store
        .execute(
            "CREATE TABLE IF NOT EXISTS marks (name TEXT NOT NULL)",
            vec![],
        )
        .await?;
    store
        .execute("INSERT INTO marks (name) VALUES ($1)", vec![name.into()])
        .await?;
    Ok(())
}

fn ready() {
    println!("{READY}");
}

fn exit_code(result: Result<(), Error>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("@@ ERROR {err}");
            err.exit_code()
        }
    }
}

/// The pattern spec 048 replaces, as the travel-memory service wrote it: a
/// `?` between open and shutdown.
async fn bare(cfg: &StoreConfig) -> Result<(), Error> {
    let store = Store::open(cfg).await?;
    mark(&store, "bare").await?;
    Err(Error::Validation(
        "the program failed after open".to_owned(),
    ))?;
    store.shutdown().await
}

async fn program(phase: &str, cfg: &StoreConfig) -> i32 {
    match phase {
        "error-after-open" => exit_code(
            Store::run(cfg, |store, _| async move {
                mark(&store, "before-error").await?;
                Err(Error::Validation(
                    "the program failed after open".to_owned(),
                ))
            })
            .await,
        ),
        "panic-after-open" => exit_code(
            Store::run(cfg, |store, _| async move {
                mark(&store, "before-panic").await?;
                panic!("the program panicked after open");
            })
            .await,
        ),
        "serve-until-stopped" => exit_code(
            Store::run(cfg, |store, stopping| async move {
                mark(&store, "before-stop").await?;
                ready();
                stopping.requested().await;
                mark(&store, "after-stop").await
            })
            .await,
        ),
        "ignore-stop" => exit_code(
            Store::run(cfg, |store, _| async move {
                mark(&store, "ignoring").await?;
                ready();
                std::future::pending::<Result<(), Error>>().await
            })
            .await,
        ),
        "stop-during-open" => {
            // Requested before the node has started: the body must never
            // run, and the node must still be shut down.
            let stopping = Stopping::on(async {});
            exit_code(
                Store::run_until(cfg, stopping, |_, _| async {
                    panic!("the body ran after a stop that preceded it")
                })
                .await,
            )
        }
        "bare" => exit_code(bare(cfg).await),
        "unarmed" => {
            // No handler: SIGTERM's default action ends the process with the
            // node open, as a program that only listens for Ctrl-C does.
            let store = Store::open(cfg).await.unwrap();
            mark(&store, "unarmed").await.unwrap();
            ready();
            std::future::pending::<()>().await;
            0
        }
        "read" => {
            let read = Store::run_until(cfg, Stopping::never(), |store, _| async move {
                store
                    .execute(
                        "CREATE TABLE IF NOT EXISTS marks (name TEXT NOT NULL)",
                        vec![],
                    )
                    .await?;
                store
                    .query::<Mark>("SELECT name FROM marks ORDER BY rowid", vec![])
                    .await
            })
            .await;
            match read {
                Ok(marks) => {
                    let names: Vec<_> = marks.into_iter().map(|m| m.name).collect();
                    println!("@@ MARKS {}", names.join(","));
                    0
                }
                Err(err) => {
                    println!("@@ REFUSED {err}");
                    err.exit_code()
                }
            }
        }
        other => panic!("unknown phase {other}"),
    }
}

#[derive(Deserialize)]
struct Mark {
    name: String,
}

// ----------------------------------------------------------------- parent

/// One data directory and the ports every process on it uses: hiqlite
/// records a node's addresses in its membership.
struct Volume {
    _root: tempfile::TempDir,
    dir: PathBuf,
    raft: String,
    api: String,
}

impl Volume {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("hiqlite");
        Self {
            _root: root,
            dir,
            raft: common::free_addr().to_string(),
            api: common::free_addr().to_string(),
        }
    }

    /// hiqlite's unclean-stop marker.
    fn marker(&self) -> PathBuf {
        self.dir.join("state_machine").join("lock")
    }

    fn command(&self, phase: &str) -> Command {
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args([CHILD, "--exact", "--nocapture", "--test-threads=1"])
            .env(PHASE, phase)
            .env(DIR, &self.dir)
            .env(RAFT, &self.raft)
            .env(API, &self.api)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    }

    fn spawn(&self, phase: &str) -> Running {
        let mut child = self.command(phase).spawn().unwrap();
        let out = Arc::new(Mutex::new(String::new()));
        for pipe in [
            Box::new(child.stdout.take().unwrap()) as Box<dyn std::io::Read + Send>,
            Box::new(child.stderr.take().unwrap()),
        ] {
            let out = Arc::clone(&out);
            std::thread::spawn(move || {
                let mut pipe = pipe;
                let mut buf = [0u8; 4096];
                while let Ok(n @ 1..) = pipe.read(&mut buf) {
                    out.lock()
                        .unwrap()
                        .push_str(&String::from_utf8_lossy(&buf[..n]));
                }
            });
        }
        Running { child, out }
    }

    fn run(&self, phase: &str) -> Exited {
        let mut attempt = 1;
        loop {
            let exited = self.spawn(phase).wait(EXIT_BUDGET);
            if !exited.logs.contains(PORT_TAKEN) || attempt == START_ATTEMPTS {
                return exited;
            }
            attempt += 1;
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    /// Start `phase` and wait until its body runs.
    fn start(&self, phase: &str) -> Running {
        let mut attempt = 1;
        loop {
            let mut running = self.spawn(phase);
            let deadline = Instant::now() + EXIT_BUDGET;
            loop {
                if running.logs().contains(READY) {
                    return running;
                }
                if running.child.try_wait().unwrap().is_some() {
                    break;
                }
                assert!(Instant::now() < deadline, "no {READY}\n{}", running.logs());
                std::thread::sleep(Duration::from_millis(20));
            }
            let logs = running.wait(EXIT_BUDGET).logs;
            assert!(
                logs.contains(PORT_TAKEN) && attempt < START_ATTEMPTS,
                "{phase} ended before its body ran\n{logs}"
            );
            attempt += 1;
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    /// Start the next program on this directory and read what it holds.
    fn read(&self) -> Result<Vec<String>, String> {
        let read = self.run("read");
        let line = |prefix: &str| {
            read.logs
                .lines()
                .find_map(|l| l.split_once(prefix).map(|(_, rest)| rest.trim().to_owned()))
        };
        match (read.code, line("@@ MARKS"), line("@@ REFUSED")) {
            (Some(0), Some(marks), _) => Ok(marks
                .split(',')
                .filter(|m| !m.is_empty())
                .map(str::to_owned)
                .collect()),
            (_, _, Some(refused)) => Err(refused),
            _ => panic!("the reader neither read nor refused\n{}", read.logs),
        }
    }

    /// The directory is clean and the next start reads `expected`.
    fn assert_reopens_with(&self, expected: &[&str], logs: &str) {
        assert!(
            !self.marker().exists(),
            "the unclean-stop marker outlived the program\n{logs}"
        );
        let marks = self
            .read()
            .unwrap_or_else(|refused| panic!("the next start refused: {refused}\n{logs}"));
        assert_eq!(marks, expected, "{logs}");
    }
}

struct Running {
    child: Child,
    out: Arc<Mutex<String>>,
}

struct Exited {
    /// `None` when a signal ended the process.
    code: Option<i32>,
    logs: String,
}

impl Running {
    fn logs(&self) -> String {
        self.out.lock().unwrap().clone()
    }

    fn signal(&self, name: &str) {
        // The process may already have exited between two of a burst.
        let _ = Command::new("kill")
            .arg(format!("-{name}"))
            .arg(self.child.id().to_string())
            .stderr(Stdio::null())
            .status();
    }

    fn wait(mut self, within: Duration) -> Exited {
        let deadline = Instant::now() + within;
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                panic!(
                    "the program did not exit within {within:?}\n{}",
                    self.logs()
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        // Let the readers drain what the process wrote last.
        std::thread::sleep(Duration::from_millis(100));
        Exited {
            code: status.code(),
            logs: format!("{status}\n{}", self.logs()),
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

// ------------------------------------------------------------------ tests

#[test]
fn an_error_after_open_still_shuts_the_node_down() {
    let volume = Volume::new();
    let exited = volume.run("error-after-open");
    assert_eq!(
        exited.code,
        Some(1),
        "the body's error is the exit\n{}",
        exited.logs
    );
    assert!(exited.logs.contains("the program failed after open"));
    volume.assert_reopens_with(&["before-error"], &exited.logs);
}

#[test]
fn a_panic_after_open_still_shuts_the_node_down() {
    let volume = Volume::new();
    let exited = volume.run("panic-after-open");
    // libtest's code for a panicking test: the panic went on after the stop.
    assert_eq!(exited.code, Some(101), "{}", exited.logs);
    assert!(exited.logs.contains("the program panicked after open"));
    volume.assert_reopens_with(&["before-panic"], &exited.logs);
}

#[test]
fn sigterm_and_sigint_are_graceful_stops() {
    for signal in ["TERM", "INT"] {
        let volume = Volume::new();
        let running = volume.start("serve-until-stopped");
        running.signal(signal);
        let exited = running.wait(EXIT_BUDGET);
        assert_eq!(
            exited.code,
            Some(0),
            "SIG{signal} is a stop the body finishes, not a kill\n{}",
            exited.logs
        );
        volume.assert_reopens_with(&["before-stop", "after-stop"], &exited.logs);
    }
}

#[test]
fn a_body_that_ignores_the_stop_is_abandoned_after_the_grace() {
    let volume = Volume::new();
    let running = volume.start("ignore-stop");
    let signalled = Instant::now();
    running.signal("TERM");
    let exited = running.wait(EXIT_BUDGET);
    let took = signalled.elapsed();
    assert_eq!(
        exited.code,
        Some(3),
        "an abandoned body is exit 3\n{}",
        exited.logs
    );
    assert!(exited.logs.contains("stopped:"), "{}", exited.logs);
    assert!(took >= STOP_GRACE, "the body had its grace: {took:?}");
    volume.assert_reopens_with(&["ignoring"], &exited.logs);
}

#[test]
fn a_stop_requested_during_open_shuts_the_node_down_before_the_body() {
    let volume = Volume::new();
    let exited = volume.run("stop-during-open");
    assert_eq!(exited.code, Some(3), "{}", exited.logs);
    assert!(!exited.logs.contains("the body ran"), "{}", exited.logs);
    volume.assert_reopens_with(&[], &exited.logs);
}

#[test]
fn a_burst_of_sigterm_from_the_first_instant_never_leaves_the_marker() {
    // Whichever moment a signal lands in, before the handler is armed (the
    // node has not started), while the node starts, or while the body runs,
    // the directory must be reopenable.
    for _ in 0..3 {
        let volume = Volume::new();
        let running = volume.spawn("serve-until-stopped");
        let deadline = Instant::now() + EXIT_BUDGET;
        let mut running = running;
        let status = loop {
            if let Some(status) = running.child.try_wait().unwrap() {
                break status;
            }
            assert!(Instant::now() < deadline, "no exit\n{}", running.logs());
            running.signal("TERM");
            std::thread::sleep(Duration::from_millis(25));
        };
        let logs = format!("{status}\n{}", running.logs());
        assert!(!volume.marker().exists(), "{logs}");
        volume
            .read()
            .unwrap_or_else(|refused| panic!("the next start refused: {refused}\n{logs}"));
    }
}

#[test]
fn the_bare_pattern_leaves_the_marker_and_the_documented_recovery_rebuilds() {
    // The control: without `Store::run`, a `?` after open ends the process
    // with the node running, which is the defect this spec closes.
    let volume = Volume::new();
    let exited = volume.run("bare");
    assert_eq!(exited.code, Some(1), "{}", exited.logs);
    assert!(
        exited.logs.contains("was dropped without Store::shutdown"),
        "B-6's safety net names the dropped store\n{}",
        exited.logs
    );
    assert!(volume.marker().exists(), "{}", exited.logs);
    let refused = volume.read().expect_err("the next start must refuse");
    assert!(refused.contains("did not stop cleanly"), "{refused}");

    // B-8's recovery for a single voter: with no process on the directory,
    // move the state machine's database and the marker aside, and the next
    // start rebuilds the database from the snapshot and the log.
    let aside = volume.dir.with_file_name("unclean-stop-evidence");
    std::fs::create_dir_all(&aside).unwrap();
    let state_machine = volume.dir.join("state_machine");
    std::fs::rename(state_machine.join("db"), aside.join("db")).unwrap();
    std::fs::rename(state_machine.join("lock"), aside.join("lock")).unwrap();
    volume.assert_reopens_with(&["bare"], "after the recovery");
}

#[test]
fn without_a_handler_sigterm_leaves_the_marker() {
    // The control for the signal tests: the same stop, delivered to a
    // program that armed nothing, is the incident spec 048 answers.
    let volume = Volume::new();
    let running = volume.start("unarmed");
    running.signal("TERM");
    let exited = running.wait(EXIT_BUDGET);
    assert_eq!(
        exited.code, None,
        "SIGTERM's default action\n{}",
        exited.logs
    );
    assert!(volume.marker().exists(), "{}", exited.logs);
    let refused = volume.read().expect_err("the next start must refuse");
    assert!(refused.contains("did not stop cleanly"), "{refused}");
}
