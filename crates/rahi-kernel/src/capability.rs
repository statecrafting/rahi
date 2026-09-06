//! The closed capability vocabulary (spec 015 B-1).
//!
//! A capability is a named permission: one [`CapabilityKind`] from a fixed
//! list, the [`ResourceName`] it applies to, and optional [`Constraints`] that
//! narrow it further. The list is closed on purpose. An open vocabulary would
//! let a manifest declare a permission the kernel has no way to enforce, and a
//! ceiling nobody can check is not a ceiling.
//!
//! Names are lowercase everywhere, including secret names. A manifest that
//! spells a secret `DATABASE_URL` is refused rather than silently treated as a
//! different resource from `database_url`, because two spellings of one
//! resource are two holes in one ceiling.

use std::fmt;

use rahi_types::Error;
use serde::{Deserialize, Serialize};

pub use rahi_ledger::CapabilityId;

/// The longest a declared name may be.
const MAX_NAME_LEN: usize = 128;

/// Which declared resource list a kind draws its resource from (spec 015 B-1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ResourceFamily {
    /// `resources.tables`.
    Table,
    /// `resources.kv`.
    Kv,
    /// `resources.counters`.
    Counter,
    /// `resources.secrets`.
    Secret,
    /// `resources.egress`, whose members are host patterns.
    Egress,
    /// No declared list: a lock key or a notify topic (D-3).
    Free,
}

impl ResourceFamily {
    /// The `resources` key this family is declared under, or `none`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Table => "tables",
            Self::Kv => "kv",
            Self::Counter => "counters",
            Self::Secret => "secrets",
            Self::Egress => "egress",
            Self::Free => "none",
        }
    }
}

impl fmt::Display for ResourceFamily {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a capability permits: the closed vocabulary of spec 015 B-1.
///
/// The wire form is the dotted string (`db.write`, `notify.publish`), which is
/// what a manifest writes, what an [`action_gate_types::ActionContext`] carries
/// as its action, and what a ledgered decision records as its kind. An
/// unrecognized string is [`Error::Validation`]; there is no catch-all.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum CapabilityKind {
    /// Read rows from a declared table.
    DbRead,
    /// Write rows to a declared table.
    DbWrite,
    /// Run a multi-statement transaction over declared tables.
    DbTxn,
    /// Apply schema migrations.
    DbMigrate,
    /// Read a key from a declared kv namespace.
    KvGet,
    /// Write a key to a declared kv namespace.
    KvPut,
    /// Delete a key from a declared kv namespace.
    KvDelete,
    /// Read a declared counter.
    CounterGet,
    /// Add to a declared counter.
    CounterAdd,
    /// Take a fenced lease on a lock key.
    LockAcquire,
    /// Publish a notify envelope on a topic.
    NotifyPublish,
    /// Subscribe to a notify topic.
    NotifyListen,
    /// Read a declared secret by name.
    SecretRead,
    /// Reach a declared egress host.
    HttpEgress,
}

impl CapabilityKind {
    /// Every kind, in the order spec 015 B-1 lists them.
    pub const ALL: [Self; 14] = [
        Self::DbRead,
        Self::DbWrite,
        Self::DbTxn,
        Self::DbMigrate,
        Self::KvGet,
        Self::KvPut,
        Self::KvDelete,
        Self::CounterGet,
        Self::CounterAdd,
        Self::LockAcquire,
        Self::NotifyPublish,
        Self::NotifyListen,
        Self::SecretRead,
        Self::HttpEgress,
    ];

    /// The dotted wire form.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DbRead => "db.read",
            Self::DbWrite => "db.write",
            Self::DbTxn => "db.txn",
            Self::DbMigrate => "db.migrate",
            Self::KvGet => "kv.get",
            Self::KvPut => "kv.put",
            Self::KvDelete => "kv.delete",
            Self::CounterGet => "counter.get",
            Self::CounterAdd => "counter.add",
            Self::LockAcquire => "lock.acquire",
            Self::NotifyPublish => "notify.publish",
            Self::NotifyListen => "notify.listen",
            Self::SecretRead => "secret.read",
            Self::HttpEgress => "http.egress",
        }
    }

    /// Parse the dotted wire form.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when `kind` is outside the closed vocabulary. The
    /// message lists the whole vocabulary, because the author of a manifest
    /// that got it wrong is the one who needs to see it.
    pub fn parse(kind: &str) -> Result<Self, Error> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == kind)
            .ok_or_else(|| {
                let known = Self::ALL.map(Self::as_str).join(", ");
                Error::Validation(format!(
                    "{kind:?} is not a capability kind; the vocabulary is closed: {known}"
                ))
            })
    }

    /// The resource list this kind's resource must be declared in.
    #[must_use]
    pub const fn family(self) -> ResourceFamily {
        match self {
            Self::DbRead | Self::DbWrite | Self::DbTxn | Self::DbMigrate => ResourceFamily::Table,
            Self::KvGet | Self::KvPut | Self::KvDelete => ResourceFamily::Kv,
            Self::CounterGet | Self::CounterAdd => ResourceFamily::Counter,
            Self::SecretRead => ResourceFamily::Secret,
            Self::HttpEgress => ResourceFamily::Egress,
            Self::LockAcquire | Self::NotifyPublish | Self::NotifyListen => ResourceFamily::Free,
        }
    }
}

