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
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
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

/// Record `to` when the transition stands at one of `from` (T4, T5): the
/// completions `supervise` and `serve` write. `Ok(false)` when the record is
/// absent or elsewhere, which changes nothing.
///
/// The caller holds `transition.lock` shared through its gate, so no verb,
/// abort or restore can change the layout or the record meanwhile.
///
/// # Errors
///
/// As [`read`] and [`write`].
pub fn complete(config: &Config, from: &[Phase], to: Phase) -> Result<bool> {
    let Some(mut record) = read(config)? else {
        return Ok(false);
    };
    if !from.contains(&record.phase) {
        return Ok(false);
    }
    record.phase = to;
    write(config, &record)?;
    Ok(true)
}

// ------------------------------------------------------------------ the verb

/// B-4's two preconditions, word for word: the verb prints them before T1,
/// and the README states them (AC-5).
pub const PRECONDITIONS: &str = "\
Preconditions the verb cannot establish (spec 043 B-4); establish them before running it:
  1. Every old process is stopped, and the old container is removed with every restart source \
that could bring it back disabled (a restart policy, a unit file, a controller). This includes \
a pre-043 supervisor started directly and every Rauthy it spawned.
  2. No old process on the volume, in any state, has HQL_DANGER_RAFT_STATE_RESET or \
HQL_BACKUP_RESTORE in its environment.";

/// An injectable fault point after every intent and every action (FR-003).
pub trait Faults: Send + Sync {
    /// Called at `point`; an error stops the verb there, as a crash would.
    ///
    /// # Errors
    ///
    /// Whatever the injector chooses.
    fn hit(&self, point: &str) -> Result<()>;
}

/// No faults: the verb as shipped.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoFaults;

impl Faults for NoFaults {
    fn hit(&self, _point: &str) -> Result<()> {
        Ok(())
    }
}

/// How a run of the verb ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// T3 completed: the floor is raised and the next `supervise` may start.
    Floored {
        /// The transition's id.
        id: String,
        /// T1 (f)'s instant.
        instant: u64,
    },
    /// The transition had already reached `phase`; nothing changed.
    Already {
        /// Where it stands.
        phase: Phase,
    },
    /// There is nothing to transition, and why.
    Nothing(String),
    /// `--abort` restored the pre-043 layout's operational data (B-5a).
    Aborted {
        /// The transition's id.
        id: String,
    },
}

/// The aside directory of transition `id`.
#[must_use]
pub fn aside_dir(config: &Config, id: &str) -> PathBuf {
    config.data_dir.join(WORK_DIR).join("aside").join(id)
}

/// The evidence root of transition `id`.
#[must_use]
pub fn evidence_dir(config: &Config, id: &str) -> PathBuf {
    config.data_dir.join(WORK_DIR).join("evidence").join(id)
}

fn guard_content(id: &str) -> String {
    format!("{}{id}", crate::cell_lock::GUARD_PREFIX)
}

