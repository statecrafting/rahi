//! The operational verbs of the rahi chassis (spec 030).
//!
//! Operations that exist, rather than operations that are documented. Five
//! things live here, and each one is designed so that the failure enrahitu
//! learned from cannot happen rather than being warned about:
//!
//! - **Preflight** ([`preflight`]) checks a deployment and never mutates it:
//!   config, the data volume, the key files and their modes, the app's
//!   hiqlite node, the store's engine report, rauthy on loopback, the chain
//!   at resident depth, and free disk. Every check is reported by name.
//! - **Migrate** ([`migrate`]) runs the cell's migrations as a deploy step on
//!   the leader, never at boot (constitution IX).
//! - **Backup** ([`backup`], [`archive`]) is one artifact: the app's snapshot,
//!   rauthy's snapshot taken through rauthy's own API ([`rauthy_api`]), the
//!   key material that decrypts both, and a manifest naming every part by
//!   hash, sealed with age to the deployment's backup key (constitution XII).
//!   A part missing is an error, not a smaller archive.
//! - **Restore** ([`restore`]) is a cluster reset: it runs only when neither
//!   node is running, verifies every part before it writes anything, and
//!   leaves a marker so the same archive is never applied twice. No
//!   environment variable triggers it; the verb is the only door, and
//!   [`refuse_env_restore`] closes the one hiqlite would otherwise open.
//! - **Ledger verify and export** are spec 013's verification and spec 014's
//!   depth, called from the composer; nothing of them lives here.
//!
//! The composer that turns these into a binary is `rahi-cli`. First boot,
//! key generation, and supervision are spec 031's modules in this crate; the
//! key file names they generate are fixed here, in [`KeySet`], so that the
//! verbs and the generator agree by construction.

#![forbid(unsafe_code)]

pub mod archive;
pub mod backup;
pub mod first_boot;
pub mod keys;
pub mod migrate;
pub mod preflight;
pub mod rauthy_api;
pub mod rauthy_env;
pub mod restore;
pub mod supervise;

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::str::FromStr as _;

use age::secrecy::ExposeSecret as _;
use rahi_ledger::LedgerSigner;
use rahi_store::StoreSecrets;
use rahi_types::{Config, Error, Result};

pub use rahi_idp::CLIENT_SECRET_FILE;
pub use rahi_idp::envelope::SESSION_KEY_FILE;

/// The ledger signing key: base64 of a 32-byte Ed25519 seed
/// (`rahi_ledger::LedgerSigner::load`).
pub const LEDGER_KEY_FILE: &str = "ledger.key";

/// The app store's secrets: a JSON `rahi_store::StoreSecrets`.
pub const STORE_SECRETS_FILE: &str = "hiqlite.json";

/// The backup key: one age X25519 identity, `AGE-SECRET-KEY-1...`.
///
/// Its recipient is derived, so a backup needs nothing but this file, and a
/// restore into an empty volume needs nothing but this file either.
pub const BACKUP_KEY_FILE: &str = "backup.key";

/// rauthy's admin API key, the credential the bootstrap and backup calls
/// carry (spec 021 B-5).
pub const ADMIN_TOKEN_FILE: &str = "rauthy_admin_token";

/// rauthy's own secrets (spec 031 B-2): a JSON `rauthy_env::RauthySecrets`
/// holding its encryption key, its hiqlite secrets, the bootstrap admin
/// password, and the API key that is [`ADMIN_TOKEN_FILE`]'s other half.
/// Kept under `keys/` so a backup carries what decrypts rauthy's store; not
/// in [`KeySet::REQUIRED`], because nothing before `supervise` reads it.
pub const RAUTHY_SECRETS_FILE: &str = "rauthy.json";

/// The mode every key file must have (spec 031 B-1).
pub const KEY_FILE_MODE: u32 = 0o600;

/// The mode the key directory must have (spec 031 B-1).
pub const KEY_DIR_MODE: u32 = 0o700;

/// The environment variable hiqlite reads at node start to restore a backup.
///
/// rahi never sets it and refuses to start a node while it is set: restore
/// is a verb, and a variable that re-applies on every restart is the crash
/// loop spec 030's purpose section names (enrahitu://027).
pub const RESTORE_ENV_VAR: &str = "HQL_BACKUP_RESTORE";

/// The marker `restore` writes, relative to the data directory (B-6).
pub const RESTORE_MARKER: &str = "restore.marker";

/// Where archives land when `backup` is given no destination, relative to
/// the data directory (spec 031 B-1's fourth subdirectory).
pub const BACKUPS_DIR: &str = "backups";

