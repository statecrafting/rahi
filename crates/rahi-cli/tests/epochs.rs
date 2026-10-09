//! Spec 041 end to end, against real process fixtures.
//!
//! The test binary is the fixture, as in spec 043's stop suite: a child
//! started through `stop_fixture::Node` runs a verb against its stop cell,
//! whose deny route makes a ledgered denial on every request.
//!
//! - FR-007 (B-13, B-14): with the cell serving, `ledger verify` and `ledger
//!   export` run beside it, attached to its node and as a pure store client,
//!   and the export carries its coverage document.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod stop_fixture;

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::Command;

use stop_fixture::{DENY_PATH, Finished, Node, READY_BUDGET, http_get};

fixture_entry!();

fn finished(cmd: &mut Command) -> Finished {
    let output = cmd.output().unwrap();
    Finished {
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// The record hashes of an exported chain's records, in file order. The
/// segment reference lines carry no `record_hash`.
fn record_hashes(jsonl: &str) -> Vec<String> {
    jsonl
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter_map(|v| {
            v.get("record_hash")
                .and_then(|h| h.as_str())
                .map(str::to_owned)
        })
        .collect()
}

fn read_coverage(export: &Path) -> serde_json::Value {
    let path = rahi_cli::coverage_path(export);
    serde_json::from_slice(
        &std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())),
    )
    .unwrap()
}

/// `node`'s command for `verb`, as a pure client of its running node: its
/// own empty data directory with a copy of the keys, and the node named as
/// the cluster (spec 032 B-6).
fn as_store_client(node: &Node, verb: &str, job: &Path) -> Command {
    let keys = job.join("keys");
    std::fs::create_dir_all(&keys).unwrap();
    for entry in std::fs::read_dir(node.data_dir.join("keys")).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), keys.join(entry.file_name())).unwrap();
    }
    std::fs::set_permissions(&keys, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut cmd = node.command(verb);
    cmd.env_remove("RAHI_HIQLITE_API_ADDR")
        .env_remove("RAHI_HIQLITE_RAFT_ADDR")
        .env("RAHI_DATA_DIR", job)
        .env(rahi_ops::ENV_STORE_CLIENT, "true")
        .env(
            rahi_ops::ENV_HIQ_NODES,
            format!(
                "1 {} {}",
                node.var("RAHI_HIQLITE_RAFT_ADDR"),
                node.var("RAHI_HIQLITE_API_ADDR")
            ),
        );
    cmd
}

/// FR-007, B-13, B-14.
#[test]
fn fr007_a_live_cell_is_verified_and_exported_beside_its_replica() {
    let node = Node::new();
    let mut cell = node.spawn("serve");
    node.wait_ready(&mut cell, READY_BUDGET);
    for _ in 0..3 {
        let (status, _) = http_get(&node.listen, DENY_PATH).unwrap();
        assert_eq!(status, 403, "the stop cell's route is a ledgered denial");
    }

    // B-14: attached to the running replica's node.
    let verify = node.run("ledger verify");
    assert_eq!(
        verify.code,
        Some(0),
        "ledger verify beside serve\n{}",
        verify.logs()
    );
    let out = tempfile::tempdir().unwrap();
    let export = out.path().join("attached.jsonl");
    let run = node.run(&format!("ledger export {}", export.display()));
    assert_eq!(
        run.code,
        Some(0),
        "ledger export beside serve\n{}",
        run.logs()
    );
    let jsonl = std::fs::read_to_string(&export).unwrap();
    let hashes = record_hashes(&jsonl);
    assert!(!hashes.is_empty(), "the export holds the resident chain");

    // B-13: the coverage document beside it.
    let coverage = read_coverage(&export);
    assert_eq!(coverage["depth"], "resident");
    assert_eq!(coverage["resident"]["count"], hashes.len());
    assert_eq!(coverage["sealed_not_included"], serde_json::json!([]));
    assert!(
        coverage["verifying_key"]
            .as_str()
            .is_some_and(|k| !k.is_empty())
    );
    let epoch = &coverage["current_epoch"];
    assert_eq!(epoch["type"], rahi_ledger::EPOCH_REF_TYPE, "{coverage}");
    assert_eq!(epoch["number"], 0, "no deploy step has appended an epoch");
    assert_eq!(
        epoch["chain"],
        hashes[0].as_str(),
        "the chain is its genesis record"
    );
    assert_eq!(epoch["epoch"], hashes[0].as_str(), "epoch 0 is the genesis");
    let gaps = coverage["gaps"].as_array().unwrap();
    assert!(gaps.iter().any(|g| g.as_str().unwrap().contains("015 D-5")));
    assert!(gaps.iter().any(|g| g.as_str().unwrap().contains("035 B-4")));

    // B-14: and as a pure store client, opening no node of its own.
    let job = tempfile::tempdir().unwrap();
    let client_export = out.path().join("client.jsonl");
    let run = finished(&mut as_store_client(
        &node,
        &format!("ledger export {}", client_export.display()),
        job.path(),
    ));
    assert_eq!(
        run.code,
        Some(0),
        "ledger export as a store client\n{}",
        run.logs()
    );
    assert!(
        !job.path().join("app-store").join("state_machine").exists(),
        "a client opens no node of its own"
    );
    let client_hashes = record_hashes(&std::fs::read_to_string(&client_export).unwrap());
    assert!(
        client_hashes.starts_with(&hashes),
        "a later export is the same chain, read no earlier"
    );
    let run = finished(&mut as_store_client(&node, "ledger verify", job.path()));
    assert_eq!(
        run.code,
        Some(0),
        "ledger verify as a store client\n{}",
        run.logs()
    );

    cell.sigterm();
    let stopped = cell.wait(std::time::Duration::from_secs(60));
    assert_eq!(stopped.code, Some(0), "{}", stopped.logs());
}
