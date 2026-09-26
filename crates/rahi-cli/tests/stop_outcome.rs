//! Spec 043 B-10 and AC-7a: the stop record, the outcome each stop
//! observed, the exit status that carries it, and the next boot's
//! classification of what the previous one left.
//!
//! Every process here is a real cell on a real hiqlite node, stopped with
//! real signals. The test is the harness AC-7a names: where it SIGKILLs a
//! process, it writes the witness record that names the boot it killed.
//! None of these runs counts toward AC-7's graceful series
//! (`stop_budget.rs`).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod stop_fixture;

use std::time::Duration;

use rahi_ops::stop::{Outcome, Reason};
use stop_fixture::{Node, OpenStream, READY_BUDGET, slow_in_flight};

fixture_entry!();

/// Far above serve's own stop (spec 043 B-9's forty seconds).
const STOP_BUDGET: Duration = Duration::from_secs(90);

#[test]
fn a_graceful_stop_is_confirmed_recorded_whole_and_read_by_the_next_boot() {
    let node = Node::new();
    let mut serve = node.spawn("serve");
    node.wait_ready(&mut serve, READY_BUDGET);
    let started = node.stop_record().expect("the boot wrote its record");
    assert_eq!(started.boot, 1);
    assert_eq!(started.entry, "serve");
    assert!(started.received_at.is_none() && started.outcome.is_none());

    let stream = OpenStream::open(&node.listen);
    let reader = std::thread::spawn(move || stream.read_to_close());
    serve.sigterm();
    let stopped = serve.wait(STOP_BUDGET);
    reader.join().unwrap();
    assert_eq!(stopped.code, Some(0), "{}", stopped.logs());

    let record = node.stop_record().unwrap();
    assert_eq!(record.boot, 1);
    assert!(record.received_at.is_some(), "{record:?}");
    assert_eq!(record.outcome, Some(Outcome::Confirmed), "{record:?}");
    assert_eq!(record.exit_code, Some(0));
    let phases = stop_fixture::phases(&record);
    for phase in [
        "stream_drain",
        "connection_drain",
        "denial_drain",
        "store_shutdown",
    ] {
        assert!(phases.contains_key(phase), "{phase} in {record:?}");
    }

    let mut next = node.spawn("serve");
    node.wait_ready(&mut next, READY_BUDGET);
    next.sigterm();
    let again = next.wait(STOP_BUDGET);
    assert_eq!(again.code, Some(0), "{}", again.logs());
    assert!(
        again
            .stderr
            .contains("previous stop: boot 1, confirmed, cause recorded"),
        "{}",
        again.logs()
    );
    assert_eq!(node.stop_record().unwrap().boot, 2);
}

/// AC-7a: a phase made to overrun. A request that outlives the connection
/// drain (`DRAIN_BUDGET`, ten seconds) is cut, and the stop says so.
#[test]
fn a_connection_drain_overrun_is_unconfirmed_and_exits_three() {
    let node = Node::new();
    let mut serve = node.spawn("serve");
    node.wait_ready(&mut serve, READY_BUDGET);
    let slow = slow_in_flight(&node.listen);
    std::thread::sleep(Duration::from_millis(300));
    serve.sigterm();
    let stopped = serve.wait(STOP_BUDGET);
    let _ = slow.join();
    assert_eq!(stopped.code, Some(3), "{}", stopped.logs());
    let record = node.stop_record().unwrap();
    let Some(Outcome::Unconfirmed { reasons }) = &record.outcome else {
        panic!("an unconfirmed outcome: {record:?}");
    };
    assert!(
        reasons.contains(&Reason::ConnectionDrainOverrun),
        "{reasons:?}"
    );
    assert_eq!(record.exit_code, Some(3));
    assert!(
        stopped.stderr.contains("connection_drain_overrun"),
        "{}",
        stopped.logs()
    );

    // The next boot needs no manual step.
    let mut next = node.spawn("serve");
    node.wait_ready(&mut next, READY_BUDGET);
    next.sigterm();
    let again = next.wait(STOP_BUDGET);
    assert!(
        again.stderr.contains(
            "previous stop: boot 1, unconfirmed (connection_drain_overrun), cause recorded"
        ),
        "{}",
        again.logs()
    );
}

