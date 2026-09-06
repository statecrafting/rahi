//! spec 010 FR-002: `Revision::next` and `FenceToken` ordering.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use rahi_types::{FenceToken, Revision};

#[test]
fn revision_next_advances_by_one() {
    assert_eq!(Revision::ZERO.next(), Revision::new(1));
    assert_eq!(Revision::new(41).next().get(), 42);
    assert!(Revision::new(1) < Revision::new(1).next());
}

#[test]
fn revision_next_saturates_instead_of_wrapping() {
    let max = Revision::new(u64::MAX);
    assert_eq!(max.next(), max);
}

#[test]
fn revision_orders_by_value() {
    let mut revs = vec![Revision::new(3), Revision::ZERO, Revision::new(2)];
    revs.sort();
    assert_eq!(revs, [Revision::ZERO, Revision::new(2), Revision::new(3)]);
}

#[test]
fn fence_tokens_order_by_value() {
    let older = FenceToken::new(7);
    let newer = FenceToken::new(8);
    assert!(older < newer);
    assert!(older <= FenceToken::new(7));
    assert_eq!(newer.get(), 8);
    assert_eq!(older.max(newer), newer);
}

#[test]
fn both_newtypes_serialize_as_bare_integers() {
    assert_eq!(serde_json::to_string(&Revision::new(5)).unwrap(), "5");
    assert_eq!(serde_json::to_string(&FenceToken::new(9)).unwrap(), "9");
    let r: Revision = serde_json::from_str("12").unwrap();
    assert_eq!(r, Revision::new(12));
}

#[test]
fn both_newtypes_are_transparent_over_u64() {
    assert_eq!(size_of::<Revision>(), size_of::<u64>());
    assert_eq!(size_of::<FenceToken>(), size_of::<u64>());
}
