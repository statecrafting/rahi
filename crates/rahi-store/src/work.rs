//! Work claims: reservation, retry, and the dead letter over a processing
//! identity (spec 045 3.4, 3.5).
//!
//! A processing identity is a receipt revision plus a processor and its
//! policy revision (spec 045 B-11): a correction or a new policy is new
//! work. [`Work::reserve`] and [`Work::next`] take the queue's
//! [`crate::Lease`] and write a fenced claim row, following 012 D-10's
//! recommendation literally (spec 045 D-5): the claim is a chassis row with
//! `(key, holder, fence, expires_at)`, minted and renewed under a
//! short-held lease. [`Work::guard`] carries the same fencing forward to
//! [`Work::complete`] and [`Work::fail`], which stage into the caller's own
//! [`TxnBuilder`] beside its domain writes, using 012 D-3's `NOT NULL`
//! technique so a superseded claim aborts the whole batch.

use std::time::Duration;

use rahi_types::{Error, FenceToken, UnixSeconds};
use serde::Deserialize;

use crate::outbox::TxnBuilder;
use crate::query::{Page, Value};
use crate::receipt::ReceiptKey;
use crate::store::StoreHandle;
use crate::txn::Statement;

const MAX_DETAIL_BYTES: usize = 2048;

/// The identity of one processing attempt over a receipt revision (spec 045
/// B-11).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessingKey {
    /// The receipt this work is over.
    pub receipt: ReceiptKey,
    /// The receipt revision this work is over.
    pub revision: u32,
    /// The stage name (`extract.itinerary`).
    pub processor: String,
    /// The policy, model, or code version whose output this stage writes.
    pub processor_revision: String,
}

impl ProcessingKey {
    /// Build a processing key.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when `processor` or `processor_revision` is
    /// empty.
    pub fn new(
        receipt: ReceiptKey,
        revision: u32,
        processor: impl Into<String>,
        processor_revision: impl Into<String>,
    ) -> Result<Self, Error> {
        let processor = processor.into();
        let processor_revision = processor_revision.into();
        if processor.is_empty() {
            return Err(Error::Validation(
                "a processing key's processor must not be empty".to_owned(),
            ));
        }
        if processor_revision.is_empty() {
            return Err(Error::Validation(
                "a processing key's processor_revision must not be empty".to_owned(),
            ));
        }
        Ok(Self {
            receipt,
            revision,
            processor,
            processor_revision,
        })
    }

    fn key_digest(&self) -> String {
        self.receipt.key_digest()
    }

    /// The lease key that guards every reservation and renewal on this
    /// identity's queue (spec 045 B-14): one key per `(namespace,
    /// processor)`, not per item.
    fn queue_lease_key(&self) -> String {
        format!("rahi.work/{}/{}", self.receipt.namespace, self.processor)
    }
}

/// A held reservation (spec 045 B-14).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claim {
    /// The processing identity this claim reserves.
    pub key: ProcessingKey,
    /// The row's own fence, minted from the queue's lease token. Every later
    /// write under this claim carries it (spec 045 B-15, I-5).
    pub token: FenceToken,
    /// This attempt's number, counted from one.
    pub attempt: u32,
    /// When the claim expires unless renewed.
    pub expires_at: UnixSeconds,
}

/// Bounded exponential retry with a cap and id-seeded jitter (spec 045
/// B-16), the technique 013 D-9 uses for the ledger's own append retry.
#[derive(Clone, Copy, Debug)]
pub struct RetryPolicy {
    /// Attempts at or above this count are dead, not retried.
    pub max_attempts: u32,
    /// The first window's width.
    pub base: Duration,
    /// The window never grows past this.
    pub cap: Duration,
}

impl RetryPolicy {
    /// The wait before `attempt` (counted from one), a point in the upper
    /// half of a window that doubles from `base` to `cap`, picked by
    /// hashing `key_digest` with `attempt`. Nothing here reads a random
    /// source: the same attempt waits the same way every time.
    #[must_use]
    pub fn backoff(&self, key_digest: &str, attempt: u32) -> Duration {
        let doublings = attempt.saturating_sub(1).min(16);
        let window = self.base.saturating_mul(1_u32 << doublings).min(self.cap);
        let half = window / 2;
        let spread = u64::try_from(half.as_micros()).unwrap_or(u64::MAX).max(1);
        half + Duration::from_micros(fnv1a(key_digest, attempt) % spread)
    }
}

