//! The ceiling, verified at build (spec 034 FR-002, spec 015 B-3).
//!
//! `verify!` walks this crate's `src/` for every governed call site and
//! holds the manifest to it. One site is outside the ceiling on purpose:
//! the `db.migrate` facade behind `POST /api/notes/migrate`, which the
//! manifest never grants so that the end-to-end test can read a ledgered
//! denial (B-1, B-3). The check here is therefore exact rather than empty:
//! the verifier names that one site and nothing else, so a grant removed
//! while its call remains adds a second name and fails the build (D-6).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use rahi_kernel::{CapabilityKind, Manifest, Usage, scan_crate, verify_usage};

const MANIFEST: &str = include_str!("../manifest.toml");

fn usage() -> Vec<Usage> {
    scan_crate(std::path::Path::new(env!("CARGO_MANIFEST_DIR"))).expect("every site is literal")
}

/// The one uncovered triple, named.
fn outside(manifest: &Manifest) -> Vec<String> {
    match verify_usage(manifest, &usage()) {
        Ok(()) => Vec::new(),
        Err(err) => err
            .to_string()
            .lines()
            .filter(|line| line.trim_start().starts_with('('))
            .map(|line| line.trim().to_owned())
            .collect(),
    }
}

#[test]
fn the_manifest_parses_and_declares_what_b1_says() {
    let manifest = Manifest::parse(MANIFEST).expect("the embedded manifest parses");
    assert_eq!(manifest.app.name.as_str(), "hello-cell");
    let notes = rahi_kernel::ServiceName::parse("notes".to_owned()).unwrap();
    for kind in [
        CapabilityKind::DbRead,
        CapabilityKind::DbWrite,
        CapabilityKind::DbTxn,
    ] {
        assert!(manifest.covers(&notes, kind, "notes"), "{kind} on notes");
    }
    assert!(
        !manifest.covers(&notes, CapabilityKind::DbMigrate, "notes"),
        "db.migrate on notes is deliberately absent"
    );
}

#[test]
fn the_verifier_names_the_demonstration_and_nothing_else() {
    let manifest = Manifest::parse(MANIFEST).unwrap();
    let outside = outside(&manifest);
    assert_eq!(
        outside.len(),
        1,
        "exactly one site is outside the ceiling: {outside:?}"
    );
    assert!(
        outside[0].starts_with("(notes, db.migrate, notes) at "),
        "{}",
        outside[0]
    );
    // The same walk, as `verify!` runs it from this crate.
    let err = rahi_kernel::verify!(&manifest).expect_err("the demonstration is named");
    assert!(
        err.to_string().contains("(notes, db.migrate, notes)"),
        "{err}"
    );
}

#[test]
fn a_grant_removed_while_its_call_remains_fails_the_build() {
    let without_write = MANIFEST.replace("  \"notes-write\",\n", "");
    assert_ne!(without_write, MANIFEST, "the grant was there to remove");
    let manifest = Manifest::parse(&without_write).unwrap();
    let outside = outside(&manifest);
    assert_eq!(outside.len(), 2, "{outside:?}");
    assert!(
        outside
            .iter()
            .any(|site| site.starts_with("(notes, db.write, notes) at ")),
        "{outside:?}"
    );
}