/// rauthy's half of the volume, relative to the data directory (spec 031
/// B-1). The app never opens it; the two paths this crate derives from it
/// are a lock file's existence and the place a restored snapshot is left for
/// rauthy's own restore.
pub const RAUTHY_DIR: &str = "rauthy";

/// Where `restore` leaves rauthy's snapshot for the next start, relative to
/// the data directory. Outside rauthy's own directory, so that placing it is
/// not a write into a store this crate never opens (constitution VIII).
pub const RAUTHY_RESTORE_DIR: &str = "restore/rauthy";

/// The lock file hiqlite keeps under a node's data directory while it runs.
pub const HIQLITE_LOCK_FILE: &str = "state_machine/lock";

/// The key files every deployment carries, and how each one is read.
///
/// The names are the contract between spec 031's `first-boot`, which writes
/// them, and the verbs here, which read them. The client secret is the one
/// file not in [`KeySet::REQUIRED`]: rauthy mints it at bootstrap (spec 021
/// B-5), so it is absent until the first supervised start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeySet {
    dir: PathBuf,
}

impl KeySet {
    /// The files a deployment must carry before any verb but `first-boot`.
    pub const REQUIRED: [&'static str; 5] = [
        LEDGER_KEY_FILE,
        SESSION_KEY_FILE,
        STORE_SECRETS_FILE,
        BACKUP_KEY_FILE,
        ADMIN_TOKEN_FILE,
    ];

    /// The key set of `config`'s deployment.
    #[must_use]
    pub fn of(config: &Config) -> Self {
        Self::at(config.keys_dir())
    }

    /// A key set rooted at `dir`.
    #[must_use]
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The directory the files live in.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The path of `name` inside this set.
    #[must_use]
    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    /// The directory has mode [`KEY_DIR_MODE`] and every required file is
    /// present with mode [`KEY_FILE_MODE`], or the first problem found, named.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] naming the file that is missing or has the wrong
    /// mode; [`Error::Io`] when the directory cannot be read.
    pub fn check(&self) -> Result<()> {
        let dir_meta = std::fs::metadata(&self.dir).map_err(|err| {
            Error::Config(format!(
                "key directory {} is missing: {err}",
                self.dir.display()
            ))
        })?;
        let dir_mode = dir_meta.permissions().mode() & 0o777;
        // A Secret mounted read-only (spec 032 B-2) carries the mount's own
        // directory mode; what matters is that nobody else can write to it
        // and that this process cannot either. Anything else must be 0700.
        let read_only_mount = is_read_only_dir(&self.dir);
        if dir_mode != KEY_DIR_MODE && !read_only_mount {
            return Err(Error::Config(format!(
                "key directory {} has mode {dir_mode:04o}, expected {KEY_DIR_MODE:04o} (or a read-only mount)",
                self.dir.display()
            )));
        }
        for name in Self::REQUIRED {
            let path = self.path(name);
            let meta = std::fs::metadata(&path).map_err(|err| {
                Error::Config(format!("key file {} is missing: {err}", path.display()))
            })?;
            let mode = meta.permissions().mode() & 0o777;
            // On a read-only mount the kubelet owns the files (root, the
            // pod's fsGroup) and grants the group a read bit; a file nobody
            // can write and the world cannot read is as private as 0600.
            let private_on_mount = read_only_mount && mode & 0o400 != 0 && mode & 0o227 == 0;
            if mode != KEY_FILE_MODE && !private_on_mount {
                return Err(Error::Config(format!(
                    "key file {} has mode {mode:04o}, expected {KEY_FILE_MODE:04o}",
                    path.display()
                )));
            }
        }
        Ok(())
    }

    /// Read `name` as trimmed text.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the file cannot be read; [`Error::Config`] when it
    /// is not UTF-8.
    pub fn read_text(&self, name: &str) -> Result<String> {
        let path = self.path(name);
        let bytes = std::fs::read(&path).map_err(|err| {
            Error::Io(format!("key file {} cannot be read: {err}", path.display()))
        })?;
        String::from_utf8(bytes)
            .map(|text| text.trim().to_owned())
            .map_err(|_| Error::Config(format!("key file {} is not UTF-8", path.display())))
    }

