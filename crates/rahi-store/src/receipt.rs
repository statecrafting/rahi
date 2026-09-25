//! Receipts: an inbound item recorded once per content revision (spec 045
//! 3.1 to 3.3, 3.6).
//!
//! A receipt names an item by tenant, namespace, and key (or a synthetic key
//! derived from stable content parts), and records a digest of its content.
//! [`Receipts::classify`] answers whether a delivery is new, a repeat, a
//! change under the same key, or erased; the `stage_*` calls turn that
//! answer into statements the caller appends to its own [`TxnBuilder`]
//! beside its domain writes and [`crate::Outbox::stage`] (spec 045 B-9).
//! Nothing here submits a batch: the caller owns the `txn`.
//!
//! Two tables carry the identity: `rahi_receipt_head` is the one row per
//! identity a staging call compare-and-swaps against (012 D-3's `NOT NULL`
//! technique, spec 045 B-8), and `rahi_receipt` is one row per revision,
//! accepted or collision. Both are created by [`receipt_migration`].

use attest_ledger_core::sha256_hex;
use rahi_types::{Error, UnixSeconds};
use serde::Deserialize;

use crate::migrate::Migration;
use crate::outbox::TxnBuilder;
use crate::query::Value;
use crate::store::StoreHandle;
use crate::txn::Statement;

/// A revision number of one receipt identity. Dense and strictly increasing
/// per identity (spec 045 I-1).
pub type ReceiptRevision = u32;

const KEY_DOMAIN: &[u8] = b"rahi.receipt.key/v1";
const MAX_NAMESPACE_BYTES: usize = 256;
const MAX_KEY_BYTES: usize = 1024;
const MAX_OUTCOME_BYTES: usize = 4096;

/// sha256 of `bytes`, as plain lowercase hex with no `sha256:` prefix.
///
/// [`attest_ledger_core::sha256_hex`] is the one digest primitive this crate
/// carries (spec 036 B-7); its `sha256:` prefix is stripped here because
/// spec 045 B-2 and B-4 store the digest as bare hex.
fn sha256_hex_raw(bytes: &[u8]) -> String {
    sha256_hex(bytes)
        .strip_prefix("sha256:")
        .map_or_else(|| sha256_hex(bytes), ToOwned::to_owned)
}

/// A length-prefixed, domain-separated encoding of `parts`, so no
/// concatenation of two different part sets can collide.
fn domain_separated(domain: &[u8], parts: &[&[u8]]) -> Vec<u8> {
    let mut buf =
        Vec::with_capacity(domain.len() + parts.iter().map(|p| p.len() + 8).sum::<usize>());
    buf.extend_from_slice(domain);
    for part in parts {
        buf.extend_from_slice(&(part.len() as u64).to_be_bytes());
        buf.extend_from_slice(part);
    }
    buf
}

/// The identity of an inbound item: tenant, source namespace, and the
/// source's own key, or a synthetic one (spec 045 B-1, B-3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceiptKey {
    /// The application's tenant. Never interpreted by the chassis.
    pub tenant: String,
    /// The source and provider account (`email:imap:acct-7`).
    pub namespace: String,
    /// The source's own identifier, or `syn:<hex>` for a synthetic key.
    pub key: String,
}

impl ReceiptKey {
    /// Build a key from the source's own identifier.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when `tenant` or `namespace` is empty,
    /// `namespace` exceeds 256 bytes, `key` is empty, or `key` exceeds 1 KiB.
    pub fn new(
        tenant: impl Into<String>,
        namespace: impl Into<String>,
        key: impl Into<String>,
    ) -> Result<Self, Error> {
        let key_value = key.into();
        if key_value.is_empty() {
            return Err(Error::Validation(
                "a receipt key must not be empty; use ReceiptKey::synthetic for a source with \
                 none"
                    .to_owned(),
            ));
        }
        Self::build(tenant.into(), namespace.into(), key_value)
    }

