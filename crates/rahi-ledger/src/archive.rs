//! The object store sealed segments are written to (spec 014 B-3, B-5).
//!
//! The trait is three verbs wide on purpose. An archive is append-only
//! storage of immutable bodies: nothing in this crate deletes or overwrites
//! one, so there is no `delete`, and [`Archive::put`] to a key that already
//! exists is [`Error::Conflict`] rather than a silent replacement (B-5). An
//! archive that cannot express that refusal is not an archive; it is a
//! directory.
//!
//! Two implementations ship: [`S3Archive`] against any S3-compatible
//! endpoint, and [`FsArchive`] against a directory, which is what the tests
//! and a single-node development cell use.
//!
//! This is not the backup surface (B-6). Store snapshots are hiqlite's, taken
//! by [`rahi_store::StoreHandle::backup`] into the operator's backup bucket,
//! and they answer a different question: a snapshot restores a cell to
//! yesterday, an archive proves what the cell decided three years ago. The
//! two never share a prefix and never read each other's objects.

use std::path::{Component, Path, PathBuf};

use async_trait::async_trait;
use rahi_types::Error;
use s3_simple::{Bucket, BucketOptions, Credentials, Region, S3Error};
use serde::{Deserialize, Serialize};
use url::Url;

/// Immutable, append-only storage for sealed segment bodies.
///
/// Implementations are held behind `&dyn Archive`, so the ledger can be
/// handed a filesystem archive in a test and an S3 archive in a cell without
/// either being a type parameter of the chain.
#[async_trait]
pub trait Archive: Send + Sync + std::fmt::Debug {
    /// Write `bytes` at `key`, refusing to replace anything.
    ///
    /// # Errors
    ///
    /// [`Error::Conflict`] when `key` already holds an object (B-5);
    /// [`Error::Io`] or [`Error::Upstream`] when the write fails.
    async fn put(&self, key: &str, bytes: Vec<u8>) -> Result<(), Error>;

    /// Read the object at `key`.
    ///
    /// # Errors
    ///
    /// [`Error::NotFound`] when nothing is stored at `key`; [`Error::Io`] or
    /// [`Error::Upstream`] when the read fails.
    async fn get(&self, key: &str) -> Result<Vec<u8>, Error>;

    /// Every key under `prefix`, sorted.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] or [`Error::Upstream`] when the listing fails.
    async fn list(&self, prefix: &str) -> Result<Vec<String>, Error>;
}

/// An archive in a directory on the local filesystem.
///
/// Keys become relative paths under `root`, so `ledger/segments/a-b.json` is
/// a file two directories down. Writes are `create_new`, which is the
/// filesystem's own refusal to replace an existing object and needs no
/// read-then-write race of ours.
///
/// The calls are blocking `std::fs` inside `async fn`. That is the honest
/// shape for what this is for (tests and a single-node cell, spec 014 B-3):
/// a cell archiving to a real object store uses [`S3Archive`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FsArchive {
    root: PathBuf,
}

impl FsArchive {
    /// An archive rooted at `root`, which is created if it is absent.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the directory cannot be created.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, Error> {
        let root = root.into();
        std::fs::create_dir_all(&root)
            .map_err(|e| Error::Io(format!("archive root {}: {e}", root.display())))?;
        Ok(Self { root })
    }

    /// The directory this archive writes into.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolve a key to a path under the root.
    ///
    /// A key is a relative, `/`-separated name. Anything that could climb out
    /// of the root is refused here rather than trusted, because a key is
    /// built from decision ids and those come from callers.
    fn path_of(&self, key: &str) -> Result<PathBuf, Error> {
        if key.is_empty() || key.ends_with('/') {
            return Err(Error::Validation(format!(
                "{key:?} is not an archive key: a key names one object"
            )));
        }
        let relative = Path::new(key);
        for component in relative.components() {
            match component {
                Component::Normal(_) => {}
                _ => {
                    return Err(Error::Validation(format!(
                        "{key:?} is not an archive key: it leaves the archive root"
                    )));
                }
            }
        }
        Ok(self.root.join(relative))
    }

    /// Every key under the root, deepest paths included, sorted.
    fn walk(&self, dir: &Path, keys: &mut Vec<String>) -> Result<(), Error> {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(Error::Io(format!("archive list {}: {e}", dir.display()))),
        };
        for entry in entries {
            let entry =
                entry.map_err(|e| Error::Io(format!("archive list {}: {e}", dir.display())))?;
            let path = entry.path();
            if path.is_dir() {
                self.walk(&path, keys)?;
            } else if let Some(key) = key_of(&self.root, &path) {
                keys.push(key);
            }
        }
        Ok(())
    }
}

