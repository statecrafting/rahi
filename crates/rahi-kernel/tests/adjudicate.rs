//! Adjudication, the facades, and the denial path (spec 015 FR-003, FR-005,
//! B-5, B-6, B-8).
//!
//! The pure cases need no store: adjudication is a function of the manifest
//! and the roster, which is the property that lets an auditor re-derive a
//! ledgered decision. The rest boot a real single-voter cell, because a
//! denial that is not actually written to a real chain is not evidence of
//! anything.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]

use std::net::{SocketAddr, TcpListener};
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use rahi_kernel::{
    CapabilityKind, Egress, GateOutcome, Governed, Kernel, KernelOptions, Manifest, Request,
    Secrets, ServiceName, Verdict, observe,
};
use rahi_ledger::{Decision, Ledger, LedgerSigner, Outcome};
use rahi_store::{EncKey, EncKeys, Store, StoreConfig, StoreHandle, StoreSecrets};
use rahi_types::{Revision, Sub};

const VALID: &str = include_str!("../testdata/manifests/valid.toml");
const CHANGED_GRANT: &str = include_str!("../testdata/manifests/changed-grant.toml");

fn manifest() -> Manifest {
    Manifest::parse(VALID).expect("the fixture manifest parses")
}

fn service(name: &str) -> ServiceName {
    ServiceName::parse(name).expect("a well-formed service name")
}

/// Adjudicate `request` against the fixture manifest, with no kernel booted.
fn verdict(manifest: &Manifest, request: &Request) -> Verdict {
    manifest
        .gate()
        .expect("the gate assembles")
        .evaluate(&request.to_context())
}

fn request(service_name: &str, kind: CapabilityKind, resource: &str) -> Request {
    Request::new(
        service(service_name),
        kind,
        resource,
        Sub::new("rauthy-subject"),
    )
}

// ---------------------------------------------------------------- pure cases

#[test]
fn an_ungranted_kind_is_denied() {
    let manifest = manifest();
    // `notes` may read, write, and transact on `notes`; nothing grants it a
    // migration on `audit`, and nothing grants `audit` to anyone at all.
    let denied = verdict(
        &manifest,
        &request("notes", CapabilityKind::DbWrite, "audit"),
    );
    assert_eq!(denied.outcome, GateOutcome::Deny);
    assert!(denied.reason.contains("no_grant"), "{}", denied.reason);
    assert!(denied.reason.contains("db.write"), "{}", denied.reason);
    assert!(denied.reason.contains("audit"), "{}", denied.reason);

    let allowed = verdict(
        &manifest,
        &request("notes", CapabilityKind::DbWrite, "notes"),
    );
    assert!(allowed.is_allow(), "{}", allowed.reason);
}

#[test]
fn a_key_prefix_grant_admits_its_prefix_and_denies_another() {
    let manifest = manifest();
    let put = |key: &str| {
        verdict(
            &manifest,
            &request("notes", CapabilityKind::KvPut, "cache").with_key(key),
        )
    };
    assert!(put("demo:x").is_allow(), "demo: is the granted prefix");

    let denied = put("rl:x");
    assert_eq!(denied.outcome, GateOutcome::Deny);
    assert!(denied.reason.contains("constraint"), "{}", denied.reason);
    assert!(denied.reason.contains("cache-put"), "{}", denied.reason);

    // A request that says nothing about its key is not admitted by silence.
    let silent = verdict(&manifest, &request("notes", CapabilityKind::KvPut, "cache"));
    assert_eq!(silent.outcome, GateOutcome::Deny);
}

#[test]
fn an_unknown_service_is_denied() {
    let denied = verdict(
        &manifest(),
        &request("ledger-scraper", CapabilityKind::DbRead, "notes"),
    );
    assert_eq!(denied.outcome, GateOutcome::Deny);
    assert!(
        denied.reason.contains("unknown_service"),
        "{}",
        denied.reason
    );
}

#[test]
fn an_egress_wildcard_admits_a_subdomain_and_denies_a_stranger() {
    let manifest = manifest();
    let reach = |host: &str| {
        verdict(
            &manifest,
            &request("webhooks", CapabilityKind::HttpEgress, host).with_host(host),
        )
    };
    assert!(reach("api.example.com").is_allow());
    assert!(reach("hooks.eu.example.com").is_allow());
    assert_eq!(reach("example.com").outcome, GateOutcome::Deny);
    assert_eq!(reach("evil-example.com").outcome, GateOutcome::Deny);
}

