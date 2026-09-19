//! The manifest transition record, and the chain's current manifest
//! (spec 036 B-1, B-2, D-6).
//!
//! Spec 013 rooted the chain at the booted manifest's hash and spec 015
//! refused to boot against any other, which made widening a ceiling
//! indistinguishable from tampering with the audit proof. A transition is
//! the missing deploy step: one ordinary chain record, signed and
//! CAS-protected like every other, that says the cell moved from one
//! declared ceiling to another and carries the whole of the ceiling it
//! moved to.
//!
//! Two properties are worth naming, because they are what a reader of the
//! chain gets:
//!
//! - **The record retains the model, or there is no record.** `model` is the
//!   adopted manifest's canonical JSON, the first half of the bytes
//!   `Manifest::hash` digests, so an auditor recomputes the hash from the
//!   record alone. When the complete signed record would not fit the cell's
//!   `ledger.max_record_bytes`, adoption is refused
//!   ([`Ledger::append_transition`]): nothing is truncated, moved to a
//!   second field, or reduced to hash-only evidence (D-6).
//! - **The current manifest is readable at `Depth::Resident`.**
//!   [`Ledger::current_manifest`] reads the newest resident transition; when
//!   the transition has been sealed away, the last sealed segment's header
//!   carries the manifest current at its tail (B-5), so the answer survives
//!   the hot window without fetching an archived body.

use rahi_types::{Error, Revision, Sub};
use serde::{Deserialize, Serialize};

use crate::chain::Ledger;
use crate::record::{
    Decision, DecisionId, DecisionKind, Hash, Outcome, SignedRecord, revision_stamp,
};

/// The decision kind a manifest transition carries (B-2).
pub const TRANSITION_KIND: &str = "manifest.transition";

/// How many hex characters of a hash name it inside a transition's id.
///
/// The same abbreviation spec 015 D-8 uses for the boot nonce, for the same
/// reason: an id a human can read beside the full hashes the payload carries.
const ID_HASH_LEN: usize = 16;

/// Which binary adopted the manifest (B-2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BinaryVersions {
    /// The chassis version that appended the transition.
    pub rahi: String,
    /// The cell's own `contract.version` at that build.
    pub contract: String,
}

/// The payload of a `manifest.transition` decision (B-2).
///
/// Every field is what it says: `from` is the chain's current manifest when
/// the transition was built, `to` is the adopted one, `model` is the adopted
/// manifest's canonical JSON, `schema_version` is the store's after the
/// deploy's migrations, and `actor` is the operator's subject or
/// `system:deploy`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestTransition {
    /// The manifest the chain named before this record.
    pub from: Hash,
    /// The manifest the chain names after it.
    pub to: Hash,
    /// The adopted manifest's canonical JSON: the bytes its hash is taken
    /// over, retained whole or not at all (D-6).
    pub model: String,
    /// The store's schema version after this deploy's migrations.
    pub schema_version: u32,
    /// The binary that adopted it.
    pub binary: BinaryVersions,
    /// Who adopted it.
    pub actor: Sub,
}

/// The actor of a transition no operator drove (B-2).
pub const SYSTEM_DEPLOY: &str = "system:deploy";

impl ManifestTransition {
    /// A transition from `from` to `to`.
    #[must_use]
    pub fn new(
        from: Hash,
        to: Hash,
        model: impl Into<String>,
        schema_version: u32,
        binary: BinaryVersions,
        actor: Sub,
    ) -> Self {
        Self {
            from,
            to,
            model: model.into(),
            schema_version,
            binary,
            actor,
        }
    }

    /// The transition this record carries, if it carries one.
    ///
    /// # Errors
    ///
    /// [`Error::Integrity`] when the record claims the transition kind and
    /// its payload is not a transition: a record of this kind that cannot be
    /// read is damage, not an absence.
    pub fn of(record: &SignedRecord) -> Result<Option<Self>, Error> {
        let decision = record.decision()?;
        Self::of_decision(&decision)
    }

    /// [`ManifestTransition::of`], on a decision already read back.
    ///
    /// # Errors
    ///
    /// As [`ManifestTransition::of`].
    pub fn of_decision(decision: &Decision) -> Result<Option<Self>, Error> {
        if decision.kind.as_str() != TRANSITION_KIND {
            return Ok(None);
        }
        serde_json::from_value(decision.payload.as_value().clone())
            .map(Some)
            .map_err(|e| {
                Error::Integrity(format!(
                    "record {} is a {TRANSITION_KIND} whose payload is not one: {e}",
                    decision.id
                ))
            })
    }