/// FNV-1a over `text` and `salt`: a spread, not a digest.
fn fnv1a(text: &str, salt: u32) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes().chain(salt.to_le_bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// A failed attempt's error, staged by [`Work::fail`].
#[derive(Clone, Debug, Default)]
pub struct FailureDetail {
    /// A short error class (`timeout`, `provider_5xx`).
    pub class: String,
    /// A bounded detail, at most 2 KiB, never the item's body.
    pub detail: Option<String>,
}

impl FailureDetail {
    fn check(&self) -> Result<(), Error> {
        if let Some(detail) = &self.detail
            && detail.len() > MAX_DETAIL_BYTES
        {
            return Err(Error::Validation(format!(
                "a failure detail must be at most {MAX_DETAIL_BYTES} bytes, got {}",
                detail.len()
            )));
        }
        Ok(())
    }
}

/// Counts of dead and pending work, per processor (spec 045 B-17).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QueueCounts {
    /// The processor these counts are for.
    pub processor: String,
    /// Rows waiting to be reserved.
    pub pending: u64,
    /// Rows currently held.
    pub claimed: u64,
    /// Rows waiting on a retry.
    pub failed: u64,
    /// Rows that exhausted their retry budget.
    pub dead: u64,
}

/// One recorded attempt (spec 045 B-16, B-17).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttemptRecord {
    /// The attempt number, counted from one.
    pub attempt: u32,
    /// `open`, `done`, `expired`, `failed`, `dead`, or `requeued`.
    pub outcome: String,
    /// The error class [`Work::fail`] recorded, if any.
    pub error_class: Option<String>,
    /// The bounded detail [`Work::fail`] recorded, if any.
    pub detail: Option<String>,
}

/// A dead row and its attempt history (spec 045 B-17).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeadItem {
    /// The processing identity.
    pub key: ProcessingKey,
    /// How many attempts this identity has made.
    pub attempt: u32,
    /// Every recorded attempt, oldest first.
    pub attempts: Vec<AttemptRecord>,
}

/// What [`Work::dead`] filters on.
#[derive(Clone, Debug, Default)]
pub struct DeadFilter {
    /// Only this namespace, or every namespace.
    pub namespace: Option<String>,
    /// Only this processor, or every processor.
    pub processor: Option<String>,
}

/// What one [`Work::sweep`] did (spec 045 B-19).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Open attempts closed as `expired`.
    pub expired: u64,
    /// Rows promoted from `failed` (budget spent) to `dead`.
    pub dead: u64,
    /// Receipts retention compacted to a tombstone.
    pub compacted: u64,
    /// Tombstones deleted past `tombstone_until`.
    pub tombstones_deleted: u64,
}

#[derive(Debug, Deserialize)]
struct CandidateRow {
    key_digest: String,
    revision: i64,
    processor: String,
    processor_revision: String,
    tenant: String,
    namespace: String,
    key: String,
    attempt: i64,
}

const SELECT_ELIGIBLE_BY_IDENTITY: &str = "SELECT key_digest, revision, processor, \
     processor_revision, tenant, namespace, key, attempt FROM rahi_processing \
     WHERE key_digest = $1 AND revision = $2 AND processor = $3 AND processor_revision = $4 \
     AND (state = 'pending' \
          OR (state = 'claimed' AND expires_at <= $5) \
          OR (state = 'failed' AND next_attempt_at <= $5))";

const SELECT_ELIGIBLE_BY_QUEUE: &str = "SELECT key_digest, revision, processor, \
     processor_revision, tenant, namespace, key, attempt FROM rahi_processing \
     WHERE namespace = $1 AND processor = $2 \
     AND (state = 'pending' \
          OR (state = 'claimed' AND expires_at <= $3) \
          OR (state = 'failed' AND next_attempt_at <= $3)) \
     ORDER BY created_at ASC LIMIT $4";

const CLAIM_ROW: &str = "UPDATE rahi_processing SET state = 'claimed', holder = $1, \
     expires_at = $2, attempt = $3 \
     WHERE key_digest = $4 AND revision = $5 AND processor = $6 AND processor_revision = $7 \
     AND attempt = $8";

const OPEN_ATTEMPT: &str = "INSERT INTO rahi_processing_attempt \
     (key_digest, revision, processor, processor_revision, attempt, holder, outcome, \
      started_at) \
     VALUES ($1, $2, $3, $4, $5, $6, 'open', $7)";

/// spec 045 B-15: aborts the caller's whole batch (012 D-3's `NOT NULL`
/// technique) unless the row still carries this claim's fence, is still
/// `claimed`, and has not yet expired.
const GUARD_CLAIM: &str = "UPDATE rahi_processing SET \
     fence = CASE WHEN fence = $1 AND state = 'claimed' AND expires_at > $2 THEN fence \
                  ELSE NULL END \
     WHERE key_digest = $3 AND revision = $4 AND processor = $5 AND processor_revision = $6";

const COMPLETE_ROW: &str = "UPDATE rahi_processing SET state = 'done' \
     WHERE key_digest = $1 AND revision = $2 AND processor = $3 AND processor_revision = $4";

