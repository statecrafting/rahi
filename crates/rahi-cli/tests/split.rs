//! Spec 044 FR-002 and AC-2: remote identity mode, without a cluster.
//!
//! A real rahi `serve` in remote mode and a real Rauthy on separate
//! addresses, Rauthy serving its native TLS under a CA this test mints and
//! rahi trusting only that CA (B-1, B-2). With Rauthy stopped, `serve`
//! starts and is live and started but not ready, naming `identity`; once
//! Rauthy answers, readiness turns without a restart; when Rauthy stops
//! again, readiness falls back and the process stays up (B-4 to B-6).
//!
//! It needs the Rauthy binary the live workflow extracts from the pinned
//! image (`RAHI_TEST_RAUTHY`) and `openssl` to mint the CA; without the
//! binary it says the leg did not execute, and `RAHI_REQUIRE_RAUTHY=1` makes
//! that a failure.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod stop_fixture;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use rahi_edge::{HEALTHZ_PATH, READYZ_PATH, STARTUPZ_PATH};
use stop_fixture::{Node, free_port, http_get};

fixture_entry!();

/// Rauthy's boot (JWK generation) plus the background composition.
const RAUTHY_BUDGET: Duration = Duration::from_secs(240);

/// Mint a CA and a server certificate for `localhost` and `127.0.0.1`.
fn mint_tls(dir: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let run = |args: &[&str]| {
        let out = Command::new("openssl")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("openssl runs");
        assert!(
            out.status.success(),
            "openssl {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run(&[
        "req",
        "-x509",
        "-newkey",
        "rsa:2048",
        "-nodes",
        "-days",
        "2",
        "-subj",
        "/CN=rahi-split-test-ca",
        "-keyout",
        "ca.key",
        "-out",
        "ca.pem",
    ]);
    run(&[
        "req",
        "-newkey",
        "rsa:2048",
        "-nodes",
        "-subj",
        "/CN=localhost",
        "-keyout",
        "server.key",
        "-out",
        "server.csr",
    ]);
    std::fs::write(
        dir.join("san.ext"),
        "subjectAltName=DNS:localhost,IP:127.0.0.1\nextendedKeyUsage=serverAuth\n",
    )
    .unwrap();
    run(&[
        "x509",
        "-req",
        "-in",
        "server.csr",
        "-CA",
        "ca.pem",
        "-CAkey",
        "ca.key",
        "-CAcreateserial",
        "-days",
        "2",
        "-extfile",
        "san.ext",
        "-out",
        "server.pem",
    ]);
    (
        dir.join("ca.pem"),
        dir.join("server.pem"),
        dir.join("server.key"),
    )
}

/// Start Rauthy alone, as its own StatefulSet runs it: the environment
/// first-boot rendered, plus native TLS on `https_port` (spec 044 B-2).
fn start_rauthy(node: &Node, bin: &Path, cert: &Path, key: &Path, https_port: u16) -> Child {
    let env: std::collections::BTreeMap<String, String> = node.env.iter().cloned().collect();
    let config = rahi_types::Config::from_env(&env).unwrap();
    let rendered = std::fs::read_to_string(rahi_ops::rauthy_env::env_path(&config))
        .expect("first-boot rendered rauthy's environment");
    let log = std::fs::File::create(node.root.path().join("rauthy.log")).unwrap();
    let mut command = Command::new(bin);
    command
        .arg("serve")
        .arg("-c")
        .arg(rahi_ops::rauthy_env::config_path(&config))
        .current_dir(rahi_ops::rauthy_dir(&config))
        .env_clear()
        .envs(rahi_ops::rauthy_env::parse(&rendered))
        .envs(rahi_ops::rauthy_env::standalone_tls(cert, key, https_port))
        .stdin(Stdio::null())
        .stdout(log.try_clone().unwrap())
        .stderr(log);
    for inherited in ["PATH", "HOME", "TZ"] {
        if let Some(value) = std::env::var_os(inherited) {
            command.env(inherited, value);
        }
    }
    command.spawn().expect("rauthy starts")
}

