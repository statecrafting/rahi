//! Append: the unique-parent compare-and-swap (spec 013 B-3, constitution XI).
//!
//! An append reads the head, builds and signs a record chained onto it, and
//! inserts. The insert is the compare-and-swap: the unique index on
//! `prev_hash` means the store admits exactly one record per parent, so two
//! appenders racing for the same head produce one winner and one violation,
//! never a fork. There is no lock and no lease, because the constraint is
//! already the arbitration.
//!
//! Losing is ordinary: the loser waits, reloads the head, re-chains the same
//! decision onto it, and tries again. The wait is what makes a retry worth
//! having. Spec 032 runs three replicas with an appender each, and a loser
//! that retried at once met the same two rivals at the same moment: three
//! immediate tries lost 3 and 4 denials of 30 in spec 035's three-process
//! test. A wait that doubles, at a point in its window chosen by the
//! decision id, spreads the rivals apart, so one reaches the head first.
//!
//! Losing [`APPEND_ATTEMPTS`] times in a row is still not ordinary. It means
//! the head is moving faster than an appender can follow or that something
//! is answering the head query wrongly, and spec 013 B-3 makes it
//! [`Error::Integrity`] rather than another attempt, because the ledger's job
//! under doubt is to stop rather than to guess.

use std::time::Duration;

use rahi_types::Error;

use crate::chain::{AppendStage, Ledger, insert_statement};
use crate::identity::{
    Accounted, DecisionCopy, Presence, REINDEX_COMMAND, identity_digest, identity_insert,
};
use crate::record::{Decision, DecisionId, Hash, SignedRecord};

/// How many times an append chains onto the head before it gives up
/// (spec 013 B-3 as amended by D-9: twelve, with a wait before each retry).
pub const APPEND_ATTEMPTS: u32 = 12;

/// The window of the first wait after a lost compare-and-swap. Each later
/// window doubles, up to [`APPEND_BACKOFF_CAP`].
pub const APPEND_BACKOFF_BASE: Duration = Duration::from_millis(5);

/// The widest window a wait is chosen from. Eleven waits stay under two
/// seconds, which is how long an append can spend losing before it stops.
pub const APPEND_BACKOFF_CAP: Duration = Duration::from_millis(250);

/// What a failed insert turned out to mean.
enum Attempt {
    /// The record is in the chain under this hash; the error was after the
    /// fact.
    Landed(Hash),
    /// Another appender took the parent. Reload the head and re-chain.
    Retry,
}

/// Where a decision ended up, and what this invocation knows about how it
/// got there (spec 042 B-14).
///
/// The three fields answer different questions and only the first is a total
/// guarantee.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Landing {
    /// **Durable presence.** After any successful return the decision is in
    /// the chain exactly once and this names it. It holds across lost
    /// acknowledgements, retries, process restarts, and replicas, and it is
    /// the whole of what this crate guarantees to a caller.
    pub hash: Hash,
    /// **Knowledge about this invocation**, not about the decision.
    ///
    /// `true` only when this invocation's own transaction was acknowledged
    /// as committed to this invocation. Every ambiguous commit outcome (a
    /// timeout, a dropped connection, a leader change, a shutdown between
    /// the send and the acknowledgement) returns an error, or a `false`
    /// reached by re-reading; none of them returns `true`.
    ///
    /// So the field is sound and deliberately incomplete: `false` means
    /// only that the decision was already present when this invocation
    /// looked, and an earlier invocation *by the same caller* whose
    /// acknowledgement was lost is one of the ways that happens. There is no
    /// author, no attempt count and no "probably yours" hint, because a lost
    /// acknowledgement destroys the knowledge of who appended permanently
    /// and no interface can return what was never recorded.
    ///
    /// This is exactly-once **append**, never exactly-once **delivery**. A
    /// caller that fires a side effect on `true` will skip that side effect
    /// after a lost acknowledgement, because its retry sees `false`. The
    /// field is a diagnostic and must not be a delivery trigger: a caller
    /// that needs a side effect to happen exactly once records its intent in
    /// its own transaction and drives the effect from that durable row.
    pub appended_now: bool,
    /// The segment holding the decision when it is already archived.
    pub sealed_in: Option<Hash>,
}