const CLOSE_ATTEMPT_DONE: &str = "UPDATE rahi_processing_attempt SET outcome = 'done', \
     ended_at = $1 \
     WHERE key_digest = $2 AND revision = $3 AND processor = $4 AND processor_revision = $5 \
     AND attempt = $6";

const FAIL_ROW: &str = "UPDATE rahi_processing SET state = 'failed', next_attempt_at = $1 \
     WHERE key_digest = $2 AND revision = $3 AND processor = $4 AND processor_revision = $5";

const DEAD_ROW: &str = "UPDATE rahi_processing SET state = 'dead', next_attempt_at = NULL \
     WHERE key_digest = $1 AND revision = $2 AND processor = $3 AND processor_revision = $4";

const CLOSE_ATTEMPT_FAILED: &str = "UPDATE rahi_processing_attempt SET outcome = $1, \
     error_class = $2, detail = $3, ended_at = $4 \
     WHERE key_digest = $5 AND revision = $6 AND processor = $7 AND processor_revision = $8 \
     AND attempt = $9";

const RENEW_ROW: &str = "UPDATE rahi_processing SET expires_at = $1 \
     WHERE key_digest = $2 AND revision = $3 AND processor = $4 AND processor_revision = $5 \
     AND fence = $6 AND state = 'claimed' AND expires_at > $7";

const REQUEUE_ROW: &str = "UPDATE rahi_processing SET state = 'pending', next_attempt_at = NULL \
     WHERE key_digest = $1 AND revision = $2 AND processor = $3 AND processor_revision = $4 \
     AND state = 'dead'";

const REQUEUE_ATTEMPT: &str = "INSERT INTO rahi_processing_attempt \
     (key_digest, revision, processor, processor_revision, attempt, holder, outcome, \
      started_at, ended_at) \
     SELECT key_digest, revision, processor, processor_revision, attempt, holder, \
            'requeued', $1, $1 \
     FROM rahi_processing \
     WHERE key_digest = $2 AND revision = $3 AND processor = $4 AND processor_revision = $5";

/// Reservation, retry, the dead letter, and the sweep over processing rows.
#[derive(Clone, Copy, Debug)]
pub struct Work;

impl Work {
    /// Enqueue `key`, or do nothing when that identity already has a row
    /// (spec 045 B-12): enqueuing is idempotent.
    pub fn stage_work(txn: &mut TxnBuilder, key: &ProcessingKey, now: UnixSeconds) {
        txn.push(Statement::with_params(
            "INSERT INTO rahi_processing \
             (key_digest, revision, processor, processor_revision, tenant, namespace, key, \
              state, attempt, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, 'pending', 0, $8) \
             ON CONFLICT (key_digest, revision, processor, processor_revision) DO NOTHING",
            vec![
                Value::from(key.key_digest()),
                Value::Integer(i64::from(key.revision)),
                Value::from(key.processor.as_str()),
                Value::from(key.processor_revision.as_str()),
                Value::from(key.receipt.tenant.as_str()),
                Value::from(key.receipt.namespace.as_str()),
                Value::from(key.receipt.key.as_str()),
                Value::Integer(unix_to_sql(now)),
            ],
        ));
    }

    /// Reserve exactly `key`, if it is eligible (spec 045 B-14).
    ///
    /// Takes the queue's lease, reads the row's current state and attempt
    /// count with `query_consistent`, and, if eligible, writes the claim
    /// through `fenced_txn`; the attempt row is then opened through
    /// `StoreHandle::txn`, stamped with the lease's token, following the
    /// pattern [`crate::Statement::fenced`]'s own documentation states for
    /// an insert under a lease (spec 045 D-17).
    ///
    /// # Errors
    ///
    /// The store's error when the lease, the read, or either write fails.
    pub async fn reserve(
        store: &StoreHandle,
        key: &ProcessingKey,
        holder: &str,
        hold_for: Duration,
        now: UnixSeconds,
    ) -> Result<Option<Claim>, Error> {
        let lease = store.lease(&key.queue_lease_key()).await?;
        let candidates: Vec<CandidateRow> = store
            .query_consistent(
                SELECT_ELIGIBLE_BY_IDENTITY,
                vec![
                    Value::from(key.key_digest()),
                    Value::Integer(i64::from(key.revision)),
                    Value::from(key.processor.as_str()),
                    Value::from(key.processor_revision.as_str()),
                    Value::Integer(unix_to_sql(now)),
                ],
            )
            .await?;
        let Some(candidate) = candidates.into_iter().next() else {
            lease.release().await;
            return Ok(None);
        };
        let claim = claim_candidate(store, &lease, &candidate, holder, hold_for, now).await?;
        lease.release().await;
        Ok(Some(claim))
    }

