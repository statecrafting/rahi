//! The backup verb (B-5): one archive, or nothing.
//!
//! Every part is gathered into memory before a single byte of archive
//! exists: the app's snapshot through `Store::backup`, rauthy's through its
//! API, the key directory, and the manifest over all three. A part that
//! cannot be had fails the verb, and the destination is untouched (FR-001).
//! The archive is then sealed to the backup recipient and written to a
//! directory or uploaded to a bucket.

use std::path::{Path, PathBuf};

use rahi_ledger::{Archive as _, S3Archive, S3Config};
use rahi_store::Store;
use rahi_types::{Config, Error, Result};

use crate::KeySet;
use crate::archive::{self, APP_DIR, ArchiveManifest, KEYS_DIR, Part, RAUTHY_DIR};
use crate::rauthy_api::RauthyApi;

/// The scheme `--to` uses for a bucket.
pub const S3_SCHEME: &str = "s3://";

/// Environment variables the S3 destination reads its credentials from.
///
/// Spec 010 B-7 keeps object-store credentials out of `Config`; spec 032
/// provisions these in the cluster.
pub const ENV_S3_ENDPOINT: &str = "RAHI_BACKUP_S3_ENDPOINT";
/// See [`ENV_S3_ENDPOINT`].
pub const ENV_S3_REGION: &str = "RAHI_BACKUP_S3_REGION";
/// See [`ENV_S3_ENDPOINT`].
pub const ENV_S3_ACCESS_KEY: &str = "RAHI_BACKUP_S3_ACCESS_KEY";
/// See [`ENV_S3_ENDPOINT`].
pub const ENV_S3_SECRET_KEY: &str = "RAHI_BACKUP_S3_SECRET_KEY";
/// See [`ENV_S3_ENDPOINT`]; `true` for path-style addressing.
pub const ENV_S3_PATH_STYLE: &str = "RAHI_BACKUP_S3_PATH_STYLE";

/// Where an archive goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Destination {
    /// A directory on disk; the archive is written atomically inside it.
    Dir(PathBuf),
    /// `s3://bucket/prefix`: the archive is uploaded under the prefix.
    S3 {
        /// The bucket.
        bucket: String,
        /// The key prefix, possibly empty.
        prefix: String,
    },
}

impl Destination {
    /// Parse a `--to` argument.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when an `s3://` value names no bucket.
    pub fn parse(raw: &str) -> Result<Self> {
        match raw.strip_prefix(S3_SCHEME) {
            None => Ok(Self::Dir(PathBuf::from(raw))),
            Some(rest) => {
                let (bucket, prefix) = rest.split_once('/').unwrap_or((rest, ""));
                if bucket.is_empty() {
                    return Err(Error::Validation(format!("{raw} names no bucket")));
                }
                Ok(Self::S3 {
                    bucket: bucket.to_owned(),
                    prefix: prefix.trim_matches('/').to_owned(),
                })
            }
        }
    }

    /// The default destination: the volume's backups directory.
    #[must_use]
    pub fn default_for(config: &Config) -> Self {
        Self::Dir(crate::backups_dir(config))
    }
}

/// What a backup produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    /// The archive's file name.
    pub name: String,
    /// Where it landed: a path, or `s3://bucket/key`.
    pub location: String,
    /// The sealed size in bytes.
    pub bytes: u64,
    /// The manifest that went in.
    pub manifest: ArchiveManifest,
}

/// Gather every part of a backup, or fail with nothing written.
///
/// `manifest_hash` is the booted manifest's hash, recorded so a restore can
/// be checked against the cell it lands in.
///
/// # Errors
///
/// A store failure as itself; [`Error::Upstream`] or [`Error::Unauthorized`]
/// from rauthy; [`Error::Io`] when a snapshot or key file cannot be read.
pub async fn gather(
    store: &Store,
    rauthy: &RauthyApi,
    keys: &KeySet,
    manifest_hash: &str,
    created: u64,
) -> Result<(ArchiveManifest, Vec<Part>)> {
    let mut parts = Vec::new();

    let app_id = store.backup().await?;
    let app_path = store.config().backup_dir().join(app_id.as_str());
    let app_bytes = tokio::fs::read(&app_path).await.map_err(|err| {
        Error::Io(format!(
            "the app snapshot {} cannot be read: {err}",
            app_path.display()
        ))
    })?;
    parts.push(Part::new(APP_DIR, app_id.as_str(), app_bytes));

    let (rauthy_name, rauthy_bytes) = rauthy.backup().await?;
    parts.push(Part::new(RAUTHY_DIR, &rauthy_name, rauthy_bytes));

    for (name, bytes) in keys.export()? {
        parts.push(Part::new(KEYS_DIR, &name, bytes));
    }

    let manifest = ArchiveManifest::over(&parts, created, manifest_hash.to_owned());
    manifest.check_complete()?;
    Ok((manifest, parts))
}

