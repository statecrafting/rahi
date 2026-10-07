//! The binding document (spec 040): what a replica says it is, with every
//! value's basis named, assembled once at boot and never changed after.
//!
//! Section 3.1 of the spec is the contract this module serializes. Every
//! identity value is a wrapper, `{value, basis}` or `{basis: "absent",
//! reason, source?}`; `schema` and `observation` are the only members that
//! are not. A value is never omitted for being unavailable, a declared value
//! is never reported as measured, and an absence carries a token from a
//! closed set, never a path or an OS error string: the detail of an
//! `unreadable` goes to the boot log ([`Binding::log`]).
//!
//! The document is an observation. It authorizes nothing and it is not a
//! clock (B-8): a process that booted before the newest deployment reports
//! what it booted under, correctly.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use rahi_store::{SetName, StoreHandle};
use rahi_types::{EnvReader, Error, Result};
use ring::digest::{SHA256, digest};
use serde_json::{Map, Value, json};

/// The document's name and major version (3.1).
pub const SCHEMA: &str = "rahi.binding/v0";
/// The deployer's digest-pinned image reference (B-5).
pub const ENV_ARTIFACT_IMAGE: &str = "RAHI_ARTIFACT_IMAGE";
/// The pod name, from the downward API (B-4).
pub const ENV_POD_NAME: &str = "RAHI_POD_NAME";
/// The Rauthy source image the packaged build declares (B-5, B-12): the
/// image recipe sets it from the same argument it copies Rauthy from.
pub const ENV_RAUTHY_IMAGE: &str = "RAHI_RAUTHY_IMAGE";
/// The composition parameter that supplies the application's revision
/// (B-12), as an absence names it.
pub const APP_REVISION_SOURCE: &str = "app_revision";
/// The spec that will populate the reserved epoch members (B-7).
pub const EPOCH_SOURCE: &str = "041-deployment-epochs";
/// The app store's layout name: `Config::hiqlite_dir()`'s last component
/// (B-3). Relative, so no host path is exposed.
pub const STORE_LAYOUT: &str = "app-store";
/// The hiqlite package set the app store is compiled from (B-5), declared.
/// `tests::the_hiqlite_set_is_the_locked_one` holds it to `Cargo.lock`.
pub const HIQLITE_PACKAGES: [(&str, &str); 3] = [
    ("hiqlite-derive-patched", "0.15.0-patched.3"),
    ("hiqlite-patched", "0.15.0-patched.3"),
    ("hiqlite-wal-patched", "0.15.0-patched.3"),
];
/// The sampler the tracer uses: the SDK default, which keeps every span.
pub const SAMPLER: &str = "always_on";

/// The closed set of bases (B-1).
pub const BASES: [&str; 4] = ["measured", "declared", "minted", "absent"];
/// The closed set of absence reasons (3.1).
pub const REASONS: [&str; 5] = [
    "not_implemented",
    "not_declared",
    "unreadable",
    "unmapped",
    "not_applicable",
];

/// A present value and its basis.
fn present(value: impl Into<Value>, basis: &str) -> Value {
    json!({ "value": value.into(), "basis": basis })
}

/// An absence, its reason, and the chassis-defined name it concerns.
fn absent(reason: &str, source: Option<&str>) -> Value {
    let mut wrapper = Map::new();
    wrapper.insert("basis".to_owned(), Value::from("absent"));
    wrapper.insert("reason".to_owned(), Value::from(reason));
    if let Some(source) = source {
        wrapper.insert("source".to_owned(), Value::from(source));
    }
    Value::Object(wrapper)
}

