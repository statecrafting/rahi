//! A real listener on a real port, shared by both test binaries.
//!
//! The proxy dials a socket, so a stub that answers over one is the only stub
//! that proves anything about it. Every helper here serves an ordinary axum
//! router on `127.0.0.1:0` and hands back the base URL rauthy would have.

#![allow(clippy::expect_used, clippy::unwrap_used, dead_code)]

use std::collections::BTreeMap;
use std::net::SocketAddr;

use axum::Router;
use rahi_idp::IdpConfig;
use rahi_types::Config;
use tokio::task::JoinHandle;

/// A stub upstream, listening.
pub struct Served {
    /// Where it listens, as `127.0.0.1:<port>`.
    pub addr: SocketAddr,
    handle: JoinHandle<()>,
}

impl Served {
    /// The base URL of the stub.
    pub fn base(&self) -> String {
        format!("http://{}", self.addr)
    }
}

impl Drop for Served {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// Serve `app` on a free loopback port.
pub async fn serve(app: Router) -> Served {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a free port");
    let addr = listener.local_addr().expect("its address");
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Served { addr, handle }
}

/// The cell's configuration for `public_url`, with rauthy at `rauthy_addr`.
pub fn config(public_url: &str, rauthy_addr: SocketAddr) -> Config {
    let addr = rauthy_addr.to_string();
    let env = BTreeMap::from([
        ("RAHI_PUBLIC_URL", public_url),
        ("RAHI_RAUTHY_ADDR", addr.as_str()),
    ]);
    Config::from_env(&env).expect("the fixture environment is well formed")
}

/// The identity configuration for `public_url`, pointed at the stub.
pub fn idp_config(public_url: &str, rauthy_addr: SocketAddr) -> IdpConfig {
    IdpConfig::derive(&config(public_url, rauthy_addr), "hello-cell")
        .expect("the fixture configuration derives")
}