/// The key a path under `root` is stored at, or `None` when it is not under
/// the root or is not `/`-expressible.
fn key_of(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_str()?.to_owned()),
            _ => return None,
        }
    }
    Some(parts.join("/"))
}

#[async_trait]
impl Archive for FsArchive {
    async fn put(&self, key: &str, bytes: Vec<u8>) -> Result<(), Error> {
        use std::io::Write as _;

        let path = self.path_of(key)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::Io(format!("archive put {key}: {e}")))?;
        }
        let mut file = match std::fs::File::create_new(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(Error::Conflict(format!(
                    "archive key {key} already holds an object: a sealed segment is never \
                     rewritten"
                )));
            }
            Err(e) => return Err(Error::Io(format!("archive put {key}: {e}"))),
        };
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|e| Error::Io(format!("archive put {key}: {e}")))
    }

    async fn get(&self, key: &str) -> Result<Vec<u8>, Error> {
        let path = self.path_of(key)?;
        match std::fs::read(&path) {
            Ok(bytes) => Ok(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Error::NotFound(format!(
                "archive key {key} holds no object"
            ))),
            Err(e) => Err(Error::Io(format!("archive get {key}: {e}"))),
        }
    }

    async fn list(&self, prefix: &str) -> Result<Vec<String>, Error> {
        let mut keys = Vec::new();
        let root = self.root.clone();
        self.walk(&root, &mut keys)?;
        keys.retain(|key| key.starts_with(prefix));
        keys.sort();
        Ok(keys)
    }
}

/// What [`S3Archive`] needs to reach a bucket.
///
/// The chassis [`rahi_types::Config`] is derived from one public URL and
/// carries no object-store credentials (spec 010 B-7); provisioning these is
/// spec 031 and 032's, and spec 014 keeps it out of scope. They are declared
/// here, separately from [`rahi_store::S3Backup`], because an archive target
/// and a backup target are two different buckets answering two different
/// questions (B-6).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct S3Config {
    /// The endpoint URL, for example `https://s3.eu-central-1.amazonaws.com`.
    pub endpoint: String,
    /// The bucket segments are written to.
    pub bucket: String,
    /// The region.
    pub region: String,
    /// The access key id.
    pub access_key: String,
    /// The secret access key.
    pub secret_key: String,
    /// Use path-style addressing (MinIO and most self-hosted targets).
    pub path_style: bool,
}

impl std::fmt::Debug for S3Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Config")
            .field("endpoint", &self.endpoint)
            .field("bucket", &self.bucket)
            .field("region", &self.region)
            .field("access_key", &"<redacted>")
            .field("secret_key", &"<redacted>")
            .field("path_style", &self.path_style)
            .finish()
    }
}

/// An archive in an S3-compatible bucket.
///
/// The client is the one hiqlite already carries for its own backups, which
/// keeps one S3 implementation in the tree; the bucket is not.
pub struct S3Archive {
    bucket: Bucket,
}

impl std::fmt::Debug for S3Archive {
    /// Prints the bucket and its host. The credentials the bucket holds never
    /// reach a log line.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Archive")
            .field("bucket", &self.bucket.name)
            .field("host", &self.bucket.host.as_str())
            .finish()
    }
}

impl S3Archive {
    /// Open the bucket `cfg` names.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when the endpoint is not a URL or the bucket cannot
    /// be constructed from the credentials given.
    pub fn open(cfg: &S3Config) -> Result<Self, Error> {
        let host = Url::parse(&cfg.endpoint)
            .map_err(|e| Error::Config(format!("archive endpoint {:?}: {e}", cfg.endpoint)))?;
        let bucket = Bucket::new(
            host,
            cfg.bucket.clone(),
            Region::new(cfg.region.clone()),
            Credentials::new(cfg.access_key.clone(), cfg.secret_key.clone()),
            Some(BucketOptions {
                path_style: cfg.path_style,
                list_objects_v2: true,
            }),
        )
        .map_err(|e| Error::Config(format!("archive bucket {}: {e}", cfg.bucket)))?;
        Ok(Self { bucket })
    }

    /// Whether `key` already holds an object.
    async fn exists(&self, key: &str) -> Result<bool, Error> {
        match self.bucket.head(key).await {
            Ok(_) => Ok(true),
            Err(e) if status_of(&e) == Some(404) => Ok(false),
            Err(e) => Err(upstream(key, "head", &e)),
        }
    }
}

