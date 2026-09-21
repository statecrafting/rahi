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
use std::fmt;

use action_gate_core::{Gate, sha256_hex};
use rahi_ledger::Hash;
use rahi_types::{Error, LEDGER_SCHEMA_VERSION, MANIFEST_SCHEMA_VERSION};
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

/// The default access token lifetime, in seconds (spec 038 B-4).
pub const DEFAULT_ACCESS_TOKEN_LIFETIME_SECS: u64 = 600;

/// The default refresh token lifetime of a native client, in seconds
/// (spec 038 B-4). One day, in place of rauthy's 72-hour device-grant
/// default.
pub const DEFAULT_NATIVE_REFRESH_LIFETIME_SECS: u64 = 86_400;

/// rauthy's own bound on a client's access token lifetime, in seconds.
///
/// `UpdateClientRequest` validates `10 <= access_token_lifetime <= 86400`. A
/// manifest that names a number outside it is refused here, where the
/// document is read, rather than by rauthy's validator at the first boot that
/// tries to apply it.
pub const MAX_ACCESS_TOKEN_LIFETIME_SECS: u64 = 86_400;

/// The granularity rauthy's refresh token lifetime is configured at, in
/// seconds.
///
/// `DEVICE_GRANT_REFRESH_TOKEN_LIFETIME` is a number of hours, so a value
/// that is not a whole number of them cannot be applied and is refused here
/// rather than silently rounded (spec 038 D-11).
pub const NATIVE_REFRESH_GRANULARITY_SECS: u64 = 3_600;

/// One grant a native client may use (spec 038 B-2).
///
/// Closed, and named as the manifest spells it rather than as OAuth spells
/// it on the wire: a document an operator diffs says `device_code`, and
/// [`NativeFlow::grant`] is the one place that knows the URN.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeFlow {
    /// RFC 8628, what a command-line client logs in with.
    DeviceCode,
    /// RFC 6749 with PKCE, what a client with a loopback listener uses.
    AuthorizationCode,
    /// RFC 6749 §6, beside either of the two above and never alone.
    RefreshToken,
}

impl NativeFlow {
    /// The grant type as rauthy names it on the wire.
    #[must_use]
    pub const fn grant(self) -> &'static str {
        match self {
            Self::DeviceCode => "urn:ietf:params:oauth:grant-type:device_code",
            Self::AuthorizationCode => "authorization_code",
            Self::RefreshToken => "refresh_token",
        }
    }

    /// Whether this flow logs a person in, as opposed to renewing a grant
    /// one of them already made.
    #[must_use]
    pub const fn is_login(self) -> bool {
        matches!(self, Self::DeviceCode | Self::AuthorizationCode)
    }
}

impl fmt::Display for NativeFlow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::DeviceCode => "device_code",
            Self::AuthorizationCode => "authorization_code",
            Self::RefreshToken => "refresh_token",
        };
        f.write_str(name)
    }
}

/// A public client this cell declares for a program of its own (038 B-2).
///
/// It is manifest content and not deployment configuration (038 D-4): a
/// native client is part of what the cell permits, so it belongs inside the
/// ceiling the manifest declares, and declaring one moves the manifest hash
/// the way any other change to the ceiling does.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeClient {
    /// The client id, as rauthy will hold it: `[a-zA-Z0-9._-]{2,256}`.
    pub id: String,
    /// The grants this client may use. At least one of them logs a person
    /// in; `refresh_token` stands beside one, never alone.
    pub flows: Vec<NativeFlow>,
    /// The scopes it may be granted, a subset of what the cell's bearer
    /// routes require ([`Manifest::validate_native_scopes`]).
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Where an authorization code comes back to: loopback only (RFC 8252).
    /// Empty unless `flows` includes `authorization_code`.
    #[serde(default)]
    pub redirect_uris: Vec<String>,
}

impl NativeClient {
    /// Whether this client may use `flow`.
    #[must_use]
    pub fn has_flow(&self, flow: NativeFlow) -> bool {
        self.flows.contains(&flow)
    }

    /// The grant types rauthy is told to enable, in the manifest's order.
    #[must_use]
    pub fn grants(&self) -> Vec<String> {
        self.flows
            .iter()
            .map(|flow| flow.grant().to_owned())
            .collect()
    }
}

