//! Append: the unique-parent compare-and-swap (spec 013 B-3, constitution XI).
//!
//! An append reads the head, builds and signs a record chained onto it, and
//! inserts. The insert is the compare-and-swap: the unique index on
//! `prev_hash` means the store admits exactly one record per parent, so two
//! appenders racing for the same head produce one winner and one violation,
//! never a fork. There is no lock and no lease, because the constraint is
//! already the arbitration.
//!
//! Losing is ordinary: the loser reloads the head, re-chains the same
//! decision onto it, and tries again. Losing three times in a row is not
//! ordinary. It means the head is moving faster than an appender can follow
//! or that something is answering the head query wrongly, and spec 013 B-3
//! makes it [`Error::Integrity`] rather than a fourth attempt, because the
//! ledger's job under doubt is to stop rather than to guess.

use rahi_types::Error;

use crate::chain::{Ledger, hash_of, insert_statement};
use crate::record::{Decision, DecisionId, Hash, SignedRecord};

/// How many times an append re-chains after losing the compare-and-swap.
pub const APPEND_ATTEMPTS: u32 = 3;

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

        for _ in 0..APPEND_ATTEMPTS {
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