/// `sha256:` and 64 lowercase hex characters.
fn hex_digest(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(71);
    out.push_str("sha256:");
    for byte in digest(&SHA256, bytes).as_ref() {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// B-2: sha256 over the bytes read by opening the path `current_exe()`
/// returns. A file read by pathname, not a measurement of the running text
/// and not an attestation. On failure the wrapper is `absent`/`unreadable`
/// and the second value is the boot-log line that carries the detail.
#[must_use]
pub fn measure_executable() -> (Value, Option<String>) {
    let read = std::env::current_exe().and_then(|path| match std::fs::read(&path) {
        Ok(bytes) => Ok(bytes),
        Err(err) => Err(std::io::Error::new(
            err.kind(),
            format!("{}: {err}", path.display()),
        )),
    });
    match read {
        Ok(bytes) => (present(hex_digest(&bytes), "measured"), None),
        Err(err) => (
            absent("unreadable", None),
            Some(format!(
                "binding: the executable could not be read, so build.binary.sha256 is absent: {err}"
            )),
        ),
    }
}

/// B-2: the OCI platform for a Rust `(os, arch)`, through one closed table.
/// A target with no entry is `absent`/`unmapped`; no Rust spelling reaches
/// the document.
#[must_use]
pub fn platform(os: &str, arch: &str) -> Value {
    let os = match os {
        "linux" => "linux",
        "macos" => "darwin",
        _ => return absent("unmapped", None),
    };
    let arch = match arch {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        _ => return absent("unmapped", None),
    };
    present(format!("{os}/{arch}"), "declared")
}

/// B-4: `<node>-<32 lowercase hex>`, 128 bits drawn once from `fill`; the
/// prefix is `0` where the node is absent. An entropy failure is
/// [`Error::Config`], never a fallback value.
///
/// # Errors
///
/// [`Error::Config`] naming the entropy source when `fill` fails.
pub fn mint_instance<E: std::fmt::Display>(
    node: Option<u64>,
    fill: impl FnOnce(&mut [u8; 16]) -> std::result::Result<(), E>,
) -> Result<String> {
    let mut bytes = [0u8; 16];
    fill(&mut bytes).map_err(|err| {
        Error::Config(format!(
            "the operating system's entropy source failed, so this process cannot mint its \
             instance id: {err}"
        ))
    })?;
    let mut id = format!("{}-", node.unwrap_or(0));
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(id, "{byte:02x}");
    }
    Ok(id)
}

/// 3.1's reference syntax: `<repository>@sha256:<64 lowercase hex>`, or,
/// with `tagged`, also `<repository>:<tag>@sha256:<64 lowercase hex>`.
#[must_use]
pub fn is_image_reference(text: &str, tagged: bool) -> bool {
    let Some((name, hex)) = text.split_once("@sha256:") else {
        return false;
    };
    let digest_ok = hex.len() == 64
        && hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    let repository = match name.rsplit_once(':') {
        // A colon after the last slash is a tag; one before it is a port.
        Some((repo, tag)) if !tag.contains('/') => {
            if !tagged || tag.is_empty() {
                return false;
            }
            repo
        }
        _ => name,
    };
    digest_ok
        && !repository.is_empty()
        && repository
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-/:".contains(&b))
}

/// A declared image reference from `var`, or the absence its absence means.
fn declared_image(env: &dyn EnvReader, var: &str, tagged: bool, unset: Value) -> Result<Value> {
    match env.get(var) {
        None => Ok(unset),
        Some(text) if is_image_reference(&text, tagged) => Ok(present(text, "declared")),
        Some(_) => Err(Error::Config(format!(
            "{var} is not an image reference pinned by digest \
             (<repository>@sha256:<64 lowercase hex>)"
        ))),
    }
}

/// What the store says about itself at boot (B-3), both measured.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StoreFacts {
    /// The `app` set's version, from `schema_version`.
    pub schema_version: u32,
    /// Every other linked set's last recorded version, by name.
    pub migration_sets: BTreeMap<String, u32>,
}

/// Read [`StoreFacts`] from the open store: one read of every set's
/// recorded history.
///
/// # Errors
///
/// The store's error when the read fails.
pub async fn store_facts(store: &StoreHandle) -> Result<StoreFacts> {
    let histories = store.recorded_set_migrations().await?;
    let last = |set: &SetName| {
        histories
            .get(set)
            .and_then(|rows| rows.iter().map(|row| row.version).max())
            .unwrap_or(rahi_store::migrate::BASELINE_VERSION)
    };
    Ok(StoreFacts {
        schema_version: last(&SetName::app()),
        migration_sets: histories
            .keys()
            .filter(|set| !set.is_app())
            .map(|set| (set.as_str().to_owned(), last(set)))
            .collect(),
    })
}

/// What a booted cell knows that a bare binary does not (B-3, B-4).
#[derive(Clone, Debug)]
pub struct CellFacts {
    /// The hiqlite node id, declared by the deployment.
    pub node: u64,
    /// The booted manifest's hash, `sha256:<hex>`.
    pub manifest_hash: String,
    /// `[app] name`.
    pub app_name: String,
    /// `[app] org`.
    pub app_org: String,
    /// `[contract] version`.
    pub contract_version: String,
    /// The store's own account of its schema.
    pub store: StoreFacts,
}

