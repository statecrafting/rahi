//! Spec 041 end to end, against real process fixtures.
//!
//! The test binary is the fixture, as in spec 043's stop suite: a child
//! started through `stop_fixture::Node` runs a verb against its stop cell,
//! whose deny route makes a ledgered denial on every request.
//!
//! - FR-003: each producer of `testdata/binding/` in order, the deploy step,
//!   a served denial, and the consumer's join written here, not in the
//!   chassis;
//! - FR-004: a second binary on the same volume, and another declared image,
//!   are reported as mismatches, and the join names a Statement that does
//!   not cover the binary;
//! - FR-005: the deploy step is idempotent, and a restore is an epoch;
//! - FR-006: a rollback to an older build is a new epoch;
//! - FR-007 (B-13, B-14): with the cell serving, `ledger verify` and `ledger
//!   export` run beside it, attached to its node and as a pure store client,
//!   and the export carries its coverage document.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod stop_fixture;

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::Command;

use axum::Router;
use rahi_cli::Cell;
use rahi_edge::AppState;
use rahi_ledger::{Cause, DeploymentEpoch, SignedRecord};
use rahi_store::Migration;
use stop_fixture::{DENY_PATH, FIXTURE, FIXTURE_VERB, Finished, Node, READY_BUDGET, http_get};

/// Set on a child to `v1` or `v2` to run that rollback cell (FR-006); unset,
/// the child runs the stop cell.
const CELL_VAR: &str = "RAHI_TEST_EPOCH_CELL";

/// The fixture entry: a rollback cell when [`CELL_VAR`] names one, the stop
/// cell otherwise.
#[test]
fn fixture_cell() {
    let Ok(verb) = std::env::var(FIXTURE_VERB) else {
        return;
    };
    let args: Vec<String> = verb.split_whitespace().map(str::to_owned).collect();
    let env = rahi_cli::process_env();
    let code = match std::env::var(CELL_VAR).as_deref() {
        Ok("v1") => rahi_cli::run_with::<V1>(&args, &env),
        Ok("v2") => rahi_cli::run_with::<V2>(&args, &env),
        _ => {
            stop_fixture::fixture_main();
            return;
        }
    };
    std::process::exit(code);
}

const V1_MANIFEST: &str = r#"schema_version = "1.0.0"

[app]
name = "epoch-cell"
org = "rahi-tests"

[resources]
tables = ["items"]

[[capabilities]]
id = "items-read"
kind = "db.read"
resource = "items"

[services.items]
capabilities = ["items-read"]

[ledger]
schema_version = "1.0.0"
max_record_bytes = 65536

[observability]
metrics_path = "/metrics"
otel = false

[auth]
operator_role = "epoch-cell-operator"

[contract]
version = "1.0.0"
"#;

/// V1 with one added grant (FR-006).
const V2_MANIFEST: &str = r#"schema_version = "1.0.0"

[app]
name = "epoch-cell"
org = "rahi-tests"

[resources]
tables = ["items"]

[[capabilities]]
id = "items-read"
kind = "db.read"
resource = "items"

[[capabilities]]
id = "items-write"
kind = "db.write"
resource = "items"

[services.items]
capabilities = ["items-read", "items-write"]

[ledger]
schema_version = "1.0.0"
max_record_bytes = 65536

[observability]
metrics_path = "/metrics"
otel = false

[auth]
operator_role = "epoch-cell-operator"

[contract]
version = "1.0.0"
"#;

struct V1;
struct V2;

impl Cell for V1 {
    fn manifest() -> &'static str {
        V1_MANIFEST
    }
    fn migrations() -> &'static [Migration] {
        &[]
    }
    fn routes(_state: AppState) -> Router {
        Router::new()
    }
}

impl Cell for V2 {
    fn manifest() -> &'static str {
        V2_MANIFEST
    }
    fn migrations() -> &'static [Migration] {
        &[]
    }
    fn routes(_state: AppState) -> Router {
        Router::new()
    }
}

// ------------------------------------------------------------- helpers

fn sha256_hex(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{}", sha256_hex(bytes))
}

