//! The cache transition, `rahi upgrade-cache` (spec 043 B-4).
//!
//! The transition is a state machine persisted in `<data>/upgrade-cache.json`.
//! Each step records its intent before it acts and its completion after, and
//! recovery reads the last record. This module holds the record and its
//! durable write; every entry point reads it through B-4a's gate
//! ([`crate::cell_lock::gate`]), after its locks.

use std::path::{Path, PathBuf};

use rahi_types::{Config, Error, Result};
use serde::{Deserialize, Serialize};

/// The state file, relative to the data directory.
pub const STATE_FILE: &str = "upgrade-cache.json";

/// The transition's working directory, relative to the data directory:
/// the aside directory and the evidence names live under it.
pub const WORK_DIR: &str = "upgrade-cache";

/// The version of the record's shape this build writes and reads.
pub const RECORD_VERSION: u32 = 1;

/// Where the transition stands: the last intent or completion recorded
/// (spec 043 B-4's table).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    /// T1's intent: the guard is being written.
    Begin,
    /// T1's completion: the guard is whole, the legacy path quiescent, the
    /// supervisor fence installed and `instant` read.
    Guarded,
    /// T1a's intent.
    Verifying,
    /// T1a's completion: the archive verified.
    Verified,
    /// T2's intent: the plan is recorded.
    Relocating,
    /// T2's completion: the legacy path is the fence.
    Relocated,
    /// T3's intent: the app store is being opened to raise the floor.
    Flooring,
    /// T3's completion: the floor is raised; Rauthy may now move its cache.
    Floored,
    /// T4's completion, written by `supervise` once Rauthy answers ready.
    RauthyDone,
    /// T5's completion, written by `serve` after its first ready answer.
    Done,
    /// `--abort` completed before `flooring` (B-5a); kept as history.
    Aborted,
}

impl Phase {
    /// The name the record and every message use.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Begin => "begin",
            Self::Guarded => "guarded",
            Self::Verifying => "verifying",
            Self::Verified => "verified",
            Self::Relocating => "relocating",
            Self::Relocated => "relocated",
            Self::Flooring => "flooring",
            Self::Floored => "floored",
            Self::RauthyDone => "rauthy-done",
            Self::Done => "done",
            Self::Aborted => "aborted",
        }
    }

    /// Whether the app store may be opened by `serve` and `supervise` (B-4a:
    /// `floored`, `rauthy-done` or `done`).
    #[must_use]
    pub fn serves(self) -> bool {
        matches!(self, Self::Floored | Self::RauthyDone | Self::Done)
    }
}

/// A file or directory's identity: its `(st_dev, st_ino)` pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    /// `st_dev`.
    pub dev: u64,
    /// `st_ino`.
    pub ino: u64,
}

impl Identity {
    /// The identity of `path` itself, never following a final symbolic link;
    /// `None` when nothing is there.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] for any failure but absence.
    pub fn of(path: &Path) -> Result<Option<Self>> {
        use std::os::unix::fs::MetadataExt as _;
        match std::fs::symlink_metadata(path) {
            Ok(meta) => Ok(Some(Self {
                dev: meta.dev(),
                ino: meta.ino(),
            })),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(Error::Io(format!(
                "{} cannot be inspected: {err}",
                path.display()
            ))),
        }
    }
}

/// One entry T2 moves, listed once in its intent (B-4 T2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedMove {
    /// Where it is before the move.
    pub source: PathBuf,
    /// Where it is after the move.
    pub destination: PathBuf,
    /// Its identity, which recovery follows instead of existence.
    pub identity: Identity,
}

/// One move to an evidence name (B-4, FR-013): recorded, with its identity,
/// in an intent before its rename.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceMove {
    /// The six-digit counter this move allocated.
    pub seq: u64,
    /// The step that moved it (`t1e`, `t2`, `t3`, `abort`).
    pub step: String,
    /// Where it was.
    pub source: PathBuf,
    /// Its evidence name.
    pub destination: PathBuf,
    /// Its identity.
    pub identity: Identity,
    /// Whether its rename completed.
    pub done: bool,
}

/// The persisted transition record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    /// [`RECORD_VERSION`].
    pub version: u32,
    /// The transition's id: 128 random bits in hex.
    pub id: String,
    /// The last intent or completion.
    pub phase: Phase,
    /// T1 (f)'s wall-clock reading, whole seconds since the epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instant: Option<u64>,
    /// The guard's identity as `link` gave it (T1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marker: Option<Identity>,
    /// The verified archive's digest (T1a).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    /// T2's aside directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<PathBuf>,
    /// T2's plan.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plan: Vec<PlannedMove>,
    /// The last evidence counter allocated.
    #[serde(default)]
    pub seq: u64,
    /// Every move to an evidence name, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<EvidenceMove>,
}

impl Record {
    /// A new record at [`Phase::Begin`].
    #[must_use]
    pub fn begin(id: String) -> Self {
        Self {
            version: RECORD_VERSION,
            id,
            phase: Phase::Begin,
            instant: None,
            marker: None,
            digest: None,
            target: None,
            plan: Vec::new(),
            seq: 0,
            evidence: Vec::new(),
        }
    }
}

/// The state file's path.
#[must_use]
pub fn state_path(config: &Config) -> PathBuf {
    config.data_dir.join(STATE_FILE)
}

/// Read the state file; `None` when there is none.
///
/// # Errors
///
/// [`Error::Io`] when it cannot be read; [`Error::Integrity`] when it is not
/// a record this build understands, which every entry point refuses rather
/// than guesses about.
pub fn read(config: &Config) -> Result<Option<Record>> {
    let path = state_path(config);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(Error::Io(format!(
                "{} cannot be read: {err}",
                path.display()
            )));
        }
    };
    let record: Record = serde_json::from_slice(&bytes).map_err(|err| {
        Error::Integrity(format!(
            "{} is not a transition record this build reads: {err}",
            path.display()
        ))
    })?;
    if record.version != RECORD_VERSION {
        return Err(Error::Integrity(format!(
            "{} has record version {}, this build reads {RECORD_VERSION}",
            path.display(),
            record.version
        )));
    }
    Ok(Some(record))
}

/// Write the state file durably: write-to-temporary, fsync, rename, fsync of
/// the data directory (B-4).
///
/// # Errors
///
/// [`Error::Io`] when a step fails.
pub fn write(config: &Config, record: &Record) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(record)
        .map_err(|err| Error::Io(format!("the transition record cannot be encoded: {err}")))?;
    crate::write_replacing(&state_path(config), &bytes)
}
