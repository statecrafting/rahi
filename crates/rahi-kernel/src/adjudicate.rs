//! Deny by default (spec 015 B-4, constitution X).
//!
//! Adjudication is a pure function of the manifest and the gate's check
//! roster. Nothing here reads a clock, touches the store, or performs the
//! operation it is deciding about, which is what makes a decision reproducible
//! from a manifest and a request alone: an auditor holding the ledgered
//! decision and the manifest hash it was made under can re-derive the outcome
//! without running the cell.
//!
//! The first check in every gate is [`GrantCheck`], and it is not part of the
//! roster. A manifest cannot omit it, reorder it, or configure it away.
//! Absence is never permission: a request whose service is unknown, whose
//! kind is outside the vocabulary, whose context is missing a field the check
//! needs, or that simply has no covering grant, is denied. There is no branch
//! in this file that admits an action because it could not find a rule against
//! it.
//!
//! The roster is the *rest* of the gate, drawn from a closed catalog
//! ([`OPTIONAL_CHECKS`], D-4). A check the kernel does not know is a hole in
//! the ceiling nobody can audit, so an unknown id is refused when the manifest
//! is parsed rather than ignored when the gate is built.

use std::collections::BTreeMap;

use action_gate_core::{Gate, sha256_hex};
use action_gate_types::{ActionContext, Check, Decision as GateDecision, Outcome as GateOutcome};
use rahi_ledger::Outcome;
use rahi_types::{Error, Revision, Sub};

use crate::capability::{Capability, CapabilityId, CapabilityKind, ServiceName};
use crate::manifest::Manifest;

/// The gate's answer to one request.
///
/// This is `action-gate`'s own decision type, aliased because `Decision` in
/// this chassis already means the ledgered record (spec 013). Its
/// [`GateOutcome`] covers the three answers spec 015 B-4 names, and `reason`
/// carries the machine-readable code plus the detail a human reading the chain
/// needs.
pub type Verdict = GateDecision;

/// The id of the capability check. Always first, never in the roster.
pub const GRANT_CHECK_ID: &str = "grants";

/// The roster's closed catalog (D-4).
///
/// `secrets` is `action-gate`'s own reference check: it denies, blockingly, a
/// request whose payload looks like it carries a credential. Adding an entry
/// here is a spec amendment, not a manifest author's choice.
pub const OPTIONAL_CHECKS: [&str; 1] = ["secrets"];

/// The context attribute naming the service a request is made on behalf of.
pub const ATTR_SERVICE: &str = "service";
/// The context attribute naming the resource a request addresses.
pub const ATTR_RESOURCE: &str = "resource";
/// The context attribute carrying the request's key, for `key_prefix`.
pub const ATTR_KEY: &str = "key";
/// The context attribute carrying the request's table, for `table`.
pub const ATTR_TABLE: &str = "table";
/// The context attribute carrying the request's host, for `host`.
pub const ATTR_HOST: &str = "host";
/// The context attribute carrying the acting principal's subject.
pub const ATTR_ACTOR: &str = "actor";

/// One proposed governed operation, before it is adjudicated.
///
/// The typed form of an [`ActionContext`]: [`Request::to_context`] lowers it
/// and [`crate::Kernel::adjudicate`] raises the answer. Facades build one per
/// call; an app that adjudicates something the chassis has no facade for
/// builds one directly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// The service the operation is attributed to.
    pub service: ServiceName,
    /// What the operation is.
    pub kind: CapabilityKind,
    /// The resource it addresses: a table, a kv namespace, a counter, a lock
    /// key, a topic, a secret name, or a concrete egress host.
    pub resource: String,
    /// Who is acting. `system` for the cell itself (spec 013 B-1).
    pub actor: Sub,
    /// The key inside the resource, when there is one.
    pub key: Option<String>,
    /// The table the operation touches, when it is not the resource itself.
    pub table: Option<String>,
    /// The concrete host, for egress.
    pub host: Option<String>,
    /// What the `secrets` roster check scans. Empty unless the caller opts in:
    /// the chassis facades never put SQL or values here.
    pub payload: Option<String>,
    /// The store revision the request is made against (spec 013 D-3).
    pub at: Revision,
}