#[test]
fn a_context_the_kernel_cannot_read_is_denied_rather_than_admitted() {
    let gate = manifest().gate().expect("the gate assembles");
    for ctx in [
        rahi_kernel::GateContext::new("db.read"),
        rahi_kernel::GateContext::new("db.read").with_attr("service", "notes".into()),
        rahi_kernel::GateContext::new("db.explode")
            .with_attr("service", "notes".into())
            .with_attr("resource", "notes".into()),
    ] {
        let d = gate.evaluate(&ctx);
        assert_eq!(d.outcome, GateOutcome::Deny, "{ctx:?} was admitted");
    }
}

#[test]
fn the_capability_check_is_first_and_the_roster_follows() {
    let kernel_checks = manifest().gate().expect("assembles");
    assert_eq!(kernel_checks.check_ids(), vec!["grants", "secrets"]);
}

#[test]
fn the_roster_denies_a_payload_that_carries_a_credential() {
    let denied = verdict(
        &manifest(),
        &request("notes", CapabilityKind::DbWrite, "notes")
            .with_payload("api_key = 'aaaaaaaaaaaaaaaaaaaaaaaaaaaa'"),
    );
    assert_eq!(denied.outcome, GateOutcome::Deny);
    assert!(denied.blocking, "a leaked credential blocks regardless");
    assert!(denied.reason.contains("secrets"), "{}", denied.reason);
}

// ------------------------------------------------------------ a booted cell

fn free_addr() -> SocketAddr {
    TcpListener::bind("127.0.0.1:0")
        .expect("a free port")
        .local_addr()
        .expect("its address")
}

fn store_config(data_dir: &Path) -> StoreConfig {
    StoreConfig {
        node_id: 1,
        nodes: Vec::new(),
        data_dir: data_dir.to_path_buf(),
        raft_addr: free_addr(),
        api_addr: free_addr(),
        secrets: StoreSecrets {
            secret_raft: "raft-secret-for-tests-0000".to_owned(),
            secret_api: "api-secret-for-tests-00000".to_owned(),
            enc_keys: EncKeys {
                active: "test".to_owned(),
                keys: vec![EncKey {
                    id: "test".to_owned(),
                    key: vec![7u8; 32],
                }],
            },
        },
        backup_keep_days: 1,
        s3: None,
    }
}

struct Cell {
    kernel: Kernel,
    store: StoreHandle,
    _node: Store,
    _dir: tempfile::TempDir,
}

/// Boot a single-voter cell whose chain is rooted at `manifest`.
async fn boot(manifest: Manifest) -> Cell {
    boot_rooted_at(manifest.clone(), &manifest, 64).await
}

/// Boot `manifest` against a chain rooted at `root`'s hash, with `capacity`
/// slots in the denial queue.
async fn boot_rooted_at(manifest: Manifest, root: &Manifest, capacity: usize) -> Cell {
    let dir = tempfile::tempdir().expect("a temp dir");
    let node = Store::open(&store_config(&dir.path().join("hiqlite")))
        .await
        .expect("a single-voter node opens");
    let store = node.handle();
    let ledger = Ledger::open(
        store.clone(),
        LedgerSigner::from_seed([9u8; 32]),
        root.hash().expect("hashes"),
    )
    .await
    .expect("the chain opens");
    let kernel = Kernel::boot_with(
        manifest,
        store.clone(),
        ledger,
        KernelOptions {
            queue_capacity: capacity,
            clock: Some(std::sync::Arc::new(|| 1_767_225_600)),
        },
    )
    .await
    .expect("the kernel boots against its own manifest");
    Cell {
        kernel,
        store,
        _node: node,
        _dir: dir,
    }
}

/// Every decision the kernel has observed in this test binary.
///
/// Registering the observers here also silences the stderr floor the kernel
/// falls back to when nobody is listening (D-7), and exercises the
/// [`observe::on_failure`] hook the appender reports through.
fn observed() -> &'static Mutex<Vec<Decision>> {
    static SEEN: OnceLock<Mutex<Vec<Decision>>> = OnceLock::new();
    SEEN.get_or_init(|| {
        observe::on_decision(|decision| {
            observed()
                .lock()
                .expect("not poisoned")
                .push(decision.clone());
        });
        observe::on_failure(|id, err| {
            reported()
                .lock()
                .expect("not poisoned")
                .push((id.as_str().to_owned(), err.kind().to_owned()));
        });
        Mutex::new(Vec::new())
    })
}

/// Every failure the appender or the queue has reported.
fn reported() -> &'static Mutex<Vec<(String, String)>> {
    static REPORTED: OnceLock<Mutex<Vec<(String, String)>>> = OnceLock::new();
    REPORTED.get_or_init(|| Mutex::new(Vec::new()))
}

/// The decisions this test's own actor produced.
fn observed_for(actor: &str) -> Vec<Decision> {
    observed()
        .lock()
        .expect("not poisoned")
        .iter()
        .filter(|d| d.actor.as_str() == actor)
        .cloned()
        .collect()
}

