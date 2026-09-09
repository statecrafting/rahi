//! The backup archive format (B-5): one tar stream, sealed with age.
//!
//! An archive holds four kinds of part under four fixed directories, plus a
//! manifest that names every part by its sha256. The manifest is what makes
//! a restore refuse a tampered or truncated part before it writes anything
//! (B-6): the archive is opened whole, every hash is recomputed, and only a
//! set that matches its manifest is handed to the caller.
//!
//! The tar carries no owner, no mode, and a zero mtime for every entry, so
//! that two archives of the same parts are byte-identical before sealing;
//! the age layer is what varies between runs, by design.

use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};

use serde::{Deserialize, Serialize};

use rahi_types::{Error, Result};

/// The archive name prefix (B-5).
pub const NAME_PREFIX: &str = "rahi-backup-";

/// The archive name suffix (B-5).
pub const NAME_SUFFIX: &str = ".tar.age";

/// The manifest's path inside the archive.
pub const MANIFEST_PATH: &str = "manifest.json";

/// The app store's snapshot directory inside the archive.
pub const APP_DIR: &str = "app-hiqlite";

/// rauthy's snapshot directory inside the archive.
pub const RAUTHY_DIR: &str = "rauthy";

/// The key material directory inside the archive.
pub const KEYS_DIR: &str = "keys";

/// The format this crate writes; a reader refuses any other.
pub const FORMAT: u32 = 1;

/// One file inside the archive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    /// The path inside the archive, `<dir>/<file>`.
    pub path: String,
    /// The bytes.
    pub bytes: Vec<u8>,
}

impl Part {
    /// A part at `dir/name`.
    #[must_use]
    pub fn new(dir: &str, name: &str, bytes: Vec<u8>) -> Self {
        Self {
            path: format!("{dir}/{name}"),
            bytes,
        }
    }

    /// The directory the part is filed under.
    #[must_use]
    pub fn dir(&self) -> &str {
        self.path.split('/').next().unwrap_or("")
    }

    /// The file name inside its directory.
    #[must_use]
    pub fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or("")
    }
}

/// The versions the archive records.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Versions {
    /// The chassis version that wrote the archive.
    pub rahi: String,
    /// `rahi_types::STORE_SCHEMA_VERSION` at the time.
    pub store_schema: String,
    /// `rahi_types::LEDGER_SCHEMA_VERSION` at the time.
    pub ledger_schema: String,
}

/// `manifest.json`: what the archive holds and what each part hashes to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveManifest {
    /// [`FORMAT`].
    pub format: u32,
    /// When the archive was taken, seconds since the epoch.
    pub created: u64,
    /// The versions in play.
    pub versions: Versions,
    /// The booted manifest's hash, so a restore can be checked against the
    /// cell it is restored into.
    pub manifest_hash: String,
    /// Every part's path and its sha256, hex.
    pub parts: BTreeMap<String, String>,
}

impl ArchiveManifest {
    /// A manifest over `parts`.
    #[must_use]
    pub fn over(parts: &[Part], created: u64, manifest_hash: String) -> Self {
        Self {
            format: FORMAT,
            created,
            versions: Versions {
                rahi: env!("CARGO_PKG_VERSION").to_owned(),
                store_schema: rahi_types::STORE_SCHEMA_VERSION.to_owned(),
                ledger_schema: rahi_types::LEDGER_SCHEMA_VERSION.to_owned(),
            },
            manifest_hash,
            parts: parts
                .iter()
                .map(|p| (p.path.clone(), sha256_hex(&p.bytes)))
                .collect(),
        }
    }

    /// The archive is complete: every directory B-5 names has at least one
    /// part.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] naming the first directory with no part.
    pub fn check_complete(&self) -> Result<()> {
        for dir in [APP_DIR, RAUTHY_DIR, KEYS_DIR] {
            let prefix = format!("{dir}/");
            if !self.parts.keys().any(|k| k.starts_with(&prefix)) {
                return Err(Error::Validation(format!(
                    "backup is missing its {dir}/ part; a partial archive is never written"
                )));
            }
        }
        Ok(())
    }
}

/// The archive name for a backup taken at `created`.
#[must_use]
pub fn archive_name(created: u64) -> String {
    format!("{NAME_PREFIX}{}{NAME_SUFFIX}", crate::utc_stamp(created))
}

