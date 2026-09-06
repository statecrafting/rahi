//! What a decision is, and the signed envelope it travels in (spec 013 B-1).
//!
//! A [`Decision`] is the domain payload: who acted, under which capability,
//! what the kernel decided, and why. It is carried inside an
//! [`attest_ledger_types::LedgerRecord`], the family's record envelope, whose
//! `record_hash` is a sha256 over the canonical (key-sorted) JSON of the
//! record with that field removed. [`SignedRecord`] is the envelope plus the
//! cell's Ed25519 signature over that hash, which is what a stored row holds
//! and what an export writes.
//!
//! Nothing in this module reads a clock. `Decision::at` is a store revision
//! and the envelope's `timestamp` slot carries the same revision rendered as
//! `revision:<n>`, so a reader is never tempted to parse it as wall time. A
//! caller that wants wall time puts it in `payload`.

use attest_ledger_types::LedgerRecord;
use rahi_types::{Error, Revision, Sub};
use serde::{Deserialize, Serialize};

use crate::signer::LedgerSigner;

/// The `sha256:` prefix every hash in the chain carries.
const HASH_PREFIX: &str = "sha256:";

/// The number of hex characters in a sha256 digest.
const HASH_HEX_LEN: usize = 64;

/// A `sha256:<hex>` digest: a record's own hash, or the parent it links to.
///
/// The prefix is carried from `attest-ledger` so the value is self-describing
/// and a chain written here verifies byte-identically under the published
/// `attest-ledger` verifier.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, std::hash::Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Hash(String);

impl Hash {
    /// Parse a `sha256:<64 lowercase hex>` digest.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when the prefix or the hex body is wrong. A
    /// malformed hash is refused here rather than at link time, so a broken
    /// link always means a broken link.
    pub fn parse(value: impl Into<String>) -> Result<Self, Error> {
        let value = value.into();
        let body = value.strip_prefix(HASH_PREFIX).ok_or_else(|| {
            Error::Validation(format!(
                "{value:?} is not a hash: it lacks the sha256: prefix"
            ))
        })?;
        if body.len() != HASH_HEX_LEN || !body.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::Validation(format!(
                "{value:?} is not a hash: {HASH_HEX_LEN} lowercase hex digits are expected"
            )));
        }
        if body.bytes().any(|b| b.is_ascii_uppercase()) {
            return Err(Error::Validation(format!(
                "{value:?} is not a hash: hex digits are lowercase"
            )));
        }
        Ok(Self(value))
    }

    /// The parent of a decision that has not been chained yet.
    ///
    /// [`crate::Ledger::append`] overwrites it with the head it chains onto,
    /// so a record carrying this value never reaches the store. It is
    /// well-formed but links to nothing, so a chain that somehow contains one
    /// fails verification as an orphan rather than as a parse error.
    #[must_use]
    pub fn unchained() -> Self {
        Self(format!("{HASH_PREFIX}{}", "0".repeat(HASH_HEX_LEN)))
    }

    /// The `sha256:<hex>` text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Hash {
    type Error = Error;

    fn try_from(value: String) -> Result<Self, Error> {
        Self::parse(value)
    }
}

impl From<Hash> for String {
    fn from(hash: Hash) -> Self {
        hash.0
    }
}

impl std::fmt::Display for Hash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The id of one decision, unique across the chain.
///
/// The caller mints it, because the caller has to name the decision before
/// the append completes: spec 015's `Error::Denied` carries the id while the
/// append is still queued.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, std::hash::Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DecisionId(String);

impl DecisionId {
    /// Wrap an id.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The id text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for DecisionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a decision is about.
///
/// Deliberately open: the vocabulary belongs to the kernel (spec 015), which
/// declares the capability kinds and emits the decisions. The ledger records
/// whatever it is handed and never adjudicates it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, std::hash::Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DecisionKind(String);

impl DecisionKind {
    /// The kind written by [`crate::Ledger::open`] for the genesis record.
    pub const GENESIS: &'static str = "ledger.genesis";

    /// Wrap a kind.
    #[must_use]
    pub fn new(kind: impl Into<String>) -> Self {
        Self(kind.into())
    }