/// The role the operator verbs require (spec 024 enforces it), the token
/// lifetimes this cell fixes, and the native clients it declares.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Auth {
    /// The rauthy role name an operator carries.
    pub operator_role: String,
    /// How long an access token this cell's clients are issued lives, in
    /// seconds (spec 038 B-4). Absent means
    /// [`DEFAULT_ACCESS_TOKEN_LIFETIME_SECS`].
    ///
    /// Optional in the type, and skipped when it is absent, so a manifest
    /// that says nothing about lifetimes serializes exactly as it did before
    /// this table grew: the model is what [`Manifest::hash`] digests, and a
    /// field defaulted into it would move the genesis parent of every chain
    /// whose manifest never changed (spec 038 D-9).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_token_lifetime_secs: Option<u64>,
    /// How long a native client's refresh token lives, in seconds
    /// (spec 038 B-4). Absent means
    /// [`DEFAULT_NATIVE_REFRESH_LIFETIME_SECS`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_refresh_lifetime_secs: Option<u64>,
    /// Whether a browser logout also deny-lists the subject's bearer tokens
    /// (spec 038 B-5).
    #[serde(default, skip_serializing_if = "is_false")]
    pub logout_revokes_bearer: bool,
    /// The public clients this cell declares (spec 038 B-2).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub native_clients: Vec<NativeClient>,
}