impl Ledger {
    /// Append `decision` to the chain and return the hash it landed under.
    ///
    /// The decision's `prev_hash` is set here, not by the caller: it is the
    /// head this append wins, and on a retry it is the new head. Everything
    /// else, including the id, travels unchanged, so a decision that spec 015
    /// has already named in an `Error::Denied` keeps that name however many
    /// attempts it takes.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when the decision has no id; [`Error::Conflict`]
    /// when its id is already in the chain under other content;
    /// [`Error::Integrity`] when [`APPEND_ATTEMPTS`] compare-and-swaps all
    /// lose; the store's own error otherwise. Spec 015's denial path logs the
    /// error with the decision id and never swallows it.
    pub async fn append(&self, decision: Decision) -> Result<Hash, Error> {
        self.append_reported(decision).await.map(|(hash, _)| hash)
    }

    /// Append `decision` exactly once, reporting what this invocation knows
    /// (spec 042 B-14).
    ///
    /// The append itself is unchanged: the same compare-and-swap, the same
    /// identity insert committing with it. What [`Landing`] adds is the
    /// separation between the decision's durable presence, which is total,
    /// and this invocation's knowledge of its own commit, which is not.
    ///
    /// # Errors
    ///
    /// As [`Ledger::append`], and [`Error::Conflict`] on a handle opened by
    /// [`Ledger::open_for_repair`] (B-15).
    pub async fn append_once(&self, decision: Decision) -> Result<Landing, Error> {
        let id = decision.id.clone();
        let (hash, appended_now) = self.append_reported(decision).await?;
        if appended_now {
            // This invocation's own transaction committed, so the record is
            // resident: nothing has had the chance to seal it away.
            return Ok(Landing {
                hash,
                appended_now,
                sealed_in: None,
            });
        }
        let sealed_in = match self.lookup(&id).await? {
            Presence::Sealed { segment_hash, .. } => Some(segment_hash),
            _ => None,
        };
        Ok(Landing {
            hash,
            appended_now,
            sealed_in,
        })
    }

    /// The chain's first record, which neither gate may block
    /// (spec 042 B-10).
    ///
    /// `Ledger::open` writes it only when nothing is resident and nothing is
    /// sealed, a state [`Ledger`]'s own second-genesis refusal establishes
    /// before this is reached. Such a chain has no uncovered segment and no
    /// unaccounted record by construction, so neither B-10's boot gate nor
    /// B-11's backstop has anything to say about it, and a repair handle has
    /// to be able to create the chain it was asked to diagnose.
    ///
    /// # Errors
    ///
    /// As [`Ledger::append`].
    pub(crate) async fn append_genesis(&self, decision: Decision) -> Result<Hash, Error> {
        let digest = identity_digest(&decision)?;
        self.append_loop(decision, &digest).await.map(|(h, _)| h)
    }

    /// [`Ledger::append`], reporting whether this invocation's own
    /// transaction was acknowledged as committed to it (spec 042 B-14).
    async fn append_reported(&self, decision: Decision) -> Result<(Hash, bool), Error> {
        if decision.id.as_str().is_empty() {
            return Err(Error::Validation(
                "a decision needs an id: it is the row's primary key and the name spec 015 \
                 reports back to a denied caller"
                    .to_owned(),
            ));
        }
        // Spec 042 B-15: the refusal is a property of the handle, whatever
        // coverage says, so nothing can forget to check it.
        if self.is_repair() {
            return Err(Error::Conflict(format!(
                "decision {} was offered to a ledger opened for repair, which never appends: \
                 the repair handle exists so that diagnosis, export and `{REINDEX_COMMAND}` \
                 work on a chain whose coverage is incomplete, and normal service opens the \
                 chain the ordinary way",
                decision.id
            )));
        }
        // The digest does not cover the parent (B-2), so it is computable
        // before the head is read and before any record is built.
        let digest = identity_digest(&decision)?;
        // Spec 042 B-11, the backstop: a replica on a binary that does not
        // stamp can append an unstamped record after this node booted, so a
        // verdict this node has seen move to incomplete refuses every id it
        // cannot prove free. The verified retry is still admitted, because
        // refusing the unknown and admitting the verified is what keeps a
        // recovery working through an incident a full refusal would strand.
        // This reads nothing to *establish* uniqueness; it reads only to
        // refuse, and on a covered chain it does not read at all.
        if let Some(landed) = self.backstop(&decision.id, &digest).await? {
            return Ok((landed, false));
        }
        self.append_loop(decision, &digest).await
    }

