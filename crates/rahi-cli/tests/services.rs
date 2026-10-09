//! Spec 047: managed services, against real process fixtures (AC-1).
//!
//! Each test boots the services cell of `service_fixture` as a child of this
//! test binary, stops it the way an orchestrator does, and judges what the
//! process left: its exit code, its stop record, the owner locks, and the
//! log its services wrote, read back by the next boot.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod service_fixture;
mod stop_fixture;

use std::time::{Duration, Instant};

use rahi_ops::stop::{Outcome, Reason, StopRecord};
use service_fixture::{
    ALPHA_ERROR, ALPHA_PANIC, MODE_DECLARE_ERROR, MODE_DUPLICATE, MODE_EARLY, MODE_EMPTY_NAME,
    MODE_ERROR, MODE_HANG, MODE_NORMAL, MODE_PANIC, MODE_PLAIN, MODE_REPORT, MODE_VAR,
    locks_released,
};
use stop_fixture::{Node, READY_BUDGET};

#[test]
fn fixture_cell() {
    service_fixture::fixture_main();
}

/// A migrated node whose children run the services cell in `mode`.
fn node(mode: &str) -> Node {
    let mut node = Node::new();
    node.set_env(MODE_VAR, mode);
    let migrated = node.run("migrate");
    assert_eq!(migrated.code, Some(0), "migrate\n{}", migrated.logs());
    node
}

/// The phase names the record holds, in the order they ran.
fn phase_order(record: &StopRecord) -> Vec<&str> {
    record.phases.iter().map(|p| p.phase.as_str()).collect()
}

fn position(order: &[&str], phase: &str) -> usize {
    order
        .iter()
        .position(|p| *p == phase)
        .unwrap_or_else(|| panic!("no {phase} phase in {order:?}"))
}

/// B-5, D-4: the services are joined before the denials drain, and the
/// denials drain before the store shuts down.
fn assert_join_precedes_store(record: &StopRecord) {
    let order = phase_order(record);
    let join = position(&order, "service_join");
    let denials = position(&order, "denial_drain");
    let store = position(&order, "store_shutdown");
    assert!(
        join < denials && denials < store,
        "service_join, denial_drain, store_shutdown in that order: {order:?}"
    );
}

fn reasons(record: &StopRecord) -> Vec<Reason> {
    match &record.outcome {
        Some(Outcome::Unconfirmed { reasons }) => reasons.clone(),
        other => panic!("an unconfirmed outcome was expected, not {other:?}"),
    }
}

/// Boot the node again in [`MODE_REPORT`]: the log the previous boot's
/// services wrote, and the line naming how the previous stop ended.
fn next_boot(node: &mut Node) -> (Vec<(String, String)>, String) {
    node.set_env(MODE_VAR, MODE_REPORT);
    let mut cell = node.spawn("serve");
    node.wait_ready(&mut cell, READY_BUDGET);
    let deadline = Instant::now() + Duration::from_secs(30);
    while !cell.logs().contains(service_fixture::REPORT_PREFIX) {
        assert!(Instant::now() < deadline, "no report\n{}", cell.logs());
        std::thread::sleep(Duration::from_millis(50));
    }
    cell.sigterm();
    let finished = cell.wait(Duration::from_secs(60));
    assert_eq!(finished.code, Some(0), "{}", finished.logs());
    let previous = finished
        .stderr
        .lines()
        .find(|line| line.contains("previous stop:"))
        .unwrap_or_default()
        .to_owned();
    (service_fixture::report(&finished.stdout), previous)
}

fn rows(log: &[(String, String)], service: &str, event: &str) -> usize {
    log.iter()
        .filter(|(s, e)| s == service && e == event)
        .count()
}