/// AC-7a: the harness SIGKILLs a process after SIGTERM. With its witness the
/// next boot classifies **incomplete after SIGTERM**, cause
/// `witnessed_kill`; the same state without the witness is cause `unknown`;
/// a SIGKILL with no SIGTERM is **stopped without a recorded signal**.
///
/// Each kill here lands before the app store's shutdown, so hiqlite's
/// unclean-stop marker is left and the next start refuses it without
/// `auto-heal` (spec 043 D-20 (c)); the classification is made before the
/// store is opened, so it is logged either way. The test then takes the
/// operator's step (nothing else uses the volume, so the marker goes) and
/// the boot after it serves. A kill that lands after the store's shutdown
/// needs no step at all: the Rauthy test below.
#[test]
fn a_kill_after_sigterm_is_classified_by_the_next_boot_with_and_without_a_witness() {
    let node = Node::new();
    let marker = node.data_dir.join("app-store/state_machine/lock");

    // Boot 1: SIGTERM, then SIGKILL mid-stop, witnessed by this harness.
    kill_mid_stop(&node, 1);
    let received = node.stop_record().unwrap();
    rahi_ops::stop::write_witness(&node.data_dir, received.boot, "the test harness").unwrap();

    // Boot 2 classifies it and meets hiqlite's refusal.
    let second = node.run("serve");
    assert!(
        second
            .stderr
            .contains("previous stop: boot 1, incomplete_after_sigterm, cause witnessed_kill"),
        "{}",
        second.logs()
    );
    assert_eq!(second.code, Some(3), "{}", second.logs());
    assert!(marker.exists(), "the kill left hiqlite's marker");
    std::fs::remove_file(&marker).unwrap();

    // Boot 3 reads boot 2's own recorded refusal, then is killed the same
    // way with no witness.
    let third = kill_mid_stop(&node, 3);
    assert!(
        third
            .stderr
            .contains("previous stop: boot 2, unconfirmed (serve_error), cause recorded"),
        "{}",
        third.logs()
    );
    let fourth = node.run("serve");
    assert!(
        fourth
            .stderr
            .contains("previous stop: boot 3, incomplete_after_sigterm, cause unknown"),
        "{}",
        fourth.logs()
    );
    std::fs::remove_file(&marker).unwrap();

    // Boot 5 is killed with no SIGTERM at all.
    let mut fifth = node.spawn("serve");
    node.wait_ready(&mut fifth, READY_BUDGET);
    fifth.sigkill();
    let _ = fifth.wait(STOP_BUDGET);
    let sixth = node.run("serve");
    assert!(
        sixth
            .stderr
            .contains("previous stop: boot 5, stopped_without_signal, cause unknown"),
        "{}",
        sixth.logs()
    );
    std::fs::remove_file(&marker).unwrap();

    // Boot 7 serves and stops confirmed.
    let mut seventh = node.spawn("serve");
    node.wait_ready(&mut seventh, READY_BUDGET);
    seventh.sigterm();
    let seventh = seventh.wait(STOP_BUDGET);
    assert_eq!(seventh.code, Some(0), "{}", seventh.logs());
}

/// Start `serve`, hold a request past SIGTERM, and SIGKILL the process once
/// its record says the SIGTERM arrived. The boot is `boot`.
fn kill_mid_stop(node: &Node, boot: u64) -> stop_fixture::Finished {
    let mut serve = node.spawn("serve");
    node.wait_ready(&mut serve, READY_BUDGET);
    let slow = slow_in_flight(&node.listen);
    std::thread::sleep(Duration::from_millis(300));
    serve.sigterm();
    let received = node.wait_received(Duration::from_secs(10));
    assert_eq!(received.boot, boot);
    serve.sigkill();
    let killed = serve.wait(STOP_BUDGET);
    let _ = slow.join();
    assert_eq!(killed.code, None, "a signal death\n{}", killed.logs());
    killed
}