    /// Build a key for a source with no stable identifier of its own.
    ///
    /// `parts` are the caller's chosen stable bytes (spec 045 B-3); the key
    /// becomes `syn:` followed by the hex sha256 of their domain-separated
    /// encoding. A synthetic key derived from content can never classify as
    /// `Changed`, since a change in the parts is a different key.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when `tenant` or `namespace` fails the same
    /// checks as [`Self::new`], or `parts` is empty.
    pub fn synthetic(
        tenant: impl Into<String>,
        namespace: impl Into<String>,
        parts: &[&[u8]],
    ) -> Result<Self, Error> {
        if parts.is_empty() {
            return Err(Error::Validation(
                "a synthetic key needs at least one stable part".to_owned(),
            ));
        }
        let digest = sha256_hex_raw(&domain_separated(b"rahi.receipt.synthetic/v1", parts));
        Self::build(tenant.into(), namespace.into(), format!("syn:{digest}"))
    }

    fn build(tenant: String, namespace: String, key: String) -> Result<Self, Error> {
        if tenant.is_empty() {
            return Err(Error::Validation(
                "a receipt's tenant must not be empty".to_owned(),
            ));
        }
        if namespace.is_empty() || namespace.len() > MAX_NAMESPACE_BYTES {
            return Err(Error::Validation(format!(
                "a receipt's namespace must be 1 to {MAX_NAMESPACE_BYTES} bytes, got {}",
                namespace.len()
            )));
        }
        if key.len() > MAX_KEY_BYTES {
            return Err(Error::Validation(format!(
                "a receipt's key must be at most {MAX_KEY_BYTES} bytes, got {}",
                key.len()
            )));
        }
        Ok(Self {
            tenant,
            namespace,
            key,
        })
    }

    /// Whether this key was built by [`Self::synthetic`].
    #[must_use]
    pub fn is_synthetic(&self) -> bool {
        self.key.starts_with("syn:")
    }

    /// `explicit` or `synthetic`, as recorded in `key_kind` (spec 045 B-3).
    #[must_use]
    pub fn kind(&self) -> &'static str {
        if self.is_synthetic() {
            "synthetic"
        } else {
            "explicit"
        }
    }

    /// sha256 over a domain-separated, length-prefixed encoding of the three
    /// parts (spec 045 B-2): the identity column, stable across processes.
    #[must_use]
    pub fn key_digest(&self) -> String {
        sha256_hex_raw(&domain_separated(
            KEY_DOMAIN,
            &[
                self.tenant.as_bytes(),
                self.namespace.as_bytes(),
                self.key.as_bytes(),
            ],
        ))
    }
}

/// A content digest: 32 bytes, stored as lowercase hex (spec 045 B-4).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ContentDigest(String);

impl ContentDigest {
    /// sha256 of `bytes`, the common case.
    #[must_use]
    pub fn of(bytes: &[u8]) -> Self {
        Self(sha256_hex_raw(bytes))
    }

    /// Wrap an already-computed digest.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when `hex` is not exactly 64 lowercase hex
    /// characters.
    pub fn from_hex(hex: impl Into<String>) -> Result<Self, Error> {
        let hex = hex.into();
        if hex.len() != 64
            || !hex
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Err(Error::Validation(format!(
                "a content digest must be 64 lowercase hex characters, got {hex:?}"
            )));
        }
        Ok(Self(hex))
    }

    /// The lowercase hex form.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ContentDigest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The typed handle a `Changed` classification carries (spec 045 B-7).
///
/// The only way to obtain one is a matching [`Classification::Changed`] from
/// [`Receipts::classify`]; there is no public constructor. This is what
/// makes [`Receipts::stage_revision`] and [`Receipts::stage_collision`] the
/// only calls that can acknowledge changed content, and makes them
/// impossible without first classifying it.
///
/// ```compile_fail
/// # use rahi_store::receipt::Changed;
/// // `Changed`'s fields are private and it has no public constructor: a
/// // caller cannot fabricate one to stage a revision without first calling
/// // `Receipts::classify` and matching `Classification::Changed`.
/// let handle = Changed { head_revision: 0, head_digest: String::new() };
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
#[must_use = "a Changed classification is acted on by stage_revision or stage_collision, or the change is lost"]
pub struct Changed {
    head_revision: ReceiptRevision,
    head_digest: String,
}

