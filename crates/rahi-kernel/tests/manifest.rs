//! The manifest: parse, refuse, hash, and verify at build (spec 015 FR-001,
//! FR-002, FR-004).

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;

use rahi_kernel::{CapabilityKind, Manifest, ServiceName, verify_crate};

const VALID: &str = include_str!("../testdata/manifests/valid.toml");
const REORDERED: &str = include_str!("../testdata/manifests/valid-reordered.toml");
const CHANGED_GRANT: &str = include_str!("../testdata/manifests/changed-grant.toml");
const UNKNOWN_KIND: &str = include_str!("../testdata/manifests/unknown-kind.toml");
const UNDECLARED: &str = include_str!("../testdata/manifests/undeclared-capability.toml");
const UPPERCASE_SECRET: &str = include_str!("../testdata/manifests/uppercase-secret.toml");
const UNKNOWN_KEY: &str = include_str!("../testdata/manifests/unknown-key.toml");

fn valid() -> Manifest {
    Manifest::parse(VALID).expect("the fixture manifest parses")
}

fn service(name: &str) -> ServiceName {
    ServiceName::parse(name).expect("a well-formed service name")
}

#[test]
fn the_fixture_manifest_declares_what_the_fixture_cell_may_do() {
    let manifest = valid();
    assert_eq!(manifest.app.name.as_str(), "hello-cell");
    assert_eq!(manifest.capabilities.len(), 14);
    assert_eq!(manifest.services.len(), 2);
    assert!(manifest.covers(&service("notes"), CapabilityKind::DbWrite, "notes"));
    assert!(manifest.covers(
        &service("webhooks"),
        CapabilityKind::HttpEgress,
        "api.example.com"
    ));
    // The grant is per service: webhooks holds no store capability at all.
    assert!(!manifest.covers(&service("webhooks"), CapabilityKind::DbWrite, "notes"));
    // Declared as a resource, granted to nobody: absence is never permission.
    assert!(!manifest.covers(&service("notes"), CapabilityKind::DbWrite, "audit"));
}

#[test]
fn every_kind_in_the_vocabulary_is_a_kind_the_fixture_exercises() {
    let manifest = valid();
    let declared: Vec<CapabilityKind> = manifest.capabilities.iter().map(|c| c.kind).collect();
    for kind in CapabilityKind::ALL {
        assert!(
            declared.contains(&kind),
            "the fixture manifest does not exercise {kind}"
        );
    }
}

#[test]
fn an_unknown_kind_names_the_kind_and_the_vocabulary() {
    let err = Manifest::parse(UNKNOWN_KIND).expect_err("db.drop is not a kind");
    assert_eq!(err.kind(), "validation");
    assert!(err.message().contains("db.drop"), "{err}");
    assert!(err.message().contains("db.write"), "{err}");
}

#[test]
fn a_service_granted_a_capability_the_catalog_lacks_is_refused() {
    let err = Manifest::parse(UNDECLARED).expect_err("notes-purge is not declared");
    assert_eq!(err.kind(), "validation");
    assert!(err.message().contains("notes-purge"), "{err}");
    assert!(err.message().contains("notes"), "{err}");
}

#[test]
fn an_uppercase_secret_name_is_refused() {
    let err = Manifest::parse(UPPERCASE_SECRET).expect_err("secret names are lowercase");
    assert_eq!(err.kind(), "validation");
    assert!(err.message().contains("WEBHOOK_TOKEN"), "{err}");
    assert!(err.message().contains("lowercase"), "{err}");
}

#[test]
fn an_unknown_key_is_refused_rather_than_ignored() {
    let err = Manifest::parse(UNKNOWN_KEY).expect_err("resources has no views");
    assert_eq!(err.kind(), "validation");
    assert!(err.message().contains("views"), "{err}");
}