/// The chassis's own telemetry configuration (B-9). Plain values, no basis.
#[derive(Clone, Debug)]
pub struct Observation {
    /// Whether spans leave the process; `None` outside a cell.
    pub export: Option<bool>,
    /// The trace ring's capacity; `0` outside a cell.
    pub ring_capacity: usize,
    /// The kernel's denial queue capacity; `0` outside a cell.
    pub queue_capacity: usize,
    /// The counters that count lost denials, by name.
    pub loss_counters: Vec<String>,
}

/// What [`assemble`] reads.
pub struct Inputs<'a> {
    /// The process environment.
    pub env: &'a dyn EnvReader,
    /// The application's revision, from the composer (B-12).
    pub app_revision: Option<&'a str>,
    /// The booted cell, or `None` for `rahi version --binding` (B-13).
    pub cell: Option<CellFacts>,
    /// The telemetry configuration.
    pub observation: Observation,
}

impl std::fmt::Debug for Inputs<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Inputs")
            .field("app_revision", &self.app_revision)
            .field("cell", &self.cell)
            .field("observation", &self.observation)
            .finish_non_exhaustive()
    }
}

/// The assembled document: its value, the bytes `/binding` serves, and the
/// boot-log lines its absences owe.
#[derive(Clone, Debug)]
pub struct Binding {
    document: Value,
    bytes: Vec<u8>,
    log: Vec<String>,
}

impl Binding {
    /// The document.
    #[must_use]
    pub const fn document(&self) -> &Value {
        &self.document
    }

    /// The serialized document, byte-identical for the life of the process.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The lines the boot log carries for the document's absences.
    #[must_use]
    pub fn log(&self) -> &[String] {
        &self.log
    }

    /// `build.rahi_version`, for `rahi_build_info` (B-11).
    #[must_use]
    pub fn rahi_version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }
}

/// Assemble the document once (B-6): measure the executable, mint the
/// instance id, and read every declared value.
///
/// # Errors
///
/// [`Error::Config`] when [`ENV_ARTIFACT_IMAGE`] or [`ENV_RAUTHY_IMAGE`] is
/// set and malformed, or when the entropy source fails (B-4).
pub fn assemble(inputs: &Inputs<'_>) -> Result<Binding> {
    let (binary_sha256, binary_log) = measure_executable();
    assemble_with(inputs, binary_sha256, |bytes: &mut [u8; 16]| {
        getrandom::fill(bytes)
    })
    .map(|mut binding| {
        binding.log.extend(binary_log);
        binding
    })
}

