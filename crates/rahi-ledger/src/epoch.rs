//! Deployment epochs (spec 041): the chain's record of each deployment it
//! serves under.
//!
//! An epoch is a stretch of the chain during which replicas serve one
//! deployment: one artifact, one current manifest, one set of external
//! references (B-1). Epoch 0 is the genesis record and needs no record of its
//! own, so every chain written before this spec is at epoch 0 and opens and
//! verifies byte for byte as it did. Each later epoch is one ordinary
//! `deployment.epoch` decision appended by the deploy step through
//! [`Ledger::append`] (B-2, B-5).
//!
//! Three properties a reader of the chain gets:
//!
//! - **An epoch is its hash** (B-7). Its number orders it within one chain;
//!   a restore rewinds the chain, so a number can be reused by two
//!   timelines, and only the record hash tells them apart.
//! - **References point backwards** (B-4). An epoch names the build, the
//!   deployment record and the authority snapshot that preceded it; nothing
//!   it names is an input to a digest that names it. The chassis records a
//!   reference and never fetches, parses or trusts what it names (B-3).
//! - **The current epoch is readable at `Depth::Resident`** (B-6). The
//!   newest resident epoch record wins; once it has been sealed away, the
//!   newest segment header carries the epoch current at its tail
//!   ([`EpochTail`]), as spec 036 B-5 does for the manifest.

use std::collections::BTreeMap;

use rahi_types::{Error, Sub};
use serde::{Deserialize, Serialize};

use crate::chain::Ledger;
use crate::record::{Decision, DecisionId, DecisionKind, Hash, Outcome, SignedRecord};
use crate::segment::SegmentHeader;

/// The decision kind an epoch record carries (B-2).
pub const EPOCH_KIND: &str = "deployment.epoch";

/// The type of an epoch reference (D-1): `{type, chain, epoch, number}`.
pub const EPOCH_REF_TYPE: &str = "rahi.epoch-ref/v0";

/// The largest serialized reference the deploy step accepts (B-3).
pub const MAX_REFERENCE_BYTES: usize = 4096;

/// The three reference members `RAHI_DEPLOYMENT_REFS` may carry (B-3).
pub const REFERENCE_MEMBERS: [&str; 3] = ["build", "deployment", "authority"];

/// A typed, opaque reference to a record the chassis does not read (B-3).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reference {
    /// A URI naming the referenced record's schema and version.
    #[serde(rename = "type")]
    pub kind: String,
    /// `sha256:<64 hex>` over the referenced record's original bytes.
    pub digest: String,
    /// An identifier the producer assigns, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

/// The external records an epoch names (B-2's `refs`), each optional.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct References {
    /// The build provenance.
    pub build: Option<Reference>,
    /// The deployer's record for this rollout.
    pub deployment: Option<Reference>,
    /// The authority snapshot the build was accepted under.
    pub authority: Option<Reference>,
}

impl References {
    /// Parse `RAHI_DEPLOYMENT_REFS`: one JSON object with optional members
    /// `build`, `deployment` and `authority` (B-3).
    ///
    /// # Errors
    ///
    /// [`Error::Config`] naming the problem when the text is not one object,
    /// names an unknown member, carries a malformed digest or an empty type,
    /// or holds a reference over [`MAX_REFERENCE_BYTES`].
    pub fn parse(text: &str) -> Result<Self, Error> {
        let config =
            |why: String| Error::Config(format!("RAHI_DEPLOYMENT_REFS {why} (spec 041 B-3)"));
        let object: BTreeMap<String, serde_json::Value> = serde_json::from_str(text)
            .map_err(|e| config(format!("is not one JSON object: {e}")))?;
        for (member, value) in &object {
            if !REFERENCE_MEMBERS.contains(&member.as_str()) {
                return Err(config(format!(
                    "names the unknown member {member:?}; only {} are known",
                    REFERENCE_MEMBERS.join(", ")
                )));
            }
            let size = serde_json::to_string(value).map_or(usize::MAX, |s| s.len());
            if size > MAX_REFERENCE_BYTES {
                return Err(config(format!(
                    "member {member:?} is {size} bytes, over the {MAX_REFERENCE_BYTES}-byte bound"
                )));
            }
        }
        let refs: Self = serde_json::from_str(text)
            .map_err(|e| config(format!("does not hold typed references: {e}")))?;
        for (member, reference) in refs.named() {
            if reference.kind.trim().is_empty() {
                return Err(config(format!("member {member:?} has an empty type")));
            }
            if !is_sha256(&reference.digest) {
                return Err(config(format!(
                    "member {member:?} has the digest {:?}, which is not sha256:<64 lowercase hex>",
                    reference.digest
                )));
            }
        }
        Ok(refs)
    }