impl Changed {
    /// The accepted head's own revision number that classification saw.
    #[must_use]
    pub fn head_revision(&self) -> ReceiptRevision {
        self.head_revision
    }

    /// The accepted head's content digest that classification saw.
    #[must_use]
    pub fn head_digest(&self) -> &str {
        &self.head_digest
    }
}

/// What [`Receipts::classify`] answers (spec 045 B-5).
#[derive(Clone, Debug, PartialEq, Eq)]
#[must_use]
pub enum Classification {
    /// No row exists for this identity yet.
    FirstSeen,
    /// The digest matches a recorded accepted revision.
    Redelivered {
        /// The accepted revision the digest matched.
        revision: ReceiptRevision,
        /// Whether the matched revision is the current accepted head.
        is_head: bool,
        /// The outcome recorded with that revision, if any.
        outcome: Option<String>,
    },
    /// Rows exist and no accepted or collision revision carries this digest.
    Changed(Changed),
    /// The digest matches a recorded collision.
    CollisionRedelivered {
        /// The collision revision the digest matched.
        revision: ReceiptRevision,
    },
    /// The identity carries an erasure tombstone (spec 045 B-21, B-22).
    Erased {
        /// When the erasure was staged.
        erased_at: UnixSeconds,
    },
}

/// What accompanies a staging call (spec 045 B-6).
#[derive(Clone, Debug, Default)]
pub struct ReceiptMeta {
    /// When retention may compact this receipt, or `None` for no expiry.
    pub retain_until: Option<UnixSeconds>,
    /// When the sweep may delete a compacted tombstone, or `None`.
    pub tombstone_until: Option<UnixSeconds>,
    /// A reference (ids, a status code, a digest) at most 4 KiB, never the
    /// item's body.
    pub outcome: Option<String>,
}

impl ReceiptMeta {
    fn check(&self) -> Result<(), Error> {
        if let Some(outcome) = &self.outcome
            && outcome.len() > MAX_OUTCOME_BYTES
        {
            return Err(Error::Validation(format!(
                "a receipt outcome must be at most {MAX_OUTCOME_BYTES} bytes, got {}",
                outcome.len()
            )));
        }
        Ok(())
    }
}

/// The scope of an erasure (spec 045 B-21).
#[derive(Clone, Debug)]
pub enum EraseScope {
    /// One identity.
    Identity(ReceiptKey),
    /// Every identity of a namespace within a tenant.
    Namespace {
        /// The tenant.
        tenant: String,
        /// The namespace.
        namespace: String,
    },
    /// Every identity of a tenant.
    Tenant {
        /// The tenant.
        tenant: String,
    },
}