impl Request {
    /// A request with no key, table, host, or payload, at [`Revision::ZERO`].
    #[must_use]
    pub fn new(
        service: ServiceName,
        kind: CapabilityKind,
        resource: impl Into<String>,
        actor: Sub,
    ) -> Self {
        Self {
            service,
            kind,
            resource: resource.into(),
            actor,
            key: None,
            table: None,
            host: None,
            payload: None,
            at: Revision::ZERO,
        }
    }

    /// Name the key inside the resource, for a `key_prefix` constraint.
    #[must_use]
    pub fn with_key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }

    /// Name the table, for a `table` constraint.
    #[must_use]
    pub fn with_table(mut self, table: impl Into<String>) -> Self {
        self.table = Some(table.into());
        self
    }

    /// Name the concrete host, for a `host` constraint.
    #[must_use]
    pub fn with_host(mut self, host: impl Into<String>) -> Self {
        self.host = Some(host.into());
        self
    }

    /// Hand the roster's `secrets` check something to scan.
    #[must_use]
    pub fn with_payload(mut self, payload: impl Into<String>) -> Self {
        self.payload = Some(payload.into());
        self
    }

    /// Stamp the store revision this request is made against.
    #[must_use]
    pub fn at(mut self, revision: Revision) -> Self {
        self.at = revision;
        self
    }

    /// Lower the request into the gate's context.
    #[must_use]
    pub fn to_context(&self) -> ActionContext {
        let mut ctx = ActionContext::new(self.kind.as_str())
            .with_attr(ATTR_SERVICE, self.service.as_str().into())
            .with_attr(ATTR_RESOURCE, self.resource.as_str().into())
            .with_attr(ATTR_ACTOR, self.actor.as_str().into());
        if let Some(key) = &self.key {
            ctx = ctx.with_attr(ATTR_KEY, key.as_str().into());
        }
        if let Some(table) = &self.table {
            ctx = ctx.with_attr(ATTR_TABLE, table.as_str().into());
        }
        if let Some(host) = &self.host {
            ctx = ctx.with_attr(ATTR_HOST, host.as_str().into());
        }
        if let Some(payload) = &self.payload {
            ctx = ctx.with_summary(payload.clone());
        }
        ctx
    }
}

/// The capability check: the manifest's ceiling, enforced.
///
/// Registered first in every gate and built from the manifest's service grants
/// alone, so its answer is a function of what was declared and nothing else.
/// It returns `None` only when a declared grant covers the request outright;
/// every other path returns a deny.
pub struct GrantCheck {
    grants: BTreeMap<String, Vec<Capability>>,
    fingerprint: String,
}

impl GrantCheck {
    /// Build the check from a validated manifest.
    #[must_use]
    pub fn new(manifest: &Manifest) -> Self {
        let grants = manifest.grants();
        let fingerprint = match serde_json::to_value(&grants) {
            Ok(value) => {
                let bytes = canonical_keysort_json::to_canonical_string(&value);
                format!("{GRANT_CHECK_ID}:{}", sha256_hex(bytes.as_bytes()))
            }
            // Unreachable for a validated manifest: the grant table is strings
            // and structs. Fingerprinting the failure keeps the gate buildable
            // while making sure it can never collide with a real catalog.
            Err(err) => format!("{GRANT_CHECK_ID}:unserializable:{err}"),
        };
        Self {
            grants,
            fingerprint,
        }
    }

    /// The capabilities granted to `service`, in declaration order.
    #[must_use]
    pub fn granted(&self, service: &str) -> Option<&[Capability]> {
        self.grants.get(service).map(Vec::as_slice)
    }
}

/// A denial the check produces, with its stable code and the detail behind it.
fn deny(code: &str, detail: String) -> GateDecision {
    GateDecision::deny(
        format!("gate:deny:{GRANT_CHECK_ID}:{code}: {detail}"),
        vec![GRANT_CHECK_ID.to_owned()],
    )
}

impl Check for GrantCheck {
    fn id(&self) -> &str {
        GRANT_CHECK_ID
    }

