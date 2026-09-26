//! spec 043 FR-001: dependency identity and locked metadata.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]

use std::path::Path;
use std::process::Command;

const HIQLITE_PATCHED_CHECKSUM: &str =
    "9d3586f7db4e971836ffc5982086bcfa1de0c0293492a194efd7d4ac21ff5ebf";
const HIQLITE_WAL_PATCHED_CHECKSUM: &str =
    "df82af141317c61b135d600a93c0ec2283c40f2761ab956b20568b2c23bc2746";

#[derive(Default)]
struct LockPkg {
    name: String,
    version: String,
    source: String,
    checksum: String,
}

#[test]
fn dependency_identity_and_checksums() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("workspace root");

    let cargo_lock =
        std::fs::read_to_string(workspace_root.join("Cargo.lock")).expect("read Cargo.lock");

    let mut packages = Vec::new();
    for block in cargo_lock.split("[[package]]").skip(1) {
        let mut pkg = LockPkg::default();
        for line in block.lines() {
            let line = line.trim();
            let field = |prefix: &str| {
                line.strip_prefix(prefix)
                    .and_then(|rest| rest.strip_suffix('"'))
                    .map(str::to_owned)
            };
            if let Some(val) = field("name = \"") {
                pkg.name = val;
            } else if let Some(val) = field("version = \"") {
                pkg.version = val;
            } else if let Some(val) = field("source = \"") {
                pkg.source = val;
            } else if let Some(val) = field("checksum = \"") {
                pkg.checksum = val;
            }
        }
        packages.push(pkg);
    }

    let mut found_patched = false;
    let mut found_wal_patched = false;

    for pkg in packages {
        assert_ne!(
            pkg.name, "hiqlite",
            "Cargo.lock must not contain package 'hiqlite'; must use aliased 'hiqlite-patched'"
        );
        assert_ne!(
            pkg.name, "hiqlite-wal",
            "Cargo.lock must not contain package 'hiqlite-wal'; must use aliased 'hiqlite-wal-patched'"
        );

        if pkg.name.contains("hiqlite") {
            assert!(
                !pkg.source.contains("git+"),
                "hiqlite package '{}' must not have a git source: {}",
                pkg.name,
                pkg.source
            );
            assert!(
                pkg.source.starts_with("registry+"),
                "hiqlite package '{}' must have a registry source: {}",
                pkg.name,
                pkg.source
            );
            assert_eq!(
                pkg.version, "0.15.0-patched.3",
                "hiqlite package '{}' must be version 0.15.0-patched.3",
                pkg.name
            );
        }

        if pkg.name == "hiqlite-patched" {
            found_patched = true;
            assert_eq!(
                pkg.checksum, HIQLITE_PATCHED_CHECKSUM,
                "hiqlite-patched checksum mismatch"
            );
        } else if pkg.name == "hiqlite-wal-patched" {
            found_wal_patched = true;
            assert_eq!(
                pkg.checksum, HIQLITE_WAL_PATCHED_CHECKSUM,
                "hiqlite-wal-patched checksum mismatch"
            );
        }
    }

    assert!(found_patched, "hiqlite-patched not found in Cargo.lock");
    assert!(
        found_wal_patched,
        "hiqlite-wal-patched not found in Cargo.lock"
    );

    // Check cargo metadata --locked for both Linux targets
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        let output = Command::new("cargo")
            .current_dir(workspace_root)
            .args([
                "metadata",
                "--format-version",
                "1",
                "--locked",
                "--filter-platform",
                target,
            ])
            .output()
            .expect("cargo metadata failed to execute");

        assert!(
            output.status.success(),
            "cargo metadata for target {target} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let json: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("parse metadata JSON");
        let pkgs = json
            .get("packages")
            .and_then(serde_json::Value::as_array)
            .expect("packages array");

        let mut meta_patched = false;
        let mut meta_wal = false;

        for p in pkgs {
            let name = p
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let source = p
                .get("source")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let ver = p
                .get("version")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();

            assert_ne!(
                name, "hiqlite",
                "Target {target}: metadata must not contain package 'hiqlite'"
            );
            assert_ne!(
                name, "hiqlite-wal",
                "Target {target}: metadata must not contain package 'hiqlite-wal'"
            );

            if name.contains("hiqlite") {
                assert!(
                    !source.contains("git+"),
                    "Target {target}: package '{name}' must not have a git source"
                );
                assert_eq!(
                    ver, "0.15.0-patched.3",
                    "Target {target}: package '{name}' version mismatch"
                );
            }

            if name == "hiqlite-patched" {
                meta_patched = true;
            } else if name == "hiqlite-wal-patched" {
                meta_wal = true;
            }
        }

        assert!(
            meta_patched,
            "Target {target}: hiqlite-patched not in metadata"
        );
        assert!(
            meta_wal,
            "Target {target}: hiqlite-wal-patched not in metadata"
        );
    }
}
