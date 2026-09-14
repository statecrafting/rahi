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

use crate::chain::{Ledger, hash_of, insert_statement};
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
        let mut decision = decision;
        if decision.id.as_str().is_empty() {
            return Err(Error::Validation(
                "a decision needs an id: it is the row's primary key and the name spec 015 \
                 reports back to a denied caller"
                    .to_owned(),
            ));
        }

        for attempt in 0..APPEND_ATTEMPTS {
            if attempt > 0 {
                tokio::time::sleep(backoff(&decision.id, attempt)).await;
            }
            let parent = self.head().await?;
            decision.prev_hash = parent.clone();
            let record = SignedRecord::build(&decision, self.signer())?;
            let hash = record.hash()?;

            match self.store().txn(vec![insert_statement(&record)?]).await {
                Ok(results) => {
                    let landed = results.first().is_some_and(|r| r.rows_affected == 1);
                    if !landed {
                        return Err(Error::Integrity(format!(
                            "the append of decision {} committed without inserting a row",
                            decision.id
                        )));
                    }
                    return Ok(hash);
                }
                Err(err) => match self.classify(&decision.id, &hash, &parent, err).await? {
                    Attempt::Landed(hash) => return Ok(hash),
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
        ours: &Hash,
        parent: &Hash,
        err: Error,
    ) -> Result<Attempt, Error> {
        if let Some(resident) = hash_of(self.store(), id).await? {
            if &resident == ours {
                return Ok(Attempt::Landed(resident));
            }
            return Err(Error::Conflict(format!(
                "decision {id} is already in the chain under {resident}: an id names one \
                 decision for the life of the chain"
            )));
        }
        if &self.head().await? == parent {
            return Err(err);
        }
        Ok(Attempt::Retry)
    }
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