    /// Reserve up to `limit` of the oldest eligible rows of one queue under
    /// one lease acquisition (spec 045 B-14, B-14a).
    ///
    /// # Errors
    ///
    /// The store's error when the lease, the read, or a write fails.
    pub async fn next(
        store: &StoreHandle,
        namespace: &str,
        processor: &str,
        holder: &str,
        hold_for: Duration,
        now: UnixSeconds,
        limit: u32,
    ) -> Result<Vec<Claim>, Error> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let lease_key = format!("rahi.work/{namespace}/{processor}");
        let lease = store.lease(&lease_key).await?;
        let candidates: Vec<CandidateRow> = store
            .query_consistent(
                SELECT_ELIGIBLE_BY_QUEUE,
                vec![
                    Value::from(namespace),
                    Value::from(processor),
                    Value::Integer(unix_to_sql(now)),
                    Value::Integer(i64::from(limit)),
                ],
            )
            .await?;
        let mut claims = Vec::with_capacity(candidates.len());
        for candidate in &candidates {
            if let Ok(claim) =
                claim_candidate(store, &lease, candidate, holder, hold_for, now).await
            {
                claims.push(claim);
            }
        }
        lease.release().await;
        Ok(claims)
    }

    /// Prefix `txn` with the guard that aborts the whole batch unless `claim`
    /// still holds the row (spec 045 B-15, 012 D-3).
    pub fn guard(txn: &mut TxnBuilder, claim: &Claim, now: UnixSeconds) {
        txn.push(Statement::with_params(
            GUARD_CLAIM,
            vec![
                Value::Integer(fence_to_sql(claim.token)),
                Value::Integer(unix_to_sql(now)),
                Value::from(claim.key.key_digest()),
                Value::Integer(i64::from(claim.key.revision)),
                Value::from(claim.key.processor.as_str()),
                Value::from(claim.key.processor_revision.as_str()),
            ],
        ));
    }

    /// Stage the guard, the move to `done`, and the attempt's close (spec
    /// 045 B-15). `done` is terminal (B-13): a zombie holder whose claim was
    /// reclaimed commits nothing (I-4, I-5).
    pub fn complete(txn: &mut TxnBuilder, claim: &Claim, now: UnixSeconds) {
        Self::guard(txn, claim, now);
        let key_digest = claim.key.key_digest();
        txn.push(Statement::with_params(
            COMPLETE_ROW,
            vec![
                Value::from(key_digest.clone()),
                Value::Integer(i64::from(claim.key.revision)),
                Value::from(claim.key.processor.as_str()),
                Value::from(claim.key.processor_revision.as_str()),
            ],
        ));
        txn.push(Statement::with_params(
            CLOSE_ATTEMPT_DONE,
            vec![
                Value::Integer(unix_to_sql(now)),
                Value::from(key_digest),
                Value::Integer(i64::from(claim.key.revision)),
                Value::from(claim.key.processor.as_str()),
                Value::from(claim.key.processor_revision.as_str()),
                Value::Integer(i64::from(claim.attempt)),
            ],
        ));
    }

    /// Stage the guard, the move to `failed` or `dead`, and the attempt's
    /// close (spec 045 B-16). At `policy.max_attempts` the row moves
    /// straight to `dead`; below it, `next_attempt_at` is the bounded,
    /// jittered backoff.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when `error.detail` exceeds 2 KiB.
    pub fn fail(
        txn: &mut TxnBuilder,
        claim: &Claim,
        error: &FailureDetail,
        policy: &RetryPolicy,
        now: UnixSeconds,
    ) -> Result<(), Error> {
        error.check()?;
        Self::guard(txn, claim, now);
        let key_digest = claim.key.key_digest();
        let dead = claim.attempt >= policy.max_attempts;
        if dead {
            txn.push(Statement::with_params(
                DEAD_ROW,
                vec![
                    Value::from(key_digest.clone()),
                    Value::Integer(i64::from(claim.key.revision)),
                    Value::from(claim.key.processor.as_str()),
                    Value::from(claim.key.processor_revision.as_str()),
                ],
            ));
        } else {
            let wait = policy.backoff(&key_digest, claim.attempt);
            let next_attempt_at = now.get().saturating_add(wait.as_secs());
            txn.push(Statement::with_params(
                FAIL_ROW,
                vec![
                    Value::Integer(unix_to_sql(UnixSeconds::new(next_attempt_at))),
                    Value::from(key_digest.clone()),
                    Value::Integer(i64::from(claim.key.revision)),
                    Value::from(claim.key.processor.as_str()),
                    Value::from(claim.key.processor_revision.as_str()),
                ],
            ));
        }
        txn.push(Statement::with_params(
            CLOSE_ATTEMPT_FAILED,
            vec![
                Value::from(if dead { "dead" } else { "failed" }),
                Value::from(error.class.as_str()),
                Value::from(error.detail.clone()),
                Value::Integer(unix_to_sql(now)),
                Value::from(key_digest),
                Value::Integer(i64::from(claim.key.revision)),
                Value::from(claim.key.processor.as_str()),
                Value::from(claim.key.processor_revision.as_str()),
                Value::Integer(i64::from(claim.attempt)),
            ],
        ));
        Ok(())
    }

    /// Renew `claim`'s hold under the queue's lease (spec 045 B-15, 012
    /// D-10's renewal of the application row). The row's fence advances to
    /// the fresh lease token, so the returned claim carries it forward.
    ///
    /// # Errors
    ///
    /// [`Error::Conflict`] when the claim is no longer held (reclaimed,
    /// completed, or dead-lettered); the store's error otherwise.
    pub async fn renew(
        store: &StoreHandle,
        claim: &Claim,
        hold_for: Duration,
        now: UnixSeconds,
    ) -> Result<Claim, Error> {
        let lease = store.lease(&claim.key.queue_lease_key()).await?;
        let new_expires = UnixSeconds::new(now.get().saturating_add(hold_for.as_secs()));
        let statement = Statement::with_params(
            RENEW_ROW,
            vec![
                Value::Integer(unix_to_sql(new_expires)),
                Value::from(claim.key.key_digest()),
                Value::Integer(i64::from(claim.key.revision)),
                Value::from(claim.key.processor.as_str()),
                Value::from(claim.key.processor_revision.as_str()),
                Value::Integer(fence_to_sql(claim.token)),
                Value::Integer(unix_to_sql(now)),
            ],
        );
        let results = store.fenced_txn(&lease, vec![statement]).await?;
        let affected = results.first().map(|r| r.rows_affected).unwrap_or(0);
        if affected != 1 {
            lease.release().await;
            return Err(Error::Conflict(format!(
                "claim on {} attempt {} is no longer held: it was reclaimed, completed, or \
                 dead-lettered",
                claim.key.key_digest(),
                claim.attempt
            )));
        }
        let renewed = Claim {
            key: claim.key.clone(),
            token: lease.token,
            attempt: claim.attempt,
            expires_at: new_expires,
        };
        lease.release().await;
        Ok(renewed)
    }

    /// List dead rows with their attempt history (spec 045 B-17).
    ///
    /// # Errors
    ///
    /// The store's error when the read fails.
    pub async fn dead(
        store: &StoreHandle,
        filter: &DeadFilter,
        page: Page,
    ) -> Result<Vec<DeadItem>, Error> {
        let mut sql = "SELECT key_digest, revision, processor, processor_revision, tenant, \
             namespace, key, attempt FROM rahi_processing WHERE state = 'dead'"
            .to_owned();
        let mut params = Vec::new();
        if let Some(namespace) = &filter.namespace {
            params.push(Value::from(namespace.as_str()));
            sql.push_str(&format!(" AND namespace = ${}", params.len()));
        }
        if let Some(processor) = &filter.processor {
            params.push(Value::from(processor.as_str()));
            sql.push_str(&format!(" AND processor = ${}", params.len()));
        }
        sql.push_str(" ORDER BY created_at ASC");
        let rows: Vec<CandidateRow> = store.query_paged(&sql, params, page).await?;
        let mut items = Vec::with_capacity(rows.len());
        for row in rows {
            let key = candidate_key(&row)?;
            let attempts: Vec<AttemptRow> = store
                .query(
                    "SELECT attempt, outcome, error_class, detail FROM rahi_processing_attempt \
                     WHERE key_digest = $1 AND revision = $2 AND processor = $3 \
                     AND processor_revision = $4 ORDER BY attempt ASC",
                    vec![
                        Value::from(row.key_digest.clone()),
                        Value::Integer(row.revision),
                        Value::from(row.processor.clone()),
                        Value::from(row.processor_revision.clone()),
                    ],
                )
                .await?;
            items.push(DeadItem {
                attempt: to_attempt(row.attempt, &row.key_digest)?,
                key,
                attempts: attempts
                    .into_iter()
                    .map(|a| {
                        Ok(AttemptRecord {
                            attempt: to_attempt(a.attempt, &row.key_digest)?,
                            outcome: a.outcome,
                            error_class: a.error_class,
                            detail: a.detail,
                        })
                    })
                    .collect::<Result<Vec<_>, Error>>()?,
            });
        }
        Ok(items)
    }

    /// Stage moving a dead row back to `pending`, its attempt count kept and
    /// a `requeued` attempt recorded (spec 045 B-17).
    pub fn requeue(txn: &mut TxnBuilder, key: &ProcessingKey, now: UnixSeconds) {
        let key_digest = key.key_digest();
        txn.push(Statement::with_params(
            REQUEUE_ATTEMPT,
            vec![
                Value::Integer(unix_to_sql(now)),
                Value::from(key_digest.clone()),
                Value::Integer(i64::from(key.revision)),
                Value::from(key.processor.as_str()),
                Value::from(key.processor_revision.as_str()),
            ],
        ));
        txn.push(Statement::with_params(
            REQUEUE_ROW,
            vec![
                Value::from(key_digest),
                Value::Integer(i64::from(key.revision)),
                Value::from(key.processor.as_str()),
                Value::from(key.processor_revision.as_str()),
            ],
        ));
    }

    /// The number of pending, claimed, failed, and dead rows per processor,
    /// aggregated over every tenant and namespace (spec 045 B-17).
    ///
    /// # Errors
    ///
    /// The store's error when the read fails.
    pub async fn counts(store: &StoreHandle) -> Result<Vec<QueueCounts>, Error> {
        let rows: Vec<CountRow> = store
            .query(
                "SELECT processor, state, COUNT(*) AS n FROM rahi_processing \
                 WHERE state IN ('pending', 'claimed', 'failed', 'dead') \
                 GROUP BY processor, state",
                vec![],
            )
            .await?;
        let mut by_processor: std::collections::BTreeMap<String, QueueCounts> =
            std::collections::BTreeMap::new();
        for row in rows {
            let entry = by_processor
                .entry(row.processor.clone())
                .or_insert_with(|| QueueCounts {
                    processor: row.processor.clone(),
                    ..QueueCounts::default()
                });
            let n = u64::try_from(row.n).unwrap_or(0);
            match row.state.as_str() {
                "pending" => entry.pending = n,
                "claimed" => entry.claimed = n,
                "failed" => entry.failed = n,
                "dead" => entry.dead = n,
                _ => {}
            }
        }
        Ok(by_processor.into_values().collect())
    }

    /// Close expired claims, promote spent failures to `dead`, and compact
    /// or delete retention-expired receipts, in chunks under the sweep
    /// lease (spec 045 B-19).
    ///
    /// `policy` decides which claimed rows have spent their retry budget:
    /// B-16 fixes that decision for an explicit [`Work::fail`], and this is
    /// the same rule applied to a chain of pure expiries that never called
    /// it (spec 045 D-18).
    ///
    /// The lease serialises concurrent sweepers; the writes themselves run
    /// through a plain `txn`, not `fenced_txn`'s automatic per-statement
    /// rewrite, because most of the rows this sweep touches carry no
    /// `fence` column at all, and the one table that does (`rahi_processing`)
    /// stamps it from a queue lease with its own, unrelated token sequence
    /// (spec 045 D-19).
    ///
    /// # Errors
    ///
    /// The store's error when the lease or a write fails.
    pub async fn sweep(
        store: &StoreHandle,
        namespace: &str,
        policy: &RetryPolicy,
        now: UnixSeconds,
        limit: u32,
    ) -> Result<SweepReport, Error> {
        let lease = store.lease(&format!("rahi.work.sweep/{namespace}")).await?;
        let mut report = SweepReport::default();

        let expired: Vec<CandidateRow> = store
            .query_paged(
                "SELECT key_digest, revision, processor, processor_revision, tenant, \
                 namespace, key, attempt FROM rahi_processing \
                 WHERE namespace = $1 AND state = 'claimed' AND expires_at <= $2",
                vec![Value::from(namespace), Value::Integer(unix_to_sql(now))],
                Page::new(limit.max(1)),
            )
            .await?;
        for row in &expired {
            let attempt = to_attempt(row.attempt, &row.key_digest)?;
            if attempt >= policy.max_attempts {
                store
                    .txn(vec![Statement::with_params(
                        "UPDATE rahi_processing SET state = 'dead', next_attempt_at = NULL \
                         WHERE key_digest = $1 AND revision = $2 AND processor = $3 \
                         AND processor_revision = $4",
                        vec![
                            Value::from(row.key_digest.clone()),
                            Value::Integer(row.revision),
                            Value::from(row.processor.clone()),
                            Value::from(row.processor_revision.clone()),
                        ],
                    )])
                    .await?;
                report.dead += 1;
            }
            let closed = store
                .execute(
                    "UPDATE rahi_processing_attempt SET outcome = 'expired', ended_at = $1 \
                     WHERE key_digest = $2 AND revision = $3 AND processor = $4 \
                     AND processor_revision = $5 AND attempt = $6 AND outcome = 'open'",
                    vec![
                        Value::Integer(unix_to_sql(now)),
                        Value::from(row.key_digest.clone()),
                        Value::Integer(row.revision),
                        Value::from(row.processor.clone()),
                        Value::from(row.processor_revision.clone()),
                        Value::Integer(row.attempt),
                    ],
                )
                .await?;
            report.expired += closed.rows_affected;
        }

        let spent: Vec<KeyDigestRow> = store
            .query_paged(
                "SELECT key_digest, revision, processor, processor_revision FROM rahi_processing \
                 WHERE namespace = $1 AND state = 'failed' AND next_attempt_at IS NULL",
                vec![Value::from(namespace)],
                Page::new(limit.max(1)),
            )
            .await?;
        for row in &spent {
            store
                .txn(vec![Statement::with_params(
                    DEAD_ROW,
                    vec![
                        Value::from(row.key_digest.clone()),
                        Value::Integer(row.revision),
                        Value::from(row.processor.clone()),
                        Value::from(row.processor_revision.clone()),
                    ],
                )])
                .await?;
            report.dead += 1;
        }

        let compactable: Vec<HeadKeyRow> = store
            .query_paged(
                "SELECT key_digest FROM rahi_receipt_head \
                 WHERE tombstoned = 0 AND erased = 0 AND retain_until IS NOT NULL \
                 AND retain_until <= $1 \
                 AND NOT EXISTS (SELECT 1 FROM rahi_processing p \
                                 WHERE p.key_digest = rahi_receipt_head.key_digest \
                                 AND p.state NOT IN ('done', 'dead'))",
                vec![Value::Integer(unix_to_sql(now))],
                Page::new(limit.max(1)),
            )
            .await?;
        for row in &compactable {
            store
                .txn(vec![
                    Statement::with_params(
                        "DELETE FROM rahi_processing_attempt WHERE key_digest = $1",
                        vec![Value::from(row.key_digest.clone())],
                    ),
                    Statement::with_params(
                        "DELETE FROM rahi_processing WHERE key_digest = $1",
                        vec![Value::from(row.key_digest.clone())],
                    ),
                    Statement::with_params(
                        "UPDATE rahi_receipt SET outcome = NULL, seen_count = NULL, \
                         last_seen_at = NULL WHERE key_digest = $1",
                        vec![Value::from(row.key_digest.clone())],
                    ),
                    Statement::with_params(
                        "UPDATE rahi_receipt_head SET key = NULL, tombstoned = 1 \
                         WHERE key_digest = $1",
                        vec![Value::from(row.key_digest.clone())],
                    ),
                ])
                .await?;
            report.compacted += 1;
        }

        let expired_tombstones: Vec<HeadKeyRow> = store
            .query_paged(
                "SELECT key_digest FROM rahi_receipt_head \
                 WHERE tombstoned = 1 AND tombstone_until IS NOT NULL AND tombstone_until <= $1",
                vec![Value::Integer(unix_to_sql(now))],
                Page::new(limit.max(1)),
            )
            .await?;
        for row in &expired_tombstones {
            store
                .txn(vec![
                    Statement::with_params(
                        "DELETE FROM rahi_receipt WHERE key_digest = $1",
                        vec![Value::from(row.key_digest.clone())],
                    ),
                    Statement::with_params(
                        "DELETE FROM rahi_receipt_head WHERE key_digest = $1",
                        vec![Value::from(row.key_digest.clone())],
                    ),
                ])
                .await?;
            report.tombstones_deleted += 1;
        }

        lease.release().await;
        Ok(report)
    }
}