/// `rahi upgrade-cache --backup <archive>` (B-4): T0 to T3, resumable.
///
/// # Errors
///
/// [`Error::Conflict`] for every refusal B-4 names (a held lock, a live or
/// uncleanly stopped pre-043 node, a layout modified outside the verb);
/// [`Error::Unauthorized`], [`Error::Integrity`] or [`Error::Validation`]
/// when the archive does not verify; [`Error::Io`] when a step fails, after
/// which a rerun resumes from the last record.
pub async fn run(
    config: &Config,
    env: &dyn rahi_types::EnvReader,
    archive: &Path,
    faults: &dyn Faults,
) -> Result<Outcome> {
    use crate::cell_lock::{Entry, Legacy, gate};
    // T0: the locks, then the record and the layout.
    let gate = gate(config, Entry::UpgradeCache)?;
    let mut record = match gate.record() {
        Some(record) if record.phase != Phase::Aborted => record.clone(),
        _ => match gate.legacy() {
            Legacy::Absent => {
                return Ok(Outcome::Nothing(
                    "there is no legacy store on this volume; a fresh volume needs no \
                     transition"
                        .to_owned(),
                ));
            }
            Legacy::Fence { .. } => {
                return Ok(Outcome::Nothing(
                    "the legacy path is already the fence; this volume needs no transition"
                        .to_owned(),
                ));
            }
            Legacy::Other { .. } => {
                // D-23: the archive is verified, read-only, before the first
                // byte is written, so a verb without a verifying archive
                // changes nothing (AC-3); T1a verifies it again, recorded.
                verify_archive(config, archive)?;
                let record = Record::begin(crate::random_id()?);
                write(config, &record)?;
                faults.hit("begin")?;
                record
            }
        },
    };
    if matches!(
        record.phase,
        Phase::Floored | Phase::RauthyDone | Phase::Done
    ) {
        return Ok(Outcome::Already {
            phase: record.phase,
        });
    }
    if matches!(
        record.phase,
        Phase::Begin | Phase::Guarded | Phase::Verifying
    ) {
        // D-23: a resumed run whose verification is still ahead checks the
        // archive before it changes anything more.
        verify_archive(config, archive)?;
    }
    if recover_evidence(config, &mut record)? {
        faults.hit("evidence.recovered")?;
    }
    loop {
        match record.phase {
            Phase::Begin => t1(config, &mut record, faults)?,
            Phase::Guarded => advance(config, &mut record, Phase::Verifying, faults)?,
            Phase::Verifying => {
                record.digest = Some(verify_archive(config, archive)?);
                advance(config, &mut record, Phase::Verified, faults)?;
            }
            Phase::Verified => plan_t2(config, &mut record, faults)?,
            Phase::Relocating => t2(config, &mut record, faults)?,
            Phase::Relocated => advance(config, &mut record, Phase::Flooring, faults)?,
            Phase::Flooring => {
                t3(config, env, &mut record, faults).await?;
                advance(config, &mut record, Phase::Floored, faults)?;
            }
            Phase::Floored | Phase::RauthyDone | Phase::Done => {
                return Ok(Outcome::Floored {
                    id: record.id.clone(),
                    instant: record.instant.unwrap_or(0),
                });
            }
            Phase::Aborted => {
                return Err(Error::Conflict(
                    "the transition was aborted while this run held it".to_owned(),
                ));
            }
        }
    }
}

fn advance(config: &Config, record: &mut Record, to: Phase, faults: &dyn Faults) -> Result<()> {
    record.phase = to;
    write(config, record)?;
    faults.hit(to.name())
}

/// T1a: 030's read-only verification of the archive; its digest.
fn verify_archive(config: &Config, archive: &Path) -> Result<String> {
    let sealed = std::fs::read(archive).map_err(|err| {
        Error::Io(format!(
            "archive {} cannot be read: {err}; the transition needs a verified pre-upgrade \
             backup (spec 043 B-4)",
            archive.display()
        ))
    })?;
    let identity = crate::KeySet::of(config).backup_identity()?;
    crate::archive::open(&sealed, &identity)?;
    Ok(crate::archive::sha256_hex(&sealed))
}

/// Read a marker's content; `None` when absent.
fn read_marker(path: &Path) -> Result<Option<String>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).trim().to_owned())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(Error::Io(format!(
            "{} cannot be read: {err}",
            path.display()
        ))),
    }
}

/// Whether another open file description holds `path`'s advisory lock. A
/// non-blocking probe that opens an existing file only: an absent file reads
/// as not held, and nothing is created.
fn lock_held(path: &Path) -> Result<bool> {
    let file = match std::fs::OpenOptions::new().read(true).open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => {
            return Err(Error::Io(format!(
                "{} cannot be probed: {err}",
                path.display()
            )));
        }
    };
    match file.try_lock() {
        Ok(()) => Ok(false),
        Err(std::fs::TryLockError::WouldBlock) => Ok(true),
        Err(std::fs::TryLockError::Error(err)) => Err(Error::Io(format!(
            "{} cannot be probed: {err}",
            path.display()
        ))),
    }
}

/// Which pre-043 state a foreign marker means, from (c)'s probe.
fn pre043_state(legacy: &Path) -> Result<&'static str> {
    for log in ["logs", "logs_cache"] {
        if lock_held(&legacy.join(log).join("lock.hql"))? {
            return Ok("a pre-043 node is live on this volume");
        }
    }
    Ok("a pre-043 node stopped uncleanly (or is starting) on this volume")
}