/// A copy of this test binary with one byte appended: the same fixture, a
/// different executable digest (FR-004, FR-006).
fn second_binary(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("second-fixture");
    let mut bytes = std::fs::read(std::env::current_exe().unwrap()).unwrap();
    bytes.push(b'\n');
    std::fs::write(&path, bytes).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// `node`'s command for `verb`, run from `exe` rather than this binary.
fn command_from(node: &Node, exe: &Path, verb: &str) -> Command {
    let mut cmd = Command::new(exe);
    cmd.args([FIXTURE, "--exact", "--nocapture", "--test-threads=1"]);
    for (key, _) in std::env::vars() {
        if key.starts_with("RAHI_") || key.starts_with("HQL_") {
            cmd.env_remove(key);
        }
    }
    cmd.envs(node.env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    cmd.env(FIXTURE_VERB, verb);
    cmd
}

/// Run the deploy step and return what it printed.
fn deploy(node: &Node, exe: Option<&Path>) -> Finished {
    let run = match exe {
        Some(exe) => finished(&mut command_from(node, exe, "migrate --adopt-manifest")),
        None => node.run("migrate --adopt-manifest"),
    };
    assert_eq!(run.code, Some(0), "the deploy step\n{}", run.logs());
    run
}

/// Serve from `exe` until ready, read `paths`, provoke `denials` ledgered
/// denials, then stop it with SIGTERM: the answers in order, and every
/// denial's decision id.
fn serve_and_read(
    node: &Node,
    exe: Option<&Path>,
    paths: &[&str],
    denials: usize,
) -> (Vec<String>, Vec<String>) {
    let mut child = match exe {
        Some(exe) => command_from(node, exe, "serve"),
        None => node.command("serve"),
    }
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .spawn()
    .unwrap();
    let deadline = std::time::Instant::now() + READY_BUDGET;
    while !matches!(http_get(&node.listen, rahi_edge::READYZ_PATH), Ok((200, _))) {
        assert!(
            child.try_wait().unwrap().is_none(),
            "serve exited before it was ready"
        );
        assert!(std::time::Instant::now() < deadline, "serve was not ready");
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    let answers = paths
        .iter()
        .map(|path| {
            let (status, body) = http_get(&node.listen, path).unwrap();
            assert_eq!(status, 200, "{path}: {body}");
            body
        })
        .collect();
    let ids = (0..denials)
        .map(|_| {
            let (status, body) = http_get(&node.listen, DENY_PATH).unwrap();
            assert_eq!(status, 403, "{body}");
            stop_fixture::decision_of(&body).expect("a denial names its decision")
        })
        .collect();
    let status = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    let exit = child.wait().unwrap();
    assert_eq!(exit.code(), Some(0), "serve stopped cleanly");
    (answers, ids)
}

/// Every record of the node's chain, through `ledger export`.
fn chain(node: &Node) -> Vec<SignedRecord> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("chain.jsonl");
    let run = node.run(&format!("ledger export {}", path.display()));
    assert_eq!(run.code, Some(0), "{}", run.logs());
    std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .filter_map(|line| SignedRecord::from_bytes(line.as_bytes()).ok())
        .collect()
}

/// The epochs of a chain, in chain order, with their record hashes.
fn epochs(records: &[SignedRecord]) -> Vec<(DeploymentEpoch, String)> {
    records
        .iter()
        .filter_map(|r| {
            DeploymentEpoch::of(r)
                .unwrap()
                .map(|e| (e, r.hash().unwrap().to_string()))
        })
        .collect()
}

fn payload(records: &[SignedRecord], id: &str) -> serde_json::Value {
    records
        .iter()
        .find(|r| r.record.id == id)
        .unwrap_or_else(|| panic!("{id} is in the chain"))
        .decision()
        .unwrap()
        .payload
        .as_value()
        .clone()
}

/// The fixture image, digest-pinned (040 B-5).
fn image(byte: char) -> (String, String) {
    let hex: String = std::iter::repeat_n(byte, 64).collect();
    (
        format!("ghcr.io/statecrafting/epoch-fixture@sha256:{hex}"),
        hex,
    )
}

fn template(name: &str) -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("testdata/binding")
            .join(name),
    )
    .unwrap()
}

/// The three producers' records, in order (section 7), each filled only
/// with digests of the records before it.
struct Producers {
    authority: Vec<u8>,
    statement: Vec<u8>,
    deployment: Vec<u8>,
}