#[derive(Debug, Deserialize)]
struct AttemptRow {
    attempt: i64,
    outcome: String,
    error_class: Option<String>,
    detail: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CountRow {
    processor: String,
    state: String,
    n: i64,
}

#[derive(Debug, Deserialize)]
struct KeyDigestRow {
    key_digest: String,
    revision: i64,
    processor: String,
    processor_revision: String,
}

#[derive(Debug, Deserialize)]
struct HeadKeyRow {
    key_digest: String,
}

/// Claim one already-fenced UPDATE's candidate, then open its attempt row.
///
/// The `UPDATE` runs through `fenced_txn`, so its fence is minted from the
/// lease's own token (spec 045 D-17); the attempt insert follows through a
/// plain `txn`, stamped with the same token, as [`crate::Statement::fenced`]
/// documents for an insert under a lease.
async fn claim_candidate(
    store: &StoreHandle,
    lease: &crate::lock::Lease,
    candidate: &CandidateRow,
    holder: &str,
    hold_for: Duration,
    now: UnixSeconds,
) -> Result<Claim, Error> {
    let old_attempt = candidate.attempt;
    let new_attempt = old_attempt.saturating_add(1);
    let expires_at = UnixSeconds::new(now.get().saturating_add(hold_for.as_secs()));
    let claim_stmt = Statement::with_params(
        CLAIM_ROW,
        vec![
            Value::from(holder),
            Value::Integer(unix_to_sql(expires_at)),
            Value::Integer(new_attempt),
            Value::from(candidate.key_digest.clone()),
            Value::Integer(candidate.revision),
            Value::from(candidate.processor.clone()),
            Value::from(candidate.processor_revision.clone()),
            Value::Integer(old_attempt),
        ],
    );
    let results = store.fenced_txn(lease, vec![claim_stmt]).await?;
    let affected = results.first().map(|r| r.rows_affected).unwrap_or(0);
    if affected != 1 {
        return Err(Error::Conflict(format!(
            "processing row {} revision {} raced away before it could be claimed",
            candidate.key_digest, candidate.revision
        )));
    }
    store
        .txn(vec![Statement::with_params(
            OPEN_ATTEMPT,
            vec![
                Value::from(candidate.key_digest.clone()),
                Value::Integer(candidate.revision),
                Value::from(candidate.processor.clone()),
                Value::from(candidate.processor_revision.clone()),
                Value::Integer(new_attempt),
                Value::from(holder),
                Value::Integer(unix_to_sql(now)),
            ],
        )])
        .await?;
    Ok(Claim {
        key: candidate_key(candidate)?,
        token: lease.token,
        attempt: to_attempt(new_attempt, &candidate.key_digest)?,
        expires_at,
    })
}

fn candidate_key(row: &CandidateRow) -> Result<ProcessingKey, Error> {
    let receipt = ReceiptKey::new(row.tenant.clone(), row.namespace.clone(), row.key.clone())?;
    let revision = u32::try_from(row.revision).map_err(|_| {
        Error::Integrity(format!(
            "processing row {} holds an out-of-range revision {}",
            row.key_digest, row.revision
        ))
    })?;
    ProcessingKey::new(
        receipt,
        revision,
        row.processor.clone(),
        row.processor_revision.clone(),
    )
}

fn to_attempt(raw: i64, key_digest: &str) -> Result<u32, Error> {
    u32::try_from(raw).map_err(|_| {
        Error::Integrity(format!(
            "processing row {key_digest} holds an out-of-range attempt {raw}"
        ))
    })
}

fn fence_to_sql(token: FenceToken) -> i64 {
    i64::try_from(token.get()).unwrap_or(i64::MAX)
}

fn unix_to_sql(value: UnixSeconds) -> i64 {
    i64::try_from(value.get()).unwrap_or(i64::MAX)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn key() -> ProcessingKey {
        let receipt = ReceiptKey::new("t", "ns", "k").unwrap();
        ProcessingKey::new(receipt, 1, "extract", "v1").unwrap()
    }

    #[test]
    fn an_empty_processor_is_refused() {
        let receipt = ReceiptKey::new("t", "ns", "k").unwrap();
        let err = ProcessingKey::new(receipt, 1, "", "v1").unwrap_err();
        assert!(matches!(err, Error::Validation(_)), "{err}");
    }

    #[test]
    fn backoff_sits_in_the_upper_half_of_a_window_that_doubles_to_the_cap_and_is_deterministic() {
        let policy = RetryPolicy {
            max_attempts: 8,
            base: Duration::from_millis(100),
            cap: Duration::from_secs(30),
        };
        for attempt in 1..8 {
            let window = policy
                .base
                .saturating_mul(1_u32 << (attempt - 1).min(16))
                .min(policy.cap);
            let wait = policy.backoff("digest-a", attempt);
            assert!(
                wait >= window / 2 && wait < window,
                "{attempt}: {wait:?} of {window:?}"
            );
        }
        assert_eq!(policy.backoff("digest-a", 3), policy.backoff("digest-a", 3));
        assert_ne!(policy.backoff("digest-a", 1), policy.backoff("digest-b", 1));
    }

    #[test]
    fn no_statement_this_module_issues_evaluates_a_clock_or_random_function() {
        let forbidden = [
            "unixepoch(",
            "current_timestamp",
            "datetime(",
            "random(",
            "julianday(",
            "strftime(",
        ];
        let mut txn = TxnBuilder::new();
        let now = UnixSeconds::new(1);
        let key = key();
        Work::stage_work(&mut txn, &key, now);
        let claim = Claim {
            key: key.clone(),
            token: FenceToken::new(1),
            attempt: 1,
            expires_at: now,
        };
        Work::complete(&mut txn, &claim, now);
        let policy = RetryPolicy {
            max_attempts: 3,
            base: Duration::from_millis(1),
            cap: Duration::from_secs(1),
        };
        Work::fail(&mut txn, &claim, &FailureDetail::default(), &policy, now).unwrap();
        Work::requeue(&mut txn, &key, now);
        for statement in [CLAIM_ROW, OPEN_ATTEMPT, RENEW_ROW]
            .into_iter()
            .map(std::borrow::ToOwned::to_owned)
            .chain(txn.statements().iter().map(|s| s.sql.clone()))
        {
            let lower = statement.to_ascii_lowercase();
            for needle in forbidden {
                assert!(!lower.contains(needle), "{needle} in {statement}");
            }
        }
    }
}
