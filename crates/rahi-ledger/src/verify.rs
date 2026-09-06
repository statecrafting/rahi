//! Chain verification: hash links, signatures, linearity (spec 013 B-4).
//!
//! Everything here is a pure function over records the caller already read,
//! so the same code verifies a chain resident in the store, a chain exported
//! to a file, and a fixture under `testdata/`. The hash and link half is
//! `attest_ledger_core::verify_chain`, deliberately not reimplemented: a
//! second implementation of the linkage rule is a second thing that can
//! drift from the published verifier.
//!
//! Every failure is [`Error::Integrity`]. Constitution XI makes that fatal on
//! the init path: a cell serving requests under a broken audit proof is worse
//! than a cell that is down.

use std::collections::HashMap;

use attest_ledger_types::LedgerRecord;
use rahi_types::Error;

use crate::record::{Hash, SignedRecord};
use crate::signer::LedgerVerifier;

/// Put an unordered set of records into chain order, from `genesis_parent`.
///
/// The unique index on the parent hash (spec 013 B-2) means at most one
/// record claims each parent, so the chain is a list and walking it from the
/// genesis parent is the whole of linearity. A record the walk never reaches
/// is damage: it belongs to a chain rooted somewhere else, or its parent was
/// deleted.
///
/// # Errors
///
/// [`Error::Integrity`] when two records claim one parent, when a hash does
/// not parse, or when the walk does not reach every record.
pub fn order_chain(
    genesis_parent: &Hash,
    records: Vec<SignedRecord>,
) -> Result<Vec<SignedRecord>, Error> {
    let total = records.len();
    let mut by_parent: HashMap<Hash, SignedRecord> = HashMap::with_capacity(total);
    for record in records {
        let parent = record.prev_hash()?;
        if let Some(rival) = by_parent.insert(parent.clone(), record) {
            return Err(Error::Integrity(format!(
                "the chain forked at {parent}: record {} also claims that parent",
                rival.record.id
            )));
        }
    }

    let mut ordered = Vec::with_capacity(total);
    let mut cursor = genesis_parent.clone();
    while let Some(record) = by_parent.remove(&cursor) {
        cursor = record.hash()?;
        ordered.push(record);
    }

    if let Some(orphan) = by_parent.values().next() {
        return Err(Error::Integrity(format!(
            "record {} links to {} which is not in the chain: {} record(s) unreachable from the \
             genesis parent {genesis_parent}",
            orphan.record.id,
            orphan.record.previous_record_hash,
            by_parent.len()
        )));
    }
    Ok(ordered)
}