    /// The decision this transition is appended as, named after the head it
    /// was built against.
    ///
    /// The id follows the convention spec 015 D-8 set for decision ids: the
    /// chain position it was minted at, abbreviated, and then what it is
    /// about. Two transitions to one manifest are necessarily built against
    /// different heads, so a cell that goes H1 to H2 to H1 to H2 names four
    /// distinct records. A retry keeps the name, as spec 013 B-3 requires of
    /// every decision.
    ///
    /// # Errors
    ///
    /// [`Error::Integrity`] when the transition does not serialize.
    pub fn decision(&self, head: &Hash) -> Result<Decision, Error> {
        let payload = serde_json::to_value(self).map_err(|e| {
            Error::Integrity(format!("a manifest transition does not serialize: {e}"))
        })?;
        Ok(Decision::new(
            DecisionId::new(format!(
                "manifest:{}:{}",
                abbreviate(head),
                abbreviate(&self.to)
            )),
            DecisionKind::new(TRANSITION_KIND),
            self.actor.clone(),
            Outcome::Allow,
            format!(
                "the cell adopts manifest {} in place of {}",
                self.to, self.from
            ),
        )
        .with_payload(payload))
    }
}

/// The last [`ID_HASH_LEN`] hex characters of a hash.
fn abbreviate(hash: &Hash) -> String {
    let text = hash.as_str();
    let from = text.len().saturating_sub(ID_HASH_LEN);
    text.get(from..).unwrap_or(text).to_owned()
}

/// A parent of the exact length every hash in this chain has, for a record
/// that has not been chained yet (D-6).
///
/// The measurement below has to stand in for a record whose parent is not
/// known until [`Ledger::append`] wins a compare-and-swap. Every hash is
/// `sha256:` and sixty-four hex digits, so a placeholder of that shape makes
/// the measured length the appended length for that field.
fn placeholder_parent() -> Hash {
    Hash::unchained()
}

impl Ledger {
    /// The chain's current manifest (B-1).
    ///
    /// The genesis parent until the first transition, then the `to` of the
    /// latest one. Latest is by chain order, not by read order: the newest
    /// resident transition wins, and when none is resident the last sealed
    /// segment's header carries the manifest current at its tail (B-5). A
    /// segment sealed by a binary older than this spec records none, and the
    /// answer falls back to the genesis parent, which is what such a chain
    /// has always named.
    ///
    /// # Errors
    ///
    /// [`Error::Integrity`] when the chain does not read back as one list or
    /// a transition record's payload is not a transition; the store's own
    /// error when the leader cannot be reached.
    pub async fn current_manifest(&self) -> Result<Hash, Error> {
        for record in self.records().await?.iter().rev() {
            if let Some(transition) = ManifestTransition::of(record)? {
                return Ok(transition.to);
            }
        }
        if let Some(header) = self.segments().await?.last()
            && let Some(current) = &header.current_manifest
        {
            return Ok(current.clone());
        }
        Ok(self.genesis_parent().clone())
    }

    /// The manifest current at the tail of a run of records, given what the
    /// run's predecessor named (B-5).
    ///
    /// Used by a seal to stamp the segment it is about to write.
    ///
    /// # Errors
    ///
    /// [`Error::Integrity`] when a transition record's payload is not a
    /// transition.
    pub fn manifest_at_tail(records: &[SignedRecord], before: Hash) -> Result<Hash, Error> {
        for record in records.iter().rev() {
            if let Some(transition) = ManifestTransition::of(record)? {
                return Ok(transition.to);
            }
        }
        Ok(before)
    }

    /// How many bytes the record for `transition` will occupy (D-6).
    ///
    /// The measure is the byte length of [`SignedRecord::to_canonical_json`],
    /// which is exactly what the `record` column stores and exactly what one
    /// line of an export carries. Two fields are not final until the append
    /// chains the record, and both are made deterministic here: the parent is
    /// a placeholder of the fixed length every hash has, and the envelope's
    /// revision stamp is widened to the largest `u64`. The result is
    /// therefore an upper bound that can exceed the appended record only by
    /// the decimal digits the revision did not need.
    ///
    /// # Errors
    ///
    /// [`Error::Integrity`] when the record does not serialize.
    pub fn measure_transition(&self, transition: &ManifestTransition) -> Result<usize, Error> {
        let mut decision = transition.decision(&placeholder_parent())?;
        decision.prev_hash = placeholder_parent();
        decision.at = Revision::new(u64::MAX);
        debug_assert_eq!(
            revision_stamp(decision.at).len(),
            "revision:18446744073709551615".len(),
            "the widest revision stamp"
        );
        let record = SignedRecord::build(&decision, self.signer())?;
        Ok(record.to_canonical_json()?.len())
    }