impl fmt::Display for CapabilityKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl TryFrom<String> for CapabilityKind {
    type Error = Error;

    fn try_from(value: String) -> Result<Self, Error> {
        Self::parse(&value)
    }
}

impl From<CapabilityKind> for String {
    fn from(kind: CapabilityKind) -> Self {
        kind.as_str().to_owned()
    }
}

/// A declared resource name: a table, a kv namespace, a counter, a secret, or
/// an egress host pattern.
///
/// Lowercase, non-empty, and drawn from `a-z 0-9 _ . : - *`, with `*` reserved
/// for the leading label of an egress wildcard. One shape for every family
/// keeps a manifest's names comparable to the strings a request carries at
/// runtime without a per-family normalization step nobody would remember.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ResourceName(String);

impl ResourceName {
    /// Parse a resource name.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when the name is empty, too long, contains an
    /// uppercase letter, or contains a character outside the allowed set.
    pub fn parse(name: impl Into<String>) -> Result<Self, Error> {
        let name = name.into();
        if name.is_empty() {
            return Err(Error::Validation("a resource name is not empty".to_owned()));
        }
        if name.len() > MAX_NAME_LEN {
            return Err(Error::Validation(format!(
                "resource name {name:?} is longer than {MAX_NAME_LEN} bytes"
            )));
        }
        if name.bytes().any(|b| b.is_ascii_uppercase()) {
            return Err(Error::Validation(format!(
                "resource name {name:?} is not lowercase; two spellings of one resource are two \
                 holes in one ceiling"
            )));
        }
        let allowed = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.:-*".contains(&b);
        if !name.bytes().all(allowed) {
            return Err(Error::Validation(format!(
                "resource name {name:?} may hold only a-z, 0-9, and _ . : - *"
            )));
        }
        Ok(Self(name))
    }

    /// The name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether `host` matches this name read as an egress pattern.
    ///
    /// Exact unless the pattern starts with `*.`, in which case it matches any
    /// host with at least one more label: `*.example.com` covers
    /// `api.example.com` and not `example.com`, which is the reading every
    /// certificate authority uses and therefore the one an operator expects.
    #[must_use]
    pub fn matches_host(&self, host: &str) -> bool {
        match self.0.strip_prefix('*') {
            Some(suffix) => suffix.starts_with('.') && host.ends_with(suffix),
            None => self.0 == host,
        }
    }
}

impl fmt::Display for ResourceName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for ResourceName {
    type Error = Error;

    fn try_from(value: String) -> Result<Self, Error> {
        Self::parse(value)
    }
}

impl From<ResourceName> for String {
    fn from(name: ResourceName) -> Self {
        name.0
    }
}

/// The name of a service: one bundle of grants inside the app.
///
/// A service is the unit a grant is made to, so it is also the unit a denial
/// names. Lowercase, starting with a letter.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ServiceName(String);

impl ServiceName {
    /// Parse a service name.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when the name is empty, too long, does not start
    /// with a lowercase letter, or holds anything but `a-z 0-9 _ -`.
    pub fn parse(name: impl Into<String>) -> Result<Self, Error> {
        let name = name.into();
        let first = name
            .bytes()
            .next()
            .ok_or_else(|| Error::Validation("a service name is not empty".to_owned()))?;
        if name.len() > MAX_NAME_LEN {
            return Err(Error::Validation(format!(
                "service name {name:?} is longer than {MAX_NAME_LEN} bytes"
            )));
        }
        if !first.is_ascii_lowercase() {
            return Err(Error::Validation(format!(
                "service name {name:?} starts with a lowercase letter"
            )));
        }
        let allowed =
            |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-';
        if !name.bytes().all(allowed) {
            return Err(Error::Validation(format!(
                "service name {name:?} may hold only a-z, 0-9, _ and -"
            )));
        }
        Ok(Self(name))
    }

    /// The name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ServiceName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for ServiceName {
    type Error = Error;

    fn try_from(value: String) -> Result<Self, Error> {
        Self::parse(value)
    }
}

