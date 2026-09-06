//! The declared application model (spec 015 B-1, B-2).
//!
//! Encore's lasting idea was a static application model separated from runtime
//! configuration. enrahitu extracted that model from an app's TypeScript with
//! a parser; rahi declares it, because the property that mattered was never
//! the extraction, it was that a *ceiling* exists and the build checks
//! observed usage against it. A parser is a second implementation of the
//! compiler; a TOML file is a document a reviewer can read in an afternoon and
//! an operator can diff between releases.
//!
//! The file is embedded at compile time, so a running cell cannot be pointed
//! at a wider ceiling than the one it was built with:
//!
//! ```no_run
//! # use rahi_kernel::Manifest;
//! # fn f() -> Result<(), rahi_types::Error> {
//! let manifest = Manifest::parse(include_str!("../testdata/manifests/valid.toml"))?;
//! let genesis_parent = manifest.hash()?; // spec 013 B-2's genesis parent
//! # Ok(())
//! # }
//! ```
//!
//! [`Manifest::hash`] is over the parsed model, never over the file's bytes.
//! Reordering the tables, reflowing a list, or adding a comment must not
//! change what the ledger is rooted at, because none of those change what the
//! cell is permitted to do. Changing a grant must, and does.

use std::collections::{BTreeMap, BTreeSet};

use action_gate_core::{Gate, sha256_hex};
use rahi_ledger::Hash;
use rahi_types::{Error, LEDGER_SCHEMA_VERSION};
use serde::{Deserialize, Serialize};

use crate::adjudicate::{GRANT_CHECK_ID, OPTIONAL_CHECKS, build_gate};
use crate::capability::{
    Capability, CapabilityId, CapabilityKind, ResourceFamily, ResourceName, ServiceName,
};

/// Who the cell is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct App {
    /// The application's name, lowercase.
    pub name: ResourceName,
    /// The organization it belongs to, lowercase.
    pub org: ResourceName,
}

/// Everything the cell may address, by name (spec 015 B-1).
///
/// A capability's resource must appear in the list its kind draws from, so a
/// grant cannot name a table nobody declared. Lock keys and notify topics have
/// no list (D-3): they are ephemeral coordination names rather than durable
/// resources, and a capability names them directly.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resources {
    /// SQL tables.
    #[serde(default)]
    pub tables: Vec<ResourceName>,
    /// Key-value namespaces in the cache group.
    #[serde(default)]
    pub kv: Vec<ResourceName>,
    /// Counters in the cache group.
    #[serde(default)]
    pub counters: Vec<ResourceName>,
    /// Secret names. Names only: no value ever appears in a manifest.
    #[serde(default)]
    pub secrets: Vec<ResourceName>,
    /// Egress host patterns, exact or `*.suffix`.
    #[serde(default)]
    pub egress: Vec<ResourceName>,
}

impl Resources {
    /// The declared names of one family, or `None` when the family has no list.
    #[must_use]
    pub fn family(&self, family: ResourceFamily) -> Option<&[ResourceName]> {
        match family {
            ResourceFamily::Table => Some(&self.tables),
            ResourceFamily::Kv => Some(&self.kv),
            ResourceFamily::Counter => Some(&self.counters),
            ResourceFamily::Secret => Some(&self.secrets),
            ResourceFamily::Egress => Some(&self.egress),
            ResourceFamily::Free => None,
        }
    }
}

/// One service's grants: the capability ids it may exercise.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Service {
    /// Capability ids from the manifest's catalog.
    #[serde(default)]
    pub capabilities: Vec<CapabilityId>,
}

/// The gate roster: the optional checks, in evaluation order (D-4).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatePolicy {
    /// Check ids from [`OPTIONAL_CHECKS`]. The capability check is always
    /// first and is never listed here.
    #[serde(default)]
    pub checks: Vec<String>,
}

/// What the cell writes to its decision chain.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerPolicy {
    /// The record schema the app expects, `MAJOR.MINOR.PATCH`.
    pub schema_version: String,
    /// The largest record the cell will write.
    pub max_record_bytes: u64,
}

/// How the cell is observed (spec 023 reads this).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observability {
    /// Where the metrics endpoint is served, an absolute path.
    pub metrics_path: String,
    /// Whether OpenTelemetry export is on.
    pub otel: bool,
}

/// The role the operator verbs require (spec 024 enforces it).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Auth {
    /// The rauthy role name an operator carries.
    pub operator_role: String,
}

/// The app's own contract version, for its clients.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contract {
    /// `MAJOR.MINOR.PATCH`.
    pub version: String,
}