    /// Every present reference, with its member name.
    fn named(&self) -> impl Iterator<Item = (&'static str, &Reference)> {
        [
            ("build", self.build.as_ref()),
            ("deployment", self.deployment.as_ref()),
            ("authority", self.authority.as_ref()),
        ]
        .into_iter()
        .filter_map(|(name, reference)| reference.map(|r| (name, r)))
    }
}

/// `sha256:` and 64 lowercase hex characters.
#[must_use]
pub fn is_sha256(text: &str) -> bool {
    text.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// What was deployed (B-2's `artifact`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    /// sha256 of the deploy step's own executable, measured (040 B-2), or
    /// `None` when it could not be read.
    pub binary: Option<String>,
    /// The OCI platform the binary was built for, or `None` when unmapped.
    pub platform: Option<String>,
    /// The declared digest-pinned image reference (040 B-5), or `None`.
    pub image: Option<String>,
    /// The chassis version of the deploy step.
    pub rahi_version: String,
    /// The application's declared revision, or `None`.
    pub build_revision: Option<String>,
}

/// Why an epoch was appended (B-2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cause {
    /// A deploy step changed what is deployed.
    Deploy,
    /// The first deploy step after a restore (B-12).
    Restore,
}

/// The restore an epoch follows (B-2's `restore`): the marker's archive and
/// its digest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreRef {
    /// The archive the restore applied.
    pub archive: String,
    /// Its sha256, as the marker records it.
    pub sha256: String,
}

/// The payload of a `deployment.epoch` decision (B-2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploymentEpoch {
    /// This epoch's number.
    pub epoch: u64,
    /// The record hash of epoch n-1, or of the genesis record for epoch 1.
    pub previous: Hash,
    /// Why it was appended.
    pub cause: Cause,
    /// The chain's current manifest after the deploy step (036 B-1).
    pub manifest: Hash,
    /// The record hash of the 036 transition the same step appended, if any.
    pub transition: Option<Hash>,
    /// The store's schema version after the step's migrations.
    pub schema_version: u32,
    /// What was deployed.
    pub artifact: Artifact,
    /// The external records it names.
    pub refs: References,
    /// The restore it follows, when `cause` is `restore`.
    pub restore: Option<RestoreRef>,
    /// The deploy step's wall clock, seconds since the Unix epoch.
    pub wall_time: u64,
}

impl DeploymentEpoch {
    /// The epoch this record carries, if it carries one.
    ///
    /// # Errors
    ///
    /// [`Error::Integrity`] when the record claims the epoch kind and its
    /// payload is not an epoch.
    pub fn of(record: &SignedRecord) -> Result<Option<Self>, Error> {
        Self::of_decision(&record.decision()?)
    }

    /// [`DeploymentEpoch::of`], on a decision already read back.
    ///
    /// # Errors
    ///
    /// As [`DeploymentEpoch::of`].
    pub fn of_decision(decision: &Decision) -> Result<Option<Self>, Error> {
        if decision.kind.as_str() != EPOCH_KIND {
            return Ok(None);
        }
        serde_json::from_value(decision.payload.as_value().clone())
            .map(Some)
            .map_err(|e| {
                Error::Integrity(format!(
                    "record {} is a {EPOCH_KIND} whose payload is not one: {e}",
                    decision.id
                ))
            })
    }

    /// The decision this epoch is appended as: id `epoch:<n>`, outcome
    /// `allow`, actor `actor` (B-2).
    ///
    /// # Errors
    ///
    /// [`Error::Integrity`] when the epoch does not serialize.
    pub fn decision(&self, actor: Sub) -> Result<Decision, Error> {
        let payload = serde_json::to_value(self)
            .map_err(|e| Error::Integrity(format!("a deployment epoch does not serialize: {e}")))?;
        Ok(Decision::new(
            DecisionId::new(format!("epoch:{}", self.epoch)),
            DecisionKind::new(EPOCH_KIND),
            actor,
            Outcome::Allow,
            format!(
                "the cell serves epoch {} ({}) under manifest {}",
                self.epoch,
                match self.cause {
                    Cause::Deploy => "deploy",
                    Cause::Restore => "restore",
                },
                self.manifest
            ),
        )
        .with_payload(payload))
    }