/// sha256 of `bytes`, lowercase hex.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    digest.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// Seal `parts` and their `manifest` to `recipient`: the bytes of one
/// `.tar.age` file.
///
/// # Errors
///
/// [`Error::Validation`] when the manifest is incomplete or does not match
/// the parts; [`Error::Io`] when the stream cannot be written.
pub fn seal(
    parts: &[Part],
    manifest: &ArchiveManifest,
    recipient: &age::x25519::Recipient,
) -> Result<Vec<u8>> {
    manifest.check_complete()?;
    for part in parts {
        match manifest.parts.get(&part.path) {
            Some(hash) if *hash == sha256_hex(&part.bytes) => {}
            _ => {
                return Err(Error::Validation(format!(
                    "part {} is not what the manifest names",
                    part.path
                )));
            }
        }
    }
    let manifest_bytes = serde_json::to_vec_pretty(manifest)
        .map_err(|err| Error::Io(format!("the archive manifest cannot be serialised: {err}")))?;

    let mut tar = tar::Builder::new(Vec::new());
    append(&mut tar, MANIFEST_PATH, &manifest_bytes)?;
    for part in parts {
        append(&mut tar, &part.path, &part.bytes)?;
    }
    let plain = tar
        .into_inner()
        .map_err(|err| Error::Io(format!("the archive cannot be finished: {err}")))?;

    let encryptor =
        age::Encryptor::with_recipients(std::iter::once(recipient as &dyn age::Recipient))
            .map_err(|err| Error::Config(format!("the backup recipient is unusable: {err}")))?;
    let mut sealed = Vec::with_capacity(plain.len() + 512);
    let mut writer = encryptor
        .wrap_output(&mut sealed)
        .map_err(|err| Error::Io(format!("the archive header cannot be written: {err}")))?;
    writer
        .write_all(&plain)
        .and_then(|()| writer.finish().map(|_| ()))
        .map_err(|err| Error::Io(format!("the archive cannot be sealed: {err}")))?;
    Ok(sealed)
}

/// Open a sealed archive with `identity`, verifying every part against the
/// manifest before anything is returned.
///
/// # Errors
///
/// [`Error::Unauthorized`] when the identity does not open the archive;
/// [`Error::Integrity`] when a part is missing, extra, or does not hash to
/// what the manifest names; [`Error::Validation`] when the manifest is not
/// [`FORMAT`] or is incomplete; [`Error::Io`] on a malformed stream.
pub fn open(
    sealed: &[u8],
    identity: &age::x25519::Identity,
) -> Result<(ArchiveManifest, Vec<Part>)> {
    let decryptor = age::Decryptor::new(sealed)
        .map_err(|err| Error::Io(format!("not an age archive: {err}")))?;
    let mut reader = decryptor
        .decrypt(std::iter::once(identity as &dyn age::Identity))
        .map_err(|err| match err {
            age::DecryptError::NoMatchingKeys => {
                Error::Unauthorized("the backup key does not open this archive".to_owned())
            }
            other => Error::Io(format!("the archive cannot be opened: {other}")),
        })?;
    let mut plain = Vec::new();
    reader
        .read_to_end(&mut plain)
        .map_err(|err| Error::Integrity(format!("the archive body is damaged: {err}")))?;

    let mut manifest: Option<ArchiveManifest> = None;
    let mut parts = Vec::new();
    let mut tar = tar::Archive::new(plain.as_slice());
    let entries = tar
        .entries()
        .map_err(|err| Error::Io(format!("the archive is not a tar stream: {err}")))?;
    for entry in entries {
        let mut entry = entry.map_err(|err| Error::Io(format!("a tar entry is damaged: {err}")))?;
        let path = entry
            .path()
            .map_err(|err| Error::Io(format!("a tar entry has no readable path: {err}")))?
            .to_string_lossy()
            .into_owned();
        let mut bytes = Vec::new();
        entry
            .read_to_end(&mut bytes)
            .map_err(|err| Error::Io(format!("tar entry {path} is damaged: {err}")))?;
        if path == MANIFEST_PATH {
            manifest = Some(serde_json::from_slice(&bytes).map_err(|err| {
                Error::Validation(format!("the archive manifest does not parse: {err}"))
            })?);
        } else {
            parts.push(Part { path, bytes });
        }
    }
    let manifest = manifest
        .ok_or_else(|| Error::Integrity("the archive carries no manifest.json".to_owned()))?;
    if manifest.format != FORMAT {
        return Err(Error::Validation(format!(
            "archive format {} is not the {FORMAT} this build reads",
            manifest.format
        )));
    }
    manifest.check_complete()?;
    for part in &parts {
        match manifest.parts.get(&part.path) {
            None => {
                return Err(Error::Integrity(format!(
                    "part {} is not named by the manifest",
                    part.path
                )));
            }
            Some(expected) if *expected != sha256_hex(&part.bytes) => {
                return Err(Error::Integrity(format!(
                    "part {} does not hash to what the manifest names; refusing before anything is written",
                    part.path
                )));
            }
            Some(_) => {}
        }
    }
    for named in manifest.parts.keys() {
        if !parts.iter().any(|p| p.path == *named) {
            return Err(Error::Integrity(format!(
                "part {named} is named by the manifest but absent"
            )));
        }
    }
    Ok((manifest, parts))
}

fn append(tar: &mut tar::Builder<Vec<u8>>, path: &str, bytes: &[u8]) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o600);
    header.set_mtime(0);
    header.set_uid(0);
    header.set_gid(0);
    tar.append_data(&mut header, path, bytes)
        .map_err(|err| Error::Io(format!("the archive entry {path} cannot be written: {err}")))
}