    /// The kind text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for DecisionKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The id of the manifest capability a decision was adjudicated against.
///
/// Open for the same reason as [`DecisionKind`]: spec 015's manifest declares
/// the catalog.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, std::hash::Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CapabilityId(String);

impl CapabilityId {
    /// Wrap a capability id.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The capability id text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for CapabilityId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// How the decision came out.
///
/// The closed vocabulary the kernel adjudicates with (spec 015 B-4). It is
/// closed because a fourth outcome would change what a chain means, which is
/// a spec amendment rather than a caller's choice.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, std::hash::Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    /// The action was admitted.
    Allow,
    /// The action was refused. Constitution X: every denial is ledgered.
    Deny,
    /// The action was admitted in a reduced form named by `reason`.
    Degrade,
}

impl Outcome {
    /// The stable lowercase name, for logs and metrics labels.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
            Self::Degrade => "degrade",
        }
    }
}

impl std::fmt::Display for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A JSON value whose object keys are sorted, at every depth.
///
/// The whole record is canonicalized again when it is hashed, so this type is
/// not what makes the hash reproducible; it makes the value a caller holds
/// and the bytes an auditor reads the same thing, so a payload that looks
/// equal is equal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "serde_json::Value", into = "serde_json::Value")]
pub struct CanonicalJson(serde_json::Value);

impl CanonicalJson {
    /// Canonicalize a value.
    #[must_use]
    pub fn new(value: serde_json::Value) -> Self {
        Self(canonical_keysort_json::canonicalize_value(value))
    }

    /// The JSON `null` payload, for a decision that carries no detail.
    #[must_use]
    pub fn null() -> Self {
        Self(serde_json::Value::Null)
    }

    /// The canonicalized value.
    #[must_use]
    pub fn as_value(&self) -> &serde_json::Value {
        &self.0
    }

    /// The canonical byte string, as text.
    #[must_use]
    pub fn to_canonical_string(&self) -> String {
        canonical_keysort_json::to_canonical_string(&self.0)
    }
}

impl Default for CanonicalJson {
    fn default() -> Self {
        Self::null()
    }
}

impl From<serde_json::Value> for CanonicalJson {
    fn from(value: serde_json::Value) -> Self {
        Self::new(value)
    }
}

impl From<CanonicalJson> for serde_json::Value {
    fn from(value: CanonicalJson) -> Self {
        value.0
    }
}

/// One governance decision: the payload of a chain record (spec 013 B-1).
///
/// The fields are public because a decision is plain data that has to
/// round-trip through JSON unchanged. Two of them are not the caller's to
/// pick, though, and [`Decision::new`] fills them with values that say so:
/// `prev_hash` starts at [`Hash::unchained`] and is overwritten by
/// [`crate::Ledger::append`] with the head it wins, and `at` starts at
/// [`Revision::ZERO`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    /// Unique across the chain; the row's primary key.
    pub id: DecisionId,
    /// The record this one links to. Set by the append that wins the CAS.
    pub prev_hash: Hash,
    /// What the decision is about (spec 015 owns the vocabulary).
    pub kind: DecisionKind,
    /// The IdP subject that acted, or `system` for the cell itself.
    pub actor: Sub,
    /// The manifest capability adjudicated, when there was one.
    pub capability: Option<CapabilityId>,
    /// How it came out.
    pub outcome: Outcome,
    /// Why, in one line, for a human reading the chain.
    pub reason: String,
    /// Everything else the emitter wants recorded, including wall time.
    pub payload: CanonicalJson,
    /// The store revision the decision was made against. Never a clock.
    pub at: Revision,
}