#[derive(Debug, Deserialize)]
struct HeadRow {
    revision: i64,
    accepted_revision: Option<i64>,
    accepted_digest: Option<String>,
    erased: i64,
    erased_at: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct RevisionRow {
    revision: i64,
    disposition: String,
    outcome: Option<String>,
}

const SELECT_HEAD: &str = "SELECT revision, accepted_revision, accepted_digest, erased, erased_at \
     FROM rahi_receipt_head WHERE key_digest = $1";

const SELECT_REVISION_BY_DIGEST: &str = "SELECT revision, disposition, outcome FROM rahi_receipt \
     WHERE key_digest = $1 AND digest = $2 LIMIT 1";

/// Seed an identity's head row at `revision = 0` if none exists yet.
///
/// `ON CONFLICT DO NOTHING` never raises: two concurrent `stage_first`
/// batches for the same identity both seed harmlessly, and the CAS that
/// decides which one actually accepts revision 1 is [`GUARD_ACCEPT`], the
/// same mechanism [`Receipts::stage_revision`] uses. A bare `INSERT`'s
/// primary-key violation is not used for this: hiqlite maps a constraint
/// failure raised inside a `txn` batch to `Error::Validation`, not
/// `Error::Conflict` (spec 045 D-16), so the CAS has to be the explicit
/// `NOT NULL` technique everywhere, not the primary key.
const SEED_HEAD: &str = "INSERT INTO rahi_receipt_head \
     (key_digest, tenant, namespace, key, key_kind, revision, accepted_revision, \
      accepted_digest, retain_until, tombstone_until, tombstoned, erased, erased_at, \
      created_at) \
     VALUES ($1, $2, $3, $4, $5, 0, NULL, NULL, $6, $7, 0, 0, NULL, $8) \
     ON CONFLICT (key_digest) DO NOTHING";

const INSERT_REVISION: &str = "INSERT INTO rahi_receipt \
     (key_digest, revision, digest, disposition, outcome, seen_count, last_seen_at, \
      created_at) \
     VALUES ($1, $2, $3, $4, $5, 1, $6, $6)";

/// spec 045 B-8: guards on the CAS baseline the caller was classified
/// against and on the identity not being erased, using 012 D-3's `NOT NULL`
/// technique so a mismatch aborts the whole batch, including the revision
/// insert that follows it.
const GUARD_ACCEPT: &str = "UPDATE rahi_receipt_head SET \
     revision = CASE WHEN revision = $1 AND erased = 0 THEN $2 ELSE NULL END, \
     accepted_revision = CASE WHEN revision = $1 AND erased = 0 THEN $2 ELSE accepted_revision END, \
     accepted_digest = CASE WHEN revision = $1 AND erased = 0 THEN $3 ELSE accepted_digest END \
     WHERE key_digest = $4";

const GUARD_COLLISION: &str = "UPDATE rahi_receipt_head SET \
     revision = CASE WHEN revision = $1 AND erased = 0 THEN $2 ELSE NULL END \
     WHERE key_digest = $3";

const STAGE_SEEN: &str = "UPDATE rahi_receipt SET \
     seen_count = COALESCE(seen_count, 0) + 1, last_seen_at = $1 \
     WHERE key_digest = $2 AND revision = $3";

/// Classification, staging, retention, and erasure over receipt rows.
#[derive(Clone, Copy, Debug)]
pub struct Receipts;

impl Receipts {
    /// Classify a delivery against what is already recorded (spec 045 B-5).
    ///
    /// Reads with `query_consistent`: a stale answer here is a wrong
    /// decision (012's rule for admission).
    ///
    /// # Errors
    ///
    /// The store's error when the read fails, or [`Error::Integrity`] when a
    /// recorded row does not fit the columns this spec defines.
    pub async fn classify(
        store: &StoreHandle,
        key: &ReceiptKey,
        digest: &ContentDigest,
    ) -> Result<Classification, Error> {
        let key_digest = key.key_digest();
        let heads: Vec<HeadRow> = store
            .query_consistent(SELECT_HEAD, vec![Value::from(key_digest.clone())])
            .await?;
        let Some(head) = heads.into_iter().next() else {
            return Ok(Classification::FirstSeen);
        };
        if head.erased != 0 {
            let raw = head.erased_at.ok_or_else(|| {
                Error::Integrity(format!(
                    "receipt {key_digest} is erased but has no erased_at"
                ))
            })?;
            let secs = u64::try_from(raw).map_err(|_| {
                Error::Integrity(format!("receipt {key_digest} holds a negative erased_at"))
            })?;
            return Ok(Classification::Erased {
                erased_at: UnixSeconds::new(secs),
            });
        }

        let matches: Vec<RevisionRow> = store
            .query_consistent(
                SELECT_REVISION_BY_DIGEST,
                vec![
                    Value::from(key_digest.clone()),
                    Value::from(digest.as_str()),
                ],
            )
            .await?;
        if let Some(row) = matches.into_iter().next() {
            let revision = to_revision(row.revision, &key_digest)?;
            return match row.disposition.as_str() {
                "accepted" => {
                    let is_head = head.accepted_revision == Some(row.revision);
                    Ok(Classification::Redelivered {
                        revision,
                        is_head,
                        outcome: row.outcome,
                    })
                }
                "collision" => Ok(Classification::CollisionRedelivered { revision }),
                other => Err(Error::Integrity(format!(
                    "receipt {key_digest} revision {revision} holds an unknown disposition \
                     {other:?}"
                ))),
            };
        }

        Ok(Classification::Changed(Changed {
            head_revision: to_revision(head.revision, &key_digest)?,
            head_digest: head.accepted_digest.unwrap_or_default(),
        }))
    }

