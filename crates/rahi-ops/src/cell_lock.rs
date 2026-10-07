//! The cell's locks, its two fences, and the one gate every entry point
//! passes (spec 043 B-4a, B-5, FR-008).
//!
//! Two locks, because two questions: `cell.lock` says who owns the app
//! store's node, and `transition.lock` says whether the layout may change.
//! Both are taken non-blocking, through one descriptor each, opened
//! close-on-exec so Rauthy never inherits them, and held for the life of
//! the [`Gate`] that took them. A process that cannot take one refuses
//! rather than waits, so no two entry points can deadlock.
//!
//! The gate runs in one order and nothing else touches the volume first:
//! refuse the three hiqlite variables in this process's own environment,
//! take the locks, and only then read the transition record and inspect the
//! legacy path, the supervisor fence and the app store. On a volume with
//! neither a legacy path nor a record it creates both fences before anything
//! creates or opens the app store.

use std::ffi::OsString;
use std::fs::{File, TryLockError};
use std::path::{Path, PathBuf};

use rahi_types::{Config, Error, Result};

use crate::upgrade::{self, Phase, Record};

/// The node-ownership lock, relative to the data directory.
pub const CELL_LOCK_FILE: &str = "cell.lock";

/// The layout lock, relative to the data directory.
pub const TRANSITION_LOCK_FILE: &str = "transition.lock";

/// The file inside the supervisor fence's directory.
pub const SUPERVISOR_FENCE_FILE: &str = "FENCE";

/// The prefix of the legacy marker's content on a volume this version
/// fenced without a transition.
pub const FENCE_PREFIX: &str = "rahi-fence ";

/// The prefix of the legacy marker's content written by T1's guard.
pub const GUARD_PREFIX: &str = "rahi-upgrade-cache ";

/// hiqlite 0.14 and 0.15 move a legacy cache aside when this is set; rahi
/// never honours an operator's value (B-3).
pub const ENV_CACHE_LEGACY_MOVE_ASIDE: &str = "HQL_CACHE_LEGACY_MOVE_ASIDE";

/// hiqlite deletes a node's raft logs and snapshots when this is set (B-3).
pub const ENV_DANGER_RAFT_STATE_RESET: &str = "HQL_DANGER_RAFT_STATE_RESET";

/// The three variables B-4a's step (1) refuses in this process's own
/// environment, each with why.
pub const REFUSED_ENV: [(&str, &str); 3] = [
    (
        ENV_CACHE_LEGACY_MOVE_ASIDE,
        "the only supported crossing of the cache boundary is `rahi upgrade-cache --backup <archive>`",
    ),
    (
        ENV_DANGER_RAFT_STATE_RESET,
        "hiqlite would delete the app store's raft logs and snapshots when it opens",
    ),
    (
        crate::RESTORE_ENV_VAR,
        "rahi restores only through the restore verb",
    ),
];

/// An entry point of B-4a's table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entry {
    /// `rahi upgrade-cache`: both locks exclusive; any state; any layout.
    UpgradeCache,
    /// `rahi upgrade-cache --abort`: as [`Entry::UpgradeCache`].
    Abort,
    /// `rahi supervise` (and so the image): the cell exclusively for its
    /// life, the layout shared; its in-process `serve` uses this gate.
    Supervise,
    /// `rahi serve` alone: as [`Entry::Supervise`].
    Serve,
    /// `rahi first-boot`: the cell exclusively, the layout shared; any
    /// state, any layout.
    FirstBoot,
    /// `migrate`, `preflight`, `backup`, `ledger` and every other verb that
    /// opens the store: the cell exclusively when free, and when another
    /// process holds it and `may_attach`, attached without it; the layout
    /// shared; `done` or no record.
    Store {
        /// Whether the verb supports attaching to a running node.
        may_attach: bool,
    },
    /// `rahi restore`: both locks exclusive; `done` or no record.
    Restore,
}