impl Producers {
    fn compose(image_ref: &str, image_hex: &str, binary: &str) -> Self {
        let authority = template("authority-snapshot.json").into_bytes();
        let statement = template("build-provenance.json")
            .replace("{{IMAGE_NAME}}", image_ref.split('@').next().unwrap())
            .replace("{{IMAGE_SHA256}}", image_hex)
            .replace("{{BINARY_SHA256}}", binary.trim_start_matches("sha256:"))
            .replace("{{AUTHORITY_SHA256}}", &sha256_hex(&authority))
            .into_bytes();
        let deployment = template("deployment-record.json")
            .replace("{{STATEMENT_DIGEST}}", &digest(&statement))
            .replace("{{IMAGE}}", image_ref)
            .into_bytes();
        Self {
            authority,
            statement,
            deployment,
        }
    }

    /// `RAHI_DEPLOYMENT_REFS` naming all three (B-3).
    fn refs(&self) -> String {
        serde_json::json!({
            "build": {"type": "https://in-toto.io/Statement/v1", "digest": digest(&self.statement)},
            "deployment": {
                "type": "https://statecraft.ing/deployment-record/v0",
                "digest": digest(&self.deployment),
                "id": "fixture-rollout-1",
            },
            "authority": {
                "type": "https://spec-spine.dev/authority-snapshot/v0",
                "digest": digest(&self.authority),
            },
        })
        .to_string()
    }
}

/// The consumer's join (FR-003), written here because it is the consumer's
/// logic and not a chassis feature. It answers what failed to join.
fn join(
    producers: &Producers,
    epoch: &DeploymentEpoch,
    epoch_hash: &str,
    binding: &serde_json::Value,
    denial: &serde_json::Value,
) -> Result<(), String> {
    let statement: serde_json::Value = serde_json::from_slice(&producers.statement).unwrap();
    let materials = statement["predicate"]["buildDefinition"]["resolvedDependencies"]
        .as_array()
        .unwrap();
    let snapshot = sha256_hex(&producers.authority);
    if !materials
        .iter()
        .any(|m| m["digest"]["sha256"] == snapshot.as_str())
    {
        return Err(format!(
            "the Statement's materials do not name the snapshot {snapshot}"
        ));
    }
    let subjects: Vec<&str> = statement["subject"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["digest"]["sha256"].as_str())
        .collect();
    for (what, value) in [
        ("binary", epoch.artifact.binary.clone().unwrap_or_default()),
        (
            "image",
            epoch
                .artifact
                .image
                .clone()
                .and_then(|i| i.split_once("@sha256:").map(|(_, h)| format!("sha256:{h}")))
                .unwrap_or_default(),
        ),
    ] {
        let hex = value.trim_start_matches("sha256:");
        if !subjects.contains(&hex) {
            return Err(format!(
                "no Statement subject names the epoch's {what} {value}"
            ));
        }
    }
    let named = |r: &Option<rahi_ledger::Reference>| r.as_ref().map(|r| r.digest.clone());
    if named(&epoch.refs.build) != Some(digest(&producers.statement)) {
        return Err("refs.build is not the Statement".to_owned());
    }
    if named(&epoch.refs.deployment) != Some(digest(&producers.deployment)) {
        return Err("refs.deployment is not the deployment record".to_owned());
    }
    if named(&epoch.refs.authority) != Some(digest(&producers.authority)) {
        return Err("refs.authority is not the snapshot".to_owned());
    }
    if binding["epoch"]["ref"]["value"]["epoch"] != epoch_hash {
        return Err("the binding names another epoch".to_owned());
    }
    if denial["epoch"] != epoch_hash {
        return Err("the denial names another epoch".to_owned());
    }
    if denial["instance"] != binding["instance"]["id"]["value"] {
        return Err("the denial names another instance".to_owned());
    }
    Ok(())
}