/// FR-001, FR-002, AC-3: two services start once each, hear the same
/// SIGTERM, finish their store work, and are joined before the store shuts
/// down; the stop is confirmed, and the next boot reads it so.
#[test]
fn fr001_two_services_start_once_and_are_joined_before_the_store_shuts() {
    let mut node = node(MODE_NORMAL);
    let mut cell = node.spawn("serve");
    node.wait_ready(&mut cell, READY_BUDGET);
    cell.sigterm();
    let finished = cell.wait(Duration::from_secs(60));
    assert_eq!(finished.code, Some(0), "{}", finished.logs());
    assert!(locks_released(&node.data_dir), "the store is released");

    let record = node.stop_record().expect("a stop record");
    assert_eq!(
        record.outcome,
        Some(Outcome::Confirmed),
        "{}",
        finished.logs()
    );
    assert_join_precedes_store(&record);
    let join = record
        .phases
        .iter()
        .find(|p| p.phase == "service_join")
        .unwrap();
    assert_eq!(join.bound_millis, 10_000, "B-6: ten seconds");

    let (log, previous) = next_boot(&mut node);
    for service in ["alpha", "beta"] {
        assert_eq!(
            rows(&log, service, "started"),
            1,
            "{service} started once: {log:?}"
        );
        assert_eq!(
            rows(&log, service, "final"),
            1,
            "{service}'s write after the stop is durable: {log:?}"
        );
    }
    assert!(
        previous.contains("confirmed") && !previous.contains("unconfirmed"),
        "AC-3: the next boot reads the stop as confirmed: {previous}"
    );
}

/// One failure fixture: it stops on its own, non-zero, joins its sibling,
/// releases the store, and names `alpha` with `expected`.
fn assert_failure_stops_the_process(mode: &str, expected: fn(&Reason) -> bool) {
    let mut node = node(mode);
    let mut cell = node.spawn("serve");
    let finished = cell.wait(Duration::from_secs(120));
    assert_eq!(
        finished.code,
        Some(rahi_types::error::EXIT_INFRA),
        "{mode}: non-zero\n{}",
        finished.logs()
    );
    assert!(
        locks_released(&node.data_dir),
        "{mode}: the store is released"
    );
    let record = node.stop_record().expect("a stop record");
    assert!(
        record.exit_code.is_some(),
        "{mode}: a whole record: {record:?}"
    );
    assert_join_precedes_store(&record);
    let reasons = reasons(&record);
    assert!(
        reasons.iter().any(expected),
        "{mode}: alpha named with its class: {reasons:?}"
    );
    let (log, _) = next_boot(&mut node);
    assert_eq!(
        rows(&log, "beta", "final"),
        1,
        "{mode}: beta was cancelled and joined: {log:?}"
    );
    assert_eq!(
        rows(&log, "beta", "started"),
        1,
        "{mode}: never restarted: {log:?}"
    );
    assert_eq!(
        rows(&log, "alpha", "started"),
        1,
        "{mode}: never restarted: {log:?}"
    );
}

/// FR-003, AC-4: an early `Ok(())` is an unexpected exit.
#[test]
fn fr003_a_service_that_returns_early_stops_the_process() {
    assert_failure_stops_the_process(
        MODE_EARLY,
        |r| matches!(r, Reason::ServiceExited { service } if service == "alpha"),
    );
}

/// FR-003, AC-4: a returned error is named with what it said.
#[test]
fn fr003_a_service_that_returns_an_error_stops_the_process() {
    assert_failure_stops_the_process(MODE_ERROR, |r| {
        matches!(r, Reason::ServiceError { service, error }
            if service == "alpha" && error.contains(ALPHA_ERROR))
    });
}

/// FR-003, AC-4: a panic is named with its message.
#[test]
fn fr003_a_service_that_panics_stops_the_process() {
    assert_failure_stops_the_process(MODE_PANIC, |r| {
        matches!(r, Reason::ServicePanicked { service, panic }
            if service == "alpha" && panic.contains(ALPHA_PANIC))
    });
}

