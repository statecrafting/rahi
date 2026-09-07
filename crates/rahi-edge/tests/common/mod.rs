//! One booted cell, shared by both test binaries (spec 020 FR-001).
//!
//! The router is tested against a real single-voter node, a real chain, and a
//! real kernel, because a probe that answers from a mock proves nothing about
//! the thing the harness (spec 033) waits on.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    dead_code
)]

use std::collections::BTreeMap;
use std::net::{SocketAddr, TcpListener};
use std::path::Path;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use rahi_edge::AppState;
use rahi_kernel::{Kernel, KernelOptions, Manifest};
use rahi_ledger::{Ledger, LedgerSigner};
use rahi_store::{EncKey, EncKeys, Store, StoreConfig, StoreHandle, StoreSecrets};
use rahi_types::Config;
use tower::ServiceExt as _;

/// The fixture manifest the kernel boots against.
pub const MANIFEST: &str = include_str!("../../testdata/manifest.toml");

/// A cell whose store, chain, and kernel are all real.
pub struct Cell {
    pub state: AppState,
    pub store: StoreHandle,
    pub node: Store,
    pub dir: tempfile::TempDir,
}

impl Cell {
    /// Stop the node under the cell, leaving the router in place.
    pub async fn stop(&self) {
        self.node.shutdown().await.expect("the node stops");
    }
}

/// Boot a cell whose public origin is `public_url`.
pub async fn boot(public_url: &str) -> Cell {
    let dir = tempfile::tempdir().expect("a temp dir");
    let node = Store::open(&store_config(&dir.path().join("hiqlite")))
        .await
        .expect("a single-voter node opens");
    let store = node.handle();
    let manifest = Manifest::parse(MANIFEST).expect("the fixture manifest parses");
    let ledger = Ledger::open(
        store.clone(),
        LedgerSigner::from_seed([9u8; 32]),
        manifest.hash().expect("the manifest hashes"),
    )
    .await
    .expect("the chain opens and verifies");
    let kernel = Kernel::boot_with(
        manifest,
        store.clone(),
        ledger.clone(),
        KernelOptions {
            queue_capacity: 16,
            clock: Some(std::sync::Arc::new(|| 1_767_225_600)),
        },
    )
    .await
    .expect("the kernel boots against its own manifest");

    let state = AppState::new(kernel, store.clone(), ledger, config(public_url));
    Cell {
        state,
        store,
        node,
        dir,
    }
}

/// The configuration tree derived from one public URL (spec 010 B-7).
pub fn config(public_url: &str) -> Config {
    let env = BTreeMap::from([("RAHI_PUBLIC_URL", public_url)]);
    Config::from_env(&env).expect("the fixture environment is well formed")
}

/// Drive one request through `router` and read the whole answer.
pub async fn send(router: &Router, request: Request<Body>) -> Answer {
    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("the router is infallible");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("the body is readable");
    Answer {
        status,
        headers,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

/// A whole response, read into memory.
pub struct Answer {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: String,
}

impl Answer {
    /// The body as JSON. Panics when it is not JSON, which is a test failure
    /// either way.
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).expect("the body is JSON")
    }

    /// The value of `name`, as a string.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|value| value.to_str().ok())
    }
}

/// A `GET` for `path` with no cookies and no connection info.
pub fn get(path: &str) -> Request<Body> {
    Request::builder()
        .uri(path)
        .body(Body::empty())
        .expect("a well-formed request")
}

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