impl Entry {
    /// The name messages use.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::UpgradeCache => "upgrade-cache",
            Self::Abort => "upgrade-cache --abort",
            Self::Supervise => "supervise",
            Self::Serve => "serve",
            Self::FirstBoot => "first-boot",
            Self::Store { .. } => "this verb",
            Self::Restore => "restore",
        }
    }

    fn layout_exclusive(self) -> bool {
        matches!(self, Self::UpgradeCache | Self::Abort | Self::Restore)
    }

    fn accepts(self, phase: Option<Phase>) -> bool {
        match self {
            Self::UpgradeCache | Self::Abort | Self::FirstBoot => true,
            // Spec 043 D-22: the verbs that open the store accept every
            // state the image serves at, so the entrypoint's deploy step
            // (031 D-5, 036 B-3) runs at `floored` and `rauthy-done` too.
            Self::Supervise | Self::Serve | Self::Store { .. } => phase.is_none_or(Phase::serves),
            Self::Restore => phase.is_none_or(|p| p == Phase::Done),
        }
    }

    fn creates_fences(self) -> bool {
        !matches!(self, Self::UpgradeCache | Self::Abort)
    }

    fn needs_fence(self) -> bool {
        !matches!(self, Self::UpgradeCache | Self::Abort | Self::FirstBoot)
    }
}

/// One held lock: its descriptor, open for as long as this value lives.
#[derive(Debug)]
pub struct LockFile {
    _file: File,
    path: PathBuf,
    exclusive: bool,
}

impl LockFile {
    /// The lock file's path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether it is held exclusively.
    #[must_use]
    pub fn exclusive(&self) -> bool {
        self.exclusive
    }
}

/// Take the lock at `path` without waiting: `Ok(None)` when another open
/// file description holds it in a conflicting mode.
///
/// The descriptor is opened close-on-exec (the standard library's default on
/// Unix), so a spawned child never inherits it.
///
/// # Errors
///
/// [`Error::Io`] when the file cannot be opened or locked for any other
/// reason.
pub fn try_lock(path: &Path, exclusive: bool) -> Result<Option<LockFile>> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
        .map_err(|err| Error::Io(format!("{} cannot be opened: {err}", path.display())))?;
    let taken = if exclusive {
        file.try_lock()
    } else {
        file.try_lock_shared()
    };
    match taken {
        Ok(()) => Ok(Some(LockFile {
            _file: file,
            path: path.to_path_buf(),
            exclusive,
        })),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(err)) => Err(Error::Io(format!(
            "{} cannot be locked: {err}",
            path.display()
        ))),
    }
}

/// What the legacy path holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Legacy {
    /// Nothing at the legacy path.
    Absent,
    /// The fence: the marker with a fence content, and no database file.
    Fence {
        /// The marker's content.
        marker: String,
        /// Everything else under the legacy path, relative to it.
        debris: Vec<PathBuf>,
    },
    /// Anything else: a store, a partial layout, a marker absent or foreign.
    Other {
        /// Why it is not the fence.
        why: String,
    },
}

/// What the supervisor fence's path holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SupervisorFence {
    /// Nothing.
    Absent,
    /// A directory holding [`SUPERVISOR_FENCE_FILE`].
    Fence,
    /// A regular file: a pre-043 rendered environment.
    File,
    /// Anything else.
    Other,
}

/// Whether `content` is one of the fence contents (B-4 Terms).
#[must_use]
pub fn is_fence_content(content: &str) -> bool {
    content.starts_with(FENCE_PREFIX) || content.starts_with(GUARD_PREFIX)
}