/// The HTTP status an S3 failure carries, when it carries one.
fn status_of(err: &S3Error) -> Option<u16> {
    match err {
        S3Error::HttpFailWithBody(status, _) => Some(*status),
        _ => None,
    }
}

fn upstream(key: &str, verb: &str, err: &S3Error) -> Error {
    Error::Upstream(format!("archive {verb} {key}: {err}"))
}

#[async_trait]
impl Archive for S3Archive {
    /// Refuses an existing key (B-5).
    ///
    /// S3 has no compare-and-swap this client exposes, so the refusal is a
    /// `HEAD` before the `PUT`. Two sealers racing on the same key would both
    /// pass it; they cannot reach that point, because the segment they would
    /// be sealing is arbitrated first by the unique parent index on
    /// `kernel_segments` (spec 014 B-2).
    async fn put(&self, key: &str, bytes: Vec<u8>) -> Result<(), Error> {
        if self.exists(key).await? {
            return Err(Error::Conflict(format!(
                "archive key {key} already holds an object: a sealed segment is never rewritten"
            )));
        }
        self.bucket
            .put(key, &bytes)
            .await
            .map(|_| ())
            .map_err(|e| upstream(key, "put", &e))
    }

    async fn get(&self, key: &str) -> Result<Vec<u8>, Error> {
        let response = match self.bucket.get(key).await {
            Ok(response) => response,
            Err(e) if status_of(&e) == Some(404) => {
                return Err(Error::NotFound(format!(
                    "archive key {key} holds no object"
                )));
            }
            Err(e) => return Err(upstream(key, "get", &e)),
        };
        response
            .bytes()
            .await
            .map(|bytes| bytes.to_vec())
            .map_err(|e| Error::Upstream(format!("archive get {key}: {e}")))
    }

    async fn list(&self, prefix: &str) -> Result<Vec<String>, Error> {
        let pages = self
            .bucket
            .list(prefix, None)
            .await
            .map_err(|e| upstream(prefix, "list", &e))?;
        let mut keys: Vec<String> = pages
            .into_iter()
            .flat_map(|page| page.contents.into_iter().map(|object| object.key))
            .collect();
        keys.sort();
        Ok(keys)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    fn archive() -> (tempfile::TempDir, FsArchive) {
        let dir = tempfile::tempdir().expect("a temp dir");
        let archive = FsArchive::open(dir.path().join("archive")).expect("opens");
        (dir, archive)
    }

    #[tokio::test]
    async fn a_key_is_written_once_and_read_back() {
        let (_dir, archive) = archive();
        archive
            .put("ledger/segments/a-b.json", b"body".to_vec())
            .await
            .expect("writes");
        assert_eq!(
            archive
                .get("ledger/segments/a-b.json")
                .await
                .expect("reads"),
            b"body".to_vec()
        );
        assert_eq!(
            archive.list("ledger/segments/").await.expect("lists"),
            vec!["ledger/segments/a-b.json".to_owned()]
        );
        assert!(
            archive.list("other/").await.expect("lists").is_empty(),
            "the prefix is honoured"
        );
    }

    #[tokio::test]
    async fn a_missing_key_is_not_found() {
        let (_dir, archive) = archive();
        let err = archive
            .get("ledger/segments/none.json")
            .await
            .expect_err("refused");
        assert!(matches!(err, Error::NotFound(_)), "{err}");
    }

    #[tokio::test]
    async fn a_key_that_climbs_out_of_the_root_is_refused() {
        let (_dir, archive) = archive();
        for key in [
            "",
            "../escape.json",
            "/absolute.json",
            "ledger/../../x.json",
        ] {
            let err = archive.put(key, b"x".to_vec()).await.expect_err("refused");
            assert!(matches!(err, Error::Validation(_)), "{key:?}: {err}");
        }
    }

    #[test]
    fn the_credentials_never_reach_a_log_line() {
        let cfg = S3Config {
            endpoint: "https://s3.example".to_owned(),
            bucket: "cell-archive".to_owned(),
            region: "eu-central-1".to_owned(),
            access_key: "AKIAEXAMPLE".to_owned(),
            secret_key: "super-secret".to_owned(),
            path_style: true,
        };
        let printed = format!("{cfg:?}");
        assert!(!printed.contains("super-secret"), "{printed}");
        assert!(!printed.contains("AKIAEXAMPLE"), "{printed}");

        let archive = S3Archive::open(&cfg).expect("opens");
        let printed = format!("{archive:?}");
        assert!(printed.contains("cell-archive"), "{printed}");
        assert!(!printed.contains("super-secret"), "{printed}");
    }
}
