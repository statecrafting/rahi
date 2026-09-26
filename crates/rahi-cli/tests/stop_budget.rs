//! Spec 043 B-9: the stop budget, composed (FR-007) and measured (AC-7).
//!
//! The composition check is a test over the configured values and the
//! shipped manifests: `SERVE_GRACE >= S + C + D + H`, and the pod's
//! `terminationGracePeriodSeconds` and the documented `docker stop -t` are
//! each `>= SERVE_GRACE + R`. It reads the values the code runs with, not
//! copies of them.
//!
//! The workload check is AC-7's bounded series under the declared workload:
//! streams open up to spec 026's per-identity limit, a 200-denial backlog in
//! flight, and Rauthy running. Every run must be confirmed, and the largest
//! measured time from SIGTERM to exit, times 1.5 (D-6), must not exceed the
//! configured grace. Each run's phases, outcome, time to exit and time to
//! both owner locks' release are printed (and written to
//! `RAHI_STOP_SERIES_OUT` when it names a file), so the series is recorded
//! evidence. Without Rauthy the series runs `serve` alone and says the
//! Rauthy leg did not execute; `RAHI_REQUIRE_RAUTHY=1` makes that a failure.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod stop_fixture;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rahi_ops::stop::{self, Outcome};
use stop_fixture::{Node, OpenStream, deny_in_flight, tally};

fixture_entry!();

/// The repository root, where the shipped manifests are.
fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn seconds_after(text: &str, key: &str) -> Vec<u64> {
    text.lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .filter_map(|line| line.split_once(key).map(|(_, rest)| rest))
        .filter_map(|rest| {
            rest.trim()
                .split(|c: char| !c.is_ascii_digit())
                .next()
                .and_then(|n| n.parse().ok())
        })
        .collect()
}

/// FR-007: the configured bounds compose, and the shipped manifests carry
/// graces that cover them.
#[test]
fn fr007_the_configured_bounds_and_the_shipped_graces_compose() {
    // Each phase's bound, read from the code that enforces it.
    let s = rahi_edge::StreamOptions::default().drain_timeout;
    let c = rahi_cli::serve::DRAIN_BUDGET;
    let d = rahi_cli::serve::DEFAULT_DENIAL_DRAIN_TIMEOUT;
    let h = rahi_store::SHUTDOWN_WAIT;
    let r = rahi_ops::supervise::SHUTDOWN_TERM_GRACE;
    let serve_grace = rahi_ops::supervise::SERVE_GRACE;
    assert_eq!(
        [s, c, d, h, r],
        [
            stop::STREAM_DRAIN,
            stop::CONNECTION_DRAIN,
            stop::DENIAL_DRAIN,
            stop::STORE_SHUTDOWN,
            stop::RAUTHY_STOP
        ],
        "the stop module's names for the bounds are the bounds"
    );
    assert_eq!(h, Duration::from_secs(15), "hiqlite's own SHUTDOWN_WAIT");
    stop::check_composition([s, c, d, h], serve_grace, r, stop::CONTAINER_GRACE).unwrap();
    assert_eq!(
        serve_grace,
        Duration::from_secs(40),
        "B-9: 40 s with the defaults"
    );
    assert_eq!(stop::CONTAINER_GRACE, Duration::from_secs(50), "B-9: 50 s");

    // The pod's grace.
    let statefulset = std::fs::read_to_string(repo().join("deploy/k8s/statefulset.yaml")).unwrap();
    let pod = seconds_after(&statefulset, "terminationGracePeriodSeconds:");
    assert_eq!(pod.len(), 1, "one pod grace in the StatefulSet: {pod:?}");
    stop::check_composition([s, c, d, h], serve_grace, r, Duration::from_secs(pod[0])).unwrap();

    // The documented `docker stop -t`: the README and the smoke script.
    for doc in ["deploy/README.md", "docker/smoke.sh"] {
        let text = std::fs::read_to_string(repo().join(doc)).unwrap();
        let graces = seconds_after(&text, "docker stop -t ");
        assert!(!graces.is_empty(), "{doc} documents `docker stop -t`");
        for grace in graces {
            stop::check_composition([s, c, d, h], serve_grace, r, Duration::from_secs(grace))
                .unwrap_or_else(|err| panic!("{doc}: {err}"));
        }
    }
}

/// One run of AC-7's series, as recorded.
#[derive(Debug)]
struct Run {
    run: usize,
    entry: &'static str,
    outcome: String,
    phases: Vec<stop::PhaseTime>,
    sigterm_to_exit_ms: u128,
    sigterm_to_locks_released_ms: Option<u128>,
    denials: String,
    streams: usize,
}