/// Inspect the legacy path.
///
/// # Errors
///
/// [`Error::Io`] when a directory cannot be listed.
pub fn inspect_legacy(config: &Config) -> Result<Legacy> {
    let legacy = config.legacy_hiqlite_dir();
    let meta = match std::fs::symlink_metadata(&legacy) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Legacy::Absent),
        Err(err) => {
            return Err(Error::Io(format!(
                "{} cannot be inspected: {err}",
                legacy.display()
            )));
        }
    };
    if !meta.is_dir() {
        return Ok(Legacy::Other {
            why: format!("{} is not a directory", legacy.display()),
        });
    }
    let db = legacy.join("state_machine").join("db");
    if let Some(first) = list(&db)?.into_iter().next() {
        return Ok(Legacy::Other {
            why: format!(
                "{} holds {}: a store is at the legacy path",
                db.display(),
                first.display()
            ),
        });
    }
    let marker_path = crate::legacy_marker(config);
    let marker = match std::fs::read(&marker_path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).trim().to_owned(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Legacy::Other {
                why: format!("{} is absent", marker_path.display()),
            });
        }
        Err(err) => {
            return Err(Error::Io(format!(
                "{} cannot be read: {err}",
                marker_path.display()
            )));
        }
    };
    if !is_fence_content(&marker) {
        return Ok(Legacy::Other {
            why: format!(
                "{} holds {:?}, which is not a fence (a pre-043 node's marker, live or stopped uncleanly)",
                marker_path.display(),
                marker
            ),
        });
    }
    let mut debris = Vec::new();
    for entry in list(&legacy)? {
        if entry != Path::new("state_machine") {
            debris.push(entry);
        }
    }
    for entry in list(&legacy.join("state_machine"))? {
        if entry != Path::new("lock") {
            debris.push(Path::new("state_machine").join(entry));
        }
    }
    Ok(Legacy::Fence { marker, debris })
}

/// Inspect the supervisor fence's path.
///
/// # Errors
///
/// [`Error::Io`] when the path cannot be inspected.
pub fn inspect_supervisor_fence(config: &Config) -> Result<SupervisorFence> {
    let path = crate::rauthy_env::legacy_env_path(config);
    match std::fs::symlink_metadata(&path) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(SupervisorFence::Absent),
        Err(err) => Err(Error::Io(format!(
            "{} cannot be inspected: {err}",
            path.display()
        ))),
        Ok(meta) if meta.is_dir() && path.join(SUPERVISOR_FENCE_FILE).is_file() => {
            Ok(SupervisorFence::Fence)
        }
        Ok(meta) if meta.is_file() => Ok(SupervisorFence::File),
        Ok(_) => Ok(SupervisorFence::Other),
    }
}

/// The names in `dir`, relative to it and sorted; none when it is absent.
pub(crate) fn list(dir: &Path) -> Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) if err.kind() == std::io::ErrorKind::NotADirectory => return Ok(Vec::new()),
        Err(err) => {
            return Err(Error::Io(format!(
                "{} cannot be listed: {err}",
                dir.display()
            )));
        }
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|err| Error::Io(format!("{} cannot be listed: {err}", dir.display())))?;
        names.push(PathBuf::from(entry.file_name()));
    }
    names.sort();
    Ok(names)
}

/// The content of a fence this version writes on a volume it did not
/// transition.
#[must_use]
pub fn fence_content() -> String {
    format!("{FENCE_PREFIX}{}", env!("CARGO_PKG_VERSION"))
}

/// The prefix of a fence's temporary: a legacy fence being built in the
/// data directory, or a supervisor fence being built in Rauthy's.
pub const FENCE_TEMP_PREFIX: &str = ".rahi-fence-";

/// Create both fences on a volume with no legacy path and no transition
/// record (B-4a step (4)), so that a crash at any instant leaves a volume the
/// next entry point fences again.
///
/// The supervisor fence goes first, so a legacy fence in place implies a
/// supervisor fence in place: the gate only fences a volume whose legacy
/// path is absent, and would never finish the pair otherwise. The legacy
/// fence is then built whole in a temporary directory beside its path, its
/// marker published inside it by `link(2)`, and put in place by one
/// no-replace rename, so the legacy path is absent or the whole fence and
/// never an empty `state_machine/` that every later start refuses as
/// pre-043 (spec 048 D-6, spec 043 D-28). With `sweep`, temporaries a
/// crashed run left are removed first; only the holder of `cell.lock`
/// sweeps, and it removes only a temporary no live process holds (see
/// [`FenceTemp`]), since an attaching process may be building its own.
///
/// `faults` is called after every step, as [`upgrade::Faults`] is for the
/// verb; the gate passes [`upgrade::NoFaults`].
///
/// # Errors
///
/// [`Error::Conflict`] when what appeared at the legacy path while the
/// fence was being built is not a fence; [`Error::Io`] when a step fails;
/// whatever `faults` returns.
pub fn create_fences(config: &Config, sweep: bool, faults: &dyn upgrade::Faults) -> Result<()> {
    if sweep {
        sweep_fence_temps(&config.data_dir)?;
        sweep_fence_temps(&crate::rauthy_dir(config))?;
        faults.hit("fence.sweep")?;
    }
    if inspect_supervisor_fence(config)? == SupervisorFence::Absent {
        let built = build_supervisor_fence(config)?;
        faults.hit("fence.supervisor.build")?;
        match place_supervisor_fence(config, built.path()) {
            Ok(()) => {}
            // Another process placed one first: ours is a temporary.
            Err(Error::Conflict(_)) => remove_temp(built.path())?,
            Err(err) => return Err(err),
        }
        faults.hit("fence.supervisor.place")?;
    }
    create_legacy_fence(config, faults)
}

