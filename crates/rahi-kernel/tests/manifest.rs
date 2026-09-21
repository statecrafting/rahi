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
const NO_SCHEMA: &str = include_str!("../testdata/manifests/no-schema-version.toml");
const FUTURE_SCHEMA: &str = include_str!("../testdata/manifests/future-schema.toml");
const LATER_MINOR: &str = include_str!("../testdata/manifests/later-minor-schema.toml");

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

#[test]
fn a_manifest_that_names_no_schema_version_is_refused() {
    let err = Manifest::parse(NO_SCHEMA).expect_err("a manifest names its schema");
    assert_eq!(err.kind(), "validation");
    assert!(err.message().contains("names no schema_version"), "{err}");
    assert!(
        err.message().contains("1.0.0"),
        "it names the one this build speaks: {err}"
    );
}

#[test]
fn a_manifest_of_another_major_is_refused_and_a_later_minor_is_read() {
    let err = Manifest::parse(FUTURE_SCHEMA).expect_err("another major is another schema");
    assert_eq!(err.kind(), "validation");
    assert!(err.message().contains("\"2.0.0\""), "{err}");
    assert!(err.message().contains("different major"), "{err}");

    let later = Manifest::parse(LATER_MINOR).expect("a later minor of this major reads");
    assert_eq!(later.schema_version.as_deref(), Some("1.7.0"));
    assert_eq!(
        later.hash().expect("hashes"),
        Manifest::parse(LATER_MINOR)
            .expect("parses")
            .hash()
            .expect("hashes")
    );
}

#[test]
fn the_schema_version_is_part_of_what_the_chain_is_rooted_at() {
    // Spec 015 D-1 hashes the parsed model, so the version a manifest names
    // moves its hash: spec 039 D-4 is the consequence for an existing volume.
    let valid = valid().hash().expect("hashes");
    let later = Manifest::parse(LATER_MINOR)
        .expect("parses")
        .hash()
        .expect("hashes");
    assert_ne!(valid, later, "the named schema version is inside the hash");
}

// ---------------------------------------------------------------------------
// Native clients and the token lifetimes (spec 038 B-2, B-4, FR-002).
// ---------------------------------------------------------------------------

/// `VALID` with an `[auth]` table replaced by `table`.
fn with_auth(table: &str) -> String {
    let head = VALID
        .split("[auth]")
        .next()
        .expect("the fixture has an [auth] table");
    let tail = VALID
        .split_once("[contract]")
        .expect("the fixture has a [contract] table")
        .1;
    format!("{head}{table}\n[contract]{tail}")
}

/// The hash `valid.toml` had before the `[auth]` table grew the lifetimes
/// and the native clients, measured at `0caff51`.
///
/// A manifest that says nothing about either must hash to exactly this: the
/// model is what the chain is rooted at (spec 015 B-2), and a field
/// defaulted into every document's model would move the genesis parent of
/// every chain whose manifest never changed (spec 038 D-9).
const VALID_HASH_BEFORE_038: &str =
    "sha256:f3407fd4af0c4bfb852e8349872397b4810557decdd5fd87dc0fbdb38d3f8f2f";

#[test]
fn a_manifest_that_declares_no_native_client_hashes_as_it_did_before() {
    assert_eq!(
        valid().hash().expect("hashes").as_str(),
        VALID_HASH_BEFORE_038
    );
}

#[test]
fn the_lifetimes_default_and_a_declared_one_is_read() {
    let manifest = valid();
    assert_eq!(manifest.access_token_lifetime_secs(), 600);
    assert_eq!(manifest.native_refresh_lifetime_secs(), 86_400);
    assert!(!manifest.logout_revokes_bearer());
    assert!(manifest.native_clients().is_empty());

    let declared = Manifest::parse(&with_auth(
        "[auth]\noperator_role = \"op\"\naccess_token_lifetime_secs = 300\n\
         native_refresh_lifetime_secs = 7200\nlogout_revokes_bearer = true\n",
    ))
    .expect("parses");
    assert_eq!(declared.access_token_lifetime_secs(), 300);
    assert_eq!(declared.native_refresh_lifetime_secs(), 7_200);
    assert!(declared.logout_revokes_bearer());
}

#[test]
fn a_declared_lifetime_moves_the_hash() {
    let declared = Manifest::parse(&with_auth(
        "[auth]\noperator_role = \"op\"\naccess_token_lifetime_secs = 600\n",
    ))
    .expect("parses");
    assert_ne!(
        declared.hash().expect("hashes").as_str(),
        VALID_HASH_BEFORE_038,
        "saying the default out loud is still a different ceiling document"
    );
}

#[test]
fn a_lifetime_rauthy_would_refuse_is_refused_here() {
    for bad in [
        "access_token_lifetime_secs = 9",
        "access_token_lifetime_secs = 86401",
    ] {
        let err = Manifest::parse(&with_auth(&format!(
            "[auth]\noperator_role = \"op\"\n{bad}\n"
        )))
        .expect_err("refused");
        assert_eq!(err.kind(), "validation");
        assert!(
            err.message().contains("access_token_lifetime_secs"),
            "{err}"
        );
    }
    let err = Manifest::parse(&with_auth(
        "[auth]\noperator_role = \"op\"\nnative_refresh_lifetime_secs = 5400\n",
    ))
    .expect_err("whole hours only");
    assert!(err.message().contains("whole hours"), "{err}");
}

