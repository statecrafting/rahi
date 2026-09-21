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

/// The boot spec for this run, and the rauthy binary if there is one.
///
/// A missing rauthy is a skip, and a skip is a pass only when the runner did
/// not ask for rauthy: `RAHI_REQUIRE_RAUTHY=1` makes it a failure
/// (spec 037 B-4).
fn spec() -> (BootSpec, Option<PathBuf>) {
    match rahi_harness::boot::test_rauthy_binary() {
        Some(rauthy) => (BootSpec::new(binary()).with_rauthy(&rauthy), Some(rauthy)),
        None => (BootSpec::new(binary()), None),
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

/// One bearer write of a note: the status, and the body if it was not one.
///
/// Deliberately no CSRF pair: that is what spec 038 B-1 is, and a helper
/// that quietly added one would make every assertion above vacuous.
async fn bearer_note(
    client: &rahi_harness::Client,
    token: &str,
    body: &str,
) -> (StatusCode, String) {
    let answer = client
        .send(
            client
                .request(reqwest::Method::POST, "/api/v1/notes")
                .await
                .unwrap()
                .header("authorization", format!("Bearer {token}"))
                .json(&serde_json::json!({ "body": body })),
        )
        .await
        .unwrap();
    let status = answer.status();
    (status, answer.text().await.unwrap_or_default())
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
    let (spec, rauthy_bin) = spec();
    let with_rauthy = rauthy_bin.is_some();
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

    let alice = User::new("alice@example.com", "Correct-Horse-Battery-Staple-2026");
    let mut decision: Option<String> = None;
    let mut alice_sub: Option<String> = None;
    if with_rauthy {
        let user = alice.clone();
        cell.login_as(&client, &user).await.expect("alice logs in");
        // Her `sub` as rauthy holds it, which is the only principal id the
        // chassis knows (constitution VII) and what the restored cell must
        // hand back unchanged (spec 037 B-6).
        alice_sub = Some(rauthy_sub(&cell, &user.email).await);

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

        // ---------------------------------------------------------------
        // Spec 038 B-7, FR-005: the command-line client, for real.
        //
        // A device grant against rauthy through the cell's origin, approved
        // as alice, then a bearer write with no CSRF pair, a renewal at the
        // issuer, a second write with the renewed token, a revocation, and
        // a refusal. The client here is a fresh one with an empty cookie
        // jar, which is what a CLI is: a request carrying both a session
        // cookie and a token is refused 400 (025 B-10), and that refusal is
        // asserted at the end.
        let tokens = rahi_harness::device_login(
            cell.base_url(),
            &user,
            "hello-cli",
            "openid profile email notes:write",
        )
        .await
        .expect("the device grant completes");
        assert_eq!(
            tokens.expires_in, 600,
            "the lifetime as issued is the manifest's, not rauthy's 1800 (B-4, FR-006)"
        );
        let refresh_token = tokens
            .refresh_token
            .clone()
            .expect("the declared refresh flow yields a refresh token");

        let cli = cell.client();
        let wrote = bearer_note(&cli, &tokens.access_token, "from the cli").await;
        assert_eq!(
            wrote.0,
            StatusCode::CREATED,
            "a bearer write passes the CSRF layer with no pair (B-1): {}",
            wrote.1
        );

        // Renewal is the client's, at the issuer, and driven by what the
        // token response said rather than by anything the manifest told it
        // (D-1). The chassis has no leg of this.
        let renewed = rahi_harness::refresh_tokens(cell.base_url(), "hello-cli", &refresh_token)
            .await
            .expect("the refresh grant renews");
        assert_ne!(renewed.access_token, tokens.access_token);
        assert_eq!(renewed.expires_in, 600);
        let again = bearer_note(&cli, &renewed.access_token, "after the renewal").await;
        assert_eq!(again.0, StatusCode::CREATED, "{}", again.1);

        // The revocation: the client ends its own token, and the next
        // request with it is refused inside one cache read (B-5).
        let revoked = cli
            .send(
                cli.request(reqwest::Method::POST, "/session/token/revoke")
                    .await
                    .unwrap()
                    .header("authorization", format!("Bearer {}", renewed.access_token)),
            )
            .await
            .unwrap();
        assert_eq!(
            revoked.status(),
            StatusCode::OK,
            "{}",
            revoked.text().await.unwrap()
        );
        let refused = bearer_note(&cli, &renewed.access_token, "after the revocation").await;
        assert_eq!(
            refused.0,
            StatusCode::UNAUTHORIZED,
            "a revoked token is refused: {}",
            refused.1
        );

        // 025 B-10, at the composed cell: a request carrying both
        // credentials is refused rather than resolved by a rule. `client`
        // still holds alice's session cookie.
        let both = client
            .send(
                client
                    .request(reqwest::Method::POST, "/api/v1/notes")
                    .await
                    .unwrap()
                    .header("authorization", format!("Bearer {}", tokens.access_token))
                    .json(&serde_json::json!({ "body": "two credentials" })),
            )
            .await
            .unwrap();
        assert_eq!(
            both.status(),
            StatusCode::BAD_REQUEST,
            "{}",
            both.text().await.unwrap()
        );

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

    let (Some(rauthy_bin), Some(alice_sub)) = (rauthy_bin, alice_sub) else {
        eprintln!("skipped (no RAHI_TEST_RAUTHY): backup and restore need rauthy's snapshot");
        return;
    };

    // ---------------------------------------------------- spec 037 B-6
    //
    // Recovery, with identity, against the pinned rauthy release. Nothing
    // here is stubbed: the backup is taken against the rauthy that answered
    // alice's login, the restore lands on a fresh volume, and the restored
    // cell is booted *with* identity on the same ports, where alice logs in
    // as herself and reads what she wrote.
    //
    // This runs rauthy three times, and a rauthy takes the better part of a
    // minute to come up: a restart of the same volume, and the restored
    // volume. That is the cost of proving recovery rather than describing
    // it. `.github/workflows/live.yml` is where it runs on every change.

    // First, the restart without a restore: the same volume, the same
    // ports, identity mounted. alice's existing session renews against the
    // rauthy that came back, and the chain is where it was.
    let again = Harness::boot(
        BootSpec::new(binary())
            .on_data_dir(&data_dir)
            .with_ports(cell.ports())
            .with_rauthy(&rauthy_bin),
    )
    .expect("the same volume boots again, with identity");
    assert!(again.has_rauthy());
    // The client from before the restart, cookie and all: the cell is on
    // the same origin, so this is the session renewing, not a new login.
    let renewed = client.get("/api/notes").await.unwrap();
    assert_eq!(
        renewed.status(),
        StatusCode::OK,
        "alice's session renews across a restart: {}",
        renewed.text().await.unwrap()
    );
    let listed: Vec<serde_json::Value> = renewed.json().await.unwrap();
    assert_eq!(listed.len(), 1, "her surviving note: {listed:?}");
    assert_eq!(listed[0]["body"], "second");
    assert_eq!(
        rauthy_sub(&again, &alice.email).await,
        alice_sub,
        "the restart changed no principal id"
    );

    // The backup, against that real rauthy. The verb attaches to the
    // running node for the app's half and logs in as the dedicated backup
    // admin for rauthy's (spec 037 B-1); no route is stubbed.
    let archives = tempfile::tempdir().unwrap();
    let backed = verb(
        &again,
        &["backup", "--to", archives.path().to_str().unwrap()],
        &[],
    );
    assert!(backed.starts_with("backup: rahi-backup-"), "{backed}");
    eprintln!("e2e: {}", backed.trim());
    again.stop().unwrap();

    // The restart appended nothing: the head is the one measured before it.
    let verified = verb(&again, &["ledger", "verify"], &[]);
    assert_eq!(
        head_of(&verified),
        head,
        "a restart without a restore leaves the chain where it was"
    );

    let archive = std::fs::read_dir(archives.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|e| e == "age"))
        .expect("one archive");

    // Restore into a fresh volume.
    let fresh = tempfile::tempdir().unwrap();
    let key = data_dir.join("keys").join("backup.key");
    let restored_out = verb(
        &again,
        &[
            "restore",
            archive.to_str().unwrap(),
            "--key",
            key.to_str().unwrap(),
        ],
        &[("RAHI_DATA_DIR", fresh.path().to_str().unwrap())],
    );
    assert!(
        restored_out.starts_with("restore: applied"),
        "{restored_out}"
    );
    eprintln!("e2e: restored into {}", fresh.path().display());

    // The rows are there, read directly, on the addresses the volume was
    // written under (spec 034 D-5).
    assert_eq!(
        count_notes(fresh.path(), cell.env()).await,
        1,
        "one note survived the round trip"
    );

    // And the cell comes up *with* identity: the supervisor hands rauthy the
    // snapshot the restore placed, exactly once (spec 037 B-3).
    let restored = Harness::boot(
        BootSpec::new(binary())
            .on_data_dir(fresh.path())
            .with_ports(cell.ports())
            .with_rauthy(&rauthy_bin),
    )
    .expect("the restored cell boots with identity");
    assert!(restored.has_rauthy());
    restored
        .client()
        .expect_get("/readyz", StatusCode::OK)
        .await
        .unwrap();

    // alice is who she was. The harness's login creates a user that is
    // absent, so the assertion that matters is her `sub`: an alice rauthy
    // did not already hold would come back with a new one, and her note
    // would be unreadable behind it.
    assert_eq!(
        rauthy_sub(&restored, &alice.email).await,
        alice_sub,
        "the restored rauthy holds alice with her original sub"
    );
    // Her login, not her creation. `Instance::login_as` would `ensure_user`
    // first, which on a restored cell is both beside the point and refused:
    // rauthy will not take a password it holds in its own history. She is
    // already there, so the flow drives her login and nothing else.
    let restored_client = restored.client();
    rahi_harness::rauthy::login(&restored_client, restored.base_url(), &alice)
        .await
        .expect("alice logs in on the restored cell, as herself");
    let listed: Vec<serde_json::Value> = restored_client
        .get("/api/notes")
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        listed.len(),
        1,
        "alice reads the note she wrote before the backup: {listed:?}"
    );
    assert_eq!(listed[0]["body"], "second");

    // The marker records the hand-off, and a second start passes nothing.
    let marker: serde_json::Value =
        serde_json::from_slice(&std::fs::read(fresh.path().join("restore.marker")).unwrap())
            .unwrap();
    assert!(
        marker["rauthy_snapshot_applied"].as_u64().is_some(),
        "the supervisor recorded the hand-off: {marker}"
    );

    restored.stop().unwrap();
    let verified = verb(&restored, &["ledger", "verify"], &[]);
    assert_eq!(
        head_of(&verified),
        head,
        "the same ledger head after restore"
    );
}

/// The `sub` rauthy holds for `email`, read through the admin API on the
/// instance's own loopback (spec 037 B-6).
async fn rauthy_sub(instance: &Instance, email: &str) -> String {
    rahi_harness::Rauthy::new(&instance.rauthy_loopback(), instance.admin_token())
        .find_user(email)
        .await
        .expect("rauthy answers the admin API")
        .unwrap_or_else(|| panic!("rauthy holds {email}"))
        .get("id")
        .and_then(|v| v.as_str())
        .expect("the user carries an id")
        .to_owned()
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
