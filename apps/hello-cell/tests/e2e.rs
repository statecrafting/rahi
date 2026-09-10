//! The end-to-end proof (spec 034 B-5, B-6): the whole chassis, through
//! the harness, against the built `hello-cell` binary.
//!
//! With `RAHI_TEST_RAUTHY` naming a rauthy binary the full path runs:
//! boot, log in, write, read, delete, be denied an ungranted operation,
//! verify the ledger, back up, restore into a fresh volume, boot again.
//! Without one, the unauthenticated subset runs and the skipped steps are
//! reported: probes, the page, every route's classification as the edge
//! answers it, and the ledger verbs on the stopped volume.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::path::{Path, PathBuf};

use rahi_harness::{BootSpec, Harness, Instance, User};
use reqwest::StatusCode;

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_hello-cell"))
}

fn spec() -> (BootSpec, bool) {
    match std::env::var("RAHI_TEST_RAUTHY") {
        Ok(rauthy) => (BootSpec::new(binary()).with_rauthy(rauthy), true),
        Err(_) => (BootSpec::new(binary()), false),
    }
}

/// Run a verb on a stopped cell and return its stdout, failing on a
/// nonzero exit with its stderr.
fn verb(cell: &Instance, args: &[&str], env: &[(&str, &str)]) -> String {
    let mut command = cell.command(args);
    for (k, v) in env {
        command.env(k, v);
    }
    let out = command.output().expect("the binary runs");
    assert!(
        out.status.success(),
        "{} exited {:?}: {}",
        args.join(" "),
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn head_of(verify_output: &str) -> String {
    verify_output
        .split("head ")
        .nth(1)
        .map(|h| h.trim().to_owned())
        .expect("ledger verify names the head")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_cell_end_to_end() {
    let (spec, with_rauthy) = spec();
    let cell = Harness::boot(spec).expect("hello-cell boots");
    let client = cell.client();

    // Probes and the page: public, always.
    client.expect_get("/healthz", StatusCode::OK).await.unwrap();
    client.expect_get("/readyz", StatusCode::OK).await.unwrap();
    let page = client.expect_get("/", StatusCode::OK).await.unwrap();
    assert!(
        page.contains("<title>hello-cell</title>"),
        "the page is served"
    );
    let script = client.expect_get("/app.js", StatusCode::OK).await.unwrap();
    assert!(
        script.contains("X-CSRF-Token"),
        "the page carries the CSRF helper"
    );
    let metrics = client.expect_get("/metrics", StatusCode::OK).await.unwrap();
    assert!(metrics.contains("rahi_"), "{metrics}");

    // B-6: every route is classified. The edge refuses to build with an
    // unclassified route, so a booted cell is one whose table is complete;
    // what is checked here is that each class answers as its class does.
    client
        .expect_get("/api/notes", StatusCode::UNAUTHORIZED)
        .await
        .unwrap();
    let denied_unauth = client.post("/api/notes/migrate").await.unwrap();
    assert_eq!(
        denied_unauth.status(),
        StatusCode::UNAUTHORIZED,
        "authentication precedes adjudication"
    );
    let ops = client.get("/operator/traces").await.unwrap();
    assert!(
        ops.status() == StatusCode::UNAUTHORIZED || ops.status() == StatusCode::FORBIDDEN,
        "the operator surface is gated: {}",
        ops.status()
    );

    let mut decision: Option<String> = None;
    if with_rauthy {
        let user = User::new("alice@example.com", "Correct-Horse-Battery-Staple-2026");
        cell.login_as(&client, &user).await.expect("alice logs in");

        // Two notes, one txn each: the row, its revision, its outbox row.
        let first = client
            .post_json("/api/notes", &serde_json::json!({ "body": "first" }))
            .await
            .unwrap();
        assert_eq!(
            first.status(),
            StatusCode::CREATED,
            "{}",
            first.text().await.unwrap()
        );
        let first: serde_json::Value = first.json().await.unwrap();
        let second = client
            .post_json("/api/notes", &serde_json::json!({ "body": "second" }))
            .await
            .unwrap();
        assert_eq!(second.status(), StatusCode::CREATED);
        let second: serde_json::Value = second.json().await.unwrap();
        assert_eq!(first["revision"].as_i64(), Some(1));
        assert_eq!(second["revision"].as_i64(), Some(2));

        let listed: Vec<serde_json::Value> = client
            .get("/api/notes")
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(listed.len(), 2, "{listed:?}");
        assert_eq!(listed[0]["body"], "first");

        let gone = client
            .send(
                client
                    .request(
                        reqwest::Method::DELETE,
                        &format!("/api/notes/{}", first["id"].as_str().unwrap()),
                    )
                    .await
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(gone.status(), StatusCode::NO_CONTENT);
        let listed: Vec<serde_json::Value> = client
            .get("/api/notes")
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0]["body"], "second");

        // The demonstration: db.migrate was never granted to notes.
        let denied = client.post("/api/notes/migrate").await.unwrap();
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
        let body: serde_json::Value = denied.json().await.unwrap();
        let id = body["decision"]
            .as_str()
            .expect("the 403 names its decision")
            .to_owned();
        assert!(!id.is_empty());
        decision = Some(id);

        // The operator surface, as an operator (spec 024 B-2): the trace
        // ring holds the requests above, and the exposure table names
        // every route with a class (B-6).
        let operator = cell.client();
        let ops = User::new("ops@example.com", "Correct-Horse-Battery-Staple-2026")
            .with_role("hello_operator");
        cell.login_as(&operator, &ops)
            .await
            .expect("the operator logs in");
        let traces: serde_json::Value = operator
            .get("/operator/traces")
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(
            traces["traces"].as_array().is_some_and(|t| !t.is_empty()),
            "the ring holds the requests so far: {traces}"
        );
        let table = operator
            .expect_get("/operator/exposure", StatusCode::OK)
            .await
            .unwrap();
        assert!(
            !table.contains("UNCLASSIFIED"),
            "every route is classified:\n{table}"
        );
        // The table is by mount: the notes router is the cell's routes at
        // the root, authenticated; the page is the static slot, public.
        for row in [
            "Authenticated  /\n",
            "Public         /*\n",
            "Operator       /operator\n",
            "Public         /session\n",
            "Proxy          /auth\n",
            "Probe          /healthz\n",
            "Probe          /readyz\n",
            "Probe          /metrics\n",
        ] {
            assert!(table.contains(row), "{row:?} is in the table:\n{table}");
        }
        eprintln!("e2e: exposure table\n{table}");
    } else {
        eprintln!(
            "skipped (no RAHI_TEST_RAUTHY): login, two notes, list, delete, the ledgered denial, backup, restore"
        );
    }

    // The ledger, on the stopped volume: verified, and the denial is the
    // last record when there was one.
    let data_dir = cell.data_dir();
    cell.stop().unwrap();
    let verified = verb(&cell, &["ledger", "verify"], &[]);
    assert!(verified.starts_with("ledger verify: ok"), "{verified}");
    let head = head_of(&verified);
    let export = data_dir.join("chain.jsonl");
    verb(&cell, &["ledger", "export", export.to_str().unwrap()], &[]);
    let chain = std::fs::read_to_string(&export).unwrap();
    let last = chain
        .lines()
        .rfind(|l| !l.trim().is_empty())
        .unwrap_or_default();
    if let Some(id) = &decision {
        assert!(
            last.contains(id),
            "the denial {id} is the last record: {last}"
        );
        assert!(last.contains("db.migrate"), "{last}");
    }

    // FR-003: the independent verifier over the exported chain, when it is
    // at hand (`RAHI_TEST_ATTEST_LEDGER`, or `attest-ledger` on the PATH).
    match independent_verifier() {
        Some(verifier) => {
            let out = std::process::Command::new(&verifier)
                .arg("verify")
                .arg(&export)
                .output()
                .expect("the verifier runs");
            assert!(
                out.status.success(),
                "attest-ledger verify: {}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            eprintln!(
                "e2e: attest-ledger verify: {}",
                String::from_utf8_lossy(&out.stdout).trim()
            );
        }
        None => eprintln!("skipped (no attest-ledger): the independent chain verification"),
    }

    if !with_rauthy {
        eprintln!("skipped (no RAHI_TEST_RAUTHY): backup and restore need rauthy's snapshot");
        return;
    }

    // Backup, then restore into a fresh volume. rauthy is stopped with the
    // cell; its backup routes are answered by a stub on its address, since
    // a live rauthy refuses the API key there (spec 030 D-3).
    let stub = stub_rauthy_backup(cell.ports().rauthy);
    let archives = tempfile::tempdir().unwrap();
    let backed = verb(
        &cell,
        &["backup", "--to", archives.path().to_str().unwrap()],
        &[],
    );
    assert!(backed.starts_with("backup: rahi-backup-"), "{backed}");
    stub.shutdown_background();
    let archive = std::fs::read_dir(archives.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|e| e == "age"))
        .expect("one archive");

    let fresh = tempfile::tempdir().unwrap();
    let key = data_dir.join("keys").join("backup.key");
    let restored = verb(
        &cell,
        &[
            "restore",
            archive.to_str().unwrap(),
            "--key",
            key.to_str().unwrap(),
        ],
        &[("RAHI_DATA_DIR", fresh.path().to_str().unwrap())],
    );
    assert!(restored.starts_with("restore: applied"), "{restored}");
    eprintln!("e2e: restored into {}", fresh.path().display());

    // The restored store holds the remaining note, read directly, on the
    // addresses the volume was written under: hiqlite binds a node to the
    // membership it stored, so another pair never elects (spec 034 D-5).
    assert_eq!(
        count_notes(fresh.path(), cell.env()).await,
        1,
        "one note survived the round trip"
    );

    // Boot again on the restored volume, on the same ports (D-5): first-boot
    // finds the keys, migrate is current, serve verifies the same chain.
    // rauthy's own restore is the operator's step (spec 030 B-6), so this
    // boot mounts no identity.
    let again = Harness::boot(
        BootSpec::new(binary())
            .on_data_dir(fresh.path())
            .with_ports(cell.ports()),
    )
    .expect("the restored cell boots");
    assert!(!again.has_rauthy());
    again
        .client()
        .expect_get("/readyz", StatusCode::OK)
        .await
        .unwrap();
    again.stop().unwrap();
    let verified = verb(&again, &["ledger", "verify"], &[]);
    assert_eq!(
        head_of(&verified),
        head,
        "the same ledger head after restore"
    );
}

/// The independent verifier, if installed.
fn independent_verifier() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("RAHI_TEST_ATTEST_LEDGER") {
        return Some(PathBuf::from(path));
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join("attest-ledger"))
            .find(|candidate| candidate.is_file())
    })
}