#[test]
fn a_roster_check_outside_the_catalog_is_refused() {
    let text = VALID.replace(r#"checks = ["secrets"]"#, r#"checks = ["vibes"]"#);
    let err = Manifest::parse(&text).expect_err("vibes is not a check");
    assert!(err.message().contains("vibes"), "{err}");

    let text = VALID.replace(r#"checks = ["secrets"]"#, r#"checks = ["grants"]"#);
    let err = Manifest::parse(&text).expect_err("grants is never listed");
    assert!(err.message().contains("deny by default"), "{err}");
}

#[test]
fn the_hash_is_over_the_model_and_not_over_the_toml_text() {
    let a = valid();
    let b = Manifest::parse(REORDERED).expect("the reordered fixture parses");
    assert_eq!(a, b, "the two fixtures are the same model");
    assert_ne!(VALID, REORDERED, "and they are not the same text");
    assert_eq!(
        a.hash().expect("hashes"),
        b.hash().expect("hashes"),
        "key order and whitespace do not move the chain's root"
    );
}

#[test]
fn the_hash_moves_when_a_grant_changes() {
    let a = valid();
    let b = Manifest::parse(CHANGED_GRANT).expect("the narrowed fixture parses");
    assert_ne!(a, b);
    assert_ne!(
        a.hash().expect("hashes"),
        b.hash().expect("hashes"),
        "a changed grant is a changed cell"
    );
}

#[test]
fn the_hash_folds_in_the_gate_configuration() {
    let a = valid();
    let b = Manifest::parse(CHANGED_GRANT).expect("parses");
    let gate_a = a.gate().expect("assembles").config_hash();
    let gate_b = b.gate().expect("assembles").config_hash();
    assert!(gate_a.starts_with("sha256:"), "{gate_a}");
    // The capability check is fingerprinted from the grant table, so the gate
    // hash alone already distinguishes the two cells (spec 015 B-2).
    assert_ne!(gate_a, gate_b);

    let same = Manifest::parse(REORDERED).expect("parses");
    assert_eq!(gate_a, same.gate().expect("assembles").config_hash());
}

#[test]
fn the_hash_is_a_chain_hash_the_ledger_accepts() {
    let hash = valid().hash().expect("hashes");
    assert!(hash.as_str().starts_with("sha256:"), "{hash}");
    assert_eq!(hash.as_str().len(), "sha256:".len() + 64);
}

/// Write a throwaway crate whose `src/lib.rs` holds `body`.
fn app_crate(body: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("a temp dir");
    let src = dir.path().join("src");
    fs::create_dir_all(&src).expect("src/");
    fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"app\"\n").expect("a manifest");
    fs::write(src.join("lib.rs"), body).expect("a source file");
    dir
}

#[test]
fn a_covered_facade_call_passes_verify() {
    let app = app_crate(
        r#"
        pub async fn write(kernel: &Kernel, store: StoreHandle) -> Result<(), Error> {
            let notes = Governed::new(kernel, "notes", CapabilityKind::DbWrite, "notes", store)?;
            notes.execute(&actor, "DELETE FROM notes", vec![]).await?;
            Ok(())
        }
        "#,
    );
    verify_crate(&valid(), app.path()).expect("the call site is inside the ceiling");
}

#[test]
fn an_uncovered_facade_call_fails_verify_naming_the_triple() {
    let app = app_crate(
        r#"
        pub async fn audit(kernel: &Kernel, store: StoreHandle) -> Result<(), Error> {
            let audit = Governed::new(kernel, "notes", CapabilityKind::DbWrite, "audit", store)?;
            audit.execute(&actor, "INSERT INTO audit VALUES (1)", vec![]).await?;
            Ok(())
        }
        "#,
    );
    let err = verify_crate(&valid(), app.path()).expect_err("audit is not granted");
    assert_eq!(err.kind(), "validation");
    let message = err.message();
    assert!(message.contains("notes"), "{message}");
    assert!(message.contains("db.write"), "{message}");
    assert!(message.contains("audit"), "{message}");
    assert!(message.contains("lib.rs:3"), "the site is named: {message}");
}

#[test]
fn a_marker_covers_a_call_site_no_literal_could() {
    let app = app_crate(
        r#"
        pub async fn dynamic(kernel: &Kernel, store: StoreHandle) -> Result<(), Error> {
            // #[governed(service = "notes", kind = "kv.put", resource = "cache")]
            let cache = Governed::new(kernel, svc, kind, resource, store)?;
            Ok(())
        }
        "#,
    );
    verify_crate(&valid(), app.path()).expect("the marker names a granted triple");

    let app = app_crate(
        r#"
        pub async fn dynamic(kernel: &Kernel, store: StoreHandle) -> Result<(), Error> {
            // #[governed(service = "notes", kind = "http.egress", resource = "*.elsewhere.test")]
            let out = Governed::new(kernel, svc, kind, resource, store)?;
            Ok(())
        }
        "#,
    );
    let err = verify_crate(&valid(), app.path()).expect_err("notes holds no egress grant");
    assert!(err.message().contains("*.elsewhere.test"), "{err}");
}

#[test]
fn an_unmarked_opaque_call_site_fails_the_build() {
    let app =
        app_crate("pub fn f() { let _ = Governed::new(kernel, svc, kind, resource, store); }");
    let err = verify_crate(&valid(), app.path()).expect_err("nothing names the triple");
    assert!(
        err.message().contains("absence is never permission"),
        "{err}"
    );
}

#[test]
fn a_crate_with_no_src_is_not_silently_clean() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let err = verify_crate(&valid(), dir.path()).expect_err("there is nothing to walk");
    assert_eq!(err.kind(), "io");
}