/// Remove the legacy path when it is exactly an abandoned fence: a
/// `state_machine/` directory with nothing in it and nothing beside it, which
/// a build before spec 043 D-28 left when it was killed between creating the
/// directory and publishing the marker. The owner chose to read it as absent
/// (spec 043 D-32, spec 048 D-6): no marker means no node claimed it and no
/// file means no store was in it. Each `rmdir` refuses a non-empty directory,
/// so a process writing into it meanwhile keeps what it wrote and the volume
/// is refused as before. Returns whether the path was removed.
///
/// # Errors
///
/// [`Error::Io`] when the path cannot be inspected or a removal fails for a
/// reason other than a concurrent writer.
fn remove_abandoned_fence(config: &Config) -> Result<bool> {
    let legacy = config.legacy_hiqlite_dir();
    let state_machine = legacy.join("state_machine");
    let is_real_dir =
        |path: &Path| std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_dir());
    if !is_real_dir(&legacy)
        || list(&legacy)? != [PathBuf::from("state_machine")]
        || !is_real_dir(&state_machine)
        || !list(&state_machine)?.is_empty()
    {
        return Ok(false);
    }
    for dir in [&state_machine, &legacy] {
        match std::fs::remove_dir(dir) {
            Ok(()) => {}
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                ) =>
            {
                return Ok(false);
            }
            Err(err) => {
                return Err(Error::Io(format!(
                    "{} cannot be removed: {err}",
                    dir.display()
                )));
            }
        }
    }
    crate::fsync_dir(&config.data_dir)?;
    eprintln!(
        "{}: an empty abandoned fence directory was removed and the volume is fenced as fresh \
         (spec 043 D-32)",
        legacy.display()
    );
    Ok(true)
}

/// Build the legacy fence beside its path and rename it into place.
fn create_legacy_fence(config: &Config, faults: &dyn upgrade::Faults) -> Result<()> {
    let legacy = config.legacy_hiqlite_dir();
    let held = FenceTemp::create(&config.data_dir)?;
    let temp = held.path();
    let state_machine = temp.join("state_machine");
    std::fs::create_dir(&state_machine).map_err(|err| {
        Error::Io(format!(
            "{} cannot be created: {err}",
            state_machine.display()
        ))
    })?;
    crate::set_mode(temp, 0o700)?;
    crate::set_mode(&state_machine, 0o700)?;
    faults.hit("fence.legacy.build")?;
    let marker = temp.join(crate::HIQLITE_LOCK_FILE);
    let marker_temp = format!("{FENCE_TEMP_PREFIX}{}", crate::random_id()?);
    crate::publish_whole(&marker, &marker_temp, fence_content().as_bytes())?;
    crate::fsync_dir(temp)?;
    faults.hit("fence.legacy.marker")?;
    match crate::rename_noreplace(temp, &legacy) {
        Ok(()) => {}
        Err(Error::Conflict(_)) => {
            remove_temp(temp)?;
            let content = std::fs::read_to_string(crate::legacy_marker(config)).unwrap_or_default();
            if !is_fence_content(content.trim()) {
                return Err(Error::Conflict(format!(
                    "{} appeared while the fence was being created and is not a fence",
                    legacy.display()
                )));
            }
        }
        Err(err) => return Err(err),
    }
    crate::fsync_dir(&config.data_dir)?;
    faults.hit("fence.legacy.place")
}