    /// Stage the first accepted revision of a new identity (spec 045 B-6).
    ///
    /// Seeds the head row at revision 0 if none exists (harmless when it
    /// already does, [`SEED_HEAD`]), then applies the same CAS
    /// [`Receipts::stage_revision`] uses, expecting revision 0: a second
    /// `stage_first` racing the same identity aborts on that guard and the
    /// batch that submits it second returns [`Error::Conflict`] (spec 045
    /// FR-002).
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when `meta.outcome` exceeds 4 KiB.
    pub fn stage_first(
        txn: &mut TxnBuilder,
        key: &ReceiptKey,
        digest: &ContentDigest,
        meta: &ReceiptMeta,
        now: UnixSeconds,
    ) -> Result<(), Error> {
        meta.check()?;
        let key_digest = key.key_digest();
        txn.push(Statement::with_params(
            SEED_HEAD,
            vec![
                Value::from(key_digest.clone()),
                Value::from(key.tenant.as_str()),
                Value::from(key.namespace.as_str()),
                Value::from(key.key.as_str()),
                Value::from(key.kind()),
                Value::from(meta.retain_until.map(unix_to_sql)),
                Value::from(meta.tombstone_until.map(unix_to_sql)),
                Value::Integer(unix_to_sql(now)),
            ],
        ));
        txn.push(Statement::with_params(
            GUARD_ACCEPT,
            vec![
                Value::Integer(0),
                Value::Integer(1),
                Value::from(digest.as_str()),
                Value::from(key_digest.clone()),
            ],
        ));
        txn.push(Statement::with_params(
            INSERT_REVISION,
            vec![
                Value::from(key_digest),
                Value::Integer(1),
                Value::from(digest.as_str()),
                Value::from("accepted"),
                Value::from(meta.outcome.clone()),
                Value::Integer(unix_to_sql(now)),
            ],
        ));
        Ok(())
    }

    /// Stage a new accepted revision over changed content (spec 045 B-6,
    /// B-7). `changed` is the handle [`Receipts::classify`] returned; there
    /// is no other way to build one.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when `meta.outcome` exceeds 4 KiB.
    pub fn stage_revision(
        txn: &mut TxnBuilder,
        key: &ReceiptKey,
        digest: &ContentDigest,
        changed: &Changed,
        meta: &ReceiptMeta,
        now: UnixSeconds,
    ) -> Result<(), Error> {
        meta.check()?;
        let key_digest = key.key_digest();
        let expected = i64::from(changed.head_revision);
        let next = expected.saturating_add(1);
        txn.push(Statement::with_params(
            GUARD_ACCEPT,
            vec![
                Value::Integer(expected),
                Value::Integer(next),
                Value::from(digest.as_str()),
                Value::from(key_digest.clone()),
            ],
        ));
        txn.push(Statement::with_params(
            INSERT_REVISION,
            vec![
                Value::from(key_digest),
                Value::Integer(next),
                Value::from(digest.as_str()),
                Value::from("accepted"),
                Value::from(meta.outcome.clone()),
                Value::Integer(unix_to_sql(now)),
            ],
        ));
        Ok(())
    }