/// Count the notes in a stopped volume's store, through the chassis's own
/// store crate with the volume's keys, on the hiqlite addresses `cell_env`
/// names (the ones the volume's membership was stored under).
async fn count_notes(data_dir: &Path, cell_env: &[(String, String)]) -> i64 {
    let keys = rahi_ops::KeySet::at(data_dir.join("keys"));
    let secrets = keys.store_secrets().expect("the restored keys read");
    let addr = |name: &str| -> String {
        cell_env
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| panic!("the cell's environment names {name}"))
    };
    let env = std::collections::BTreeMap::from([
        (
            "RAHI_PUBLIC_URL".to_owned(),
            "http://localhost:1".to_owned(),
        ),
        ("RAHI_DATA_DIR".to_owned(), data_dir.display().to_string()),
        (
            "RAHI_HIQLITE_API_ADDR".to_owned(),
            addr("RAHI_HIQLITE_API_ADDR"),
        ),
        (
            "RAHI_HIQLITE_RAFT_ADDR".to_owned(),
            addr("RAHI_HIQLITE_RAFT_ADDR"),
        ),
    ]);
    let config = rahi_types::Config::from_env(&env).unwrap();
    let store = rahi_store::Store::open(&rahi_store::StoreConfig::from_config(&config, secrets))
        .await
        .expect("the restored store opens");
    #[derive(serde::Deserialize)]
    struct Count {
        n: i64,
    }
    let rows: Vec<Count> = store
        .query("SELECT COUNT(*) AS n FROM notes", vec![])
        .await
        .unwrap();
    store.shutdown().await.unwrap();
    rows.first().map_or(0, |c| c.n)
}

/// rauthy's backup routes, as `rahi backup` calls them, on `port`.
fn stub_rauthy_backup(port: u16) -> tokio::runtime::Runtime {
    use axum::routing::get;
    let taken = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let list = taken.clone();
    let app = axum::Router::new()
        .route("/auth/v1/health", get(|| async { "ok" }))
        .route(
            "/auth/v1/backup",
            get(move || {
                let n = list.load(std::sync::atomic::Ordering::SeqCst);
                async move {
                    let local: Vec<serde_json::Value> = (1..=n)
                        .map(|i| serde_json::json!({"name": format!("rauthy_backup_{i}.sqlite"), "last_modified": i, "size": 6}))
                        .collect();
                    serde_json::json!({"local": local, "s3": []}).to_string()
                }
            })
            .post(move || {
                taken.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async { "" }
            }),
        )
        .route("/auth/v1/backup/local/{name}", get(|| async { "rauthy" }));
    let listener = std::net::TcpListener::bind(("127.0.0.1", port))
        .expect("rauthy's port is free once it stopped");
    listener.set_nonblocking(true).unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.spawn(async move {
        let listener = tokio::net::TcpListener::from_std(listener).unwrap();
        axum::serve(listener, app).await.unwrap();
    });
    runtime
}