#[tokio::test]
async fn a_denial_produces_exactly_one_decision_with_the_requests_actor() {
    let _ = observed();
    let cell = boot(manifest()).await;
    let actor = Sub::new("fr003-denial-actor");

    let request = Request::new(
        service("notes"),
        CapabilityKind::DbWrite,
        "audit",
        actor.clone(),
    )
    .at(Revision::new(41));
    let err = cell
        .kernel
        .admit(&request)
        .await
        .expect_err("audit is not granted to notes");
    assert_eq!(err.kind(), "denied");

    let seen = observed_for(actor.as_str());
    assert_eq!(seen.len(), 1, "exactly one decision: {seen:?}");
    let decision = &seen[0];
    assert_eq!(decision.actor, actor);
    assert_eq!(decision.outcome, Outcome::Deny);
    assert_eq!(decision.kind.as_str(), "db.write");
    assert_eq!(decision.at, Revision::new(41));
    assert!(
        decision.capability.is_none(),
        "nothing covered it, so nothing is named"
    );
    assert!(
        err.message().starts_with(decision.id.as_str()),
        "the error carries the decision id: {err}"
    );
    let payload = decision.payload.as_value();
    assert_eq!(payload["service"], "notes");
    assert_eq!(payload["resource"], "audit");
    assert_eq!(payload["manifest"], cell.kernel.manifest_hash().as_str());
    assert_eq!(payload["wall_time"], 1_767_225_600_u64);

    // And it reaches the chain, without the request path having waited for it.
    cell.kernel
        .flush(Duration::from_secs(5))
        .await
        .expect("the appender drains");
    let ledger = Ledger::open(
        cell.store.clone(),
        LedgerSigner::from_seed([9u8; 32]),
        cell.kernel.manifest_hash().clone(),
    )
    .await
    .expect("the chain reopens and verifies");
    let ids: Vec<String> = ledger
        .records()
        .await
        .expect("records")
        .iter()
        .map(|r| r.record.id.clone())
        .collect();
    assert!(ids.contains(&decision.id.as_str().to_owned()), "{ids:?}");
}

#[tokio::test]
async fn a_constraint_denial_names_the_capability_it_fell_out_of() {
    let _ = observed();
    let cell = boot(manifest()).await;
    let actor = Sub::new("constraint-denial-actor");

    let request = Request::new(
        service("notes"),
        CapabilityKind::KvPut,
        "cache",
        actor.clone(),
    )
    .with_key("rl:client-7");
    cell.kernel
        .admit(&request)
        .await
        .expect_err("rl: is outside the granted prefix");

    let seen = observed_for(actor.as_str());
    assert_eq!(seen.len(), 1);
    assert_eq!(
        seen[0].capability.as_ref().map(|c| c.as_str().to_owned()),
        Some("cache-put".to_owned()),
        "the nearest capability is the one the request fell out of"
    );
    assert_eq!(seen[0].payload.as_value()["key"], "rl:client-7");
}

#[tokio::test]
async fn a_ledger_rooted_at_another_manifest_will_not_boot() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let node = Store::open(&store_config(&dir.path().join("hiqlite")))
        .await
        .expect("a node opens");
    let other = Manifest::parse(CHANGED_GRANT).expect("parses");
    let ledger = Ledger::open(
        node.handle(),
        LedgerSigner::from_seed([9u8; 32]),
        other.hash().expect("hashes"),
    )
    .await
    .expect("the chain opens");

    let err = Kernel::boot(manifest(), node.handle(), ledger)
        .await
        .expect_err("the manifest changed without a deploy genesis record");
    assert_eq!(err.kind(), "integrity");
    assert!(
        err.message().contains("deploy genesis record"),
        "{}",
        err.message()
    );
}

#[tokio::test]
async fn a_facade_performs_on_allow_and_refuses_otherwise() {
    let cell = boot(manifest()).await;
    let actor = Sub::new("facade-actor");
    cell.store
        .execute(
            "CREATE TABLE IF NOT EXISTS notes (id TEXT PRIMARY KEY)",
            vec![],
        )
        .await
        .expect("the table is created out of band");

    let writer = Governed::new(
        &cell.kernel,
        "notes",
        CapabilityKind::DbWrite,
        "notes",
        cell.store.clone(),
    )
    .expect("notes is a declared service");
    writer
        .execute(&actor, "INSERT INTO notes (id) VALUES ('n1')", vec![])
        .await
        .expect("the grant covers it");

    // Same facade, an operation it was not declared for.
    let err = writer
        .query::<(String,)>(&actor, "SELECT id FROM notes", vec![])
        .await
        .expect_err("one facade is one capability");
    assert_eq!(err.kind(), "validation");

    // The grant is per resource: `audit` is declared but granted to nobody.
    let audit = Governed::new(
        &cell.kernel,
        "notes",
        CapabilityKind::DbWrite,
        "audit",
        cell.store.clone(),
    )
    .expect("notes is a declared service");
    let err = audit
        .execute(&actor, "INSERT INTO audit (id) VALUES ('a1')", vec![])
        .await
        .expect_err("audit is not granted");
    assert_eq!(err.kind(), "denied");

    // A facade for a service the manifest never declared is a typo, not a
    // policy question, and is refused before anything is adjudicated.
    let err = Governed::new(
        &cell.kernel,
        "scraper",
        CapabilityKind::DbRead,
        "notes",
        cell.store.clone(),
    )
    .expect_err("scraper is not a service");
    assert_eq!(err.kind(), "validation");
}