    /// The deployment this epoch names, as a digest over the members B-5
    /// compares: the manifest, the binary, the image and the references
    /// (D-7). Two epochs with one fingerprint name one deployment.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        fingerprint(
            &self.manifest,
            self.artifact.binary.as_deref(),
            self.artifact.image.as_deref(),
            &self.refs,
        )
    }
}

/// [`DeploymentEpoch::fingerprint`] over its parts, so a deploy step can
/// fingerprint its candidate before it builds one.
#[must_use]
pub fn fingerprint(
    manifest: &Hash,
    binary: Option<&str>,
    image: Option<&str>,
    refs: &References,
) -> String {
    let value = serde_json::json!({
        "manifest": manifest.as_str(),
        "binary": binary,
        "image": image,
        "refs": refs,
    });
    attest_ledger_core::sha256_hex(canonical_keysort_json::to_canonical_string(&value).as_bytes())
}

/// The epoch current at a point in the chain (B-6): what a segment header
/// carries at its tail and what [`Ledger::current_epoch`] answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpochTail {
    /// The epoch's number; 0 at the genesis.
    pub number: u64,
    /// The epoch record's hash; the genesis record's hash at epoch 0.
    pub hash: Hash,
    /// The deployment it names ([`DeploymentEpoch::fingerprint`]); `None` at
    /// epoch 0, which names no deployment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    /// The newest restore any epoch at or before this point followed, so a
    /// deploy step can tell a restore it has already recorded from a new one
    /// (B-5, D-8).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_restore: Option<RestoreRef>,
    /// What the epoch deployed, which a booting replica compares itself
    /// against (B-8); `None` at epoch 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deployed: Option<Deployed>,
}

/// The values of an epoch a replica compares itself against (B-8), kept in
/// the tail so the comparison survives the record being sealed away.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deployed {
    /// The chain's current manifest after the deploy step.
    pub manifest: Hash,
    /// The deploy step's measured binary, if it was readable.
    pub binary: Option<String>,
    /// The platform that binary was built for, if mapped.
    pub platform: Option<String>,
    /// The declared image, if one was declared.
    pub image: Option<String>,
}

impl EpochTail {
    /// Epoch 0: the genesis record, naming no deployment.
    #[must_use]
    pub fn genesis(genesis_record: Hash) -> Self {
        Self {
            number: 0,
            hash: genesis_record,
            fingerprint: None,
            last_restore: None,
            deployed: None,
        }
    }

    /// The epoch current after `records`, given what was current before them.
    ///
    /// # Errors
    ///
    /// [`Error::Integrity`] when an epoch record's payload is not an epoch or
    /// its hash does not parse.
    pub fn after(records: &[SignedRecord], before: Self) -> Result<Self, Error> {
        let mut tail = before;
        for record in records {
            if let Some(epoch) = DeploymentEpoch::of(record)? {
                tail = Self {
                    number: epoch.epoch,
                    hash: record.hash()?,
                    fingerprint: Some(epoch.fingerprint()),
                    last_restore: epoch.restore.clone().or(tail.last_restore),
                    deployed: Some(Deployed {
                        manifest: epoch.manifest.clone(),
                        binary: epoch.artifact.binary.clone(),
                        platform: epoch.artifact.platform.clone(),
                        image: epoch.artifact.image.clone(),
                    }),
                };
            }
        }
        Ok(tail)
    }

    /// The epoch reference of 041 D-1 for this epoch in the chain `chain`
    /// (the genesis record's hash): `{type, chain, epoch, number}`.
    #[must_use]
    pub fn reference(&self, chain: &Hash) -> serde_json::Value {
        serde_json::json!({
            "type": EPOCH_REF_TYPE,
            "chain": chain.as_str(),
            "epoch": self.hash.as_str(),
            "number": self.number,
        })
    }

    /// Text for the `kernel_segments.current_epoch` column.
    ///
    /// # Errors
    ///
    /// [`Error::Integrity`] when it does not serialize.
    pub fn to_column(&self) -> Result<String, Error> {
        serde_json::to_string(self)
            .map_err(|e| Error::Integrity(format!("an epoch tail does not serialize: {e}")))
    }

    /// Read the `kernel_segments.current_epoch` column.
    ///
    /// # Errors
    ///
    /// [`Error::Integrity`] when the column holds something else.
    pub fn from_column(text: &str) -> Result<Self, Error> {
        serde_json::from_str(text)
            .map_err(|e| Error::Integrity(format!("a segment's current_epoch is not one: {e}")))
    }
}

