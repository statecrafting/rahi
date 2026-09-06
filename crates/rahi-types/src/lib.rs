//! The plain-data substrate of the rahi chassis (spec 010).
//!
//! Every other workspace crate depends on this one, and this one depends on
//! nothing in the workspace. It holds the value types whose meaning must not
//! drift between crates: the workspace [`Error`] with its four exit codes,
//! the [`Principal`] whose only identifier is the IdP's subject, the
//! [`Revision`] and [`FenceToken`] newtypes the store's invariants are
//! written against, the [`Config`] tree derived from one public URL, and the
//! schema version constants.
//!
//! Nothing here performs I/O, reads a clock, or reads the process
//! environment. Callers inject what they know; this crate only gives it a
//! shape.

#![forbid(unsafe_code)]

pub mod config;
pub mod error;
pub mod principal;
pub mod revision;
pub mod version;

pub use config::{Config, CookieScheme, EnvReader, HiqliteConfig, PublicUrl};
pub use error::{Error, Result};
pub use principal::{Email, Principal, Role, Sub, UnixSeconds};
pub use revision::{FenceToken, Revision};
pub use version::{LEDGER_SCHEMA_VERSION, MANIFEST_SCHEMA_VERSION, STORE_SCHEMA_VERSION};
