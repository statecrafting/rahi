//! Schema version constants (spec 010 B-8).
//!
//! Each is `MAJOR.MINOR.PATCH`. Bumping a MAJOR is a spec amendment, never a
//! build-session decision.

/// Version of the application store schema (`rahi-store`, spec 011).
pub const STORE_SCHEMA_VERSION: &str = "1.0.0";

/// Version of the decision ledger record and segment schema (spec 013).
pub const LEDGER_SCHEMA_VERSION: &str = "1.0.0";

/// Version of the capability manifest schema (spec 015).
pub const MANIFEST_SCHEMA_VERSION: &str = "1.0.0";