/// The SQL that finds the genesis record's hash once the genesis has been
/// sealed: spec 042's identity index keeps every record's hash for the life
/// of the chain.
const GENESIS_IDENTITY_SQL: &str =
    "SELECT record_hash AS hash FROM kernel_decision_identity WHERE id = $1";

#[derive(Debug, Deserialize)]
struct HashRow {
    hash: String,
}

impl Ledger {
    /// The genesis record's hash: the chain's identity in an epoch
    /// reference (D-1, 040 B-7).
    ///
    /// Read from the resident chain while the genesis is resident, and from
    /// spec 042's lifetime identity index once it has been sealed away.
    ///
    /// # Errors
    ///
    /// [`Error::Integrity`] when neither holds it: a chain whose genesis
    /// cannot be named has an identity gap, which spec 042's reindex closes.
    pub async fn genesis_record_hash(&self) -> Result<Hash, Error> {
        let id = self.genesis_id();
        if let Some(hash) = crate::chain::hash_of(self.store(), &id).await? {
            return Ok(hash);
        }
        let rows: Vec<HashRow> = self
            .store()
            .query_consistent(
                GENESIS_IDENTITY_SQL,
                vec![rahi_store::Value::from(id.as_str())],
            )
            .await?;
        match rows.into_iter().next() {
            Some(row) => Hash::parse(row.hash),
            None => Err(Error::Integrity(format!(
                "the genesis record {id} is neither resident nor in the lifetime identity index; \
                 run `{}` before reading epochs",
                crate::identity::REINDEX_COMMAND
            ))),
        }
    }

    /// The current epoch (B-6): the newest resident epoch record; once that
    /// has been sealed away, the newest segment header that names one; with
    /// neither, epoch 0 at the genesis record.
    ///
    /// Read from one snapshot of the chain, as the current manifest is
    /// (spec 036 D-15), so a seal committing beside this read cannot hide an
    /// epoch from both halves.
    ///
    /// A segment sealed by a binary older than this spec names no epoch.
    /// The read walks back to the newest header that does (D-9): such a
    /// segment holds no epoch record unless an older replica sealed a run
    /// that a newer deploy step wrote into, and then the newest known epoch
    /// is the best evidence the headers keep. An epoch is an observation and
    /// never a ceiling (B-8, B-10a), so the walk cannot admit anything.
    ///
    /// # Errors
    ///
    /// [`Error::Integrity`] as [`Ledger::current_manifest`] refuses, or when
    /// an epoch record or header does not parse.
    pub async fn current_epoch(&self) -> Result<EpochTail, Error> {
        let snapshot = self
            .chain_snapshot_at(crate::chain::SnapshotSeam::Read)
            .await?;
        let before = match snapshot
            .segments
            .iter()
            .rev()
            .find_map(|header| header.current_epoch.clone())
        {
            Some(tail) => tail,
            None => EpochTail::genesis(self.genesis_record_hash().await?),
        };
        EpochTail::after(&snapshot.records, before)
    }

    /// The epoch current at the tail of a run about to be sealed, given the
    /// segments already sealed (B-6). Used by a seal to stamp its header.
    pub(crate) async fn epoch_at_tail(
        &self,
        sealed: &[SegmentHeader],
        records: &[SignedRecord],
    ) -> Result<EpochTail, Error> {
        let before = match sealed
            .iter()
            .rev()
            .find_map(|header| header.current_epoch.clone())
        {
            Some(tail) => tail,
            None => EpochTail::genesis(self.genesis_record_hash().await?),
        };
        EpochTail::after(records, before)
    }

    /// Append `epoch` as the deploy step's actor (B-2): synchronously,
    /// through the compare-and-swap of spec 013 B-3, never the denial queue.
    ///
    /// # Errors
    ///
    /// As [`Ledger::append`].
    pub async fn append_epoch(&self, epoch: &DeploymentEpoch, actor: Sub) -> Result<Hash, Error> {
        self.append(epoch.decision(actor)?).await
    }

