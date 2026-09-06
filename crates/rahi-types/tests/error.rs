//! spec 010 FR-002: every `Error` variant maps to its exit code.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use rahi_types::Error;
use rahi_types::error::{EXIT_FAILURE, EXIT_INFRA, EXIT_STALE};

fn m() -> String {
    "detail".to_owned()
}

#[test]
fn every_variant_maps_to_its_exit_code() {
    let cases = [
        (Error::Validation(m()), EXIT_FAILURE, "validation"),
        (Error::NotFound(m()), EXIT_FAILURE, "not_found"),
        (Error::Conflict(m()), EXIT_FAILURE, "conflict"),
        (Error::Integrity(m()), EXIT_FAILURE, "integrity"),
        (Error::Denied(m()), EXIT_FAILURE, "denied"),
        (Error::Unauthorized(m()), EXIT_FAILURE, "unauthorized"),
        (Error::Stale(m()), EXIT_STALE, "stale"),
        (Error::Io(m()), EXIT_INFRA, "io"),
        (Error::Config(m()), EXIT_INFRA, "config"),
        (Error::Upstream(m()), EXIT_INFRA, "upstream"),
    ];
    assert_eq!(cases.len(), 10, "the variant list of B-4 has ten entries");
    for (err, code, kind) in cases {
        assert_eq!(err.exit_code(), code, "{err:?}");
        assert_eq!(err.kind(), kind, "{err:?}");
        assert_eq!(err.message(), "detail");
        assert_eq!(err.to_string(), format!("{kind}: detail"));
    }
}

#[test]
fn the_four_codes_are_spec_spines() {
    assert_eq!(EXIT_FAILURE, 1);
    assert_eq!(EXIT_STALE, 2);
    assert_eq!(EXIT_INFRA, 3);
}

#[test]
fn error_round_trips_through_serde() {
    let err = Error::Conflict("revision moved".to_owned());
    let json = serde_json::to_string(&err).unwrap();
    let back: Error = serde_json::from_str(&json).unwrap();
    assert_eq!(back, err);
}

#[test]
fn error_is_a_std_error() {
    fn takes(_: &dyn std::error::Error) {}
    takes(&Error::Io(m()));
}