/// A fence temporary this process is building, and the lock that says so.
///
/// The temporary's own directory is locked exclusively, non-blocking, for as
/// long as this value lives, and the kernel releases the lock when the
/// process dies. A sweep therefore removes only a temporary it can lock: one
/// a crashed run left, never one a live process is still building (spec 043
/// D-29).
#[derive(Debug)]
pub struct FenceTemp {
    path: PathBuf,
    _lock: File,
}

impl FenceTemp {
    /// How many fresh names [`FenceTemp::create`] tries before it gives up.
    const ATTEMPTS: usize = 8;

    /// Create a fresh temporary directly in `dir` and take its lock.
    ///
    /// Between creating the directory and locking it a sweep may lock and
    /// remove it, so the locked directory must still be the one at the path;
    /// when it is not, a fresh name is tried.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when a step fails, or when every attempt was swept.
    fn create(dir: &Path) -> Result<Self> {
        for _ in 0..Self::ATTEMPTS {
            let path = dir.join(format!("{FENCE_TEMP_PREFIX}{}", crate::random_id()?));
            std::fs::create_dir(&path)
                .map_err(|err| Error::Io(format!("{} cannot be created: {err}", path.display())))?;
            let Some(lock) = lock_temp(&path)? else {
                // A sweep holds it and is removing it.
                continue;
            };
            if is_at(&lock, &path)? {
                return Ok(Self { path, _lock: lock });
            }
        }
        Err(Error::Io(format!(
            "no fence temporary in {} outlived a concurrent sweep in {} attempts",
            dir.display(),
            Self::ATTEMPTS
        )))
    }

    /// The temporary's path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Open `path` and lock it exclusively, non-blocking: `None` when it is gone
/// or another descriptor holds the lock.
fn lock_temp(path: &Path) -> Result<Option<File>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(Error::Io(format!(
                "{} cannot be opened: {err}",
                path.display()
            )));
        }
    };
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(err)) => Err(Error::Io(format!(
            "{} cannot be locked: {err}",
            path.display()
        ))),
    }
}

/// Whether `held` is still the file at `path`.
fn is_at(held: &File, path: &Path) -> Result<bool> {
    use std::os::unix::fs::MetadataExt as _;
    let held = held
        .metadata()
        .map_err(|err| Error::Io(format!("{} cannot be inspected: {err}", path.display())))?;
    match std::fs::symlink_metadata(path) {
        Ok(meta) => Ok(meta.dev() == held.dev() && meta.ino() == held.ino()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(Error::Io(format!(
            "{} cannot be inspected: {err}",
            path.display()
        ))),
    }
}

/// Remove every fence temporary directly in `dir` that no live process holds.
fn sweep_fence_temps(dir: &Path) -> Result<()> {
    for name in list(dir)? {
        if !name.to_string_lossy().starts_with(FENCE_TEMP_PREFIX) {
            continue;
        }
        let path = dir.join(name);
        // Held while it is removed, so its creator, if it is still between
        // creating and locking it, sees it gone and tries a fresh name.
        if let Some(lock) = lock_temp(&path)?
            && is_at(&lock, &path)?
        {
            remove_temp(&path)?;
        }
    }
    Ok(())
}

fn remove_temp(path: &Path) -> Result<()> {
    std::fs::remove_dir_all(path)
        .map_err(|err| Error::Io(format!("{} cannot be removed: {err}", path.display())))
}

/// The supervisor fence's `FENCE` text.
#[must_use]
pub fn supervisor_fence_text(config: &Config) -> String {
    format!(
        "{}\nThis path is a fence (spec 043 B-5). The rendered Rauthy environment is at {}; \
         a pre-043 `rahi supervise` fails its read here and exits before it spawns Rauthy.\n",
        fence_content(),
        crate::rauthy_env::env_path(config).display()
    )
}

/// Build the supervisor fence in a temporary directory beside its path and
/// return that directory, held and not yet in place (B-5, T1 (e)).
///
/// # Errors
///
/// [`Error::Io`] when a step fails.
pub fn build_supervisor_fence(config: &Config) -> Result<FenceTemp> {
    use std::io::Write as _;
    let rauthy = crate::rauthy_dir(config);
    std::fs::create_dir_all(&rauthy)
        .map_err(|err| Error::Io(format!("{} cannot be created: {err}", rauthy.display())))?;
    crate::set_mode(&rauthy, crate::KEY_DIR_MODE)?;
    let held = FenceTemp::create(&rauthy)?;
    let temp = held.path();
    let file = temp.join(SUPERVISOR_FENCE_FILE);
    let mut out = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&file)
        .map_err(|err| Error::Io(format!("{} cannot be created: {err}", file.display())))?;
    out.write_all(supervisor_fence_text(config).as_bytes())
        .and_then(|()| out.sync_all())
        .map_err(|err| Error::Io(format!("{} cannot be written: {err}", file.display())))?;
    crate::fsync_dir(temp)?;
    crate::fsync_dir(&rauthy)?;
    Ok(held)
}