/// T1: the guard, quiescence, identity, the supervisor fence, the instant.
fn t1(config: &Config, record: &mut Record, faults: &dyn Faults) -> Result<()> {
    let legacy = config.legacy_hiqlite_dir();
    let state_machine = legacy.join("state_machine");
    let marker = crate::legacy_marker(config);
    let guard = guard_content(&record.id);
    let temp = state_machine.join(format!(".rahi-guard-{}", record.id));

    // (b) The guard, whole or not at all.
    match read_marker(&marker)? {
        Some(content) if content == guard => {}
        Some(content) => {
            return Err(Error::Conflict(format!(
                "{} holds {content:?}: {}; the existing marker is left as it is. Stop and \
                 remove every old process (spec 043 B-4's first precondition) and run the \
                 verb again",
                marker.display(),
                pre043_state(&legacy)?
            )));
        }
        None => {
            use std::io::Write as _;
            std::fs::create_dir_all(&state_machine).map_err(|err| {
                Error::Io(format!(
                    "{} cannot be created: {err}",
                    state_machine.display()
                ))
            })?;
            let mut file = std::fs::File::create(&temp)
                .map_err(|err| Error::Io(format!("{} cannot be created: {err}", temp.display())))?;
            file.write_all(guard.as_bytes())
                .map_err(|err| Error::Io(format!("{} cannot be written: {err}", temp.display())))?;
            faults.hit("t1.temp")?;
            file.sync_all()
                .map_err(|err| Error::Io(format!("{} cannot be synced: {err}", temp.display())))?;
            drop(file);
            faults.hit("t1.fsync")?;
            match std::fs::hard_link(&temp, &marker) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    return Err(Error::Conflict(format!(
                        "{} appeared while the guard was being placed: {}; the existing \
                         marker is left as it is",
                        marker.display(),
                        pre043_state(&legacy)?
                    )));
                }
                Err(err) => {
                    return Err(Error::Io(format!(
                        "{} cannot be linked to {}: {err}",
                        temp.display(),
                        marker.display()
                    )));
                }
            }
            record.marker = Identity::of(&marker)?;
            write(config, record)?;
            faults.hit("t1.link")?;
            crate::fsync_dir(&state_machine)?;
            faults.hit("t1.dirsync")?;
        }
    }
    if temp.exists() {
        std::fs::remove_file(&temp)
            .map_err(|err| Error::Io(format!("{} cannot be removed: {err}", temp.display())))?;
        crate::fsync_dir(&state_machine)?;
    }

    // (c) Quiescence, after the marker is durable.
    for log in ["logs", "logs_cache"] {
        let lock = legacy.join(log).join("lock.hql");
        if lock_held(&lock)? {
            return Err(Error::Conflict(format!(
                "{} is held: a pre-043 node is live on this volume; the guard stays in place \
                 and the verb changes nothing else",
                lock.display()
            )));
        }
    }
    for name in crate::cell_lock::list(&state_machine.join("db"))? {
        let name = name.to_string_lossy();
        if name.ends_with("-wal") || name.ends_with("-shm") {
            return Err(Error::Conflict(format!(
                "{} holds {name}: a pre-043 node still has its database open, or stopped \
                 uncleanly; the guard stays in place",
                state_machine.join("db").display()
            )));
        }
    }

    // (d) The marker is still the one `link` gave.
    let now = Identity::of(&marker)?;
    if read_marker(&marker)?.as_deref() != Some(guard.as_str())
        || now.is_none()
        || record.marker.is_some_and(|linked| Some(linked) != now)
    {
        return Err(Error::Conflict(format!(
            "{} is no longer this transition's guard: a pre-043 node started or stopped \
             while T1 ran; the marker is left as it is",
            marker.display()
        )));
    }
    record.marker = now;

    // (e) The supervisor fence.
    install_supervisor_fence(config, record, faults)?;

    // (f) The instant.
    record.instant = Some(crate::unix_now());
    advance(config, record, Phase::Guarded, faults)
}