/// The declared ceiling of one cell.
///
/// Parse it with [`Manifest::parse`]; everything else on this type assumes the
/// validation that ran there. The fields are public because a manifest is
/// plain data an operator reads, but there is no constructor that skips
/// validation, so a `Manifest` in hand is one that passed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Identity.
    pub app: App,
    /// Everything addressable, by name.
    #[serde(default)]
    pub resources: Resources,
    /// The capability catalog.
    #[serde(default)]
    pub capabilities: Vec<Capability>,
    /// Per service grants.
    #[serde(default)]
    pub services: BTreeMap<ServiceName, Service>,
    /// The gate roster.
    #[serde(default)]
    pub gate: GatePolicy,
    /// Ledger policy.
    pub ledger: LedgerPolicy,
    /// Observability policy.
    pub observability: Observability,
    /// The operator role.
    pub auth: Auth,
    /// The app's contract version.
    pub contract: Contract,
}

impl Manifest {
    /// Parse and validate a manifest.
    ///
    /// Unknown keys are refused at every level: a typo in a governance
    /// document is a hole in the ceiling that reads like a rule.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when the TOML does not parse, when it carries a
    /// key this schema does not define, or when any rule in
    /// [`Manifest::validate`] fails.
    pub fn parse(toml_text: &str) -> Result<Self, Error> {
        let manifest: Self = toml::from_str(toml_text)
            .map_err(|e| Error::Validation(format!("the manifest does not parse: {e}")))?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Check every rule a parsed manifest must satisfy.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`], naming the offending id, service, or resource.
    pub fn validate(&self) -> Result<(), Error> {
        self.validate_resources()?;
        self.validate_capabilities()?;
        self.validate_services()?;
        self.validate_gate()?;
        self.validate_policy()
    }

    /// Every declared resource, deduplicated per family.
    fn validate_resources(&self) -> Result<(), Error> {
        for family in [
            ResourceFamily::Table,
            ResourceFamily::Kv,
            ResourceFamily::Counter,
            ResourceFamily::Secret,
            ResourceFamily::Egress,
        ] {
            let Some(names) = self.resources.family(family) else {
                continue;
            };
            let mut seen = BTreeSet::new();
            for name in names {
                if !seen.insert(name.clone()) {
                    return Err(Error::Validation(format!(
                        "resources.{family} declares {name:?} twice"
                    )));
                }
            }
        }
        Ok(())
    }

    /// Unique ids, and a resource its kind's family declares.
    fn validate_capabilities(&self) -> Result<(), Error> {
        let mut seen = BTreeSet::new();
        for cap in &self.capabilities {
            if cap.id.as_str().is_empty() {
                return Err(Error::Validation(
                    "a capability needs an id: it is what a grant names".to_owned(),
                ));
            }
            if !seen.insert(cap.id.clone()) {
                return Err(Error::Validation(format!(
                    "capability {} is declared twice",
                    cap.id
                )));
            }
            let family = cap.kind.family();
            if let Some(declared) = self.resources.family(family)
                && !declared.contains(&cap.resource)
            {
                return Err(Error::Validation(format!(
                    "capability {} names {:?}, which resources.{family} does not declare",
                    cap.id, cap.resource
                )));
            }
            if let Some(table) = &cap.constraints.table
                && !self.resources.tables.contains(table)
            {
                return Err(Error::Validation(format!(
                    "capability {} constrains to table {table:?}, which resources.tables does \
                     not declare",
                    cap.id
                )));
            }
        }
        Ok(())
    }

    /// Every granted capability id exists in the catalog.
    fn validate_services(&self) -> Result<(), Error> {
        let catalog: BTreeSet<&CapabilityId> = self.capabilities.iter().map(|c| &c.id).collect();
        for (service, grants) in &self.services {
            let mut seen = BTreeSet::new();
            for id in &grants.capabilities {
                if !catalog.contains(id) {
                    return Err(Error::Validation(format!(
                        "service {service} is granted {id}, which the capability catalog does \
                         not declare"
                    )));
                }
                if !seen.insert(id.clone()) {
                    return Err(Error::Validation(format!(
                        "service {service} is granted {id} twice"
                    )));
                }
            }
        }
        Ok(())
    }

    /// The roster names known optional checks, once each, and never the
    /// capability check.
    fn validate_gate(&self) -> Result<(), Error> {
        let mut seen = BTreeSet::new();
        for id in &self.gate.checks {
            if id == GRANT_CHECK_ID {
                return Err(Error::Validation(format!(
                    "gate.checks names {GRANT_CHECK_ID}, which is always first and never listed: \
                     deny by default is not a roster entry"
                )));
            }
            if !OPTIONAL_CHECKS.contains(&id.as_str()) {
                return Err(Error::Validation(format!(
                    "{id:?} is not a gate check; the catalog is closed: {}",
                    OPTIONAL_CHECKS.join(", ")
                )));
            }
            if !seen.insert(id.clone()) {
                return Err(Error::Validation(format!("gate.checks names {id:?} twice")));
            }
        }
        Ok(())
    }

    /// The ledger, observability, auth, and contract policies.
    fn validate_policy(&self) -> Result<(), Error> {
        let major = |v: &str| v.split('.').next().unwrap_or_default().to_owned();
        if major(&self.ledger.schema_version) != major(LEDGER_SCHEMA_VERSION) {
            return Err(Error::Validation(format!(
                "ledger.schema_version {:?} is a different major than the chain this crate \
                 writes ({LEDGER_SCHEMA_VERSION})",
                self.ledger.schema_version
            )));
        }
        if self.ledger.max_record_bytes == 0 {
            return Err(Error::Validation(
                "ledger.max_record_bytes is greater than zero".to_owned(),
            ));
        }
        if !self.observability.metrics_path.starts_with('/') {
            return Err(Error::Validation(format!(
                "observability.metrics_path {:?} is an absolute path",
                self.observability.metrics_path
            )));
        }
        if self.auth.operator_role.is_empty() {
            return Err(Error::Validation(
                "auth.operator_role names the rauthy role an operator carries".to_owned(),
            ));
        }
        if self.contract.version.is_empty() {
            return Err(Error::Validation(
                "contract.version is the app's own MAJOR.MINOR.PATCH".to_owned(),
            ));
        }
        Ok(())
    }

    /// The grant table: service name to the capabilities it may exercise.
    ///
    /// Resolved from the catalog, so a consumer never has to join the two
    /// lists itself. Order inside a service follows the service's own list,
    /// which is the order a constraint denial reports.
    #[must_use]
    pub fn grants(&self) -> BTreeMap<String, Vec<Capability>> {
        let catalog: BTreeMap<&CapabilityId, &Capability> =
            self.capabilities.iter().map(|c| (&c.id, c)).collect();
        self.services
            .iter()
            .map(|(service, grants)| {
                let caps = grants
                    .capabilities
                    .iter()
                    .filter_map(|id| catalog.get(id).map(|c| (*c).clone()))
                    .collect();
                (service.as_str().to_owned(), caps)
            })
            .collect()
    }

    /// The capabilities granted to one service, resolved from the catalog.
    #[must_use]
    pub fn granted(&self, service: &ServiceName) -> Option<Vec<Capability>> {
        let grants = self.services.get(service)?;
        let catalog: BTreeMap<&CapabilityId, &Capability> =
            self.capabilities.iter().map(|c| (&c.id, c)).collect();
        Some(
            grants
                .capabilities
                .iter()
                .filter_map(|id| catalog.get(id).map(|c| (*c).clone()))
                .collect(),
        )
    }

    /// Whether the manifest declares `kind` on `resource` for `service`.
    ///
    /// The build-time question ([`crate::verify`]): a grant exists, whatever
    /// its constraints say about a particular runtime request.
    #[must_use]
    pub fn covers(&self, service: &ServiceName, kind: CapabilityKind, resource: &str) -> bool {
        self.granted(service)
            .is_some_and(|caps| caps.iter().any(|cap| cap.covers(kind, resource)))
    }

    /// Assemble the gate this manifest describes.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when the roster names an unknown check. A
    /// manifest from [`Manifest::parse`] cannot.
    pub fn gate(&self) -> Result<Gate, Error> {
        build_gate(self)
    }

    /// The manifest hash: what the decision chain is rooted at (spec 015 B-2).
    ///
    /// sha256 over the canonical (key-sorted) JSON of the parsed model, then
    /// the gate's `config_hash()`. Over the model and not the text, so
    /// whitespace and key order do not move the root; over the gate too, so a
    /// reconfigured roster is a different cell even when the grants are
    /// identical.
    ///
    /// # Errors
    ///
    /// [`Error::Integrity`] when the model does not serialize, which would
    /// mean the manifest types drifted; [`Error::Validation`] when the gate
    /// cannot be assembled.
    pub fn hash(&self) -> Result<Hash, Error> {
        let value = serde_json::to_value(self)
            .map_err(|e| Error::Integrity(format!("the manifest does not serialize: {e}")))?;
        let model = canonical_keysort_json::to_canonical_string(&value);
        let gate = self.gate()?.config_hash();
        Hash::parse(sha256_hex(format!("{model}\n{gate}").as_bytes()))
    }
}