/// FR-003: the composed consumer fixture, joined.
#[test]
fn fr003_the_producers_the_deploy_step_and_a_denial_join_in_order() {
    let mut node = Node::new();
    let exe = std::fs::read(std::env::current_exe().unwrap()).unwrap();
    let binary = digest(&exe);
    let (image_ref, image_hex) = image('a');
    let producers = Producers::compose(&image_ref, &image_hex, &binary);
    node.set_env("RAHI_ARTIFACT_IMAGE", &image_ref);
    node.set_env(rahi_ops::migrate::ENV_DEPLOYMENT_REFS, &producers.refs());

    let run = deploy(&node, None);
    assert!(
        run.stdout.contains("epoch: appended epoch 1 (deploy)"),
        "{}",
        run.logs()
    );

    let (answers, denials) = serve_and_read(&node, None, &[rahi_edge::binding::BINDING_PATH], 1);
    let binding: serde_json::Value = serde_json::from_str(&answers[0]).unwrap();
    assert_eq!(
        binding["epoch"]["match"]["value"]["state"], "bound",
        "{binding}"
    );
    let records = chain(&node);
    let found = epochs(&records);
    assert_eq!(found.len(), 1);
    let (epoch, epoch_hash) = &found[0];
    assert_eq!(
        epoch.artifact.binary.as_deref(),
        Some(binary.as_str()),
        "the step measured itself"
    );
    assert_eq!(epoch.artifact.image.as_deref(), Some(image_ref.as_str()));
    let denial = payload(&records, &denials[0]);
    assert_eq!(denial["epoch_number"], 1);
    join(&producers, epoch, epoch_hash, &binding, &denial).unwrap();

    // B-4: each digest was taken over bytes holding only digests of records
    // earlier in the order, so no cycle is possible.
    let text = |b: &[u8]| String::from_utf8(b.to_vec()).unwrap();
    let later = [
        (
            text(&producers.authority),
            vec![
                digest(&producers.statement),
                digest(&producers.deployment),
                epoch_hash.clone(),
            ],
        ),
        (
            text(&producers.statement),
            vec![digest(&producers.deployment), epoch_hash.clone()],
        ),
        (text(&producers.deployment), vec![epoch_hash.clone()]),
    ];
    for (bytes, names) in later {
        for name in names {
            let hex = name.trim_start_matches("sha256:");
            assert!(
                !bytes.contains(hex),
                "a record names a later record's digest {name}"
            );
        }
    }
}

/// FR-004: what a replica reports when it is not what its epoch deployed.
#[test]
fn fr004_a_mismatch_is_observable_and_the_join_names_the_subject() {
    let dir = tempfile::tempdir().unwrap();
    let mut node = Node::new();
    let binary = digest(&std::fs::read(std::env::current_exe().unwrap()).unwrap());
    let (image_ref, image_hex) = image('b');
    let producers = Producers::compose(&image_ref, &image_hex, &binary);
    node.set_env("RAHI_ARTIFACT_IMAGE", &image_ref);
    node.set_env(rahi_ops::migrate::ENV_DEPLOYMENT_REFS, &producers.refs());
    deploy(&node, None);

    // The same volume and manifest, another executable.
    let second = second_binary(dir.path());
    let (answers, _) = serve_and_read(
        &node,
        Some(&second),
        &[rahi_edge::binding::BINDING_PATH, "/metrics"],
        0,
    );
    let binding: serde_json::Value = serde_json::from_str(&answers[0]).unwrap();
    let found = &binding["epoch"]["match"]["value"];
    assert_eq!(found["state"], "mismatch", "{binding}");
    assert_eq!(found["differs"], serde_json::json!(["binary"]));
    assert!(
        answers[1].contains("rahi_binding_mismatch{kind=\"binary\"} 1"),
        "{}",
        answers[1]
    );
    assert!(
        answers[1].contains("rahi_binding_epoch 1"),
        "{}",
        answers[1]
    );

    // Another declared image.
    let (other_image, _) = image('c');
    node.set_env("RAHI_ARTIFACT_IMAGE", &other_image);
    let (answers, _) = serve_and_read(
        &node,
        None,
        &[rahi_edge::binding::BINDING_PATH, "/metrics"],
        0,
    );
    let binding: serde_json::Value = serde_json::from_str(&answers[0]).unwrap();
    assert_eq!(
        binding["epoch"]["match"]["value"]["differs"],
        serde_json::json!(["image"])
    );
    assert!(
        answers[1].contains("rahi_binding_mismatch{kind=\"image\"} 1"),
        "{}",
        answers[1]
    );

    // The join fails, naming the subject, when the Statement names another
    // binary.
    let records = chain(&node);
    let (epoch, epoch_hash) = epochs(&records).remove(0);
    let other = Producers::compose(&image_ref, &image_hex, &digest(b"another binary"));
    let err = join(
        &other,
        &epoch,
        &epoch_hash,
        &binding,
        &serde_json::json!({}),
    )
    .unwrap_err();
    assert!(err.contains("binary") && err.contains(&binary), "{err}");
}