    /// Write `name` with [`KEY_FILE_MODE`], creating the directory with
    /// [`KEY_DIR_MODE`] when it is absent.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the directory or the file cannot be written.
    pub fn write(&self, name: &str, bytes: &[u8]) -> Result<()> {
        std::fs::create_dir_all(&self.dir).map_err(|err| {
            Error::Io(format!(
                "key directory {} cannot be created: {err}",
                self.dir.display()
            ))
        })?;
        set_mode(&self.dir, KEY_DIR_MODE)?;
        let path = self.path(name);
        std::fs::write(&path, bytes).map_err(|err| {
            Error::Io(format!(
                "key file {} cannot be written: {err}",
                path.display()
            ))
        })?;
        set_mode(&path, KEY_FILE_MODE)
    }

    /// The ledger signer from [`LEDGER_KEY_FILE`].
    ///
    /// # Errors
    ///
    /// Whatever [`LedgerSigner::load`] returns.
    pub fn ledger_signer(&self) -> Result<LedgerSigner> {
        LedgerSigner::load(&self.path(LEDGER_KEY_FILE))
    }

    /// The store secrets from [`STORE_SECRETS_FILE`].
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the file cannot be read; [`Error::Config`] when it
    /// is not the JSON shape `rahi_store::StoreSecrets` serialises to.
    pub fn store_secrets(&self) -> Result<StoreSecrets> {
        let text = self.read_text(STORE_SECRETS_FILE)?;
        serde_json::from_str(&text).map_err(|err| {
            Error::Config(format!(
                "key file {} is not a store secrets document: {err}",
                self.path(STORE_SECRETS_FILE).display()
            ))
        })
    }

    /// The backup identity from [`BACKUP_KEY_FILE`].
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the file cannot be read; [`Error::Config`] when it
    /// is not an age X25519 identity.
    pub fn backup_identity(&self) -> Result<age::x25519::Identity> {
        let text = self.read_text(BACKUP_KEY_FILE)?;
        parse_backup_identity(&text).map_err(|err| {
            Error::Config(format!(
                "key file {} is not an age identity: {err}",
                self.path(BACKUP_KEY_FILE).display()
            ))
        })
    }

    /// The recipient every backup is sealed to: the public half of
    /// [`BACKUP_KEY_FILE`].
    ///
    /// # Errors
    ///
    /// As [`KeySet::backup_identity`].
    pub fn backup_recipient(&self) -> Result<age::x25519::Recipient> {
        Ok(self.backup_identity()?.to_public())
    }

    /// rauthy's admin token from [`ADMIN_TOKEN_FILE`].
    ///
    /// # Errors
    ///
    /// As [`KeySet::read_text`].
    pub fn admin_token(&self) -> Result<String> {
        self.read_text(ADMIN_TOKEN_FILE)
    }

    /// Every file in the set, name and bytes, sorted by name: what a backup
    /// carries under `keys/`.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the directory or a file cannot be read.
    pub fn export(&self) -> Result<Vec<(String, Vec<u8>)>> {
        let entries = std::fs::read_dir(&self.dir).map_err(|err| {
            Error::Io(format!(
                "key directory {} cannot be listed: {err}",
                self.dir.display()
            ))
        })?;
        let mut files = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|err| Error::Io(format!("key directory entry: {err}")))?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let bytes = std::fs::read(&path).map_err(|err| {
                Error::Io(format!("key file {} cannot be read: {err}", path.display()))
            })?;
            files.push((name, bytes));
        }
        files.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(files)
    }
}

/// Parse an age X25519 identity, as [`BACKUP_KEY_FILE`] holds it.
///
/// # Errors
///
/// The parser's own message when the text is not a bech32 identity.
pub fn parse_backup_identity(
    text: &str,
) -> std::result::Result<age::x25519::Identity, &'static str> {
    age::x25519::Identity::from_str(text.trim())
}

/// Mint a backup identity, serialised as [`BACKUP_KEY_FILE`] holds it.
#[must_use]
pub fn generate_backup_identity() -> String {
    age::x25519::Identity::generate()
        .to_string()
        .expose_secret()
        .to_owned()
}

/// Whether `dir` is a read-only mount as far as this deployment is
/// concerned (spec 032 B-2): this process cannot create a file in it. The
/// mode bits say nothing here; the kubelet mounts a Secret on a tmpfs
/// whose root is `1777` regardless of `defaultMode`, and `readOnly: true`
/// is what makes it unwritable.
#[must_use]
pub fn is_read_only_dir(dir: &Path) -> bool {
    dir.is_dir() && !dir_is_writable(dir)
}