/// AC-7a: Rauthy made to ignore SIGTERM. The supervisor kills it after its
/// grace, witnesses the kill in its own outcome, and exits `3`.
///
/// The wrapper ignores SIGTERM and keeps the real Rauthy as its child, so
/// the process the supervisor signals is the one that ignores it; the real
/// Rauthy it leaves behind is stopped by the test with its own SIGTERM.
///
/// The same wrapper holds the supervisor inside its Rauthy phase, after the
/// app store has shut, which is where the harness's SIGKILL lands in the
/// first half: the next boot classifies **incomplete after SIGTERM** with
/// cause `witnessed_kill` and serves with no manual step.
#[test]
fn a_rauthy_that_ignores_sigterm_is_killed_and_the_stop_says_so() {
    let Some(rauthy) = stop_fixture::test_rauthy() else {
        eprintln!("skipped: RAHI_TEST_RAUTHY is not set (the live workflow sets it)");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let wrapper = Stubborn::new(dir.path(), &rauthy);
    let node = Node::with_rauthy(&wrapper.path);
    let marker = node.data_dir.join("app-store/state_machine/lock");

    // Boot 1: SIGTERM; once the app store has shut, the harness kills the
    // supervisor while it waits on Rauthy.
    let mut first = node.spawn("supervise");
    node.wait_ready(&mut first, Duration::from_secs(180));
    first.sigterm();
    let received = node.wait_received(Duration::from_secs(10));
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while marker.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the app store did not shut\n{}",
            first.logs()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    first.sigkill();
    let killed = first.wait(STOP_BUDGET);
    assert_eq!(killed.code, None, "{}", killed.logs());
    rahi_ops::stop::write_witness(&node.data_dir, received.boot, "the test harness").unwrap();
    wrapper.stop_orphans();

    // Boot 2 needs no manual step; its own stop meets the stubborn Rauthy.
    let mut second = node.spawn("supervise");
    node.wait_ready(&mut second, Duration::from_secs(180));
    second.sigterm();
    let stopped = second.wait(STOP_BUDGET);
    wrapper.stop_orphans();
    assert!(
        stopped
            .stderr
            .contains("previous stop: boot 1, incomplete_after_sigterm, cause witnessed_kill"),
        "{}",
        stopped.logs()
    );
    assert_eq!(stopped.code, Some(3), "{}", stopped.logs());
    let record = node.stop_record().unwrap();
    assert_eq!(record.entry, "supervise");
    let Some(Outcome::Unconfirmed { reasons }) = &record.outcome else {
        panic!("an unconfirmed outcome: {record:?}");
    };
    assert!(reasons.contains(&Reason::RauthyKilled), "{reasons:?}");
    let phases = stop_fixture::phases(&record);
    assert!(
        phases["rauthy_stop"] >= 10_000,
        "the kill came after the grace: {record:?}"
    );
}

/// A Rauthy wrapper that ignores SIGTERM, and the real Rauthy behind it.
struct Stubborn {
    path: std::path::PathBuf,
    rauthy_pid: std::path::PathBuf,
    wrapper_pid: std::path::PathBuf,
}

impl Stubborn {
    fn new(dir: &std::path::Path, rauthy: &std::path::Path) -> Self {
        let path = dir.join("stubborn-rauthy");
        let rauthy_pid = dir.join("rauthy.pid");
        let wrapper_pid = dir.join("wrapper.pid");
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\ntrap '' TERM\necho $$ > \"{w}\"\n\"{r}\" \"$@\" &\necho $! > \"{p}\"\nwhile kill -0 $! 2>/dev/null; do wait $!; done\n",
                w = wrapper_pid.display(),
                r = rauthy.display(),
                p = rauthy_pid.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        Self {
            path,
            rauthy_pid,
            wrapper_pid,
        }
    }

    /// SIGTERM the real Rauthy and wait for it, so its own store stops
    /// cleanly; then end the wrapper.
    fn stop_orphans(&self) {
        let alive = |pid: &str| {
            std::process::Command::new("kill")
                .args(["-0", pid])
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        };
        if let Ok(pid) = std::fs::read_to_string(&self.rauthy_pid) {
            let pid = pid.trim().to_owned();
            let _ = std::process::Command::new("kill")
                .args(["-TERM", &pid])
                .status();
            let deadline = std::time::Instant::now() + Duration::from_secs(30);
            while alive(&pid) && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        if let Ok(pid) = std::fs::read_to_string(&self.wrapper_pid) {
            let _ = std::process::Command::new("kill")
                .args(["-KILL", pid.trim()])
                .status();
        }
    }
}

/// AC-7a: the store's shutdown made to overrun, by a real fault. A member of
/// a three-node cluster is stopped while the other two are frozen: hiqlite's
/// pre-shutdown delay and its wait for a cache leader outlast
/// `Client::shutdown`'s own fifteen seconds, and it answers `Timeout`. No
/// fault available to a test drives a single node's shutdown past its wait.
///
/// The node exits `3` with `store_timeout`. If hiqlite's background shutdown
/// has not completed, its unclean-stop marker is left and the next start
/// refuses it without `auto-heal` (spec 043 D-20 (c)). If it completes after
/// the caller-side timeout, the marker is gone and the next start succeeds.
/// Both schedules retain the unconfirmed stop record and its classification.
#[test]
fn a_store_shutdown_that_overruns_is_store_timeout_and_exits_three() {
    let nodes = Node::cluster(3);
    let mut running: Vec<_> = nodes.iter().map(|n| n.spawn("serve")).collect();
    for (node, cell) in nodes.iter().zip(running.iter_mut()) {
        node.wait_ready(cell, Duration::from_secs(180));
    }
    running[0].sigstop();
    running[1].sigstop();
    running[2].sigterm();
    let stopped = running[2].wait(STOP_BUDGET);
    running[0].sigcont();
    running[1].sigcont();
    assert_eq!(stopped.code, Some(3), "{}", stopped.logs());
    let record = nodes[2].stop_record().unwrap();
    assert_eq!(
        record.outcome,
        Some(Outcome::Unconfirmed {
            reasons: vec![Reason::StoreTimeout]
        }),
        "{record:?}"
    );
    let phases = stop_fixture::phases(&record);
    assert!(
        phases["store_shutdown"] >= 15_000,
        "hiqlite's own wait, not a shorter one: {record:?}"
    );
    assert!(
        stopped.stderr.contains("store_timeout"),
        "{}",
        stopped.logs()
    );

    let marker = nodes[2].data_dir.join("app-store/state_machine/lock");
    if marker.exists() {
        let next = nodes[2].run("serve");
        assert_eq!(next.code, Some(3), "{}", next.logs());
        assert!(
            next.stderr
                .contains("previous stop: boot 1, unconfirmed (store_timeout), cause recorded"),
            "the next boot classifies before it opens the store\n{}",
            next.logs()
        );
        assert!(
            next.stderr.contains("did not stop cleanly"),
            "hiqlite refuses the unclean marker without auto-heal\n{}",
            next.logs()
        );
    } else {
        let mut next = nodes[2].spawn("serve");
        nodes[2].wait_ready(&mut next, Duration::from_secs(180));
        assert!(
            next.logs()
                .contains("previous stop: boot 1, unconfirmed (store_timeout), cause recorded"),
            "the next boot classifies before it opens the store\n{}",
            next.logs()
        );
        next.sigterm();
        let stopped = next.wait(STOP_BUDGET);
        assert_eq!(stopped.code, Some(0), "{}", stopped.logs());
    }
    for cell in &running[..2] {
        cell.sigterm();
    }
    for cell in &mut running[..2] {
        let _ = cell.wait(STOP_BUDGET);
    }
}