/// A manifest declaring one native client with `flows` and `scopes`.
fn with_client(body: &str) -> String {
    with_auth(&format!(
        "[auth]\noperator_role = \"op\"\n\n[[auth.native_clients]]\n{body}"
    ))
}

#[test]
fn a_declared_native_client_is_read_with_its_flows_and_scopes() {
    let manifest = Manifest::parse(&with_client(
        "id = \"hello-cli\"\nflows = [\"device_code\", \"refresh_token\"]\n\
         scopes = [\"notes:write\"]\n",
    ))
    .expect("parses");
    let client = manifest
        .native_clients()
        .first()
        .expect("the declared client");
    assert_eq!(client.id, "hello-cli");
    assert!(client.has_flow(rahi_kernel::NativeFlow::DeviceCode));
    assert!(client.has_flow(rahi_kernel::NativeFlow::RefreshToken));
    assert!(!client.has_flow(rahi_kernel::NativeFlow::AuthorizationCode));
    assert_eq!(
        client.grants(),
        vec![
            "urn:ietf:params:oauth:grant-type:device_code".to_owned(),
            "refresh_token".to_owned(),
        ]
    );
    assert_ne!(
        manifest.hash().expect("hashes").as_str(),
        VALID_HASH_BEFORE_038,
        "a declared client is part of the ceiling and moves the hash (B-2)"
    );
}

#[test]
fn a_native_client_that_logs_nobody_in_is_refused() {
    let err = Manifest::parse(&with_client(
        "id = \"hello-cli\"\nflows = [\"refresh_token\"]\n",
    ))
    .expect_err("refresh_token never stands alone");
    assert_eq!(err.kind(), "validation");
    assert!(err.message().contains("logs a person in"), "{err}");
}

#[test]
fn two_native_clients_of_one_id_are_refused() {
    let err = Manifest::parse(&with_auth(
        "[auth]\noperator_role = \"op\"\n\n[[auth.native_clients]]\nid = \"cli\"\n\
         flows = [\"device_code\"]\n\n[[auth.native_clients]]\nid = \"cli\"\n\
         flows = [\"device_code\"]\n",
    ))
    .expect_err("ids are unique");
    assert!(err.message().contains("twice"), "{err}");
}

/// FR-002, second half: a redirect URI that is not loopback is refused, and
/// so is one on a client that redirects nowhere.
#[test]
fn a_redirect_uri_that_is_not_loopback_is_refused() {
    for bad in [
        "https://app.example.com/cb",
        "http://localhost:1234/cb",
        "http://127.0.0.1.evil.example.com/cb",
        "http://[::1/cb",
    ] {
        let err = Manifest::parse(&with_client(&format!(
            "id = \"cli\"\nflows = [\"authorization_code\"]\nredirect_uris = [\"{bad}\"]\n"
        )))
        .expect_err("RFC 8252 wants loopback");
        assert!(err.message().contains("loopback"), "{bad}: {err}");
    }
    for good in [
        "http://127.0.0.1:8123/callback",
        "http://127.0.0.1/callback",
        "http://[::1]:8123/callback",
    ] {
        Manifest::parse(&with_client(&format!(
            "id = \"cli\"\nflows = [\"authorization_code\"]\nredirect_uris = [\"{good}\"]\n"
        )))
        .unwrap_or_else(|err| panic!("{good} is loopback: {err}"));
    }

    let err = Manifest::parse(&with_client(
        "id = \"cli\"\nflows = [\"authorization_code\"]\n",
    ))
    .expect_err("authorization_code needs somewhere to come back to");
    assert!(err.message().contains("redirect_uris"), "{err}");

    let err = Manifest::parse(&with_client(
        "id = \"cli\"\nflows = [\"device_code\"]\nredirect_uris = [\"http://127.0.0.1/cb\"]\n",
    ))
    .expect_err("a device client redirects nowhere");
    assert!(err.message().contains("typo"), "{err}");
}

/// FR-002, first half: a scope no bearer route requires is refused. The
/// check takes the routes' declaration because the manifest alone cannot
/// know it (spec 038 D-9).
#[test]
fn a_native_client_scope_no_route_declares_is_refused() {
    let manifest = Manifest::parse(&with_client(
        "id = \"hello-cli\"\nflows = [\"device_code\"]\nscopes = [\"notes:write\"]\n",
    ))
    .expect("parses");

    let declared = std::collections::BTreeSet::from(["notes:write".to_owned()]);
    manifest
        .validate_native_scopes(&declared)
        .expect("the scope a route requires");

    let err = manifest
        .validate_native_scopes(&std::collections::BTreeSet::from(["notes:read".to_owned()]))
        .expect_err("a scope nothing requires");
    assert_eq!(err.kind(), "validation");
    assert!(err.message().contains("notes:write"), "{err}");
    assert!(
        err.message().contains("notes:read"),
        "it names what is available: {err}"
    );

    let err = manifest
        .validate_native_scopes(&std::collections::BTreeSet::new())
        .expect_err("a cell with no bearer route declares no scopes");
    assert!(err.message().contains("none"), "{err}");
}

#[test]
fn an_unknown_key_in_a_native_client_is_refused() {
    let err = Manifest::parse(&with_client(
        "id = \"cli\"\nflows = [\"device_code\"]\nsecret = \"hunter2\"\n",
    ))
    .expect_err("deny_unknown_fields reaches into the client table too");
    assert_eq!(err.kind(), "validation");
}
