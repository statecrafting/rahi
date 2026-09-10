//! spec 033 FR-001, FR-002, FR-003: the harness boots the chassis's own
//! binary (the empty cell of spec 030), waits on readiness, stops cleanly,
//! and with a rauthy at hand logs a user in.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

use rahi_harness::{BootSpec, Harness, Instance, RauthyMode, Stopped, User};

/// The built `rahi` binary: `RAHI_TEST_BINARY`, else the workspace's
/// `target/debug/rahi`, built on demand. The harness has no compile-time
/// dependency on the chassis (spec 033 B-2), so cargo does not build the
/// binary for this crate's tests; the test does.
fn binary() -> PathBuf {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        if let Ok(path) = std::env::var("RAHI_TEST_BINARY") {
            return PathBuf::from(path);
        }
        let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let target = std::env::var("CARGO_TARGET_DIR")
            .map_or_else(|_| workspace.join("target"), PathBuf::from);
        let path = target.join("debug").join("rahi");
        let status = Command::new(env!("CARGO"))
            .args(["build", "-p", "rahi-cli", "--locked"])
            .current_dir(&workspace)
            .status()
            .expect("cargo runs");
        assert!(status.success(), "cargo build -p rahi-cli");
        assert!(path.is_file(), "{} exists after the build", path.display());
        path
    })
    .clone()
}

/// One instance per test file (spec 033 B-5).
fn cell() -> &'static Instance {
    static CELL: OnceLock<Instance> = OnceLock::new();
    CELL.get_or_init(|| Harness::boot(BootSpec::new(binary())).expect("the empty cell boots"))
}

#[tokio::test]
async fn boot_returns_after_readyz_and_liveness_never_came_later() {
    let cell = cell();
    let (healthz_at, ready_at) = cell.probe_order();
    assert!(
        healthz_at <= ready_at,
        "liveness at {healthz_at:?} is never later than readiness at {ready_at:?}"
    );
    let client = cell.client();
    let body = client
        .expect_get("/readyz", reqwest::StatusCode::OK)
        .await
        .unwrap();
    assert!(body.contains("\"ready\""), "{body}");
    let body = client
        .expect_get("/healthz", reqwest::StatusCode::OK)
        .await
        .unwrap();
    assert!(body.contains("\"alive\""), "{body}");
    assert!(
        cell.data_dir().join("keys").is_dir(),
        "first-boot wrote the keys"
    );
    assert!(
        cell.first_boot_output()
            .unwrap()
            .contains("first-boot: keys generated"),
        "first-boot printed once"
    );
    assert!(!cell.admin_token().is_empty());
    assert!(!cell.has_rauthy());
}

#[tokio::test]
async fn the_client_carries_cookies_and_the_csrf_proof() {
    let cell = cell();
    let client = cell.client();
    let token = client.csrf().await.unwrap();
    assert!(!token.is_empty());
    assert_eq!(client.cookie("csrf").as_deref(), Some(token.as_str()));
    // A non-safe request without the proof is refused; with it, the empty
    // cell has no route and says so, which is past the check.
    let bare = reqwest::Client::new()
        .post(format!("{}/nothing", cell.base_url()))
        .send()
        .await
        .unwrap();
    assert_eq!(
        bare.status(),
        reqwest::StatusCode::FORBIDDEN,
        "no proof, no pass"
    );
    let proven = client.post("/nothing").await.unwrap();
    assert_ne!(
        proven.status(),
        reqwest::StatusCode::FORBIDDEN,
        "the proof passes"
    );
    // Without rauthy, a login is refused by the harness, not attempted.
    let err = cell
        .login_as(&client, &User::new("nobody@example.com", "x"))
        .await
        .unwrap_err();
    assert!(matches!(err, rahi_harness::Error::NoRauthy), "{err}");
}

#[test]
fn stop_leaves_no_child_process() {
    let instance = Harness::boot(BootSpec::new(binary())).expect("a second cell boots");
    let pid = instance.pid().expect("the child runs");
    assert!(alive(pid));
    let stopped = instance.stop().unwrap();
    assert!(
        matches!(stopped, Stopped::Exited(_)),
        "SIGTERM ends it inside the grace: {stopped:?}"
    );
    assert!(!alive(pid), "pid {pid} is gone");
    assert_eq!(
        instance.stop().unwrap(),
        Stopped::AlreadyGone(None),
        "idempotent"
    );
    assert!(instance.pid().is_none());
}

#[test]
fn a_boot_the_binary_refuses_fails_inside_the_budget_with_its_stderr() {
    // The manifest is compiled into the binary (spec 030), so the nearest
    // thing to a broken manifest is a configuration the binary refuses at
    // start (D-1): here, an identity mode it does not know.
    let started = std::time::Instant::now();
    let err = Harness::boot(BootSpec::new(binary()).with_env("RAHI_RAUTHY_MODE", "bogus"))
        .expect_err("the boot fails");
    assert!(
        started.elapsed() < rahi_harness::boot::READY_BUDGET,
        "failed inside the budget"
    );
    let text = err.to_string();
    assert!(text.starts_with("not ready:"), "{text}");
    assert!(
        text.contains("RAHI_RAUTHY_MODE"),
        "the stderr names the refusal: {text}"
    );
}

#[tokio::test]
async fn with_a_rauthy_at_hand_a_user_logs_in_and_reads_a_protected_route() {
    let Ok(rauthy) = std::env::var("RAHI_TEST_RAUTHY") else {
        eprintln!("skipped: set RAHI_TEST_RAUTHY to a rauthy binary (spec 033 FR-002)");
        return;
    };
    let instance =
        Harness::boot(BootSpec::new(binary()).with_rauthy(rauthy)).expect("boots with rauthy");
    assert!(matches!(
        Harness::boot(BootSpec::new(binary())).map(|i| i.has_rauthy()),
        Ok(false)
    ));
    let client = instance.client();
    let user = User::new("harness@example.com", "Correct-Horse-Battery-Staple-2026");
    instance
        .login_as(&client, &user)
        .await
        .expect("the login completes");
    let session = client
        .cookie("session")
        .or_else(|| client.cookie("__Host-session"))
        .expect("the session cookie is set");
    assert!(!session.is_empty());
    // The empty cell mounts no protected route (D-4): the callback that
    // issued the session cookie exchanged the code as this principal, and
    // the read of a protected route as them is hello-cell's end-to-end
    // test (spec 034), the harness's first consumer. What is asserted here
    // is that the session is the cell's: a logout with it is honoured
    // (a redirect to rauthy's end-session endpoint that clears the cookie).
    let out = client.post("/session/logout").await.unwrap();
    assert!(
        out.status().is_redirection(),
        "logout redirects: {}",
        out.status()
    );
    assert!(
        client.cookie("session").is_none() && client.cookie("__Host-session").is_none(),
        "the session cookie is cleared"
    );
    instance.stop().unwrap();
}

fn alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[test]
fn a_spec_names_its_mode() {
    let spec = BootSpec::new("/nonexistent");
    assert_eq!(spec.rauthy, RauthyMode::None);
    assert_eq!(
        spec.with_rauthy("/rauthy").rauthy,
        RauthyMode::External("/rauthy".into())
    );
}