/// Verify an ordered chain end to end.
///
/// Checks, in this order: the chain is not empty; its first record links the
/// genesis parent; every `record_hash` recomputes and every record binds its
/// predecessor; every record carries the cell's public key and a signature
/// over its own hash; and every payload agrees with the envelope it travels
/// in. Signing the record hash rather than the payload is what makes the
/// signature bind a decision to its place in the chain.
///
/// # Errors
///
/// [`Error::Integrity`], naming the first record that failed and how.
pub fn verify_chain(
    genesis_parent: &Hash,
    records: &[SignedRecord],
    verifier: &LedgerVerifier,
) -> Result<(), Error> {
    let Some(first) = records.first() else {
        return Err(Error::Integrity(
            "the chain is empty: not even a genesis record is resident".to_owned(),
        ));
    };
    if first.record.previous_record_hash != genesis_parent.as_str() {
        return Err(Error::Integrity(format!(
            "the genesis record links {} but this cell booted the chain rooted at \
             {genesis_parent}",
            first.record.previous_record_hash
        )));
    }

    let envelopes: Vec<LedgerRecord> = records.iter().map(|r| r.record.clone()).collect();
    attest_ledger_core::verify_chain(&envelopes)
        .map_err(|e| Error::Integrity(format!("chain does not verify: {e}")))?;

    let expected_key = verifier.public_key();
    for record in records {
        if record.public_key != expected_key {
            return Err(Error::Integrity(format!(
                "record {} was signed by a key this cell does not hold",
                record.record.id
            )));
        }
        verifier
            .verify(record.record.record_hash.as_bytes(), &record.signature)
            .map_err(|e| {
                Error::Integrity(format!("record {}: {}", record.record.id, e.message()))
            })?;
        let decision = record.decision()?;
        if decision.id.as_str() != record.record.id {
            return Err(Error::Integrity(format!(
                "record {} carries decision {}: the envelope and its payload disagree",
                record.record.id, decision.id
            )));
        }
        if decision.prev_hash.as_str() != record.record.previous_record_hash {
            return Err(Error::Integrity(format!(
                "record {}: the decision links {} and the envelope links {}",
                record.record.id, decision.prev_hash, record.record.previous_record_hash
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use rahi_types::Sub;

    use crate::record::{Decision, DecisionId, DecisionKind, Outcome};
    use crate::signer::LedgerSigner;

    fn root() -> Hash {
        Hash::parse(format!("sha256:{}", "11".repeat(32))).expect("a hash")
    }

    fn signer() -> LedgerSigner {
        LedgerSigner::from_seed([2u8; 32])
    }

    /// A chain of `n` records built the way `append` builds them, without a
    /// store: each record chains onto the previous record's hash.
    fn chain(n: usize, signer: &LedgerSigner) -> Vec<SignedRecord> {
        let mut parent = root();
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let decision = Decision::new(
                DecisionId::new(format!("d-{i}")),
                DecisionKind::new("db.write"),
                Sub::new("subject"),
                Outcome::Allow,
                "granted",
            );
            let mut decision = decision;
            decision.prev_hash = parent;
            let record = SignedRecord::build(&decision, signer).expect("builds");
            parent = record.hash().expect("a hash");
            out.push(record);
        }
        out
    }

    #[test]
    fn a_clean_chain_verifies_and_orders_from_the_genesis_parent() {
        let signer = signer();
        let records = chain(5, &signer);
        verify_chain(&root(), &records, &signer.verifier()).expect("verifies");

        let mut shuffled = records.clone();
        shuffled.reverse();
        assert_eq!(
            order_chain(&root(), shuffled).expect("orders"),
            records,
            "order is recovered from the links, not from the read order"
        );
    }

    #[test]
    fn a_broken_link_is_an_integrity_error() {
        let signer = signer();
        let mut records = chain(4, &signer);
        records[2].record.previous_record_hash = format!("sha256:{}", "ee".repeat(32));
        let err = verify_chain(&root(), &records, &signer.verifier()).expect_err("refused");
        assert!(matches!(err, Error::Integrity(_)), "{err}");
    }

    #[test]
    fn a_signature_from_another_key_is_an_integrity_error() {
        let signer = signer();
        let stranger = LedgerSigner::from_seed([8u8; 32]);
        let mut records = chain(3, &signer);
        records[1].signature = stranger.sign(records[1].record.record_hash.as_bytes());
        let err = verify_chain(&root(), &records, &signer.verifier()).expect_err("refused");
        assert!(matches!(err, Error::Integrity(_)), "{err}");

        // Re-stamping the public key beside the forged signature does not
        // help: verification pins the key the cell brought with it.
        records[1].public_key = stranger.public_key();
        let err = verify_chain(&root(), &records, &signer.verifier()).expect_err("refused");
        assert!(matches!(err, Error::Integrity(_)), "{err}");
    }

    #[test]
    fn a_chain_rooted_somewhere_else_is_refused() {
        let signer = signer();
        let records = chain(2, &signer);
        let other = Hash::parse(format!("sha256:{}", "22".repeat(32))).expect("a hash");
        let err = verify_chain(&other, &records, &signer.verifier()).expect_err("refused");
        assert!(matches!(err, Error::Integrity(_)), "{err}");
    }

    #[test]
    fn an_empty_chain_is_refused() {
        let err = verify_chain(&root(), &[], &signer().verifier()).expect_err("refused");
        assert!(matches!(err, Error::Integrity(_)), "{err}");
    }

    #[test]
    fn two_records_claiming_one_parent_are_a_fork() {
        let signer = signer();
        let a = chain(1, &signer);
        let mut b = chain(1, &signer);
        b[0].record.id = "rival".to_owned();
        b[0].record.record_hash = attest_ledger_core::compute_record_hash(&b[0].record);
        let mut both = a;
        both.extend(b);
        let err = order_chain(&root(), both).expect_err("refused");
        assert!(matches!(err, Error::Integrity(_)), "{err}");
    }

    #[test]
    fn a_record_the_walk_never_reaches_is_refused() {
        let signer = signer();
        let mut records = chain(3, &signer);
        // Detach the tail: it is well formed but rooted at nothing resident.
        records[2].record.previous_record_hash = format!("sha256:{}", "cc".repeat(32));
        records[2].record.record_hash = attest_ledger_core::compute_record_hash(&records[2].record);
        let err = order_chain(&root(), records).expect_err("refused");
        assert!(matches!(err, Error::Integrity(_)), "{err}");
    }
}