/// Put a built supervisor fence in place by a no-replace rename.
///
/// # Errors
///
/// As [`crate::rename_noreplace`].
pub fn place_supervisor_fence(config: &Config, built: &Path) -> Result<()> {
    let path = crate::rauthy_env::legacy_env_path(config);
    crate::rename_noreplace(built, &path)?;
    crate::fsync_dir(&crate::rauthy_dir(config))
}

/// What the gate found and holds.
#[derive(Debug)]
pub struct Gate {
    entry: Entry,
    cell: Option<LockFile>,
    transition: LockFile,
    record: Option<Record>,
    legacy: Legacy,
    supervisor_fence: SupervisorFence,
    fenced: bool,
}

impl Gate {
    /// The entry point this gate was taken for.
    #[must_use]
    pub fn entry(&self) -> Entry {
        self.entry
    }

    /// Whether this process owns the app store's node (it holds
    /// `cell.lock`); `false` means it attaches to the process that does.
    #[must_use]
    pub fn owns_cell(&self) -> bool {
        self.cell.is_some()
    }

    /// The cell lock, when held.
    #[must_use]
    pub fn cell_lock(&self) -> Option<&LockFile> {
        self.cell.as_ref()
    }

    /// The layout lock.
    #[must_use]
    pub fn transition_lock(&self) -> &LockFile {
        &self.transition
    }

    /// The transition record read after the locks.
    #[must_use]
    pub fn record(&self) -> Option<&Record> {
        self.record.as_ref()
    }

    /// The legacy path as the gate found it (after creating the fence).
    #[must_use]
    pub fn legacy(&self) -> &Legacy {
        &self.legacy
    }

    /// The supervisor fence as the gate found it (after creating it).
    #[must_use]
    pub fn supervisor_fence(&self) -> SupervisorFence {
        self.supervisor_fence
    }

    /// Whether this gate created both fences on a fresh volume.
    #[must_use]
    pub fn fenced(&self) -> bool {
        self.fenced
    }

    /// Debris beside the fence, relative to the legacy path.
    #[must_use]
    pub fn debris(&self) -> &[PathBuf] {
        match &self.legacy {
            Legacy::Fence { debris, .. } => debris,
            _ => &[],
        }
    }
}

/// B-4a's gate for `entry`, reading this process's own environment.
///
/// # Errors
///
/// As [`gate_with_env`].
pub fn gate(config: &Config, entry: Entry) -> Result<Gate> {
    gate_with_env(config, entry, &|name| std::env::var_os(name))
}

