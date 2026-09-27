//! Spec 048 FR-005 and FR-006: no path out of a `rahi` verb that opened the
//! app node leaves hiqlite's unclean-stop marker behind.
//!
//! The verbs run as real processes on the spec 043 stop fixture, and each
//! test ends by starting `serve` on the same volume, because the claim is
//! about the next start.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod stop_fixture;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use stop_fixture::{Node, READY_BUDGET};

fixture_entry!();

/// Far above a boot and serve's whole stop (spec 031 D-4, 035 B-1).
const EXIT_BUDGET: Duration = Duration::from_secs(90);

/// The app node's unclean-stop marker (`rahi_ops::app_lock_file`).
fn marker(node: &Node) -> PathBuf {
    node.data_dir
        .join("app-store")
        .join("state_machine")
        .join("lock")
}

/// The next start serves and stops cleanly.
fn assert_next_serve_starts(node: &Node, after: &str) {
    assert!(
        !marker(node).exists(),
        "the unclean-stop marker outlived the process\n{after}"
    );
    let mut serve = node.spawn("serve");
    node.wait_ready(&mut serve, READY_BUDGET);
    serve.sigterm();
    let stopped = serve.wait(EXIT_BUDGET);
    assert_eq!(stopped.code, Some(0), "{}", stopped.logs());
}

#[test]
fn serve_stopped_while_it_boots_stops_cleanly() {
    // `serve` prints the previous stop before it opens the node; a SIGTERM
    // then lands while the node starts, which before spec 048 ended the
    // process by the signal's default action.
    let node = Node::new();
    let serve = node.spawn("serve");
    let deadline = Instant::now() + EXIT_BUDGET;
    while !serve.logs().contains("serve: ") {
        assert!(Instant::now() < deadline, "no boot line\n{}", serve.logs());
        std::thread::sleep(Duration::from_millis(5));
    }
    serve.sigterm();
    let mut serve = serve;
    let stopped = serve.wait(EXIT_BUDGET);
    assert_eq!(
        stopped.code,
        Some(0),
        "the stop is held until serve can take it\n{}",
        stopped.logs()
    );
    assert_next_serve_starts(&node, &stopped.logs());
}

#[test]
fn a_verb_under_a_burst_of_sigterm_leaves_the_store_reopenable() {
    // Wherever a signal lands (before the handler is armed, while the node
    // starts, while the chain is verified), the next start must open.
    for verb in ["ledger verify", "migrate"] {
        let node = Node::new();
        let mut running = node.spawn(verb);
        let deadline = Instant::now() + EXIT_BUDGET;
        while running.child.try_wait().unwrap().is_none() {
            assert!(
                Instant::now() < deadline,
                "{verb} did not exit\n{}",
                running.logs()
            );
            let _ = std::process::Command::new("kill")
                .args(["-TERM", &running.child.id().to_string()])
                .stderr(std::process::Stdio::null())
                .status();
            std::thread::sleep(Duration::from_millis(25));
        }
        let ended = running.wait(EXIT_BUDGET);
        assert_next_serve_starts(&node, &format!("{verb}\n{}", ended.logs()));
    }
}

#[test]
fn backup_refuses_its_destination_before_it_opens_the_node() {
    let node = Node::new();
    let refused = node.run("backup --to s3://");
    assert_eq!(refused.code, Some(1), "{}", refused.logs());
    assert!(
        refused.stderr.contains("names no bucket"),
        "{}",
        refused.logs()
    );
    assert_next_serve_starts(&node, &refused.logs());
}