/// Take one backup and deliver it (B-5).
///
/// # Errors
///
/// As [`gather`], plus [`Error::Io`] when the destination cannot be written
/// and [`Error::Config`] when an S3 destination has no credentials.
pub async fn run(
    store: &Store,
    rauthy: &RauthyApi,
    keys: &KeySet,
    manifest_hash: &str,
    to: &Destination,
    env: &dyn rahi_types::EnvReader,
) -> Result<Outcome> {
    let created = crate::unix_now();
    let (manifest, parts) = gather(store, rauthy, keys, manifest_hash, created).await?;
    let recipient = keys.backup_recipient()?;
    let sealed = archive::seal(&parts, &manifest, &recipient)?;
    let name = archive::archive_name(created);
    let bytes = sealed.len() as u64;
    let location = deliver(to, &name, sealed, env).await?;
    Ok(Outcome {
        name,
        location,
        bytes,
        manifest,
    })
}

/// Write `sealed` as `name` to `to`; returns where it landed.
///
/// # Errors
///
/// [`Error::Io`] when a directory cannot be written; [`Error::Config`] when
/// the S3 credentials are absent; the bucket's own error on upload.
pub async fn deliver(
    to: &Destination,
    name: &str,
    sealed: Vec<u8>,
    env: &dyn rahi_types::EnvReader,
) -> Result<String> {
    match to {
        Destination::Dir(dir) => {
            let path = write_atomic(dir, name, &sealed).await?;
            Ok(path.display().to_string())
        }
        Destination::S3 { bucket, prefix } => {
            let cfg = s3_config(bucket, env)?;
            let key = if prefix.is_empty() {
                name.to_owned()
            } else {
                format!("{prefix}/{name}")
            };
            S3Archive::open(&cfg)?.put(&key, sealed).await?;
            Ok(format!("{S3_SCHEME}{bucket}/{key}"))
        }
    }
}

async fn write_atomic(dir: &Path, name: &str, bytes: &[u8]) -> Result<PathBuf> {
    tokio::fs::create_dir_all(dir).await.map_err(|err| {
        Error::Io(format!(
            "backup directory {} cannot be created: {err}",
            dir.display()
        ))
    })?;
    let path = dir.join(name);
    let tmp = dir.join(format!("{name}.partial"));
    tokio::fs::write(&tmp, bytes).await.map_err(|err| {
        Error::Io(format!(
            "archive {} cannot be written: {err}",
            tmp.display()
        ))
    })?;
    crate::set_mode(&tmp, 0o600)?;
    tokio::fs::rename(&tmp, &path).await.map_err(|err| {
        Error::Io(format!(
            "archive {} cannot be moved into place: {err}",
            path.display()
        ))
    })?;
    Ok(path)
}

fn s3_config(bucket: &str, env: &dyn rahi_types::EnvReader) -> Result<S3Config> {
    let need = |key: &str| {
        env.get(key)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| Error::Config(format!("{key} is required for an s3:// destination")))
    };
    Ok(S3Config {
        endpoint: need(ENV_S3_ENDPOINT)?,
        bucket: bucket.to_owned(),
        region: env
            .get(ENV_S3_REGION)
            .unwrap_or_else(|| "us-east-1".to_owned()),
        access_key: need(ENV_S3_ACCESS_KEY)?,
        secret_key: need(ENV_S3_SECRET_KEY)?,
        path_style: env
            .get(ENV_S3_PATH_STYLE)
            .is_some_and(|v| v.eq_ignore_ascii_case("true") || v == "1"),
    })
}
