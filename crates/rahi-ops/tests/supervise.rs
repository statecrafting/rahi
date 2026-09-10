//! The die-together supervisor (spec 031 FR-002), with stub children.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::future::pending;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use rahi_ops::supervise::{self, Exit, Reason};
use rahi_types::Error;
use tokio::process::Command;

fn sh(script: &str) -> Command {
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(script).kill_on_drop(true);
    cmd
}

/// A serve that runs until the supervisor's stop signal and records that it
/// was stopped cooperatively rather than dropped.
async fn serve_until_stopped(
    stopped: Arc<AtomicBool>,
    stop: tokio::sync::oneshot::Receiver<()>,
) -> rahi_types::Result<()> {
    let _ = stop.await;
    stopped.store(true, Ordering::SeqCst);
    Ok(())
}

/// A serve that ignores the stop signal; the supervisor gives up after its
/// grace.
async fn serve_stubborn(_stop: tokio::sync::oneshot::Receiver<()>) -> rahi_types::Result<()> {
    pending::<()>().await;
    Ok(())
}

#[tokio::test]
async fn a_child_that_exits_stops_serve_and_the_exit_code_is_the_childs() {
    let dropped = Arc::new(AtomicBool::new(false));
    let started = Instant::now();
    let stopped = dropped.clone();
    let exit = supervise::supervise(
        sh("sleep 2; exit 7"),
        async { Ok(()) },
        move |stop| serve_until_stopped(stopped, stop),
        pending::<()>(),
    )
    .await;
    assert_eq!(
        exit,
        Exit {
            code: 7,
            reason: Reason::RauthyExited
        }
    );
    assert!(
        dropped.load(Ordering::SeqCst),
        "serve was stopped cooperatively"
    );
    assert!(started.elapsed() >= Duration::from_secs(2));
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[tokio::test]
async fn a_failing_serve_terminates_the_child_and_exits_with_serves_code() {
    let dir = tempfile::tempdir().unwrap();
    let record = dir.path().join("signals");
    let script = format!(
        "trap 'echo TERM >> {record}; exit 0' TERM; while true; do sleep 0.1; done",
        record = record.display()
    );
    let started = Instant::now();
    let exit = supervise::supervise(
        sh(&script),
        async { Ok(()) },
        |_stop| async {
            tokio::time::sleep(Duration::from_millis(500)).await;
            Err(Error::Stale("behind".to_owned()))
        },
        pending::<()>(),
    )
    .await;
    assert_eq!(
        exit,
        Exit {
            code: 2,
            reason: Reason::ServeEnded
        }
    );
    assert_eq!(
        std::fs::read_to_string(&record).unwrap().trim(),
        "TERM",
        "the child recorded the SIGTERM"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "no grace period was needed"
    );
}

#[tokio::test]
async fn shutdown_reaches_both_halves_and_is_a_clean_exit() {
    let dir = tempfile::tempdir().unwrap();
    let record = dir.path().join("signals");
    let script = format!(
        "trap 'echo TERM >> {record}; exit 0' TERM; while true; do sleep 0.1; done",
        record = record.display()
    );
    let stopped = Arc::new(AtomicBool::new(false));
    let flag = stopped.clone();
    let exit = supervise::supervise(
        sh(&script),
        async { Ok(()) },
        move |stop| serve_until_stopped(flag, stop),
        tokio::time::sleep(Duration::from_millis(500)),
    )
    .await;
    assert_eq!(
        exit,
        Exit {
            code: 0,
            reason: Reason::Shutdown
        }
    );
    assert!(
        stopped.load(Ordering::SeqCst),
        "serve was stopped cooperatively"
    );
    assert_eq!(std::fs::read_to_string(&record).unwrap().trim(), "TERM");
}

#[tokio::test]
async fn a_child_that_ignores_sigterm_is_killed_after_the_grace() {
    let mut child = sh("trap '' TERM; while true; do sleep 0.1; done")
        .spawn()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let started = Instant::now();
    let code = supervise::terminate(&mut child, Duration::from_millis(700)).await;
    assert_eq!(code, 128 + 9, "SIGKILL after the grace");
    assert!(started.elapsed() >= Duration::from_millis(700));
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn an_unhealthy_child_is_terminated_and_the_exit_is_the_readiness_error() {
    let exit = supervise::supervise(
        sh("while true; do sleep 0.1; done"),
        async { Err(Error::Upstream("rauthy did not answer".to_owned())) },
        |_stop| async { Ok(()) },
        pending::<()>(),
    )
    .await;
    assert_eq!(
        exit,
        Exit {
            code: 3,
            reason: Reason::RauthyUnhealthy
        }
    );
}

#[tokio::test]
async fn a_child_exiting_during_readiness_ends_the_supervisor() {
    let exit = supervise::supervise(
        sh("exit 4"),
        async {
            tokio::time::sleep(Duration::from_secs(30)).await;
            Ok(())
        },
        |_stop| async { Ok(()) },
        pending::<()>(),
    )
    .await;
    assert_eq!(
        exit,
        Exit {
            code: 4,
            reason: Reason::RauthyExited
        }
    );
}

#[tokio::test]
async fn a_serve_that_ignores_the_stop_is_given_up_after_its_grace() {
    let started = Instant::now();
    let exit = supervise::supervise_with(
        sh("sleep 1; exit 3"),
        async { Ok(()) },
        serve_stubborn,
        pending::<()>(),
        supervise::Graces {
            term: Duration::from_millis(500),
            serve: Duration::from_millis(400),
            shutdown_term: Duration::from_millis(500),
        },
    )
    .await;
    assert_eq!(
        exit,
        Exit {
            code: 3,
            reason: Reason::RauthyExited
        }
    );
    assert!(started.elapsed() >= Duration::from_millis(1400));
    assert!(started.elapsed() < Duration::from_secs(5));
}