    /// Append `transition`, refusing a record that does not fit `max_record_bytes`
    /// (B-2, D-6).
    ///
    /// This is the only way a transition reaches the chain, so the bound is
    /// enforced on the append path itself and not only in the verb that
    /// usually calls it.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] naming the measured size and the bound when the
    /// record is too large; nothing is appended. Otherwise as
    /// [`Ledger::append`].
    pub async fn append_transition(
        &self,
        transition: &ManifestTransition,
        max_record_bytes: u64,
    ) -> Result<Hash, Error> {
        let measured = self.measure_transition(transition)?;
        check_fits(transition, measured, max_record_bytes)?;
        let head = self.head().await?;
        self.append(transition.decision(&head)?).await
    }
}

/// The size refusal, in one place so the verb and the append say the same
/// thing (D-6).
///
/// # Errors
///
/// [`Error::Validation`] naming the measured size, the bound, and the
/// setting that carries it.
pub fn check_fits(
    transition: &ManifestTransition,
    measured: usize,
    max_record_bytes: u64,
) -> Result<(), Error> {
    if u64::try_from(measured).unwrap_or(u64::MAX) <= max_record_bytes {
        return Ok(());
    }
    Err(Error::Validation(format!(
        "the transition to manifest {} measures {measured} bytes and ledger.max_record_bytes is \
         {max_record_bytes}: the manifest is retained whole or it is not adopted, so nothing was \
         appended; raise ledger.max_record_bytes in the manifest and adopt again",
        transition.to
    )))
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    fn hash(byte: &str) -> Hash {
        Hash::parse(format!("sha256:{}", byte.repeat(32))).expect("a hash")
    }

    fn transition() -> ManifestTransition {
        ManifestTransition::new(
            hash("11"),
            hash("22"),
            r#"{"app":{"name":"cell"}}"#,
            2,
            BinaryVersions {
                rahi: "0.2.0".to_owned(),
                contract: "1.0.0".to_owned(),
            },
            Sub::new(SYSTEM_DEPLOY),
        )
    }

    #[test]
    fn a_transition_round_trips_through_the_decision_it_is_appended_as() {
        let decision = transition().decision(&hash("aa")).expect("builds");
        assert_eq!(decision.kind.as_str(), TRANSITION_KIND);
        assert_eq!(
            ManifestTransition::of_decision(&decision).expect("reads"),
            Some(transition())
        );
    }

    #[test]
    fn the_id_names_the_head_it_was_built_against_and_the_manifest_it_adopts() {
        let one = transition().decision(&hash("aa")).expect("builds").id;
        let two = transition().decision(&hash("bb")).expect("builds").id;
        assert_ne!(
            one, two,
            "a later transition to one manifest is its own record"
        );
        assert!(one.as_str().starts_with("manifest:"), "{one}");
        assert!(one.as_str().ends_with(&"22".repeat(8)), "{one}");
    }

    #[test]
    fn a_decision_of_another_kind_carries_no_transition() {
        let other = Decision::new(
            DecisionId::new("d-1"),
            DecisionKind::new("db.write"),
            Sub::new("subject"),
            Outcome::Allow,
            "granted",
        );
        assert_eq!(
            ManifestTransition::of_decision(&other).expect("reads"),
            None
        );
    }

    #[test]
    fn the_size_refusal_names_the_measure_the_bound_and_the_setting() {
        check_fits(&transition(), 100, 100).expect("exactly the bound fits");
        let err = check_fits(&transition(), 101, 100).expect_err("one byte over is refused");
        assert!(matches!(err, Error::Validation(_)), "{err}");
        assert_eq!(err.exit_code(), 1);
        for fragment in ["101", "100", "ledger.max_record_bytes"] {
            assert!(err.message().contains(fragment), "{fragment}: {err}");
        }
    }
}