/// Whether this process can create a file in `dir`.
#[must_use]
pub fn dir_is_writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".rahi-probe-{}", std::process::id()));
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Set `path`'s mode bits.
///
/// # Errors
///
/// [`Error::Io`] when the mode cannot be set.
pub fn set_mode(path: &Path, mode: u32) -> Result<()> {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .map_err(|err| Error::Io(format!("mode of {} cannot be set: {err}", path.display())))
}

/// rauthy's directory on the volume.
#[must_use]
pub fn rauthy_dir(config: &Config) -> PathBuf {
    config.data_dir.join(RAUTHY_DIR)
}

/// The app node's lock file.
#[must_use]
pub fn app_lock_file(config: &Config) -> PathBuf {
    config.hiqlite_dir().join(HIQLITE_LOCK_FILE)
}

/// rauthy's node's lock file, probed for existence and nothing else.
#[must_use]
pub fn rauthy_lock_file(config: &Config) -> PathBuf {
    rauthy_dir(config).join(HIQLITE_LOCK_FILE)
}

/// The restore marker's path.
#[must_use]
pub fn restore_marker(config: &Config) -> PathBuf {
    config.data_dir.join(RESTORE_MARKER)
}

/// The default archive destination.
#[must_use]
pub fn backups_dir(config: &Config) -> PathBuf {
    config.data_dir.join(BACKUPS_DIR)
}

/// The app's hiqlite peers, one `<id> <raft_addr> <api_addr>` per line or
/// separated by `;` (spec 032 B-1). Unset means the one node the
/// configuration's own addresses describe.
pub const ENV_HIQ_NODES: &str = "RAHI_HIQ_NODES";

/// `true` (or `1`) makes a verb a pure client of the cluster named by
/// [`ENV_HIQ_NODES`], starting no node of its own (spec 032 B-6): the
/// migration Job that runs the new image before a rollout.
pub const ENV_STORE_CLIENT: &str = "RAHI_STORE_CLIENT";

/// Whether [`ENV_STORE_CLIENT`] asks for a pure client.
#[must_use]
pub fn store_client_requested(env: &dyn rahi_types::EnvReader) -> bool {
    env.get(ENV_STORE_CLIENT)
        .is_some_and(|v| v.eq_ignore_ascii_case("true") || v == "1")
}

/// The store configuration for this replica: spec 011's derivation from
/// the configuration tree, with the node id and the peers spec 032 B-1
/// renders into the environment at N=3.
///
/// # Errors
///
/// [`Error::Config`] when the node id or the peer list does not parse, or
/// when the peers do not name this node.
pub fn store_config(
    config: &Config,
    env: &dyn rahi_types::EnvReader,
    secrets: StoreSecrets,
) -> Result<rahi_store::StoreConfig> {
    let mut store = rahi_store::StoreConfig::from_config(config, secrets);
    store.node_id = rauthy_env::node_id(env)?;
    if let Some(raw) = env.get(ENV_HIQ_NODES) {
        let nodes = rauthy_env::parse_nodes(&raw)?;
        if !nodes.iter().any(|n| n.id == store.node_id) {
            return Err(Error::Config(format!(
                "{ENV_HIQ_NODES} names no node {}; this replica is not in its own peer list",
                store.node_id
            )));
        }
        store.nodes = nodes
            .into_iter()
            .map(|n| rahi_store::Peer {
                id: n.id,
                raft_addr: n.raft_addr,
                api_addr: n.api_addr,
            })
            .collect();
    }
    Ok(store)
}

/// Refuse to proceed while [`RESTORE_ENV_VAR`] is set in this process.
///
/// Called before every node open, so that the variable hiqlite honours can
/// never restore anything under rahi: B-6's "no environment variable
/// triggers restore" is enforced, not assumed.
///
/// # Errors
///
/// [`Error::Config`] naming the variable.
pub fn refuse_env_restore() -> Result<()> {
    match std::env::var_os(RESTORE_ENV_VAR) {
        None => Ok(()),
        Some(_) => Err(Error::Config(format!(
            "{RESTORE_ENV_VAR} is set; rahi restores only through the restore verb, unset it"
        ))),
    }
}

/// Seconds since the Unix epoch, now.
#[must_use]
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `secs` since the epoch as `YYYYMMDDTHHMMSSZ`, the stamp archive names
/// carry: sortable, filesystem-safe, and unambiguous about its zone.
#[must_use]
pub fn utc_stamp(secs: u64) -> String {
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Civil-from-days (Howard Hinnant), for a proleptic Gregorian calendar.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(mo <= 2);
    format!("{y:04}{mo:02}{d:02}T{h:02}{m:02}{s:02}Z")
}