/// FR-005: re-running the deploy step appends nothing, and a restore is an
/// epoch whose `previous` is the archived current epoch.
#[test]
fn fr005_the_deploy_step_is_idempotent_and_a_restore_is_an_epoch() {
    let rauthy = stub_rauthy();
    let mut node = Node::new();
    node.set_env("RAHI_RAUTHY_ADDR", &rauthy.addr);
    // Spec 037 B-1: `backup` logs the backup admin in with its passkey.
    let config = rahi_types::Config::from_env(
        &node
            .env
            .iter()
            .cloned()
            .collect::<std::collections::BTreeMap<_, _>>(),
    )
    .unwrap();
    rahi_ops::first_boot::mint_backup_passkey(&config, &rahi_ops::KeySet::of(&config)).unwrap();
    let refs = |n: u32| {
        serde_json::json!({"deployment": {
            "type": "https://statecraft.ing/deployment-record/v0",
            "digest": digest(format!("rollout-{n}").as_bytes()),
        }})
        .to_string()
    };
    for n in 1..=2 {
        node.set_env(rahi_ops::migrate::ENV_DEPLOYMENT_REFS, &refs(n));
        let run = deploy(&node, None);
        assert!(
            run.stdout.contains(&format!("appended epoch {n} (deploy)")),
            "{}",
            run.logs()
        );
    }
    // B-3: malformed references are refused before the step changes
    // anything.
    node.set_env(rahi_ops::migrate::ENV_DEPLOYMENT_REFS, r#"{"unknown": {}}"#);
    let refused = node.run("migrate --adopt-manifest");
    assert_ne!(refused.code, Some(0), "{}", refused.logs());
    assert!(
        refused.stderr.contains("RAHI_DEPLOYMENT_REFS"),
        "{}",
        refused.logs()
    );
    assert_eq!(epochs(&chain(&node)).len(), 2, "nothing was appended");
    node.set_env(rahi_ops::migrate::ENV_DEPLOYMENT_REFS, &refs(2));
    let again = deploy(&node, None);
    assert!(
        again.stdout.contains("the chain is at epoch 2")
            && again.stdout.contains("nothing appended"),
        "the same inputs append nothing: {}",
        again.logs()
    );
    let epoch2 = epochs(&chain(&node)).remove(1).1;

    let backup = node.run("backup");
    assert_eq!(backup.code, Some(0), "{}", backup.logs());
    let archive = std::fs::read_dir(node.data_dir.join("backups"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|e| e == "age"))
        .expect("one archive");

    for n in 3..=4 {
        node.set_env(rahi_ops::migrate::ENV_DEPLOYMENT_REFS, &refs(n));
        deploy(&node, None);
    }
    assert_eq!(
        epochs(&chain(&node)).len(),
        4,
        "the chain advanced to epoch 4"
    );

    // Restore into a fresh volume, as an operator restores a lost one.
    // Its own rauthy address, which nothing answers: restore refuses to run
    // beside a live rauthy.
    let fresh = Node::new();
    std::fs::remove_dir_all(fresh.data_dir.join("keys")).unwrap();
    let key = node.data_dir.join("keys").join(rahi_ops::BACKUP_KEY_FILE);
    let run = fresh.run(&format!(
        "restore {} --key {}",
        archive.display(),
        key.display()
    ));
    assert_eq!(run.code, Some(0), "{}", run.logs());
    let marker: serde_json::Value = serde_json::from_slice(
        &std::fs::read(fresh.data_dir.join(rahi_ops::RESTORE_MARKER)).unwrap(),
    )
    .unwrap();
    assert_eq!(
        marker["manifest"]["epoch"]["number"], 2,
        "B-11: the archive named epoch 2"
    );
    assert_eq!(marker["manifest"]["epoch"]["hash"], epoch2.as_str());

    let run = deploy(&fresh, None);
    assert!(
        run.stdout.contains("appended epoch 3 (restore)"),
        "{}",
        run.logs()
    );
    let records = chain(&fresh);
    let restored = epochs(&records);
    let (epoch3, epoch3_hash) = restored.last().unwrap();
    assert_eq!(epoch3.cause, Cause::Restore);
    assert_eq!(
        epoch3.previous.as_str(),
        epoch2,
        "previous is the archived current epoch"
    );
    let named = epoch3.restore.as_ref().expect("the marker's archive");
    assert_eq!(named.sha256, marker["sha256"].as_str().unwrap());
    assert_eq!(named.archive, marker["archive"].as_str().unwrap());
    let again = deploy(&fresh, None);
    assert!(
        again.stdout.contains("nothing appended"),
        "D-8: one restore, one epoch: {}",
        again.logs()
    );

    let (_, denials) = serve_and_read(&fresh, None, &[], 1);
    let denial = payload(&chain(&fresh), &denials[0]);
    assert_eq!(
        denial["epoch"],
        epoch3_hash.as_str(),
        "a denial after it names the new epoch 3"
    );
}

/// FR-006: v1, then v2 with one added grant, then v1's binary again.
#[test]
fn fr006_a_rollback_is_a_new_epoch() {
    let dir = tempfile::tempdir().unwrap();
    let v2_binary = second_binary(dir.path());
    let mut node = Node::new();

    node.set_env(CELL_VAR, "v1");
    deploy(&node, None);
    node.set_env(CELL_VAR, "v2");
    let run = deploy(&node, Some(&v2_binary));
    assert!(
        run.stdout.contains("appended as"),
        "a transition: {}",
        run.logs()
    );
    node.set_env(CELL_VAR, "v1");
    let run = deploy(&node, None);
    assert!(
        run.stdout.contains("appended epoch 3 (deploy)"),
        "{}",
        run.logs()
    );

    let records = chain(&node);
    let found = epochs(&records);
    let first_hash = found[0].1.clone();
    assert_eq!(found.len(), 3);
    let (e1, e2, e3) = (&found[0].0, &found[1], &found[2].0);
    assert_eq!(e3.artifact, e1.artifact, "an old artifact, a new epoch");
    assert_ne!(
        e2.0.artifact.binary, e1.artifact.binary,
        "v2 ran another binary"
    );
    assert_eq!(e3.previous.as_str(), e2.1, "previous is epoch 2");
    assert!(
        e2.0.transition.is_some() && e3.transition.is_some(),
        "each step's transition"
    );
    assert_eq!(found[0].1, first_hash, "epoch 1 is unchanged");
    assert_eq!(e1.manifest, e3.manifest);
    assert_ne!(e1.manifest, e2.0.manifest);
}

/// A stub rauthy for `backup` (as `tests/cli.rs` has): health, the login
/// ceremony the backup admin drives, and the backup routes.
struct StubRauthy {
    addr: String,
    _runtime: tokio::runtime::Runtime,
}

fn stub_rauthy() -> StubRauthy {
    use axum::routing::{get, post};
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let taken: std::sync::Arc<std::sync::Mutex<Vec<i64>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let list = taken.clone();
    let code = "a".repeat(48);
    let login_code = code.clone();
    let app = axum::Router::new()
        .route("/auth/v1/health", get(|| async { "ok" }))
        .route(
            "/auth/v1/.well-known/openid-configuration",
            get(|| async {
                serde_json::json!({"issuer": "http://127.0.0.1/auth/v1/"}).to_string()
            }),
        )
        .route(
            "/auth/v1/oidc/session",
            post(|| async {
                (
                    [(axum::http::header::SET_COOKIE, "RauthySession=stub; Path=/")],
                    serde_json::json!({"csrf_token": "stub-csrf"}).to_string(),
                )
            }),
        )
        .route("/auth/v1/pow", post(|| async { "1:0:0:salt:hash:" }))
        .route(
            "/auth/v1/oidc/authorize",
            post(move || {
                let code = code.clone();
                async move { serde_json::json!({"code": code, "exp": 60}).to_string() }
            }),
        )
        .route(
            "/auth/v1/users/webauthn_start",
            post(move || {
                let code = login_code.clone();
                async move {
                    serde_json::json!({"code": code, "rcr": {"publicKey": {"challenge": "c3R1Yg"}}})
                        .to_string()
                }
            }),
        )
        .route("/auth/v1/users/webauthn_finish", post(|| async { "{}" }))
        .route(
            "/auth/v1/backup",
            get(move || {
                let taken = list.lock().unwrap().clone();
                async move {
                    let local: Vec<serde_json::Value> = taken
                        .iter()
                        .map(|ts| {
                            serde_json::json!({
                                "name": format!("backup_node_1_{ts}.sqlite"),
                                "last_modified": ts,
                                "size": 6,
                            })
                        })
                        .collect();
                    serde_json::json!({"local": local, "s3": []}).to_string()
                }
            })
            .post(move || {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                taken.lock().unwrap().push(i64::try_from(now).unwrap());
                async { "" }
            }),
        )
        .route("/auth/v1/backup/local/{name}", get(|| async { "rauthy" }));
    runtime.spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    StubRauthy {
        addr,
        _runtime: runtime,
    }
}

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