/// T1 (e): render at the new path, build the fence, move the old file to
/// evidence, put the fence in place (B-5).
fn install_supervisor_fence(
    config: &Config,
    record: &mut Record,
    faults: &dyn Faults,
) -> Result<()> {
    use crate::cell_lock::{SupervisorFence, inspect_supervisor_fence};
    let old = crate::rauthy_env::legacy_env_path(config);
    let new = crate::rauthy_env::env_path(config);
    let fence = inspect_supervisor_fence(config)?;
    if fence == SupervisorFence::Fence {
        return Ok(());
    }
    if fence == SupervisorFence::Other {
        return Err(Error::Conflict(format!(
            "{} is neither the rendered environment nor the fence",
            old.display()
        )));
    }
    if fence == SupervisorFence::File && !new.exists() {
        let bytes = std::fs::read(&old)
            .map_err(|err| Error::Io(format!("{} cannot be read: {err}", old.display())))?;
        let dir = config.data_dir.join(crate::rauthy_env::ENV_DIR);
        std::fs::create_dir_all(&dir)
            .map_err(|err| Error::Io(format!("{} cannot be created: {err}", dir.display())))?;
        crate::set_mode(&dir, crate::KEY_DIR_MODE)?;
        crate::write_replacing(&new, &bytes)?;
        crate::set_mode(&new, crate::KEY_FILE_MODE)?;
        faults.hit("t1e.render")?;
    }
    // A fence built by an interrupted run is this verb's own temporary.
    for name in crate::cell_lock::list(&crate::rauthy_dir(config))? {
        if name.to_string_lossy().starts_with(".rahi-fence-") {
            let stale = crate::rauthy_dir(config).join(name);
            std::fs::remove_dir_all(&stale).map_err(|err| {
                Error::Io(format!("{} cannot be removed: {err}", stale.display()))
            })?;
        }
    }
    let built = crate::cell_lock::build_supervisor_fence(config)?;
    faults.hit("t1e.build")?;
    if fence == SupervisorFence::File {
        to_evidence(config, record, "t1e", &old, Path::new("rauthy.env"), faults)?;
    }
    crate::cell_lock::place_supervisor_fence(config, &built)?;
    faults.hit("t1e.place")
}

/// FR-013: move `source` to a fresh evidence name, recording the intent
/// with its identity before the rename.
fn to_evidence(
    config: &Config,
    record: &mut Record,
    step: &str,
    source: &Path,
    relative: &Path,
    faults: &dyn Faults,
) -> Result<()> {
    let identity = Identity::of(source)?.ok_or_else(|| {
        Error::Io(format!(
            "{} vanished before its move to evidence",
            source.display()
        ))
    })?;
    record.seq += 1;
    let destination = evidence_dir(config, &record.id)
        .join(format!("{:06}-{step}", record.seq))
        .join(relative);
    record.evidence.push(EvidenceMove {
        seq: record.seq,
        step: step.to_owned(),
        source: source.to_path_buf(),
        destination: destination.clone(),
        identity,
        done: false,
    });
    write(config, record)?;
    faults.hit(&format!("evidence.{step}.intent"))?;
    finish_evidence(record)?;
    write(config, record)?;
    faults.hit(&format!("evidence.{step}.rename"))
}

/// Complete every recorded evidence move, by identity (FR-013).
fn finish_evidence(record: &mut Record) -> Result<()> {
    for moved in record.evidence.iter_mut().filter(|m| !m.done) {
        let at_dest = Identity::of(&moved.destination)?;
        if at_dest == Some(moved.identity) {
            moved.done = true;
            continue;
        }
        if at_dest.is_some() {
            return Err(Error::Conflict(format!(
                "{} holds an identity the evidence intent does not name; the volume was \
                 modified outside this verb",
                moved.destination.display()
            )));
        }
        if Identity::of(&moved.source)? != Some(moved.identity) {
            return Err(Error::Conflict(format!(
                "the entry recorded for evidence at {} is at neither {} nor its evidence name; \
                 the volume was modified outside this verb",
                moved.source.display(),
                moved.destination.display()
            )));
        }
        let parent = moved
            .destination
            .parent()
            .ok_or_else(|| Error::Io("an evidence name has no parent".to_owned()))?;
        std::fs::create_dir_all(parent)
            .map_err(|err| Error::Io(format!("{} cannot be created: {err}", parent.display())))?;
        crate::rename_noreplace(&moved.source, &moved.destination)?;
        if let Some(from) = moved.source.parent() {
            crate::fsync_dir(from)?;
        }
        crate::fsync_dir(parent)?;
        moved.done = true;
    }
    Ok(())
}