fn stop(child: &mut Child) {
    let _ = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status();
    let deadline = Instant::now() + Duration::from_secs(60);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The status and body of `path`, `None` while nothing answers.
fn probe(node: &Node, path: &str) -> Option<(u16, serde_json::Value)> {
    http_get(&node.listen, path).ok().map(|(status, body)| {
        let start = body.find('{').unwrap_or(0);
        (
            status,
            serde_json::from_str(body[start..].trim()).unwrap_or(serde_json::Value::Null),
        )
    })
}

/// Wait until `path` answers `status`, while `cell` stays alive.
fn wait_for(
    node: &Node,
    cell: &mut stop_fixture::Running,
    path: &str,
    status: u16,
    within: Duration,
) -> serde_json::Value {
    let deadline = Instant::now() + within;
    loop {
        assert!(
            cell.child.try_wait().unwrap().is_none(),
            "B-4: rahi exited while waiting for {path} {status}\n{}",
            cell.logs()
        );
        if let Some((seen, body)) = probe(node, path)
            && seen == status
        {
            return body;
        }
        assert!(
            Instant::now() < deadline,
            "{path} did not answer {status} within {within:?}\n{}",
            cell.logs()
        );
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// AC-2.
#[test]
fn ac2_remote_identity_is_readiness_never_liveness() {
    let Some(bin) = stop_fixture::test_rauthy() else {
        eprintln!(
            "AC-2: the Rauthy leg did not execute (RAHI_TEST_RAUTHY is not set); \
             RAHI_REQUIRE_RAUTHY=1 makes this a failure"
        );
        return;
    };
    let mut node = Node::with_rauthy(&bin);
    let tls = tempfile::tempdir().unwrap();
    let (ca, cert, key) = mint_tls(tls.path());
    let https_port = free_port();
    node.set_env("RAHI_RAUTHY_MODE", "remote");
    node.set_env(
        rahi_idp::back_channel::ENV_RAUTHY_URL,
        &format!("https://localhost:{https_port}"),
    );
    node.set_env(rahi_idp::back_channel::ENV_RAUTHY_CA, ca.to_str().unwrap());
    // The public origin is https, as behind a real ingress; this test reaches
    // rahi's own listener directly, in plain HTTP, as that ingress would.
    let public = node.var("RAHI_PUBLIC_URL").replace("http://", "https://");
    node.set_env("RAHI_PUBLIC_URL", &public);

    // Rauthy stopped: serve starts, is live and started, and not ready.
    let mut cell = node.spawn("serve");
    let body = wait_for(
        &node,
        &mut cell,
        HEALTHZ_PATH,
        200,
        stop_fixture::READY_BUDGET,
    );
    assert_eq!(body["status"], "alive");
    wait_for(
        &node,
        &mut cell,
        STARTUPZ_PATH,
        200,
        Duration::from_secs(30),
    );
    let (status, body) = probe(&node, READYZ_PATH).expect("readyz answers");
    assert_eq!(status, 503, "{body}");
    assert_eq!(body["component"], "identity", "B-6 names identity: {body}");

    // Rauthy started: ready, without a rahi restart.
    let pid = cell.child.id();
    let mut rauthy = start_rauthy(&node, &bin, &cert, &key, https_port);
    wait_for(&node, &mut cell, READYZ_PATH, 200, RAUTHY_BUDGET);
    assert_eq!(cell.child.id(), pid, "the same process");
    assert!(
        cell.logs()
            .contains("identity composed against the remote rauthy"),
        "{}",
        cell.logs()
    );
    // The identity routes are mounted now: discovery through the proxy.
    let (status, _) = http_get(&node.listen, rahi_idp::DISCOVERY_PATH).unwrap();
    assert_eq!(status, 200, "the proxy reaches rauthy over TLS");

    // Rauthy stopped again: not ready, still live, no exit.
    stop(&mut rauthy);
    let body = wait_for(&node, &mut cell, READYZ_PATH, 503, Duration::from_secs(60));
    assert_eq!(body["component"], "identity", "{body}");
    let (status, _) = probe(&node, HEALTHZ_PATH).expect("healthz answers");
    assert_eq!(status, 200, "B-4: liveness never depends on Rauthy");
    std::thread::sleep(Duration::from_secs(3));
    assert!(
        cell.child.try_wait().unwrap().is_none(),
        "the process stays up"
    );

    cell.sigterm();
    let stopped = cell.wait(Duration::from_secs(60));
    assert_eq!(stopped.code, Some(0), "{}", stopped.logs());
}

/// B-2: a remote back channel that is not https, or names no CA, refuses
/// the start; nothing falls back to plaintext.
#[test]
fn b2_a_plaintext_or_untrusted_back_channel_refuses_to_start() {
    let mut node = Node::new();
    node.set_env("RAHI_RAUTHY_MODE", "remote");
    node.set_env(rahi_idp::back_channel::ENV_RAUTHY_URL, "http://localhost:1");
    let run = node.run("serve");
    assert_ne!(run.code, Some(0), "{}", run.logs());
    assert!(run.stderr.contains("not https"), "{}", run.logs());

    node.set_env(
        rahi_idp::back_channel::ENV_RAUTHY_URL,
        "https://localhost:1",
    );
    let run = node.run("serve");
    assert_ne!(run.code, Some(0), "{}", run.logs());
    assert!(
        run.stderr.contains(rahi_idp::back_channel::ENV_RAUTHY_CA),
        "{}",
        run.logs()
    );

    // An http public URL, which a remote Rauthy's issuer can never match.
    let tls = tempfile::tempdir().unwrap();
    let ca = tls.path().join("ca.pem");
    mint_tls(tls.path());
    node.set_env(rahi_idp::back_channel::ENV_RAUTHY_CA, ca.to_str().unwrap());
    let run = node.run("serve");
    assert_ne!(run.code, Some(0), "{}", run.logs());
    assert!(run.stderr.contains("https public URL"), "{}", run.logs());

    // Remote mode with no back channel named at all.
    let mut bare = Node::new();
    bare.set_env("RAHI_RAUTHY_MODE", "remote");
    let run = bare.run("serve");
    assert_ne!(run.code, Some(0), "{}", run.logs());
    assert!(run.stderr.contains("RAHI_RAUTHY_URL"), "{}", run.logs());
}

/// A remote cell's `first-boot --export` renders Rauthy's environment for
/// its own StatefulSet from the same key set: node id from the ordinal,
/// peers named, native TLS from the mounted pair, the pod network trusted.
#[test]
fn a_remote_export_renders_rauthys_standalone_environment() {
    let mut node = Node::new();
    node.set_env("RAHI_RAUTHY_MODE", "remote");
    node.set_env("RAHI_PUBLIC_URL", "https://cell.example.com");
    let run = node.run("first-boot --export");
    assert_ne!(run.code, Some(0), "peers are required: {}", run.logs());
    assert!(
        run.stderr.contains("RAHI_RAUTHY_HQL_NODES"),
        "{}",
        run.logs()
    );

    node.set_env(
        "RAHI_RAUTHY_HQL_NODES",
        "1 rauthy-0.rauthy-hl:8100 rauthy-0.rauthy-hl:8200;2 rauthy-1.rauthy-hl:8100 rauthy-1.rauthy-hl:8200",
    );
    node.set_env("RAHI_RAUTHY_TRUSTED_PROXIES", "10.244.0.0/16");
    let run = node.run("first-boot --export");
    assert_eq!(run.code, Some(0), "{}", run.logs());
    let documents: Vec<&str> = run.stdout.split("\n---\n").collect();
    assert_eq!(
        documents.len(),
        2,
        "rahi-keys, then rauthy-env: {}",
        run.stdout
    );
    assert!(documents[0].contains("name: rahi-keys"));
    let rauthy = documents[1];
    assert!(rauthy.contains("name: rauthy-env"), "{rauthy}");
    for line in [
        "HQL_NODE_ID_FROM: \"k8s\"",
        "LISTEN_SCHEME: \"https\"",
        "LISTEN_PORT_HTTPS: \"8443\"",
        "TLS_CERT: \"/tls/tls.crt\"",
        "TLS_KEY: \"/tls/tls.key\"",
        "TLS_GENERATE_SELF_SIGNED: \"false\"",
        "LISTEN_ADDRESS: \"0.0.0.0\"",
        "PROXY_MODE: \"true\"",
        "TRUSTED_PROXIES: \"10.244.0.0/16\"",
        "HQL_DATA_DIR: \"/app/data\"",
    ] {
        assert!(rauthy.contains(line), "{line} in\n{rauthy}");
    }
    assert!(
        !rauthy.contains("HQL_NODE_ID:"),
        "the ordinal names the node"
    );
    // One key, one value: a template line an override replaces is dropped,
    // never left beside it (`LISTEN_SCHEME` was once both http and https).
    let mut keys: Vec<&str> = rauthy
        .lines()
        .map(str::trim_start)
        .filter_map(|line| line.split_once(": ").map(|(key, _)| key))
        .filter(|key| {
            !key.is_empty()
                && key
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        })
        .collect();
    let total = keys.len();
    keys.sort_unstable();
    keys.dedup();
    assert_eq!(keys.len(), total, "a key appears twice in\n{rauthy}");
    assert!(
        rauthy.contains("rauthy-1.rauthy-hl:8100"),
        "the peers: {rauthy}"
    );
}