/// [`assemble`] over an already measured executable and an entropy source,
/// so tests can drive both.
///
/// # Errors
///
/// As [`assemble`].
pub fn assemble_with<E: std::fmt::Display>(
    inputs: &Inputs<'_>,
    binary_sha256: Value,
    fill: impl FnOnce(&mut [u8; 16]) -> std::result::Result<(), E>,
) -> Result<Binding> {
    let env = inputs.env;
    let cell = inputs.cell.as_ref();
    let not_applicable = || absent("not_applicable", None);
    let artifact = declared_image(
        env,
        ENV_ARTIFACT_IMAGE,
        false,
        absent("not_declared", Some(ENV_ARTIFACT_IMAGE)),
    )?;
    let rauthy = match cell {
        Some(_) => declared_image(env, ENV_RAUTHY_IMAGE, true, not_applicable())?,
        None => not_applicable(),
    };
    let started = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let id = mint_instance(cell.map(|c| c.node), fill)?;

    let hiqlite: Map<String, Value> = HIQLITE_PACKAGES
        .iter()
        .map(|(name, version)| ((*name).to_owned(), Value::from(*version)))
        .collect();
    let epoch = || absent("not_implemented", Some(EPOCH_SOURCE));
    let unless = |text: Option<&str>| match text {
        Some(text) => Value::from(text),
        None => Value::from("not_applicable"),
    };

    let document = json!({
        "schema": SCHEMA,
        "instance": {
            "id": present(id, "minted"),
            "node": cell.map_or_else(not_applicable, |c| present(c.node, "declared")),
            "pod": env.get(ENV_POD_NAME).filter(|pod| !pod.is_empty()).map_or_else(
                || absent("not_declared", Some(ENV_POD_NAME)),
                |pod| present(pod, "declared"),
            ),
            "started": present(started, "measured"),
        },
        "build": {
            "binary": {
                "sha256": binary_sha256,
                "platform": platform(std::env::consts::OS, std::env::consts::ARCH),
            },
            "rahi_version": present(env!("CARGO_PKG_VERSION"), "declared"),
            "revision": inputs.app_revision.filter(|rev| !rev.is_empty()).map_or_else(
                || absent("not_declared", Some(APP_REVISION_SOURCE)),
                |rev| present(rev, "declared"),
            ),
        },
        "manifest": {
            "hash": cell.map_or_else(not_applicable, |c| present(c.manifest_hash.clone(), "measured")),
            "app": {
                "name": cell.map_or_else(not_applicable, |c| present(c.app_name.clone(), "declared")),
                "org": cell.map_or_else(not_applicable, |c| present(c.app_org.clone(), "declared")),
            },
            "contract_version": cell.map_or_else(
                not_applicable,
                |c| present(c.contract_version.clone(), "declared"),
            ),
        },
        "artifact": { "image": artifact },
        "components": {
            "hiqlite": present(Value::Object(hiqlite), "declared"),
            "rauthy": { "image": rauthy },
        },
        "store": {
            "layout": cell.map_or_else(not_applicable, |_| present(STORE_LAYOUT, "declared")),
            "schema_version": cell.map_or_else(
                not_applicable,
                |c| present(c.store.schema_version, "measured"),
            ),
            "migration_sets": cell.map_or_else(not_applicable, |c| {
                present(
                    c.store
                        .migration_sets
                        .iter()
                        .map(|(name, version)| (name.clone(), Value::from(*version)))
                        .collect::<Map<String, Value>>(),
                    "measured",
                )
            }),
        },
        "epoch": { "ref": epoch(), "match": epoch() },
        "observation": {
            "traces": {
                "export": unless(inputs.observation.export.map(|on| if on { "on" } else { "off" })),
                "sampler": unless(cell.map(|_| SAMPLER)),
                "export_loss": "uncounted",
                "ring_capacity": inputs.observation.ring_capacity,
            },
            "metrics": { "unobserved": ["/metrics", "/binding", "unmatched routes"] },
            "decisions": {
                "allows": "not recorded",
                "queue_capacity": inputs.observation.queue_capacity,
                "loss_counters": inputs.observation.loss_counters,
            },
        },
    });
    let bytes = serde_json::to_vec(&document).map_err(|err| {
        Error::Integrity(format!("the binding document does not serialize: {err}"))
    })?;
    Ok(Binding {
        document,
        bytes,
        log: Vec::new(),
    })
}

/// Every wrapper path section 3.1's table lists, in its order.
pub const WRAPPER_PATHS: [&str; 21] = [
    "instance.id",
    "instance.node",
    "instance.pod",
    "instance.started",
    "build.binary.sha256",
    "build.binary.platform",
    "build.rahi_version",
    "build.revision",
    "manifest.hash",
    "manifest.app.name",
    "manifest.app.org",
    "manifest.contract_version",
    "artifact.image",
    "components.hiqlite",
    "components.rauthy.image",
    "store.layout",
    "store.schema_version",
    "store.migration_sets",
    "epoch.ref",
    "epoch.match",
    "observation",
];

/// Check `document` against section 3.1 (FR-001, AC-4): `schema` names
/// this major; every listed path is present; every wrapper is in exactly
/// one of the two shapes, with no `null` value, a basis from the closed
/// set, and an absence reason from the closed set; and no member outside
/// `schema` and `observation` is a bare scalar or an unlisted object.
///
/// # Errors
///
/// A message naming the first path that does not conform.
pub fn validate(document: &Value) -> std::result::Result<(), String> {
    if document.get("schema").and_then(Value::as_str) != Some(SCHEMA) {
        return Err(format!("schema is not {SCHEMA:?}"));
    }
    for path in WRAPPER_PATHS {
        let node = path
            .split('.')
            .try_fold(document, |node, step| node.get(step))
            .ok_or_else(|| format!("{path} is missing"))?;
        if path == "observation" {
            if !node.is_object() {
                return Err("observation is not an object".to_owned());
            }
            continue;
        }
        check_wrapper(path, node)?;
    }
    // Every leaf outside the wrappers and the two plain members is a
    // wrapper the table lists: nothing else, and no bare scalar.
    let Value::Object(top) = document else {
        return Err("the document is not an object".to_owned());
    };
    for (key, value) in top {
        if key == "schema" || key == "observation" {
            continue;
        }
        groups(key, value)?;
    }
    Ok(())
}