/// Recovery of an interrupted evidence move at resume.
fn recover_evidence(config: &Config, record: &mut Record) -> Result<bool> {
    if record.evidence.iter().any(|m| !m.done) {
        finish_evidence(record)?;
        write(config, record)?;
        return Ok(true);
    }
    Ok(false)
}

fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}

fn device(path: &Path) -> Result<Option<u64>> {
    Ok(Identity::of(path)?.map(|i| i.dev))
}

/// T2's intent: refuse, before recording anything, what B-4 names; then
/// record the plan.
fn plan_t2(config: &Config, record: &mut Record, faults: &dyn Faults) -> Result<()> {
    let legacy = config.legacy_hiqlite_dir();
    let state_machine = legacy.join("state_machine");
    let app = config.hiqlite_dir();
    let work = config.data_dir.join(WORK_DIR);
    let target = aside_dir(config, &record.id);
    let refuse = |why: String| Err(Error::Conflict(format!("{why}; T2 changes nothing")));
    if app.exists() && !crate::cell_lock::list(&app)?.is_empty() {
        return refuse(format!("{} exists and is not empty", app.display()));
    }
    if target.exists() {
        return refuse(format!("{} exists", target.display()));
    }
    for path in [&legacy, &state_machine, &config.data_dir, &work] {
        if is_symlink(path) {
            return refuse(format!("{} is a symbolic link", path.display()));
        }
    }
    std::fs::create_dir_all(&work)
        .map_err(|err| Error::Io(format!("{} cannot be created: {err}", work.display())))?;
    let dev = device(&legacy)?;
    if device(&work)? != dev || device(&config.data_dir)? != dev {
        return refuse(format!(
            "{}, {} and {} are not on one device",
            legacy.display(),
            work.display(),
            config.data_dir.display()
        ));
    }
    let destination = |name: &Path, under_state_machine: bool| {
        let cache = name == Path::new("logs_cache") || name == Path::new("state_machine_cache");
        if cache {
            target.join(name)
        } else if under_state_machine {
            app.join("state_machine").join(name)
        } else {
            app.join(name)
        }
    };
    let mut entries = Vec::new();
    for name in crate::cell_lock::list(&state_machine)? {
        if name == Path::new("lock") || name.to_string_lossy().starts_with(".rahi-guard-") {
            continue;
        }
        entries.push((state_machine.join(&name), destination(&name, true)));
    }
    for name in crate::cell_lock::list(&legacy)? {
        if name == Path::new("state_machine") {
            continue;
        }
        entries.push((legacy.join(&name), destination(&name, false)));
    }
    // hiqlite F-130: the snapshot cache before the cache log.
    let rank = |p: &Path| match p.file_name().and_then(|n| n.to_str()) {
        Some("state_machine_cache") => 1,
        Some("logs_cache") => 2,
        _ => 0,
    };
    entries.sort_by_key(|(source, _)| rank(source));
    let mut plan = Vec::with_capacity(entries.len());
    for (source, destination) in entries {
        if is_symlink(&source) {
            return refuse(format!("{} is a symbolic link", source.display()));
        }
        let identity = Identity::of(&source)?
            .ok_or_else(|| Error::Io(format!("{} vanished while planning", source.display())))?;
        if Some(identity.dev) != dev {
            return refuse(format!("{} is on another device", source.display()));
        }
        plan.push(PlannedMove {
            source,
            destination,
            identity,
        });
    }
    record.target = Some(target);
    record.plan = plan;
    advance(config, record, Phase::Relocating, faults)
}