/// Every `hiqlite-owner.lock` under `dir`: the app store's, and Rauthy's.
fn owner_locks(dir: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, into: &mut Vec<PathBuf>) {
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

fn all_free(locks: &[PathBuf]) -> bool {
    locks.iter().all(|path| {
        std::fs::File::open(path).is_ok_and(|file| {
            let free = file.try_lock().is_ok();
            let _ = file.unlock();
            free
        })
    })
}

/// AC-7: the bounded series under the declared workload. Every run must be
/// confirmed, and the measured maximum times 1.5 must fit the grace.
#[test]
fn ac7_graceful_stops_under_the_declared_workload_are_confirmed_within_the_grace() {
    let runs: usize = std::env::var("RAHI_STOP_SERIES_RUNS")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(3);
    let rauthy = stop_fixture::test_rauthy();
    let (entry, grace) = match &rauthy {
        Some(_) => ("supervise", stop::CONTAINER_GRACE),
        None => {
            eprintln!(
                "AC-7: the Rauthy leg did not execute (RAHI_TEST_RAUTHY is not set); \
                 the series runs serve alone against SERVE_GRACE"
            );
            ("serve", stop::SERVE_GRACE)
        }
    };
    let limit = rahi_edge::stream::DEFAULT_MAX_CONCURRENT_STREAMS;

    let mut series = Vec::new();
    for run in 1..=runs {
        let node = match &rauthy {
            Some(bin) => Node::with_rauthy(bin),
            None => Node::new(),
        };
        let mut cell = node.spawn(entry);
        node.wait_ready(&mut cell, Duration::from_secs(180));
        let locks = owner_locks(&node.data_dir);
        assert!(!locks.is_empty(), "the app store's owner lock exists");

        // Streams up to the per-identity limit, each read by a client that
        // closes when the cell says it is shutting down.
        let readers: Vec<_> = (0..limit)
            .map(|_| {
                let stream = OpenStream::open(&node.listen);
                std::thread::spawn(move || stream.read_to_close())
            })
            .collect();
        // The backlog, released together; SIGTERM while it is in flight.
        let denials = deny_in_flight(&node.listen, 200);
        std::thread::sleep(Duration::from_millis(20));
        let signalled = Instant::now();
        cell.sigterm();

        let mut released = None;
        let exited = loop {
            if released.is_none() && all_free(&locks) {
                released = Some(signalled.elapsed());
            }
            if let Some(status) = cell.child.try_wait().unwrap() {
                break (status, signalled.elapsed());
            }
            assert!(
                signalled.elapsed() < grace * 2,
                "run {run} did not exit\n{}",
                cell.logs()
            );
            std::thread::sleep(Duration::from_millis(5));
        };
        let finished = cell.wait(Duration::from_secs(5));
        if released.is_none() && all_free(&locks) {
            released = Some(signalled.elapsed());
        }
        for reader in readers {
            reader.join().unwrap();
        }
        let answers: Vec<Option<u16>> = denials.into_iter().map(|h| h.join().unwrap()).collect();
        let record = node.stop_record().expect("the run left its record");
        series.push(Run {
            run,
            entry,
            outcome: match &record.outcome {
                Some(Outcome::Confirmed) => "confirmed".to_owned(),
                Some(Outcome::Unconfirmed { reasons }) => format!(
                    "unconfirmed: {}",
                    reasons
                        .iter()
                        .map(stop::Reason::name)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                None => "none recorded".to_owned(),
            },
            phases: record.phases.clone(),
            sigterm_to_exit_ms: exited.1.as_millis(),
            sigterm_to_locks_released_ms: released.map(|d| d.as_millis()),
            denials: format!("{:?}", tally(&answers)),
            streams: limit,
        });
        assert_eq!(
            exited.0.code(),
            Some(0),
            "run {run} exits 0 on a confirmed stop\n{}",
            finished.logs()
        );
    }

    let record = serde_json::to_string_pretty(
        &series
            .iter()
            .map(|r| {
                serde_json::json!({
                    "run": r.run,
                    "entry": r.entry,
                    "outcome": r.outcome,
                    "phases": r.phases.iter().map(|p| serde_json::json!({
                        "phase": p.phase, "millis": p.millis, "bound_millis": p.bound_millis,
                    })).collect::<Vec<_>>(),
                    "sigterm_to_exit_ms": r.sigterm_to_exit_ms,
                    "sigterm_to_locks_released_ms": r.sigterm_to_locks_released_ms,
                    "denials": r.denials,
                    "streams": r.streams,
                })
            })
            .collect::<Vec<_>>(),
    )
    .unwrap();
    eprintln!("AC-7 series ({entry}, grace {grace:?}):\n{record}");
    if let Ok(out) = std::env::var("RAHI_STOP_SERIES_OUT") {
        std::fs::write(out, &record).unwrap();
    }
    for run in &series {
        assert_eq!(run.outcome, "confirmed", "every run is confirmed: {run:?}");
    }
    let max = series.iter().map(|r| r.sigterm_to_exit_ms).max().unwrap();
    assert!(
        max * 3 / 2 <= grace.as_millis(),
        "the measured maximum {max} ms times 1.5 exceeds the grace {grace:?}"
    );
}
