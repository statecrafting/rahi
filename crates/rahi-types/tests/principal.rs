//! spec 010 FR-002: `Principal` serde round-trip and the `email_verified`
//! default.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::collections::BTreeSet;

use rahi_types::{Email, Principal, Role, Sub, UnixSeconds};

fn principal() -> Principal {
    Principal {
        sub: Sub::new("rauthy-sub-1"),
        email: Some(Email::new("a@example.test")),
        email_verified: true,
        roles: BTreeSet::from([Role::new("admin"), Role::new("user")]),
        issued_at: UnixSeconds::new(1_700_000_000),
    }
}

#[test]
fn round_trips_through_serde() {
    let p = principal();
    let json = serde_json::to_string(&p).unwrap();
    let back: Principal = serde_json::from_str(&json).unwrap();
    assert_eq!(back, p);
}

#[test]
fn newtypes_serialize_transparently() {
    let json = serde_json::to_value(principal()).unwrap();
    assert_eq!(json["sub"], "rauthy-sub-1");
    assert_eq!(json["email"], "a@example.test");
    assert_eq!(json["roles"], serde_json::json!(["admin", "user"]));
    assert_eq!(json["issued_at"], 1_700_000_000);
}

#[test]
fn email_verified_defaults_to_false_when_absent() {
    let p: Principal =
        serde_json::from_str(r#"{"sub":"s","email":"x@example.test","issued_at":1}"#).unwrap();
    assert!(!p.email_verified);
    assert!(p.roles.is_empty());
    assert_eq!(p.verified_email(), None);
    assert_eq!(
        p.email_unverified().map(Email::as_str),
        Some("x@example.test")
    );
}

#[test]
fn absent_email_is_none() {
    let p: Principal = serde_json::from_str(r#"{"sub":"s","issued_at":1}"#).unwrap();
    assert_eq!(p.email, None);
    assert_eq!(p.verified_email(), None);
    assert_eq!(p.email_unverified(), None);
    let json = serde_json::to_string(&p).unwrap();
    assert!(
        !json.contains("email\""),
        "absent email is not serialised: {json}"
    );
}

#[test]
fn verified_email_requires_the_flag() {
    let p = principal();
    assert_eq!(
        p.verified_email().map(Email::as_str),
        Some("a@example.test")
    );
    let mut unverified = p.clone();
    unverified.email_verified = false;
    assert_eq!(unverified.verified_email(), None);
}

#[test]
fn roles_are_a_set() {
    let p = principal();
    assert!(p.has_role(&Role::new("admin")));
    assert!(!p.has_role(&Role::new("root")));
    assert_eq!(p.sub.as_str(), "rauthy-sub-1");
}