/// T2: every planned entry by identity, then debris to evidence.
fn t2(config: &Config, record: &mut Record, faults: &dyn Faults) -> Result<()> {
    let app = config.hiqlite_dir();
    let target = record
        .target
        .clone()
        .ok_or_else(|| Error::Integrity("the relocation intent names no target".to_owned()))?;
    for dir in [&app, &app.join("state_machine"), &target] {
        std::fs::create_dir_all(dir)
            .map_err(|err| Error::Io(format!("{} cannot be created: {err}", dir.display())))?;
    }
    crate::set_mode(&app, 0o700)?;
    let plan = record.plan.clone();
    let mut debris = Vec::new();
    for (i, planned) in plan.iter().enumerate() {
        let at_source = Identity::of(&planned.source)?;
        let at_dest = Identity::of(&planned.destination)?;
        if at_dest == Some(planned.identity) {
            if at_source.is_some() {
                debris.push(planned.source.clone());
            }
            continue;
        }
        if at_dest.is_some() {
            return Err(Error::Conflict(format!(
                "{} holds an identity the plan does not name; the volume was modified outside \
                 this verb, and T2 changes nothing",
                planned.destination.display()
            )));
        }
        if at_source != Some(planned.identity) {
            return Err(Error::Conflict(format!(
                "the planned entry {} is at neither its source nor {}; the volume was modified \
                 outside this verb, and T2 changes nothing",
                planned.source.display(),
                planned.destination.display()
            )));
        }
        crate::rename_noreplace(&planned.source, &planned.destination)?;
        for dir in [planned.source.parent(), planned.destination.parent()]
            .into_iter()
            .flatten()
        {
            crate::fsync_dir(dir)?;
        }
        faults.hit(&format!("t2.move.{i}"))?;
    }
    // Debris a refused old start left after the plan was recorded.
    let legacy = config.legacy_hiqlite_dir();
    let state_machine = legacy.join("state_machine");
    for name in crate::cell_lock::list(&legacy)? {
        if name != Path::new("state_machine") {
            debris.push(legacy.join(name));
        }
    }
    for name in crate::cell_lock::list(&state_machine)? {
        if name != Path::new("lock") {
            debris.push(state_machine.join(name));
        }
    }
    debris.sort();
    debris.dedup();
    for path in debris {
        if Identity::of(&path)?.is_none() {
            continue;
        }
        let relative = path.strip_prefix(&legacy).unwrap_or(&path).to_path_buf();
        to_evidence(config, record, "t2", &path, &relative, faults)?;
    }
    advance(config, record, Phase::Relocated, faults)
}

/// T3: open the app store in-process with 0.15, raise the floor with the
/// transition row in one `txn`, shut down and require `Ok`.
async fn t3(
    config: &Config,
    env: &dyn rahi_types::EnvReader,
    record: &mut Record,
    faults: &dyn Faults,
) -> Result<()> {
    let marker = config.hiqlite_dir().join(crate::HIQLITE_LOCK_FILE);
    if let Some(content) = read_marker(&marker)? {
        if !content.is_empty() {
            return Err(Error::Conflict(format!(
                "{} holds {content:?}, which is foreign to this store; T3 refuses",
                marker.display()
            )));
        }
        // Only a process holding both locks while the state is `flooring`
        // can have opened the app store: this is T3's own unclean stop.
        to_evidence(
            config,
            record,
            "t3",
            &marker,
            Path::new("state_machine/lock"),
            faults,
        )?;
    }
    let instant = record
        .instant
        .ok_or_else(|| Error::Integrity("the transition record names no instant".to_owned()))?;
    let keys = crate::KeySet::of(config);
    let cfg = crate::store_config(config, env, keys.store_secrets()?)?;
    let store = rahi_store::Store::open(&cfg).await?;
    let raised = store
        .handle()
        .record_transition_floor(&record.id, instant)
        .await;
    let stopped = store.shutdown().await;
    raised?;
    stopped?;
    // After the shutdown: a fault injected in-process cannot release the
    // node's owner lock the way a real crash does, and the floor's `txn`
    // is idempotent, so a rerun after either is the same T3.
    faults.hit("t3.floored")
}