/// FR-004, AC-4: a service that ignores the stop is aborted at the bound,
/// joined, and named; the denials drain and the store shuts afterwards.
#[test]
fn fr004_a_service_past_the_bound_is_aborted_and_named() {
    let mut node = node(MODE_HANG);
    let mut cell = node.spawn("serve");
    node.wait_ready(&mut cell, READY_BUDGET);
    cell.sigterm();
    let finished = cell.wait(Duration::from_secs(60));
    assert_eq!(
        finished.code,
        Some(rahi_types::error::EXIT_INFRA),
        "{}",
        finished.logs()
    );
    assert!(locks_released(&node.data_dir), "the store is released");
    let record = node.stop_record().expect("a stop record");
    assert_join_precedes_store(&record);
    let reasons = reasons(&record);
    assert_eq!(
        reasons,
        vec![Reason::ServiceJoinTimeout {
            service: "alpha".to_owned()
        }],
        "only alpha overran"
    );
    let join = record
        .phases
        .iter()
        .find(|p| p.phase == "service_join")
        .unwrap();
    assert!(
        join.millis >= 10_000,
        "aborted at the bound, not before: {join:?}"
    );
    let connections = record
        .phases
        .iter()
        .find(|p| p.phase == "connection_drain")
        .unwrap();
    assert!(
        connections.millis < 5_000,
        "C is the server's drain, not the wait for a service: {connections:?}"
    );
    let (log, _) = next_boot(&mut node);
    assert_eq!(rows(&log, "beta", "final"), 1, "beta finished: {log:?}");
    assert_eq!(rows(&log, "alpha", "final"), 0, "alpha never did: {log:?}");
}

/// FR-005: a declaration that cannot stand fails before anything listens,
/// and the store it opened is shut with its outcome recorded.
#[test]
fn fr005_a_bad_declaration_fails_before_any_request_is_accepted() {
    for (mode, says) in [
        (MODE_EMPTY_NAME, "empty name"),
        (MODE_DUPLICATE, "declared twice"),
        (MODE_DECLARE_ERROR, "refuses to declare"),
    ] {
        let node = node(mode);
        let finished = node.run("serve");
        assert_ne!(finished.code, Some(0), "{mode}\n{}", finished.logs());
        assert!(
            finished.stderr.contains(says),
            "{mode}: {says}\n{}",
            finished.logs()
        );
        assert!(
            !finished.stdout.contains("listening on"),
            "{mode}: nothing listened\n{}",
            finished.logs()
        );
        assert!(
            locks_released(&node.data_dir),
            "{mode}: the store is released"
        );
        let record = node.stop_record().expect("a stop record");
        let order = phase_order(&record);
        assert!(order.contains(&"store_shutdown"), "{mode}: {order:?}");
        assert!(
            !order.contains(&"service_join"),
            "{mode}: nothing started: {order:?}"
        );
        assert!(
            reasons(&record)
                .iter()
                .any(|r| matches!(r, Reason::ServeError { error } if error.contains(says))),
            "{mode}: the record names the refusal"
        );
    }
}

/// FR-007, AC-5: a cell written before spec 047 serves and stops exactly as
/// it did, with no service phase in its record.
#[test]
fn fr007_a_cell_without_services_keeps_its_lifecycle() {
    let node = node(MODE_PLAIN);
    let mut cell = node.spawn("serve");
    node.wait_ready(&mut cell, READY_BUDGET);
    cell.sigterm();
    let finished = cell.wait(Duration::from_secs(60));
    assert_eq!(finished.code, Some(0), "{}", finished.logs());
    let record = node.stop_record().expect("a stop record");
    assert_eq!(record.outcome, Some(Outcome::Confirmed));
    assert_eq!(
        phase_order(&record),
        [
            "readiness_window",
            "stream_drain",
            "connection_drain",
            "denial_drain",
            "store_shutdown"
        ],
        "B-9: the pre-047 phases, nothing more"
    );
}