    /// Stage a collision: changed content under a used key, recorded rather
    /// than dropped (spec 045 B-6, I-2). The accepted head is untouched.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when `meta.outcome` exceeds 4 KiB.
    pub fn stage_collision(
        txn: &mut TxnBuilder,
        key: &ReceiptKey,
        digest: &ContentDigest,
        changed: &Changed,
        meta: &ReceiptMeta,
        now: UnixSeconds,
    ) -> Result<(), Error> {
        meta.check()?;
        let key_digest = key.key_digest();
        let expected = i64::from(changed.head_revision);
        let next = expected.saturating_add(1);
        txn.push(Statement::with_params(
            GUARD_COLLISION,
            vec![
                Value::Integer(expected),
                Value::Integer(next),
                Value::from(key_digest.clone()),
            ],
        ));
        txn.push(Statement::with_params(
            INSERT_REVISION,
            vec![
                Value::from(key_digest),
                Value::Integer(next),
                Value::from(digest.as_str()),
                Value::from("collision"),
                Value::from(meta.outcome.clone()),
                Value::Integer(unix_to_sql(now)),
            ],
        ));
        Ok(())
    }

    /// Optionally record a redelivery of an already-recorded revision (spec
    /// 045 B-6). Skipping this loses a counter, never content (D-8).
    pub fn stage_seen(
        txn: &mut TxnBuilder,
        key: &ReceiptKey,
        revision: ReceiptRevision,
        now: UnixSeconds,
    ) {
        txn.push(Statement::with_params(
            STAGE_SEEN,
            vec![
                Value::Integer(unix_to_sql(now)),
                Value::from(key.key_digest()),
                Value::Integer(i64::from(revision)),
            ],
        ));
    }