/// `rahi upgrade-cache --abort` (B-5a): before `flooring`, the pre-043
/// layout's operational data back where it was.
///
/// # Errors
///
/// [`Error::Conflict`] from `flooring` on, and when an entry is not where
/// the record says; [`Error::Io`] when a step fails.
pub fn abort(config: &Config, faults: &dyn Faults) -> Result<Outcome> {
    use crate::cell_lock::{Entry, SupervisorFence, gate, inspect_supervisor_fence};
    let gate = gate(config, Entry::Abort)?;
    let Some(record) = gate.record() else {
        return Ok(Outcome::Nothing("no transition to abort".to_owned()));
    };
    let mut record = record.clone();
    match record.phase {
        Phase::Aborted => {
            return Ok(Outcome::Already {
                phase: Phase::Aborted,
            });
        }
        Phase::Flooring | Phase::Floored | Phase::RauthyDone | Phase::Done => {
            return Err(Error::Conflict(format!(
                "the transition is at `{}`: the app store has been opened by the new hiqlite, \
                 and the supported rollback is the verified pre-upgrade archive restored into a \
                 fresh volume with the old image (spec 043 B-5a)",
                record.phase.name()
            )));
        }
        _ => {}
    }
    let _ = recover_evidence(config, &mut record)?;
    let legacy = config.legacy_hiqlite_dir();
    let marker = crate::legacy_marker(config);
    // Debris a refused old start created, to evidence first.
    let planned: Vec<PathBuf> = record.plan.iter().map(|p| p.source.clone()).collect();
    let mut debris = Vec::new();
    for planned in &record.plan {
        if let Some(found) = Identity::of(&planned.source)?
            && found != planned.identity
        {
            debris.push(planned.source.clone());
        }
    }
    if record.phase >= Phase::Relocating {
        for name in crate::cell_lock::list(&legacy)? {
            let path = legacy.join(&name);
            if name != Path::new("state_machine") && !planned.contains(&path) {
                debris.push(path);
            }
        }
        let state_machine = legacy.join("state_machine");
        for name in crate::cell_lock::list(&state_machine)? {
            let path = state_machine.join(&name);
            if name != Path::new("lock") && !planned.contains(&path) {
                debris.push(path);
            }
        }
    }
    debris.sort();
    debris.dedup();
    for path in debris {
        let relative = path.strip_prefix(&legacy).unwrap_or(&path).to_path_buf();
        to_evidence(config, &mut record, "abort", &path, &relative, faults)?;
    }
    // Every planned entry back to its path, by identity.
    for planned in record.plan.iter().rev() {
        if Identity::of(&planned.source)? == Some(planned.identity) {
            continue;
        }
        if Identity::of(&planned.destination)? != Some(planned.identity) {
            return Err(Error::Conflict(format!(
                "the planned entry {} is at neither its source nor {}; abort changes nothing \
                 more",
                planned.source.display(),
                planned.destination.display()
            )));
        }
        crate::rename_noreplace(&planned.destination, &planned.source)?;
        for dir in [planned.source.parent(), planned.destination.parent()]
            .into_iter()
            .flatten()
        {
            crate::fsync_dir(dir)?;
        }
        faults.hit("abort.return")?;
    }
    // The supervisor fence replaced by the original file.
    if let Some(original) = record
        .evidence
        .iter()
        .find(|m| m.step == "t1e" && m.done)
        .cloned()
    {
        let old = crate::rauthy_env::legacy_env_path(config);
        if inspect_supervisor_fence(config)? == SupervisorFence::Fence {
            to_evidence(
                config,
                &mut record,
                "abort",
                &old,
                Path::new("rauthy.env"),
                faults,
            )?;
        }
        if Identity::of(&old)?.is_none() {
            crate::rename_noreplace(&original.destination, &old)?;
            crate::fsync_dir(&crate::rauthy_dir(config))?;
        }
    }
    // The guard, only while it is still this transition's.
    if read_marker(&marker)?.as_deref() == Some(guard_content(&record.id).as_str())
        && record
            .marker
            .is_none_or(|linked| Identity::of(&marker).ok().flatten() == Some(linked))
    {
        std::fs::remove_file(&marker)
            .map_err(|err| Error::Io(format!("{} cannot be removed: {err}", marker.display())))?;
        if let Some(dir) = marker.parent() {
            crate::fsync_dir(dir)?;
        }
    }
    // An empty app-store directory is removed.
    let app = config.hiqlite_dir();
    for dir in [app.join("state_machine"), app.clone()] {
        if dir.is_dir() && crate::cell_lock::list(&dir)?.is_empty() {
            std::fs::remove_dir(&dir)
                .map_err(|err| Error::Io(format!("{} cannot be removed: {err}", dir.display())))?;
        }
    }
    let id = record.id.clone();
    advance(config, &mut record, Phase::Aborted, faults)?;
    drop(gate);
    Ok(Outcome::Aborted { id })
}