/// Walk a grouping object down to its wrappers.
fn groups(path: &str, node: &Value) -> std::result::Result<(), String> {
    if WRAPPER_PATHS.contains(&path) {
        return Ok(());
    }
    match node {
        // A grouping object holds no `basis`; one that does is a wrapper
        // at a path the table does not list.
        Value::Object(members) if !members.contains_key("basis") => {
            for (key, value) in members {
                groups(&format!("{path}.{key}"), value)?;
            }
            Ok(())
        }
        _ => {
            // An unknown member is allowed (3.1's evolution rule), but it
            // is still a wrapper, never a bare scalar.
            if node.is_object() {
                check_wrapper(path, node)
            } else {
                Err(format!("{path} is a bare value, not a wrapper"))
            }
        }
    }
}

/// One wrapper, in exactly one of 3.1's two shapes.
fn check_wrapper(path: &str, node: &Value) -> std::result::Result<(), String> {
    let Value::Object(wrapper) = node else {
        return Err(format!("{path} is not a wrapper"));
    };
    let basis = wrapper
        .get("basis")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{path} has no basis"))?;
    if !BASES.contains(&basis) {
        return Err(format!(
            "{path} has basis {basis:?}, outside the closed set"
        ));
    }
    if basis == "absent" {
        let reason = wrapper
            .get("reason")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("{path} is absent with no reason"))?;
        if !REASONS.contains(&reason) {
            return Err(format!(
                "{path} has reason {reason:?}, outside the closed set"
            ));
        }
        if wrapper.contains_key("value") {
            return Err(format!("{path} is absent and carries a value"));
        }
        if wrapper
            .keys()
            .any(|k| !["basis", "reason", "source"].contains(&k.as_str()))
        {
            return Err(format!("{path} carries a member an absence does not have"));
        }
        if wrapper.get("source").is_some_and(|s| !s.is_string()) {
            return Err(format!("{path} has a source that is not a name"));
        }
    } else {
        match wrapper.get("value") {
            None | Some(Value::Null) => return Err(format!("{path} has no value")),
            Some(_) => {}
        }
        if wrapper.contains_key("reason") {
            return Err(format!("{path} carries a reason and is not absent"));
        }
        if wrapper
            .keys()
            .any(|k| !["basis", "value"].contains(&k.as_str()))
        {
            return Err(format!("{path} carries a member a value does not have"));
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn observation() -> Observation {
        Observation {
            export: Some(false),
            ring_capacity: 1000,
            queue_capacity: 1024,
            loss_counters: vec!["kernel_decisions_dropped_total".to_owned()],
        }
    }

    fn cell() -> CellFacts {
        CellFacts {
            node: 2,
            manifest_hash: format!("sha256:{}", "a".repeat(64)),
            app_name: "hello-cell".to_owned(),
            app_org: "statecrafting".to_owned(),
            contract_version: "1.0.0".to_owned(),
            store: StoreFacts::default(),
        }
    }

    #[test]
    fn the_platform_vocabulary_is_oci() {
        assert_eq!(platform("linux", "aarch64")["value"], "linux/arm64");
        assert_eq!(platform("linux", "x86_64")["value"], "linux/amd64");
        let unmapped = platform("linux", "riscv64");
        assert_eq!(unmapped["basis"], "absent");
        assert_eq!(unmapped["reason"], "unmapped");
        assert!(unmapped.get("value").is_none());
    }

    #[test]
    fn the_instance_draws_128_bits_and_an_entropy_failure_is_a_config_error() {
        let id = mint_instance(Some(3), |bytes: &mut [u8; 16]| {
            *bytes = [0xab; 16];
            Ok::<(), String>(())
        })
        .unwrap();
        assert_eq!(id, format!("3-{}", "ab".repeat(16)));
        let failed = mint_instance(Some(3), |_: &mut [u8; 16]| Err("no entropy"));
        assert!(matches!(failed, Err(Error::Config(m)) if m.contains("entropy")));
        let outside = mint_instance::<String>(None, |_| Ok(())).unwrap();
        assert!(outside.starts_with("0-"));
    }

    #[test]
    fn image_references_are_pinned_by_digest() {
        let hex = "0".repeat(64);
        assert!(is_image_reference(
            &format!("ghcr.io/a/b@sha256:{hex}"),
            false
        ));
        assert!(is_image_reference(
            &format!("localhost:5000/a@sha256:{hex}"),
            false
        ));
        assert!(!is_image_reference(
            &format!("ghcr.io/a/b:1@sha256:{hex}"),
            false
        ));
        assert!(is_image_reference(
            &format!("ghcr.io/a/b:1@sha256:{hex}"),
            true
        ));
        assert!(!is_image_reference("ghcr.io/a/b:latest", true));
        assert!(!is_image_reference(
            &format!("ghcr.io/a/b@sha256:{}", "A".repeat(64)),
            false
        ));
    }

    #[test]
    fn a_malformed_artifact_image_is_a_config_error_naming_it() {
        let env = BTreeMap::from([(ENV_ARTIFACT_IMAGE.to_owned(), "ghcr.io/a:latest".to_owned())]);
        let inputs = Inputs {
            env: &env,
            app_revision: None,
            cell: Some(cell()),
            observation: observation(),
        };
        let err = assemble_with(&inputs, absent("unreadable", None), |_: &mut [u8; 16]| {
            Ok::<(), String>(())
        })
        .unwrap_err();
        assert!(matches!(err, Error::Config(m) if m.contains(ENV_ARTIFACT_IMAGE)));
    }

    #[test]
    fn an_assembled_document_validates_and_states_its_absences() {
        let env = BTreeMap::<String, String>::new();
        let inputs = Inputs {
            env: &env,
            app_revision: None,
            cell: Some(cell()),
            observation: observation(),
        };
        let binding = assemble(&inputs).unwrap();
        validate(binding.document()).unwrap();
        let doc = binding.document();
        assert_eq!(doc["build"]["revision"]["source"], APP_REVISION_SOURCE);
        assert_eq!(doc["artifact"]["image"]["reason"], "not_declared");
        assert_eq!(
            doc["components"]["rauthy"]["image"]["reason"],
            "not_applicable"
        );
        assert_eq!(doc["epoch"]["ref"]["source"], EPOCH_SOURCE);
        assert_eq!(serde_json::to_vec(doc).unwrap(), binding.bytes());
    }

    #[test]
    fn the_validator_refuses_a_bare_scalar_a_null_and_an_open_reason() {
        let env = BTreeMap::<String, String>::new();
        let inputs = Inputs {
            env: &env,
            app_revision: Some("abc"),
            cell: None,
            observation: observation(),
        };
        let good = assemble(&inputs).unwrap().document().clone();
        validate(&good).unwrap();
        let mut bare = good.clone();
        bare["build"]["rahi_version"] = Value::from("0.4.0");
        assert!(validate(&bare).is_err());
        let mut null = good.clone();
        null["build"]["revision"]["value"] = Value::Null;
        assert!(validate(&null).is_err());
        let mut reason = good.clone();
        reason["artifact"]["image"]["reason"] = Value::from("ENOENT: /usr/bin/rahi");
        assert!(validate(&reason).is_err());
        let mut missing = good;
        missing["store"].as_object_mut().unwrap().remove("layout");
        assert!(validate(&missing).is_err());
    }

    /// B-5: the declared set is the one the workspace locks. The derive
    /// crate is part of the patched.3 release set but no feature this
    /// workspace enables compiles it, so the lock holds the other two
    /// (D-5).
    #[test]
    fn the_hiqlite_set_is_the_locked_one() {
        let lock = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock"),
        )
        .unwrap();
        let locked = |name: &str| {
            let entry = format!("name = \"{name}\"\nversion = \"");
            lock.find(&entry).map(|at| {
                let rest = &lock[at + entry.len()..];
                rest[..rest.find('"').unwrap()].to_owned()
            })
        };
        for (name, version) in HIQLITE_PACKAGES {
            match locked(name) {
                Some(found) => assert_eq!(found, version, "{name}"),
                None => assert_eq!(name, "hiqlite-derive-patched", "{name} is not locked"),
            }
        }
        assert_eq!(
            locked("hiqlite-patched").as_deref(),
            Some(HIQLITE_PACKAGES[1].1)
        );
    }
}