#[tokio::test]
async fn the_kv_and_secret_and_egress_facades_hold_the_same_line() {
    let cell = boot(manifest()).await;
    let actor = Sub::new("mixed-facade-actor");

    let cache = Governed::new(
        &cell.kernel,
        "notes",
        CapabilityKind::KvPut,
        "cache",
        cell.store.clone(),
    )
    .expect("declared");
    cache
        .kv_put(&actor, "demo:one", &1_u32, None)
        .await
        .expect("demo: is granted");
    let err = cache
        .kv_put(&actor, "rl:one", &1_u32, None)
        .await
        .expect_err("rl: is not");
    assert_eq!(err.kind(), "denied");

    let secrets = Secrets::new(std::sync::Arc::new(|name: &str| {
        (name == "webhook_token").then(|| "s3cr3t".to_owned())
    }));
    let token = Governed::new(
        &cell.kernel,
        "webhooks",
        CapabilityKind::SecretRead,
        "webhook_token",
        secrets.clone(),
    )
    .expect("declared");
    assert_eq!(token.read(&actor).await.expect("granted"), "s3cr3t");

    let other = Governed::new(
        &cell.kernel,
        "webhooks",
        CapabilityKind::SecretRead,
        "root_password",
        secrets,
    )
    .expect("declared");
    assert_eq!(
        other.read(&actor).await.expect_err("not granted").kind(),
        "denied"
    );

    let egress = Governed::new(
        &cell.kernel,
        "webhooks",
        CapabilityKind::HttpEgress,
        "*.example.com",
        Egress,
    )
    .expect("declared");
    assert_eq!(
        egress
            .permit(&actor, "api.example.com")
            .await
            .expect("granted")
            .host(),
        "api.example.com"
    );
    assert_eq!(
        egress
            .permit(&actor, "api.elsewhere.test")
            .await
            .expect_err("not granted")
            .kind(),
        "denied"
    );
}

#[tokio::test]
async fn the_appender_drains_under_a_failing_ledger_without_blocking_the_request_path() {
    let _ = observed();
    // A deliberately shallow queue, so the appender falls behind and both
    // halves of the bounded-channel contract are exercised at once.
    let cell = boot_rooted_at(manifest(), &manifest(), 8).await;
    let actor = Sub::new("fr005-actor");
    let failures_before = observe::ledger_failures();
    let dropped_before = observe::dropped();

    // Break the chain out from under the appender: every append now fails at
    // the head read. The request path must not notice.
    cell.store
        .execute("DROP TABLE kernel_decisions", vec![])
        .await
        .expect("the table is dropped");

    let denials = 100_u64;
    let started = Instant::now();
    for n in 0..denials {
        let request = Request::new(
            service("notes"),
            CapabilityKind::DbWrite,
            "audit",
            actor.clone(),
        )
        .at(Revision::new(n));
        let err = cell.kernel.admit(&request).await.expect_err("denied");
        assert_eq!(err.kind(), "denied");
    }
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_millis(500),
        "{denials} denials took {elapsed:?}; the request path waited on the ledger"
    );

    // Every denial was observed on the request path, whatever became of the
    // append afterwards (spec 015 B-7).
    assert_eq!(observed_for(actor.as_str()).len() as u64, denials);

    cell.kernel
        .flush(Duration::from_secs(30))
        .await
        .expect("the appender is not stuck");

    // Nothing vanished silently: each denial either failed to append or was
    // refused by the full queue, and both raise a counter and reach the
    // failure observer.
    let failed = observe::ledger_failures() - failures_before;
    let dropped = observe::dropped() - dropped_before;
    assert!(
        failed > 0,
        "the appender kept draining under a failing ledger"
    );
    assert_eq!(
        failed + dropped,
        denials,
        "{} plus {} accounts for every denial",
        observe::METRIC_LEDGER_FAILURES,
        observe::METRIC_DECISIONS_DROPPED
    );
    let reported = reported().lock().expect("not poisoned").len() as u64;
    assert!(
        reported >= failed + dropped,
        "every failure reached an observer rather than being swallowed"
    );
}
