//! The rahi test harness (spec 033): boot a built cell binary under a
//! throwaway volume with fresh keys and OS-allocated ports, wait on
//! `/readyz` and never on `/healthz`, and hand back an HTTP client with a
//! cookie jar so authenticated flows are testable.
//!
//! No chassis crate is a compile-time dependency. The harness knows the
//! binary's verbs (`first-boot`, `migrate`, `supervise`, `serve`), its
//! `RAHI_*` environment, and its HTTP surface, and nothing else, so it can
//! drive any built cell, including an app in another repository.
//!
//! # Cost, stated (B-5)
//!
//! A boot takes seconds: `first-boot` mints every key, `migrate` and then
//! `serve` each elect the app's single-voter Raft, and with rauthy present
//! its own cluster elects too. Boot **one instance per test file**, never
//! per test, and share it through a `OnceLock`:
//!
//! ```ignore
//! use std::sync::OnceLock;
//! use rahi_harness::{BootSpec, Harness, Instance, RauthyMode};
//!
//! static CELL: OnceLock<Instance> = OnceLock::new();
//!
//! fn cell() -> &'static Instance {
//!     CELL.get_or_init(|| {
//!         Harness::boot(BootSpec {
//!             binary: env!("CARGO_BIN_EXE_rahi").into(),
//!             manifest_dir: None,
//!             rauthy: RauthyMode::None,
//!         })
//!         .expect("the cell boots")
//!     })
//! }
//! ```
//!
//! [`Instance`] is `Send + Sync` for exactly that purpose. It stops the
//! child when dropped; call [`Instance::stop`] to do it explicitly and
//! learn the exit.
//!
//! # Why `/readyz`
//!
//! enrahitu's harness waited on liveness and hid a defect for six weeks:
//! `/healthz` answers as soon as the process serves, before the store has
//! elected and the chain has verified, so a test that waited on it ran
//! against a cell that was not yet a cell. [`Harness::boot`] returns only
//! after `/readyz` is `200`, and its first test asserts that `/healthz`
//! answered earlier.

#![forbid(unsafe_code)]

pub mod boot;
pub mod client;
pub mod cookies;
pub mod rauthy;

pub use boot::{BootSpec, Harness, Instance, RauthyMode, Stopped};
pub use client::Client;
pub use cookies::CookieJar;
pub use rauthy::{Rauthy, User};

/// Everything the harness can fail with.
#[derive(Debug)]
pub enum Error {
    /// The binary could not be run, or a verb before `serve` failed: the
    /// message carries the verb, its exit, and its stderr.
    Boot(String),
    /// The child exited, or `/readyz` never answered `200`, inside the
    /// budget: the message carries the binary's stderr tail.
    NotReady(String),
    /// An HTTP call the harness itself made failed.
    Http(String),
    /// rauthy is not part of this instance ([`RauthyMode::None`]), so a
    /// login was asked of a cell that mounts no identity.
    NoRauthy,
    /// rauthy answered something the driver did not expect.
    Rauthy(String),
    /// A file or directory the harness manages could not be handled.
    Io(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Boot(m) => write!(f, "boot: {m}"),
            Self::NotReady(m) => write!(f, "not ready: {m}"),
            Self::Http(m) => write!(f, "http: {m}"),
            Self::NoRauthy => f.write_str("this instance mounts no identity (RauthyMode::None)"),
            Self::Rauthy(m) => write!(f, "rauthy: {m}"),
            Self::Io(m) => write!(f, "io: {m}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<reqwest::Error> for Error {
    fn from(err: reqwest::Error) -> Self {
        Self::Http(err.to_string())
    }
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err.to_string())
    }
}

/// The harness's result type.
pub type Result<T> = std::result::Result<T, Error>;