/// B-4a's gate for `entry`, with `own_env` as this process's environment.
///
/// # Errors
///
/// [`Error::Config`] naming a refused variable, before any lock is taken;
/// [`Error::Conflict`] when a lock is held elsewhere, when the transition
/// state or the layout is not one `entry` accepts (the message names
/// `rahi upgrade-cache` where it is the way forward); [`Error::Integrity`]
/// when the record is unreadable; [`Error::Io`] when the volume cannot be
/// read or written.
pub fn gate_with_env(
    config: &Config,
    entry: Entry,
    own_env: &dyn Fn(&str) -> Option<OsString>,
) -> Result<Gate> {
    // (1) The environment, before anything is touched.
    for (name, why) in REFUSED_ENV {
        if own_env(name).is_some() {
            return Err(Error::Config(format!(
                "{name} is set in this process's environment and {} refuses it: {why}; unset it \
                 (spec 043 B-3)",
                entry.name()
            )));
        }
    }

    // (2) The locks, non-blocking, cell first.
    std::fs::create_dir_all(&config.data_dir).map_err(|err| {
        Error::Io(format!(
            "{} cannot be created: {err}",
            config.data_dir.display()
        ))
    })?;
    let cell_path = config.data_dir.join(CELL_LOCK_FILE);
    let cell = match try_lock(&cell_path, true)? {
        Some(lock) => Some(lock),
        None => match entry {
            Entry::Store { may_attach: true } => None,
            _ => {
                return Err(Error::Conflict(format!(
                    "{} is held by another process: the cell is running or another verb owns \
                     it; {} refuses rather than waits",
                    cell_path.display(),
                    entry.name()
                )));
            }
        },
    };
    let transition_path = config.data_dir.join(TRANSITION_LOCK_FILE);
    let exclusive = entry.layout_exclusive();
    let Some(transition) = try_lock(&transition_path, exclusive)? else {
        return Err(Error::Conflict(format!(
            "{} is held by another process: {}; {} refuses rather than waits",
            transition_path.display(),
            if exclusive {
                "another entry point is running on this volume"
            } else {
                "a transition, an abort or a restore is changing the layout"
            },
            entry.name()
        )));
    };

    // (3) Only now: the record and the layout.
    let record = upgrade::read(config)?;
    let mut legacy = inspect_legacy(config)?;
    let mut supervisor_fence = inspect_supervisor_fence(config)?;

    // (4) A fresh volume gets both fences before anything creates or opens
    // the app store.
    let mut fenced = false;
    if entry.creates_fences()
        && record.is_none()
        && matches!(legacy, Legacy::Other { .. })
        && remove_abandoned_fence(config)?
    {
        legacy = inspect_legacy(config)?;
    }
    if entry.creates_fences() && record.is_none() && legacy == Legacy::Absent {
        create_fences(config, cell.is_some(), &upgrade::NoFaults)?;
        legacy = inspect_legacy(config)?;
        supervisor_fence = inspect_supervisor_fence(config)?;
        fenced = true;
    }

    let phase = record.as_ref().map(|r| r.phase);
    if !entry.accepts(phase) {
        let phase = phase.map_or("none", Phase::name);
        return Err(Error::Conflict(format!(
            "{} holds a transition at `{phase}`, which {} does not accept; {}",
            upgrade::state_path(config).display(),
            entry.name(),
            if phase == "aborted" {
                "the transition was aborted: start the pre-043 image, or run \
                 `rahi upgrade-cache --backup <archive>` again"
            } else {
                "run `rahi upgrade-cache --backup <archive>` to complete it (spec 043 B-4)"
            }
        )));
    }
    if entry.needs_fence() {
        if let Legacy::Other { why } = &legacy {
            return Err(Error::Conflict(format!(
                "{} is not the fence this version requires: {why}; this volume was written by a \
                 pre-043 rahi, and the only supported crossing is \
                 `rahi upgrade-cache --backup <archive>` (spec 043 B-3)",
                config.legacy_hiqlite_dir().display()
            )));
        }
        if legacy == Legacy::Absent {
            return Err(Error::Conflict(format!(
                "{} is absent on a volume with a transition record: the fence was removed, which \
                 this version does not repair; restore the volume from its archive \
                 (spec 043 B-5a)",
                config.legacy_hiqlite_dir().display()
            )));
        }
    }
    let gate = Gate {
        entry,
        cell,
        transition,
        record,
        legacy,
        supervisor_fence,
        fenced,
    };
    let debris: &[PathBuf] = if entry.needs_fence() {
        gate.debris()
    } else {
        &[]
    };
    for entry in debris {
        eprintln!(
            "{}: debris beside the fence at {} is left in place (spec 043 B-4a)",
            gate.entry.name(),
            config.legacy_hiqlite_dir().join(entry).display()
        );
    }
    Ok(gate)
}