impl From<ServiceName> for String {
    fn from(name: ServiceName) -> Self {
        name.0
    }
}

/// The predicates that narrow a capability past its resource (spec 015 B-4).
///
/// All three are matched in [`crate::adjudicate`], against the corresponding
/// field of the request. A constraint the request says nothing about does not
/// pass by default: a grant that pins `key_prefix` and a request that carries
/// no key is a denial, because a capability narrowed to part of a namespace
/// cannot admit an operation whose place in that namespace is unknown.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Constraints {
    /// The request's key must start with this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_prefix: Option<String>,
    /// The request's table must equal this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub table: Option<ResourceName>,
    /// The request's host must match this pattern.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<ResourceName>,
}

impl Constraints {
    /// Whether every declared predicate holds. `Err` carries the reason.
    pub(crate) fn admits(
        &self,
        key: Option<&str>,
        table: Option<&str>,
        host: Option<&str>,
    ) -> Result<(), String> {
        if let Some(prefix) = &self.key_prefix {
            match key {
                Some(key) if key.starts_with(prefix.as_str()) => {}
                Some(key) => return Err(format!("key {key:?} is outside key_prefix {prefix:?}")),
                None => {
                    return Err(format!(
                        "the request carries no key to match key_prefix {prefix:?}"
                    ));
                }
            }
        }
        if let Some(want) = &self.table {
            match table {
                Some(table) if table == want.as_str() => {}
                Some(table) => {
                    return Err(format!("table {table:?} is not the granted table {want}"));
                }
                None => return Err(format!("the request carries no table to match {want}")),
            }
        }
        if let Some(pattern) = &self.host {
            match host {
                Some(host) if pattern.matches_host(host) => {}
                Some(host) => {
                    return Err(format!("host {host:?} is outside host pattern {pattern}"));
                }
                None => return Err(format!("the request carries no host to match {pattern}")),
            }
        }
        Ok(())
    }

    /// Whether nothing is declared.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.key_prefix.is_none() && self.table.is_none() && self.host.is_none()
    }
}

/// One declared capability: an entry in the manifest's catalog.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capability {
    /// Unique within the manifest; what a service's grant list names and what
    /// a ledgered decision records.
    pub id: CapabilityId,
    /// What it permits.
    pub kind: CapabilityKind,
    /// The resource it permits it on, declared in the kind's family.
    pub resource: ResourceName,
    /// What narrows it further. Absent means the whole resource.
    #[serde(default)]
    pub constraints: Constraints,
}

impl Capability {
    /// Whether this capability is about `kind` on `resource`.
    ///
    /// For [`CapabilityKind::HttpEgress`] the declared resource is a host
    /// pattern, so a request naming a concrete host matches the pattern that
    /// covers it. Every other family compares by name.
    #[must_use]
    pub fn covers(&self, kind: CapabilityKind, resource: &str) -> bool {
        if self.kind != kind {
            return false;
        }
        if kind.family() == ResourceFamily::Egress {
            self.resource.matches_host(resource)
        } else {
            self.resource.as_str() == resource
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn the_kind_vocabulary_is_closed_and_round_trips() {
        for kind in CapabilityKind::ALL {
            assert_eq!(CapabilityKind::parse(kind.as_str()), Ok(kind));
        }
        let err = CapabilityKind::parse("db.drop").expect_err("not a kind");
        assert!(err.message().contains("db.write"), "{err}");
        assert_eq!(err.kind(), "validation");
    }

    #[test]
    fn a_resource_name_is_lowercase() {
        assert!(ResourceName::parse("notes").is_ok());
        assert!(ResourceName::parse("*.example.com").is_ok());
        let err = ResourceName::parse("DATABASE_URL").expect_err("uppercase is refused");
        assert!(err.message().contains("lowercase"), "{err}");
        assert!(ResourceName::parse("").is_err());
        assert!(ResourceName::parse("notes/rows").is_err());
    }

    #[test]
    fn an_egress_wildcard_needs_a_label_of_its_own() {
        let pattern = ResourceName::parse("*.example.com").expect("parses");
        assert!(pattern.matches_host("api.example.com"));
        assert!(pattern.matches_host("a.b.example.com"));
        assert!(!pattern.matches_host("example.com"));
        assert!(!pattern.matches_host("evil-example.com"));
    }

    #[test]
    fn a_declared_constraint_is_not_satisfied_by_silence() {
        let c = Constraints {
            key_prefix: Some("demo:".to_owned()),
            ..Constraints::default()
        };
        assert!(c.admits(Some("demo:x"), None, None).is_ok());
        assert!(c.admits(Some("rl:x"), None, None).is_err());
        assert!(c.admits(None, None, None).is_err());
    }
}