    /// The compare-and-swap loop itself, with neither gate in front of it.
    async fn append_loop(&self, decision: Decision, digest: &Hash) -> Result<(Hash, bool), Error> {
        let mut decision = decision;
        for attempt in 0..APPEND_ATTEMPTS {
            if attempt > 0 {
                tokio::time::sleep(backoff(&decision.id, attempt)).await;
            }
            let parent = self.head().await?;
            decision.prev_hash = parent.clone();
            let record = SignedRecord::build(&decision, self.signer())?;
            let hash = record.hash()?;
            let accounted = Accounted::of(&record)?;
            debug_assert_eq!(&accounted.identity_digest, digest);

            if let Some(injected) = self.append_seam(AppendStage::BeforeInsert).await {
                return Err(injected);
            }
            // Spec 042 B-3: the record and its identity row commit in one
            // `txn`, which is one Raft log operation inside one SQLite
            // transaction, so the pair is atomic and the identity table's
            // primary key is the arbitration for a duplicate id exactly as
            // the unique parent index is the arbitration for a lost
            // compare-and-swap. Neither statement can land alone.
            let sent = self
                .store()
                .txn(vec![
                    insert_statement(&record)?,
                    identity_insert(&accounted),
                ])
                .await;
            let sent = match self.append_seam(AppendStage::AfterCommit).await {
                // The acknowledgement of a commit that may well have
                // happened, dropped: this invocation did not observe its own
                // commit, so it may not claim one (B-14).
                Some(injected) => Err(injected),
                None => sent,
            };

            match sent {
                Ok(results) => {
                    let landed = results.first().is_some_and(|r| r.rows_affected == 1);
                    let stamped = results.get(1).is_some_and(|r| r.rows_affected == 1);
                    if !landed || !stamped {
                        return Err(Error::Integrity(format!(
                            "the append of decision {} committed without inserting a row",
                            decision.id
                        )));
                    }
                    return Ok((hash, true));
                }
                Err(err) => match self.classify(&decision.id, digest, &parent, err).await? {
                    Attempt::Landed(hash) => return Ok((hash, false)),
                    Attempt::Retry => {}
                },
            }
        }

        Err(Error::Integrity(format!(
            "decision {} lost the compare-and-swap on the chain head {APPEND_ATTEMPTS} times; \
             the ledger stops rather than fork the chain",
            decision.id
        )))
    }

    /// The append backstop of spec 042 B-11.
    ///
    /// Reached only on a handle that opened successfully and then observed
    /// degradation: on a cluster upgraded the way B-13 requires it is never
    /// reached at all, and on a covered chain it issues no read. When the
    /// cached verdict is incomplete it is the chain's inability to prove an
    /// id free that decides:
    ///
    /// - an id with no identity row is refused, because the chain cannot
    ///   prove it is free;
    /// - an id whose row's digest matches is the verified retry of B-4, and
    ///   its stored hash is returned;
    /// - an id whose row's digest differs, or that carries any collision
    ///   row, is refused.
    ///
    /// Only [`Ledger::recheck_coverage`] moves the verdict back to complete,
    /// and `serve` never calls it, so a degraded cell never talks itself
    /// back into confidence.
    async fn backstop(&self, id: &DecisionId, digest: &Hash) -> Result<Option<Hash>, Error> {
        let verdict = self.cached_verdict();
        if verdict.complete {
            return Ok(None);
        }
        let (identity, collisions) = self.identity_rows_of(id).await?;
        if !collisions.is_empty() {
            return Err(ambiguous(id, identity.as_ref(), &collisions));
        }
        let Some(row) = identity else {
            return Err(Error::Conflict(format!(
                "decision {id} cannot be appended: {}, so the chain cannot prove this id is \
                 free, and an id that cannot be proven free is not spent",
                verdict.evidence
            )));
        };
        if &row.identity_digest != digest {
            return Err(reused(id, &row));
        }
        Ok(Some(row.record_hash))
    }