impl Decision {
    /// A decision that is not chained yet.
    ///
    /// `capability` is `None`, `payload` is JSON `null`, `at` is
    /// [`Revision::ZERO`], and `prev_hash` is [`Hash::unchained`]. Refine it
    /// with [`Decision::with_capability`], [`Decision::with_payload`], and
    /// [`Decision::at`] before handing it to [`crate::Ledger::append`].
    #[must_use]
    pub fn new(
        id: DecisionId,
        kind: DecisionKind,
        actor: Sub,
        outcome: Outcome,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            id,
            prev_hash: Hash::unchained(),
            kind,
            actor,
            capability: None,
            outcome,
            reason: reason.into(),
            payload: CanonicalJson::null(),
            at: Revision::ZERO,
        }
    }

    /// Name the capability this decision was adjudicated against.
    #[must_use]
    pub fn with_capability(mut self, capability: CapabilityId) -> Self {
        self.capability = Some(capability);
        self
    }

    /// Attach the payload, canonicalizing it.
    #[must_use]
    pub fn with_payload(mut self, payload: serde_json::Value) -> Self {
        self.payload = CanonicalJson::new(payload);
        self
    }

    /// Stamp the store revision the decision was made against.
    #[must_use]
    pub fn at(mut self, revision: Revision) -> Self {
        self.at = revision;
        self
    }
}

/// The envelope's `timestamp` slot for a decision taken at `revision`.
///
/// `attest-ledger` takes the timestamp as an argument and never reads a
/// clock, which leaves the slot free for the ordering stamp this chain
/// actually has. The `revision:` prefix is there so nobody parses it as a
/// date.
#[must_use]
pub fn revision_stamp(revision: Revision) -> String {
    format!("revision:{}", revision.get())
}

/// A chain record: the `attest-ledger` envelope plus the cell's signature.
///
/// The two extra fields are siblings of the envelope's own, not members of
/// it, so `record_hash` stays exactly what `attest-ledger` computes over a
/// [`LedgerRecord`]. The published `attest-ledger` verifier reads a stored
/// line, ignores what it does not know, and recomputes the same hash
/// (spec 013 B-6); the signature is the stronger check this crate adds on
/// top, and it covers `record_hash`, so it binds the record to its place in
/// the chain rather than only to its contents.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedRecord {
    /// The hash-linked envelope.
    #[serde(flatten)]
    pub record: LedgerRecord,
    /// Base64 Ed25519 signature over the ASCII bytes of `record.record_hash`.
    pub signature: String,
    /// Base64 Ed25519 public key of the cell's ledger signer.
    pub public_key: String,
}

impl SignedRecord {
    /// Build and sign the record for `decision` as it stands.
    ///
    /// The decision's `prev_hash` is taken as given: [`crate::Ledger::append`]
    /// has already set it to the head it is chaining onto.
    ///
    /// # Errors
    ///
    /// [`Error::Integrity`] when the decision does not serialize to JSON,
    /// which would mean the record types drifted.
    pub fn build(decision: &Decision, signer: &LedgerSigner) -> Result<Self, Error> {
        let payload = serde_json::to_value(decision)
            .map_err(|e| Error::Integrity(format!("decision does not serialize: {e}")))?;
        let mut record = LedgerRecord {
            id: decision.id.as_str().to_owned(),
            timestamp: revision_stamp(decision.at),
            previous_record_hash: decision.prev_hash.as_str().to_owned(),
            record_hash: String::new(),
            payload,
        };
        record.record_hash = attest_ledger_core::compute_record_hash(&record);
        let signature = signer.sign(record.record_hash.as_bytes());
        Ok(Self {
            record,
            signature,
            public_key: signer.public_key(),
        })
    }

    /// This record's own hash.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when the stored text is not a `sha256:` digest.
    pub fn hash(&self) -> Result<Hash, Error> {
        Hash::parse(self.record.record_hash.clone())
    }

    /// The parent this record links to.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when the stored text is not a `sha256:` digest.
    pub fn prev_hash(&self) -> Result<Hash, Error> {
        Hash::parse(self.record.previous_record_hash.clone())
    }

    /// The decision this record carries.
    ///
    /// # Errors
    ///
    /// [`Error::Integrity`] when the payload is not a decision, which means
    /// the row was written by something other than this crate.
    pub fn decision(&self) -> Result<Decision, Error> {
        serde_json::from_value(self.record.payload.clone()).map_err(|e| {
            Error::Integrity(format!(
                "record {} does not carry a decision: {e}",
                self.record.id
            ))
        })
    }