    /// Stage an erasure over `scope` (spec 045 B-21).
    ///
    /// Deletes every revision row and every processing and attempt row the
    /// scope covers, and leaves one tombstone per identity holding only
    /// `key_digest` and `erased_at`. There is no call that undoes this
    /// (B-22, D-11).
    pub fn stage_erasure(txn: &mut TxnBuilder, scope: &EraseScope, now: UnixSeconds) {
        let now_sql = unix_to_sql(now);
        match scope {
            EraseScope::Identity(key) => {
                let key_digest = key.key_digest();
                erase_by_predicate(
                    txn,
                    "key_digest = $1",
                    vec![Value::from(key_digest)],
                    now_sql,
                );
            }
            EraseScope::Namespace { tenant, namespace } => {
                erase_by_predicate(
                    txn,
                    "key_digest IN (SELECT key_digest FROM rahi_receipt_head \
                     WHERE tenant = $1 AND namespace = $2)",
                    vec![
                        Value::from(tenant.as_str()),
                        Value::from(namespace.as_str()),
                    ],
                    now_sql,
                );
            }
            EraseScope::Tenant { tenant } => {
                erase_by_predicate(
                    txn,
                    "key_digest IN (SELECT key_digest FROM rahi_receipt_head WHERE tenant = $1)",
                    vec![Value::from(tenant.as_str())],
                    now_sql,
                );
            }
        }
    }
}

fn erase_by_predicate(txn: &mut TxnBuilder, predicate: &str, params: Vec<Value>, now_sql: i64) {
    txn.push(Statement::with_params(
        format!("DELETE FROM rahi_receipt WHERE {predicate}"),
        params.clone(),
    ));
    txn.push(Statement::with_params(
        format!("DELETE FROM rahi_processing_attempt WHERE {predicate}"),
        params.clone(),
    ));
    txn.push(Statement::with_params(
        format!("DELETE FROM rahi_processing WHERE {predicate}"),
        params.clone(),
    ));
    // `now` is appended after the predicate's own params, so its `$N` is one
    // past the highest number the predicate itself uses, whatever that is.
    let erased_at_param = params.len().saturating_add(1);
    let mut set_params = params;
    set_params.push(Value::Integer(now_sql));
    txn.push(Statement::with_params(
        format!(
            "UPDATE rahi_receipt_head SET key = NULL, \
             accepted_revision = NULL, accepted_digest = NULL, retain_until = NULL, \
             tombstone_until = NULL, tombstoned = 0, erased = 1, erased_at = ${erased_at_param} \
             WHERE {predicate}"
        ),
        set_params,
    ));
}

fn to_revision(raw: i64, key_digest: &str) -> Result<ReceiptRevision, Error> {
    ReceiptRevision::try_from(raw).map_err(|_| {
        Error::Integrity(format!(
            "receipt {key_digest} holds an out-of-range revision {raw}"
        ))
    })
}

/// `UnixSeconds` as the `i64` SQLite stores. Saturates rather than wrapping.
fn unix_to_sql(value: UnixSeconds) -> i64 {
    i64::try_from(value.get()).unwrap_or(i64::MAX)
}

impl From<UnixSeconds> for Value {
    fn from(value: UnixSeconds) -> Self {
        Self::Integer(unix_to_sql(value))
    }
}

/// The receipt tables, applied only by this migration (spec 045 2.1).
///
/// Additive: every statement only creates a table or an index, so a binary
/// whose last migration is older may still serve a store ahead of it (spec
/// 036 B-8).
#[must_use]
pub fn receipt_migration(version: u32) -> Migration {
    Migration::new(
        version,
        "rahi-store receipts and work claims",
        [
            "CREATE TABLE IF NOT EXISTS rahi_receipt_head (\
                key_digest TEXT PRIMARY KEY, \
                tenant TEXT NOT NULL, \
                namespace TEXT NOT NULL, \
                key TEXT NULL, \
                key_kind TEXT NOT NULL, \
                revision INTEGER NOT NULL, \
                accepted_revision INTEGER NULL, \
                accepted_digest TEXT NULL, \
                retain_until INTEGER NULL, \
                tombstone_until INTEGER NULL, \
                tombstoned INTEGER NOT NULL DEFAULT 0, \
                erased INTEGER NOT NULL DEFAULT 0, \
                erased_at INTEGER NULL, \
                created_at INTEGER NOT NULL)",
            "CREATE INDEX IF NOT EXISTS rahi_receipt_head_scope \
                ON rahi_receipt_head (tenant, namespace)",
            "CREATE INDEX IF NOT EXISTS rahi_receipt_head_retention \
                ON rahi_receipt_head (tombstoned, tombstone_until)",
            "CREATE TABLE IF NOT EXISTS rahi_receipt (\
                key_digest TEXT NOT NULL, \
                revision INTEGER NOT NULL, \
                digest TEXT NOT NULL, \
                disposition TEXT NOT NULL, \
                outcome TEXT NULL, \
                seen_count INTEGER NULL, \
                last_seen_at INTEGER NULL, \
                created_at INTEGER NOT NULL, \
                PRIMARY KEY (key_digest, revision))",
            "CREATE INDEX IF NOT EXISTS rahi_receipt_by_digest ON rahi_receipt (key_digest, digest)",
            "CREATE TABLE IF NOT EXISTS rahi_processing (\
                key_digest TEXT NOT NULL, \
                revision INTEGER NOT NULL, \
                processor TEXT NOT NULL, \
                processor_revision TEXT NOT NULL, \
                tenant TEXT NOT NULL, \
                namespace TEXT NOT NULL, \
                key TEXT NOT NULL, \
                state TEXT NOT NULL, \
                holder TEXT NULL, \
                fence INTEGER NOT NULL DEFAULT 0, \
                attempt INTEGER NOT NULL DEFAULT 0, \
                expires_at INTEGER NULL, \
                next_attempt_at INTEGER NULL, \
                created_at INTEGER NOT NULL, \
                PRIMARY KEY (key_digest, revision, processor, processor_revision))",
            "CREATE INDEX IF NOT EXISTS rahi_processing_queue \
                ON rahi_processing (namespace, processor, state, created_at)",
            "CREATE TABLE IF NOT EXISTS rahi_processing_attempt (\
                key_digest TEXT NOT NULL, \
                revision INTEGER NOT NULL, \
                processor TEXT NOT NULL, \
                processor_revision TEXT NOT NULL, \
                attempt INTEGER NOT NULL, \
                holder TEXT NULL, \
                outcome TEXT NOT NULL, \
                error_class TEXT NULL, \
                detail TEXT NULL, \
                started_at INTEGER NOT NULL, \
                ended_at INTEGER NULL, \
                PRIMARY KEY (key_digest, revision, processor, processor_revision, attempt))",
        ]
        .join("; "),
    )
    .additive()
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn a_key_digest_is_equal_across_two_processes() {
        let a = ReceiptKey::new("t", "ns", "k").unwrap();
        let b = ReceiptKey::new("t", "ns", "k").unwrap();
        assert_eq!(a.key_digest(), b.key_digest());
        assert_eq!(a.key_digest().len(), 64);
    }

    #[test]
    fn different_parts_never_collide_through_concatenation() {
        let a = ReceiptKey::new("ab", "c", "d").unwrap();
        let b = ReceiptKey::new("a", "bc", "d").unwrap();
        assert_ne!(a.key_digest(), b.key_digest());
    }

    #[test]
    fn an_empty_tenant_is_refused() {
        let err = ReceiptKey::new("", "ns", "k").unwrap_err();
        assert!(matches!(err, Error::Validation(_)), "{err}");
    }

    #[test]
    fn a_synthetic_key_carries_the_marker_and_is_deterministic() {
        let a = ReceiptKey::synthetic("t", "ns", &[b"uidvalidity:1", b"uid:9"]).unwrap();
        let b = ReceiptKey::synthetic("t", "ns", &[b"uidvalidity:1", b"uid:9"]).unwrap();
        assert!(a.is_synthetic());
        assert_eq!(a.kind(), "synthetic");
        assert_eq!(a.key, b.key);
    }

    #[test]
    fn a_content_digest_validates_its_hex() {
        assert!(ContentDigest::from_hex("z".repeat(64)).is_err());
        assert!(ContentDigest::from_hex("A".repeat(64)).is_err());
        assert!(ContentDigest::from_hex("a".repeat(64)).is_ok());
    }

    #[test]
    fn no_statement_this_module_issues_evaluates_a_clock_or_random_function() {
        // FR-007: every statement this module can build is scanned for the
        // SQL functions hiqlite's replication forbids.
        let forbidden = [
            "unixepoch(",
            "current_timestamp",
            "datetime(",
            "random(",
            "julianday(",
            "strftime(",
        ];
        let mut txn = TxnBuilder::new();
        let key = ReceiptKey::new("t", "ns", "k").unwrap();
        let digest = ContentDigest::of(b"body");
        let meta = ReceiptMeta::default();
        let now = UnixSeconds::new(1);
        Receipts::stage_first(&mut txn, &key, &digest, &meta, now).unwrap();
        let changed = Changed {
            head_revision: 1,
            head_digest: digest.as_str().to_owned(),
        };
        Receipts::stage_revision(&mut txn, &key, &digest, &changed, &meta, now).unwrap();
        Receipts::stage_collision(&mut txn, &key, &digest, &changed, &meta, now).unwrap();
        Receipts::stage_seen(&mut txn, &key, 1, now);
        Receipts::stage_erasure(&mut txn, &EraseScope::Identity(key), now);
        for statement in txn.statements() {
            let lower = statement.sql.to_ascii_lowercase();
            for needle in forbidden {
                assert!(!lower.contains(needle), "{needle} in {}", statement.sql);
            }
        }
        for statement in receipt_migration(1).sql.split(';') {
            let lower = statement.to_ascii_lowercase();
            for needle in forbidden {
                assert!(!lower.contains(needle), "{needle} in {statement}");
            }
        }
    }
}