    /// Decide what a failed insert was: our own write landing late, a
    /// duplicate id, a lost compare-and-swap, or a store failure.
    ///
    /// The store cannot tell us directly. hiqlite reports every statement
    /// error inside a transaction as one transaction error (spec 011 maps it
    /// to [`Error::Validation`]), so the variant carries no signal and
    /// matching on its message would encode SQLite's wording as a contract.
    /// The chain itself carries the signal instead: whether the id is
    /// resident, and whether the head moved off the parent we chained onto.
    async fn classify(
        &self,
        id: &DecisionId,
        digest: &Hash,
        parent: &Hash,
        err: Error,
    ) -> Result<Attempt, Error> {
        // Spec 042 B-4: classification is lifetime-scoped. The identity row
        // and that id's collision rows come from one statement, and the row
        // survives sealing, so "appended and archived" is no longer the same
        // answer as "never appended".
        let (identity, collisions) = self.identity_rows_of(id).await?;
        if !collisions.is_empty() {
            // On a twice-spent id, a digest that matches one copy proves
            // nothing about which copy a retry meant (B-12).
            return Err(ambiguous(id, identity.as_ref(), &collisions));
        }
        if let Some(row) = identity {
            // A row whose digest equals this decision's means the decision
            // is already in the chain: return the stored record hash,
            // whether the record is resident or sealed.
            if &row.identity_digest == digest {
                return Ok(Attempt::Landed(row.record_hash));
            }
            return Err(reused(id, &row));
        }
        // No row of either kind: either the insert failed for the reason
        // spec 013 B-3 handles, which the head question decides, or coverage
        // is incomplete, which the backstop decided before the first
        // attempt.
        if &self.head().await? == parent {
            return Err(err);
        }
        Ok(Attempt::Retry)
    }
}

/// The refusal a reused id gets, naming the record the chain already holds
/// under it (spec 042 B-4, and spec 013's contract unchanged).
fn reused(id: &DecisionId, row: &DecisionCopy) -> Error {
    Error::Conflict(format!(
        "decision {id} is already in the chain under {}: an id names one decision for the life \
         of the chain",
        row.record_hash
    ))
}

/// The refusal a twice-spent id always gets, naming every recorded copy and
/// choosing between none of them (spec 042 B-12).
fn ambiguous(
    id: &DecisionId,
    identity: Option<&DecisionCopy>,
    collisions: &[DecisionCopy],
) -> Error {
    let mut copies: Vec<String> = Vec::with_capacity(collisions.len() + 1);
    if let Some(row) = identity {
        copies.push(row.to_string());
    }
    copies.extend(collisions.iter().map(ToString::to_string));
    Error::Conflict(format!(
        "decision {id} names {} recorded copies, so no retry of it can be verified against a \
         history that spent the id more than once: {}",
        copies.len(),
        copies.join("; ")
    ))
}

/// The wait before attempt `attempt` (counted from zero) of the append of
/// `id`.
///
/// The window doubles from [`APPEND_BACKOFF_BASE`] to [`APPEND_BACKOFF_CAP`],
/// and the wait is a point in the window's upper half picked by hashing the
/// id with the attempt. Rivals append different ids, so they wait different
/// times and one of them reaches the head first; nothing reads a random
/// source, so the same append waits the same way every time it is replayed.
fn backoff(id: &DecisionId, attempt: u32) -> Duration {
    let doublings = attempt.saturating_sub(1).min(16);
    let window = APPEND_BACKOFF_BASE
        .saturating_mul(1_u32 << doublings)
        .min(APPEND_BACKOFF_CAP);
    let half = window / 2;
    let spread = u64::try_from(half.as_micros()).unwrap_or(u64::MAX).max(1);
    half + Duration::from_micros(fnv1a(id.as_str(), attempt) % spread)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wait_sits_in_the_upper_half_of_a_window_that_doubles_to_the_cap() {
        let id = DecisionId::new("kernel:0123456789abcdef:2:000000000007");
        for attempt in 1..APPEND_ATTEMPTS {
            let window = APPEND_BACKOFF_BASE
                .saturating_mul(1_u32 << (attempt - 1).min(16))
                .min(APPEND_BACKOFF_CAP);
            let wait = backoff(&id, attempt);
            assert!(
                wait >= window / 2,
                "attempt {attempt}: {wait:?} of {window:?}"
            );
            assert!(wait < window, "attempt {attempt}: {wait:?} of {window:?}");
        }
        assert_eq!(
            backoff(&id, 3),
            backoff(&id, 3),
            "the same append waits the same"
        );
    }

    #[test]
    fn rivals_wait_different_times_and_every_wait_fits_the_budget() {
        let one = DecisionId::new("kernel:0123456789abcdef:1:000000000000");
        let two = DecisionId::new("kernel:0123456789abcdef:2:000000000000");
        assert_ne!(backoff(&one, 1), backoff(&two, 1));
        let total: Duration = (1..APPEND_ATTEMPTS).map(|a| backoff(&one, a)).sum();
        assert!(total < Duration::from_secs(2), "{total:?}");
    }
}