/// Whether `value` is false, for `skip_serializing_if`.
#[allow(clippy::trivially_copy_pass_by_ref)]
const fn is_false(value: &bool) -> bool {
    !*value
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
    /// The manifest schema this document was written for,
    /// `MAJOR.MINOR.PATCH` (spec 039 B-8). Optional in the type so that a
    /// manifest that names none is refused by
    /// [`Manifest::validate`] with a message about the schema rather than by
    /// serde with one about a missing field.
    #[serde(default)]
    pub schema_version: Option<String>,
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
        self.validate_schema_version()?;
        self.validate_resources()?;
        self.validate_capabilities()?;
        self.validate_services()?;
        self.validate_gate()?;
        self.validate_policy()?;
        self.validate_auth()
    }

    /// The manifest names the schema it was written for, and this build
    /// speaks its major (spec 039 B-8).
    ///
    /// A manifest outlives the binary that first read it: a document that
    /// does not say which schema it follows leaves a reader guessing which
    /// rules its silence means, and silence is what
    /// `deny_unknown_fields` exists to refuse everywhere else.
    fn validate_schema_version(&self) -> Result<(), Error> {
        let Some(named) = &self.schema_version else {
            return Err(Error::Validation(format!(
                "the manifest names no schema_version; this build speaks \
                 {MANIFEST_SCHEMA_VERSION}, so write schema_version = \
                 \"{MANIFEST_SCHEMA_VERSION}\" at the top of the document"
            )));
        };
        let major = major_of(named).ok_or_else(|| {
            Error::Validation(format!(
                "the manifest's schema_version {named:?} is not MAJOR.MINOR.PATCH"
            ))
        })?;
        let ours = major_of(MANIFEST_SCHEMA_VERSION).unwrap_or_default();
        if major == ours {
            return Ok(());
        }
        Err(Error::Validation(format!(
            "the manifest names schema_version {named:?} and this build speaks \
             {MANIFEST_SCHEMA_VERSION}: a different major is a different schema, and \
             nothing here can read it"
        )))
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

    /// The `[auth]` table: the lifetimes, and every declared native client
    /// (spec 038 B-2, B-4).
    ///
    /// What is *not* checked here is the one rule this document cannot
    /// decide alone: whether a client's scopes are ones the cell's bearer
    /// routes require. That is a fact about mounted routes, which the kernel
    /// does not and must not know, so it is
    /// [`Manifest::validate_native_scopes`], checked at boot where both
    /// halves are in hand (spec 038 D-9).
    fn validate_auth(&self) -> Result<(), Error> {
        let lifetime = self.access_token_lifetime_secs();
        if !(10..=MAX_ACCESS_TOKEN_LIFETIME_SECS).contains(&lifetime) {
            return Err(Error::Validation(format!(
                "auth.access_token_lifetime_secs is {lifetime}, and rauthy accepts 10 to \
                 {MAX_ACCESS_TOKEN_LIFETIME_SECS} seconds"
            )));
        }
        let refresh = self.native_refresh_lifetime_secs();
        if refresh < NATIVE_REFRESH_GRANULARITY_SECS
            || !refresh.is_multiple_of(NATIVE_REFRESH_GRANULARITY_SECS)
        {
            return Err(Error::Validation(format!(
                "auth.native_refresh_lifetime_secs is {refresh}, and rauthy configures the \
                 refresh lifetime in whole hours: name a positive multiple of \
                 {NATIVE_REFRESH_GRANULARITY_SECS}"
            )));
        }

        let mut seen = BTreeSet::new();
        for client in &self.auth.native_clients {
            if !is_client_id(&client.id) {
                return Err(Error::Validation(format!(
                    "the native client id {:?} is not two to 256 characters of [a-zA-Z0-9._-], \
                     which is what rauthy accepts",
                    client.id
                )));
            }
            if !seen.insert(client.id.clone()) {
                return Err(Error::Validation(format!(
                    "auth.native_clients declares {:?} twice",
                    client.id
                )));
            }
            if !client.flows.iter().any(|flow| flow.is_login()) {
                return Err(Error::Validation(format!(
                    "the native client {:?} enables no flow that logs a person in: name \
                     device_code, authorization_code, or both",
                    client.id
                )));
            }
            let mut flows = BTreeSet::new();
            for flow in &client.flows {
                if !flows.insert(*flow) {
                    return Err(Error::Validation(format!(
                        "the native client {:?} names the flow {flow} twice",
                        client.id
                    )));
                }
            }
            for scope in &client.scopes {
                if !is_scope(scope) {
                    return Err(Error::Validation(format!(
                        "the native client {:?} names the scope {scope:?}, which is not two to \
                         64 characters of [a-z0-9-_/:*]",
                        client.id
                    )));
                }
            }
            if client.has_flow(NativeFlow::AuthorizationCode) {
                if client.redirect_uris.is_empty() {
                    return Err(Error::Validation(format!(
                        "the native client {:?} uses authorization_code and names no \
                         redirect_uris; RFC 8252 wants a loopback one",
                        client.id
                    )));
                }
                for uri in &client.redirect_uris {
                    if !is_loopback_uri(uri) {
                        return Err(Error::Validation(format!(
                            "the native client {:?} names the redirect URI {uri:?}, which is not \
                             a loopback address; RFC 8252 §7.3 wants http://127.0.0.1 or \
                             http://[::1] with the port the client listens on",
                            client.id
                        )));
                    }
                }
            } else if !client.redirect_uris.is_empty() {
                return Err(Error::Validation(format!(
                    "the native client {:?} names redirect_uris and does not use \
                     authorization_code: a redirect nothing redirects to is a typo, not a \
                     wider ceiling",
                    client.id
                )));
            }
        }
        Ok(())
    }

    /// Every declared native client's scopes are ones a bearer route
    /// requires (spec 038 B-2).
    ///
    /// `declared` is what the mounted scope gates published, which is a fact
    /// about the composed cell rather than about this document, so the check
    /// is here and not in [`Manifest::validate`]: a manifest is parsed in a
    /// build that mounts nothing, and a rule that read an empty set there
    /// would refuse every manifest that declares a client.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] naming the client and the scope no route asks
    /// for.
    pub fn validate_native_scopes(&self, declared: &BTreeSet<String>) -> Result<(), Error> {
        for client in &self.auth.native_clients {
            for scope in &client.scopes {
                if !declared.contains(scope) {
                    return Err(Error::Validation(format!(
                        "the native client {:?} is declared the scope {scope:?}, which no bearer \
                         route of this cell requires; the scopes its routes require are: {}",
                        client.id,
                        if declared.is_empty() {
                            "none".to_owned()
                        } else {
                            declared.iter().cloned().collect::<Vec<_>>().join(", ")
                        }
                    )));
                }
            }
        }
        Ok(())
    }

    /// How long an access token this cell's clients are issued lives, in
    /// seconds (spec 038 B-4).
    #[must_use]
    pub const fn access_token_lifetime_secs(&self) -> u64 {
        match self.auth.access_token_lifetime_secs {
            Some(secs) => secs,
            None => DEFAULT_ACCESS_TOKEN_LIFETIME_SECS,
        }
    }

    /// How long a native client's refresh token lives, in seconds (B-4).
    #[must_use]
    pub const fn native_refresh_lifetime_secs(&self) -> u64 {
        match self.auth.native_refresh_lifetime_secs {
            Some(secs) => secs,
            None => DEFAULT_NATIVE_REFRESH_LIFETIME_SECS,
        }
    }

    /// The declared native clients (spec 038 B-2).
    #[must_use]
    pub fn native_clients(&self) -> &[NativeClient] {
        &self.auth.native_clients
    }

    /// Whether a browser logout deny-lists the subject's bearer tokens
    /// (spec 038 B-5).
    #[must_use]
    pub const fn logout_revokes_bearer(&self) -> bool {
        self.auth.logout_revokes_bearer
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
        let model = self.canonical_model()?;
        let gate = self.gate()?.config_hash();
        Hash::parse(sha256_hex(format!("{model}\n{gate}").as_bytes()))
    }

    /// Parse a manifest back from the canonical JSON [`Manifest::canonical_model`]
    /// produced.
    ///
    /// The inverse of the model bytes, for a reader of a spec 036 transition
    /// record: it recovers the ceiling the record retained, and it validates
    /// it the way [`Manifest::parse`] does, so a model that would not be
    /// accepted as a manifest is not accepted as one here either.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when the JSON is not a manifest or the manifest
    /// does not validate.
    pub fn parse_model(model: &str) -> Result<Self, Error> {
        let manifest: Self = serde_json::from_str(model).map_err(|e| {
            Error::Validation(format!("the retained manifest model does not parse: {e}"))
        })?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// The canonical (key-sorted) JSON of the parsed model.
    ///
    /// The first half of what [`Manifest::hash`] digests, and what spec 036
    /// B-2 retains in a manifest transition record, so an auditor reading the
    /// chain recomputes the hash from the record alone rather than needing
    /// the deploy's artefacts. There is one producer of these bytes, here, so
    /// the model a transition carries and the model its hash was taken over
    /// cannot drift apart.
    ///
    /// # Errors
    ///
    /// [`Error::Integrity`] when the model does not serialize, which would
    /// mean the manifest types drifted.
    pub fn canonical_model(&self) -> Result<String, Error> {
        let value = serde_json::to_value(self)
            .map_err(|e| Error::Integrity(format!("the manifest does not serialize: {e}")))?;
        Ok(canonical_keysort_json::to_canonical_string(&value))
    }
}

/// rauthy's client id rule: `^[a-zA-Z0-9._\-]{2,256}$`.
fn is_client_id(id: &str) -> bool {
    (2..=256).contains(&id.len())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// rauthy's scope rule: `^[a-z0-9-_/,:*]{2,64}$`, without the comma, which
/// separates two scopes rather than appearing inside one.
fn is_scope(scope: &str) -> bool {
    (2..=64).contains(&scope.len())
        && scope.chars().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '/' | ':' | '*')
        })
}