    fn evaluate(&self, ctx: &ActionContext) -> Option<GateDecision> {
        let Some(service) = ctx.attr_str(ATTR_SERVICE) else {
            return Some(deny(
                "malformed",
                format!("the request names no {ATTR_SERVICE}"),
            ));
        };
        let Some(resource) = ctx.attr_str(ATTR_RESOURCE) else {
            return Some(deny(
                "malformed",
                format!("service {service:?} names no {ATTR_RESOURCE}"),
            ));
        };
        let kind = match CapabilityKind::parse(&ctx.action) {
            Ok(kind) => kind,
            Err(err) => {
                return Some(deny("unknown_kind", err.message().to_owned()));
            }
        };
        let Some(granted) = self.granted(service) else {
            return Some(deny(
                "unknown_service",
                format!("{service:?} is not a service the manifest declares"),
            ));
        };

        let covering: Vec<&Capability> = granted
            .iter()
            .filter(|cap| cap.covers(kind, resource))
            .collect();
        if covering.is_empty() {
            return Some(deny(
                "no_grant",
                format!("service {service:?} has no grant for {kind} on {resource:?}"),
            ));
        }

        // One covering grant whose constraints hold is enough; a request that
        // fits none of them is denied with the first grant's reason, which is
        // the one an author reading the manifest top to bottom expects.
        let mut first_refusal = None;
        for cap in covering {
            match cap.constraints.admits(
                ctx.attr_str(ATTR_KEY),
                ctx.attr_str(ATTR_TABLE),
                ctx.attr_str(ATTR_HOST),
            ) {
                Ok(()) => return None,
                Err(reason) => {
                    if first_refusal.is_none() {
                        first_refusal = Some((cap.id.clone(), reason));
                    }
                }
            }
        }
        let (id, reason) = first_refusal?;
        Some(GateDecision::deny(
            format!(
                "gate:deny:{GRANT_CHECK_ID}:constraint: capability {id} does not admit it: {reason}"
            ),
            vec![GRANT_CHECK_ID.to_owned(), id.as_str().to_owned()],
        ))
    }

    fn config_fingerprint(&self) -> String {
        self.fingerprint.clone()
    }
}

/// Assemble the gate a manifest describes: [`GrantCheck`] then the roster.
///
/// # Errors
///
/// [`Error::Validation`] when the roster names a check outside
/// [`OPTIONAL_CHECKS`]. A validated manifest cannot reach this, but the
/// function is public and a caller may hand it one it built by hand.
pub fn build_gate(manifest: &Manifest) -> Result<Gate, Error> {
    let mut builder = Gate::builder().check(GrantCheck::new(manifest));
    for id in &manifest.gate.checks {
        builder = match id.as_str() {
            "secrets" => builder.check(action_gate_core::checks::SecretsCheck::default()),
            other => {
                return Err(Error::Validation(format!(
                    "{other:?} is not a gate check; the catalog is closed: {}, and {GRANT_CHECK_ID} \
                     is always first and never listed",
                    OPTIONAL_CHECKS.join(", ")
                )));
            }
        };
    }
    Ok(builder.build())
}

/// The capability a request is nearest to: one granted to the service for this
/// kind and resource, constraints ignored.
///
/// What a constraint denial should name in the ledger. A request with no
/// covering grant at all has no nearest capability, and the decision records
/// none rather than guessing at one.
#[must_use]
pub fn nearest_capability(manifest: &Manifest, request: &Request) -> Option<CapabilityId> {
    manifest
        .granted(&request.service)?
        .iter()
        .find(|cap| cap.covers(request.kind, &request.resource))
        .map(|cap| cap.id.clone())
}

/// The ledger's outcome for a gate verdict.
#[must_use]
pub const fn outcome_of(verdict: &Verdict) -> Outcome {
    match verdict.outcome {
        GateOutcome::Allow => Outcome::Allow,
        GateOutcome::Deny => Outcome::Deny,
        GateOutcome::Degrade => Outcome::Degrade,
    }
}

/// Whether a verdict is one the chain records (spec 015 B-6, D-5).
///
/// Denials and degrades are governance events and are ledgered; an allow is
/// the ordinary case and is not, or the chain would become the request log.
#[must_use]
pub const fn is_ledgered(verdict: &Verdict) -> bool {
    matches!(verdict.outcome, GateOutcome::Deny | GateOutcome::Degrade)
}