    /// The genesis record's id.
    fn genesis_id(&self) -> DecisionId {
        DecisionId::new(format!("genesis:{}", self.genesis_parent()))
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn digest(byte: &str) -> String {
        format!("sha256:{}", byte.repeat(32))
    }

    fn hash(byte: &str) -> Hash {
        Hash::parse(digest(byte)).expect("a hash")
    }

    fn epoch() -> DeploymentEpoch {
        DeploymentEpoch {
            epoch: 2,
            previous: hash("11"),
            cause: Cause::Deploy,
            manifest: hash("22"),
            transition: None,
            schema_version: 3,
            artifact: Artifact {
                binary: Some(digest("33")),
                platform: Some("linux/amd64".to_owned()),
                image: None,
                rahi_version: "0.7.0".to_owned(),
                build_revision: None,
            },
            refs: References::default(),
            restore: None,
            wall_time: 1,
        }
    }

    #[test]
    fn an_epoch_round_trips_through_its_decision() {
        let decision = epoch().decision(Sub::new("system:deploy")).expect("builds");
        assert_eq!(decision.id.as_str(), "epoch:2");
        assert_eq!(decision.kind.as_str(), EPOCH_KIND);
        assert_eq!(decision.outcome, Outcome::Allow);
        assert_eq!(
            DeploymentEpoch::of_decision(&decision).expect("reads"),
            Some(epoch())
        );
    }

    #[test]
    fn references_parse_as_b3_states_and_refuse_everything_else() {
        let ok = References::parse(&format!(
            r#"{{"build":{{"type":"https://slsa.dev/provenance/v1","digest":"{}","id":"b-1"}}}}"#,
            digest("aa")
        ))
        .expect("a build reference");
        assert_eq!(ok.build.expect("present").id.as_deref(), Some("b-1"));
        assert_eq!(
            References::parse("{}").expect("empty"),
            References::default()
        );

        let too_long = "x".repeat(MAX_REFERENCE_BYTES);
        for bad in [
            "[]".to_owned(),
            r#"{"other":null}"#.to_owned(),
            r#"{"build":{"type":"t","digest":"sha256:AB"}}"#.to_owned(),
            format!(r#"{{"build":{{"type":"","digest":"{}"}}}}"#, digest("aa")),
            format!(
                r#"{{"build":{{"type":"t","digest":"{}","extra":1}}}}"#,
                digest("aa")
            ),
            format!(
                r#"{{"build":{{"type":"{too_long}","digest":"{}"}}}}"#,
                digest("aa")
            ),
        ] {
            let err = References::parse(&bad).expect_err(&bad);
            assert!(matches!(err, Error::Config(_)), "{bad}: {err}");
            assert!(err.message().contains("RAHI_DEPLOYMENT_REFS"), "{err}");
        }
    }

    #[test]
    fn the_fingerprint_moves_with_what_b5_compares_and_nothing_else() {
        let base = epoch();
        let mut later = base.clone();
        later.epoch = 5;
        later.previous = hash("44");
        later.wall_time = 99;
        later.schema_version = 4;
        assert_eq!(
            base.fingerprint(),
            later.fingerprint(),
            "numbering is not a deployment"
        );
        for change in [
            |e: &mut DeploymentEpoch| {
                e.manifest = Hash::parse(format!("sha256:{}", "55".repeat(32))).expect("hash")
            },
            |e: &mut DeploymentEpoch| e.artifact.binary = None,
            |e: &mut DeploymentEpoch| e.artifact.image = Some("ghcr.io/x@sha256:1".to_owned()),
            |e: &mut DeploymentEpoch| {
                e.refs.authority = Some(Reference {
                    kind: "t".to_owned(),
                    digest: format!("sha256:{}", "66".repeat(32)),
                    id: None,
                });
            },
        ] {
            let mut changed = base.clone();
            change(&mut changed);
            assert_ne!(base.fingerprint(), changed.fingerprint());
        }
    }

    #[test]
    fn a_tail_round_trips_through_its_column() {
        let tail = EpochTail {
            number: 3,
            hash: hash("77"),
            fingerprint: Some("f".to_owned()),
            last_restore: Some(RestoreRef {
                archive: "a".to_owned(),
                sha256: digest("88"),
            }),
            deployed: Some(Deployed {
                manifest: hash("aa"),
                binary: Some(digest("bb")),
                platform: Some("linux/arm64".to_owned()),
                image: None,
            }),
        };
        assert_eq!(
            EpochTail::from_column(&tail.to_column().expect("writes")).expect("reads"),
            tail
        );
        let reference = tail.reference(&hash("99"));
        assert_eq!(reference["type"], EPOCH_REF_TYPE);
        assert_eq!(reference["number"], 3);
        assert_eq!(reference["epoch"], hash("77").as_str());
    }
}