    /// The canonical bytes stored in the row's `record` column.
    ///
    /// # Errors
    ///
    /// [`Error::Integrity`] when the record does not serialize to JSON.
    pub fn to_canonical_bytes(&self) -> Result<Vec<u8>, Error> {
        Ok(self.to_canonical_json()?.into_bytes())
    }

    /// The canonical JSON text of this record: one line of an export.
    ///
    /// # Errors
    ///
    /// [`Error::Integrity`] when the record does not serialize to JSON.
    pub fn to_canonical_json(&self) -> Result<String, Error> {
        let value = serde_json::to_value(self)
            .map_err(|e| Error::Integrity(format!("record does not serialize: {e}")))?;
        Ok(canonical_keysort_json::to_canonical_string(&value))
    }

    /// Read a record back from the bytes of a `record` column.
    ///
    /// # Errors
    ///
    /// [`Error::Integrity`] when the bytes are not a signed record.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        serde_json::from_slice(bytes)
            .map_err(|e| Error::Integrity(format!("stored record does not parse: {e}")))
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::json;

    fn signer() -> LedgerSigner {
        LedgerSigner::from_seed([7u8; 32])
    }

    fn decision() -> Decision {
        Decision::new(
            DecisionId::new("d-1"),
            DecisionKind::new("db.write"),
            Sub::new("subject-1"),
            Outcome::Allow,
            "granted by capability",
        )
        .with_capability(CapabilityId::new("cap-1"))
        .with_payload(json!({ "z": 1, "a": 2 }))
        .at(Revision::new(9))
    }

    #[test]
    fn a_hash_needs_the_prefix_and_sixty_four_lowercase_hex_digits() {
        let good = format!("sha256:{}", "ab".repeat(32));
        assert_eq!(Hash::parse(good.clone()).expect("parses").as_str(), good);
        for bad in [
            String::new(),
            "ab".repeat(32),
            format!("sha256:{}", "ab".repeat(31)),
            format!("sha256:{}", "AB".repeat(32)),
            format!("sha256:{}", "zz".repeat(32)),
        ] {
            assert!(Hash::parse(bad.clone()).is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn an_unchained_decision_carries_a_parent_that_links_to_nothing() {
        let d = Decision::new(
            DecisionId::new("d"),
            DecisionKind::new("k"),
            Sub::new("s"),
            Outcome::Deny,
            "no",
        );
        assert_eq!(d.prev_hash, Hash::unchained());
        assert_eq!(d.at, Revision::ZERO);
        assert!(d.capability.is_none());
    }

    #[test]
    fn the_payload_is_key_sorted_whatever_order_it_arrived_in() {
        let a = CanonicalJson::new(json!({ "z": 1, "a": { "n": 2, "b": 3 } }));
        let b = CanonicalJson::new(json!({ "a": { "b": 3, "n": 2 }, "z": 1 }));
        assert_eq!(a, b);
        assert_eq!(a.to_canonical_string(), r#"{"a":{"b":3,"n":2},"z":1}"#);
    }

    #[test]
    fn a_record_round_trips_through_its_stored_bytes() {
        let record = SignedRecord::build(&decision(), &signer()).expect("builds");
        let bytes = record.to_canonical_bytes().expect("serializes");
        assert_eq!(SignedRecord::from_bytes(&bytes).expect("parses"), record);
        assert_eq!(record.decision().expect("carries a decision"), decision());
    }

    #[test]
    fn the_signature_is_a_sibling_of_the_envelope_not_a_member_of_it() {
        let record = SignedRecord::build(&decision(), &signer()).expect("builds");
        let bytes = record.to_canonical_bytes().expect("serializes");
        // What the published verifier sees: the envelope alone, ignoring the
        // two fields it does not know, hashing to the same value.
        let envelope: LedgerRecord = serde_json::from_slice(&bytes).expect("envelope parses");
        assert_eq!(
            attest_ledger_core::compute_record_hash(&envelope),
            envelope.record_hash
        );
        assert_eq!(envelope, record.record);
    }

    #[test]
    fn the_envelope_timestamp_carries_the_revision_not_a_clock() {
        let record = SignedRecord::build(&decision(), &signer()).expect("builds");
        assert_eq!(record.record.timestamp, "revision:9");
    }
}
