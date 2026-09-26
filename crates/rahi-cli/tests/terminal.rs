//! Spec 043 B-7, FR-005 and AC-8: a terminal storage failure under a
//! running `serve`.
//!
//! The fault is real (FR-005): the app store's Raft log directory is made
//! unwritable under the running node, and the node keeps appending until
//! its log writer has to roll over into a new segment file, which it cannot
//! create. hiqlite takes the node out of service with `NodeFailed`; nothing
//! here mocks a client.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod stop_fixture;

use std::os::unix::fs::PermissionsExt as _;
use std::time::{Duration, Instant};

use rahi_edge::READYZ_PATH;
use rahi_ops::stop::{Outcome, Reason};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use stop_fixture::{Node, READY_BUDGET, deny_batch, http_get};

fixture_entry!();

/// B-9's bound for `serve` alone: the process must be gone within it of the
/// fault being noticed, plus the terminal watch's poll.
const TERMINAL_BOUND: Duration = Duration::from_secs(45);

#[test]
fn a_terminal_store_failure_fails_readiness_records_storage_terminal_and_exits_three() {
    let node = Node::new();
    let mut serve = node.spawn("serve");
    node.wait_ready(&mut serve, READY_BUDGET);

    // Readiness, watched from its own thread for the whole run.
    let listen = node.listen.clone();
    let watching = Arc::new(AtomicBool::new(true));
    let readiness = {
        let watching = watching.clone();
        std::thread::spawn(move || {
            let mut first_failure = None;
            while watching.load(Ordering::SeqCst) {
                if let Ok((status, body)) = http_get(&listen, READYZ_PATH)
                    && status != 200
                    && first_failure.is_none()
                {
                    first_failure = Some((status, body, Instant::now()));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            first_failure
        })
    };

    let logs = node.data_dir.join("app-store/logs");
    assert!(logs.is_dir(), "the app store's Raft log directory");
    std::fs::set_permissions(&logs, std::fs::Permissions::from_mode(0o500)).unwrap();

    // Denials append to the chain through the log until the writer's
    // rollover fails and the node goes out of service. The last batches
    // are the denials queued at the fault.
    let started = Instant::now();
    let mut recent: Vec<Vec<String>> = Vec::new();
    let exited = loop {
        if let Some(status) = serve.child.try_wait().unwrap() {
            break (status, Instant::now());
        }
        assert!(
            started.elapsed() < Duration::from_secs(900),
            "no terminal failure\n{}",
            serve.logs()
        );
        let ids = deny_batch(&node.listen, 200)
            .into_iter()
            .filter_map(|(status, id)| (status == Some(403)).then_some(id).flatten())
            .collect();
        recent.push(ids);
        if recent.len() > 3 {
            recent.remove(0);
        }
    };
    std::fs::set_permissions(&logs, std::fs::Permissions::from_mode(0o700)).unwrap();
    watching.store(false, Ordering::SeqCst);
    let first_failure = readiness.join().unwrap();
    let stopped = serve.wait(Duration::from_secs(5));

    assert_eq!(exited.0.code(), Some(3), "{}", stopped.logs());
    let fault = stopped
        .stderr
        .lines()
        .find(|l| l.contains("failed terminally"))
        .unwrap_or_else(|| panic!("serve names the terminal failure\n{}", stopped.logs()));
    assert!(fault.contains("NodeFailed"), "{fault}");
    let (status, body, failed_at) = first_failure.expect("/readyz failed while serve was stopping");
    assert_eq!(status, 503, "{body}");
    assert!(
        exited.1.duration_since(failed_at) < TERMINAL_BOUND,
        "serve exited within B-9's bound of failing readiness"
    );
    let record = node.stop_record().unwrap();
    let Some(Outcome::Unconfirmed { reasons }) = &record.outcome else {
        panic!("an unconfirmed outcome: {record:?}");
    };
    assert!(reasons.contains(&Reason::StorageTerminal), "{reasons:?}");
    assert_eq!(record.exit_code, Some(3));

    // Cleared, the next boot is ready, and every denial answered around the
    // fault is in the chain or named by the line that counted its loss.
    let mut next = node.spawn("serve");
    node.wait_ready(&mut next, READY_BUDGET);
    next.sigterm();
    let next = next.wait(Duration::from_secs(90));
    assert_eq!(next.code, Some(0), "{}", next.logs());
    assert!(
        next.stderr
            .contains("previous stop: boot 1, unconfirmed (store_error, storage_terminal)")
            || next
                .stderr
                .contains("previous stop: boot 1, unconfirmed (storage_terminal)"),
        "{}",
        next.logs()
    );
    let export = node.root.path().join("chain.jsonl");
    let exported = node.run(&format!("ledger export {}", export.display()));
    assert_eq!(exported.code, Some(0), "{}", exported.logs());
    let chain = std::fs::read_to_string(&export).unwrap();
    let unexplained: Vec<&String> = recent
        .iter()
        .flatten()
        .filter(|id| !chain.contains(&format!("\"{id}\"")))
        .filter(|id| {
            !stopped
                .stderr
                .lines()
                .any(|l| l.starts_with("ERROR rahi.decision: decision ") && l.contains(id.as_str()))
        })
        .collect();
    assert!(
        unexplained.is_empty(),
        "denials answered at the fault that are neither ledgered nor counted: {unexplained:?}"
    );
}

/// AC-8's last sentence and B-8: a Rauthy held unready fails the cell's
/// readiness, naming it, and does not end the process; released, it is
/// ready again, and the stop that follows is confirmed.
#[test]
fn a_rauthy_held_unready_fails_readiness_and_does_not_end_the_process() {
    let Some(rauthy) = stop_fixture::test_rauthy() else {
        eprintln!("skipped: RAHI_TEST_RAUTHY is not set (the live workflow sets it)");
        return;
    };
    let node = Node::with_rauthy(&rauthy);
    let mut supervise = node.spawn("supervise");
    node.wait_ready(&mut supervise, Duration::from_secs(180));

    // The Rauthy child is the process started on this volume's config.
    let config = node.data_dir.join("rauthy/config.toml");
    let found = std::process::Command::new("pgrep")
        .args(["-f", &config.display().to_string()])
        .output()
        .unwrap();
    let pids: Vec<String> = String::from_utf8_lossy(&found.stdout)
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    assert_eq!(pids.len(), 1, "one Rauthy for this volume: {pids:?}");
    let signal = |name: &str| {
        let status = std::process::Command::new("kill")
            .args([format!("-{name}"), pids[0].clone()])
            .status()
            .unwrap();
        assert!(status.success());
    };

    signal("STOP");
    let deadline = Instant::now() + Duration::from_secs(30);
    let body = loop {
        if let Ok((503, body)) = http_get(&node.listen, READYZ_PATH) {
            break body;
        }
        assert!(Instant::now() < deadline, "readiness never failed");
        std::thread::sleep(Duration::from_millis(250));
    };
    assert!(body.contains("\"component\":\"rauthy\""), "{body}");
    std::thread::sleep(Duration::from_secs(10));
    assert!(
        supervise.child.try_wait().unwrap().is_none(),
        "an unready Rauthy does not end the process\n{}",
        supervise.logs()
    );
    signal("CONT");
    node.wait_ready(&mut supervise, Duration::from_secs(60));
    supervise.sigterm();
    let stopped = supervise.wait(Duration::from_secs(90));
    assert_eq!(stopped.code, Some(0), "{}", stopped.logs());
}