/// Whether `uri` redirects to this machine (RFC 8252 §7.3).
///
/// The literal addresses only. `localhost` is deliberately refused: RFC 8252
/// §8.3 says it resolves through whatever the host's name service says, and a
/// name that can be pointed elsewhere is not the property the loopback rule
/// is buying. The port is the client's and is not fixed, which is the other
/// half of §7.3.
fn is_loopback_uri(uri: &str) -> bool {
    let Some(rest) = uri.strip_prefix("http://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = match authority.strip_prefix('[') {
        Some(after) => match after.split_once(']') {
            Some((host, port)) => {
                if !port.is_empty() && !is_port(port) {
                    return false;
                }
                host
            }
            None => return false,
        },
        None => match authority.split_once(':') {
            Some((host, port)) => {
                if !is_port(&format!(":{port}")) {
                    return false;
                }
                host
            }
            None => authority,
        },
    };
    matches!(host, "127.0.0.1" | "::1")
}

/// Whether `port` is `:` followed by a decimal port number.
fn is_port(port: &str) -> bool {
    port.strip_prefix(':')
        .is_some_and(|digits| !digits.is_empty() && digits.parse::<u16>().is_ok())
}

/// The MAJOR of a `MAJOR.MINOR.PATCH` version, if it is one.
fn major_of(version: &str) -> Option<u64> {
    let mut parts = version.split('.');
    let (Some(major), Some(minor), Some(patch), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    if minor.parse::<u64>().is_err() || patch.parse::<u64>().is_err() {
        return None;
    }
    major.parse().ok()
}
