//! Stop recording and outcome classification (spec 043 B-9, B-10).
//!
//! A long-running process (`serve` alone, or `supervise`) leaves one record
//! of its own stop at `<data>/stop.json`: written whole at boot as `{boot,
//! started_at}`, given `received_at` when SIGTERM arrives, and given the
//! outcome and every phase's duration on exit. Each write is
//! write-to-temporary, fsync, rename, fsync of `<data>`, so a reader sees one
//! whole record or the previous one.
//!
//! The process reports only what it observed. The next boot reads what the
//! previous one left, classifies it, and exports the classification as
//! `rahi_previous_stop{outcome, cause}`. A cause is named only when a witness
//! record names that boot; everything else is `unknown`, because a SIGKILL,
//! a crash, a power loss and an interrupted write leave the same record.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rahi_types::{Config, Error, Result};
use serde::{Deserialize, Serialize};

/// The stop record, under the data directory.
pub const STOP_FILE: &str = "stop.json";

/// The boot sequence, under the data directory: the number of the last boot
/// that got as far as its first write. It is written before the stop record,
/// so a boot that never wrote its record is still counted and the next boot
/// can tell the record it finds is for an older boot.
pub const BOOT_SEQ_FILE: &str = "boot.seq";

/// A witness of a forced kill, under the data directory: written by the
/// process that sent the SIGKILL, naming the boot it killed.
pub const WITNESS_FILE: &str = "stop-witness.json";

/// Stream drain S (spec 026 B-7), its default.
pub const STREAM_DRAIN: Duration = Duration::from_secs(10);
/// Connection drain C (`DRAIN_BUDGET` of `serve`).
pub const CONNECTION_DRAIN: Duration = Duration::from_secs(10);
/// Denial drain D (spec 035 D-1), its default.
pub const DENIAL_DRAIN: Duration = Duration::from_secs(5);
/// Store shutdown H: hiqlite's caller-side wait, after which
/// `Client::shutdown` answers `Error::Timeout` ([`rahi_store::SHUTDOWN_WAIT`]).
pub const STORE_SHUTDOWN: Duration = rahi_store::SHUTDOWN_WAIT;
/// Rauthy's stop R: SIGTERM to SIGKILL on a propagated SIGTERM.
pub const RAUTHY_STOP: Duration = Duration::from_secs(10);

/// How long `serve` has to finish its own stop under `supervise`:
/// `S + C + D + H` (B-9's composition, forty seconds with the defaults).
pub const SERVE_GRACE: Duration = Duration::from_secs(40);

/// The orchestrator's grace, `SERVE_GRACE + R`: the pod's
/// `terminationGracePeriodSeconds` and the documented `docker stop -t`.
pub const CONTAINER_GRACE: Duration = Duration::from_secs(50);

/// B-9's composition check over any set of bounds: the serve grace covers
/// its four phases and the container grace covers the serve grace and
/// Rauthy's stop. `Err` names the first sum that does not hold.
///
/// # Errors
///
/// [`Error::Config`] naming the sum that fails.
pub fn check_composition(
    phases: [Duration; 4],
    serve_grace: Duration,
    rauthy_stop: Duration,
    container_grace: Duration,
) -> Result<()> {
    let serve_sum = phases
        .iter()
        .try_fold(Duration::ZERO, |sum, phase| sum.checked_add(*phase))
        .ok_or_else(|| Error::Config("the stop phases overflow".to_owned()))?;
    if serve_grace < serve_sum {
        return Err(Error::Config(format!(
            "the serve grace {serve_grace:?} is below S + C + D + H = {serve_sum:?}"
        )));
    }
    let container_sum = serve_grace
        .checked_add(rauthy_stop)
        .ok_or_else(|| Error::Config("the serve grace and Rauthy's stop overflow".to_owned()))?;
    if container_grace < container_sum {
        return Err(Error::Config(format!(
            "the container grace {container_grace:?} is below SERVE_GRACE + R = {container_sum:?}"
        )));
    }
    Ok(())
}

/// One reason a stop is unconfirmed (B-10).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum Reason {
    /// `Client::shutdown` answered `Error::Timeout`.
    StoreTimeout,
    /// `Client::shutdown` answered another error.
    StoreError {
        /// What it said.
        error: String,
    },
    /// A stream was still open after S.
    StreamDrainOverrun {
        /// How many were cut.
        open: usize,
    },
    /// A connection was still open after C.
    ConnectionDrainOverrun,
    /// Denials still owed when D expired.
    DenialsAbandoned {
        /// How many.
        n: usize,
    },
    /// `serve` did not finish inside the supervisor's serve grace.
    ServeGraceOverrun,
    /// Rauthy exited with a code other than `0`.
    RauthyNonzero {
        /// Its code (`128 + signal` for a signal death).
        code: i32,
    },
    /// The supervisor sent Rauthy SIGKILL; witnessed by the supervisor.
    RauthyKilled,
    /// The app store failed terminally (B-7).
    StorageTerminal,
    /// `serve` ended with an error of its own, carried with its exit code.
    ServeError {
        /// The error.
        error: String,
    },
}

impl Reason {
    /// The reason's name as B-10 spells it.
    #[must_use]
    pub fn name(&self) -> String {
        match self {
            Self::StoreTimeout => "store_timeout".to_owned(),
            Self::StoreError { .. } => "store_error".to_owned(),
            Self::StreamDrainOverrun { .. } => "stream_drain_overrun".to_owned(),
            Self::ConnectionDrainOverrun => "connection_drain_overrun".to_owned(),
            Self::DenialsAbandoned { n } => format!("denials_abandoned{{{n}}}"),
            Self::ServeGraceOverrun => "serve_grace_overrun".to_owned(),
            Self::RauthyNonzero { code } => format!("rauthy_nonzero{{{code}}}"),
            Self::RauthyKilled => "rauthy_killed".to_owned(),
            Self::StorageTerminal => "storage_terminal".to_owned(),
            Self::ServeError { .. } => "serve_error".to_owned(),
        }
    }
}

/// One stop phase as it ran.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhaseTime {
    /// `stream_drain`, `connection_drain`, `denial_drain`, `store_shutdown`
    /// or `rauthy_stop`.
    pub phase: String,
    /// How long it took, in milliseconds.
    pub millis: u64,
    /// Its configured bound, in milliseconds.
    pub bound_millis: u64,
}

impl PhaseTime {
    /// A phase that took `took` against `bound`.
    #[must_use]
    pub fn new(phase: &str, took: Duration, bound: Duration) -> Self {
        Self {
            phase: phase.to_owned(),
            millis: millis(took),
            bound_millis: millis(bound),
        }
    }
}

/// What one process observed of its own stop.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Observed {
    /// Every phase that ran, in order.
    pub phases: Vec<PhaseTime>,
    /// Every reason the stop is unconfirmed.
    pub reasons: Vec<Reason>,
}

impl Observed {
    /// Nothing went wrong.
    #[must_use]
    pub fn confirmed(&self) -> bool {
        self.reasons.is_empty()
    }

    /// Add a phase.
    pub fn phase(&mut self, phase: &str, took: Duration, bound: Duration) {
        self.phases.push(PhaseTime::new(phase, took, bound));
    }

    /// Add a reason.
    pub fn reason(&mut self, reason: Reason) {
        self.reasons.push(reason);
    }

    /// Fold another process's (or phase group's) observation into this one.
    pub fn absorb(&mut self, other: Self) {
        self.phases.extend(other.phases);
        self.reasons.extend(other.reasons);
    }

    /// The exit status B-10 gives this observation: `0` when confirmed,
    /// `3` otherwise.
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        if self.confirmed() {
            0
        } else {
            rahi_types::error::EXIT_INFRA
        }
    }

    /// One line naming every reason.
    #[must_use]
    pub fn render(&self) -> String {
        if self.confirmed() {
            return "confirmed".to_owned();
        }
        let names: Vec<String> = self.reasons.iter().map(Reason::name).collect();
        format!("unconfirmed: {}", names.join(", "))
    }
}

/// The outcome written on exit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Outcome {
    /// Every phase inside its bound and nothing lost.
    Confirmed,
    /// At least one reason applies.
    Unconfirmed {
        /// Every reason that applies.
        reasons: Vec<Reason>,
    },
}

/// The record on disk.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopRecord {
    /// This boot's number ([`BOOT_SEQ_FILE`]).
    pub boot: u64,
    /// Which entry point wrote it: `serve` or `supervise`.
    pub entry: String,
    /// When the process started, milliseconds since the epoch.
    pub started_at: u64,
    /// When SIGTERM arrived.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub received_at: Option<u64>,
    /// How the stop ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Outcome>,
    /// Every phase that ran.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub phases: Vec<PhaseTime>,
    /// When the outcome was written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exited_at: Option<u64>,
    /// The exit status the process left with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}

/// A witness record: who killed which boot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Witness {
    /// The boot that was killed.
    pub boot: u64,
    /// Who sent the signal.
    pub by: String,
    /// When, milliseconds since the epoch.
    pub at: u64,
}

/// The previous boot's stop, as the next boot classifies it (B-10).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Previous {
    /// The previous boot recorded its outcome.
    Recorded(Outcome),
    /// It started and recorded no signal.
    StoppedWithoutSignal,
    /// It recorded SIGTERM and no outcome.
    IncompleteAfterSigterm,
    /// The record is for an older boot, or there is none.
    NoRecord,
}

impl Previous {
    /// The `outcome` label of `rahi_previous_stop`.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Recorded(Outcome::Confirmed) => "confirmed",
            Self::Recorded(Outcome::Unconfirmed { .. }) => "unconfirmed",
            Self::StoppedWithoutSignal => "stopped_without_signal",
            Self::IncompleteAfterSigterm => "incomplete_after_sigterm",
            Self::NoRecord => "no_record",
        }
    }
}

/// What caused the previous stop, as far as anything witnessed it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    /// The process recorded its own outcome.
    Recorded,
    /// A witness record names the boot as killed.
    WitnessedKill,
    /// Nothing says.
    Unknown,
}

impl Cause {
    /// The `cause` label of `rahi_previous_stop`.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Recorded => "recorded",
            Self::WitnessedKill => "witnessed_kill",
            Self::Unknown => "unknown",
        }
    }
}

/// The classification of the previous boot's stop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Classified {
    /// The previous boot's number, when one was counted.
    pub boot: Option<u64>,
    /// What its record says.
    pub previous: Previous,
    /// What caused it.
    pub cause: Cause,
}

impl Classified {
    /// The line the next boot logs.
    #[must_use]
    pub fn render(&self) -> String {
        let boot = self
            .boot
            .map_or_else(|| "none".to_owned(), |b| b.to_string());
        let detail = match &self.previous {
            Previous::Recorded(Outcome::Unconfirmed { reasons }) => format!(
                " ({})",
                reasons
                    .iter()
                    .map(Reason::name)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            _ => String::new(),
        };
        format!(
            "previous stop: boot {boot}, {}{detail}, cause {}",
            self.previous.label(),
            self.cause.label()
        )
    }
}

/// B-10's classification from what the previous boot left: its counted boot
/// number, its record, and any witness.
#[must_use]
pub fn classify(
    last_boot: Option<u64>,
    record: Option<&StopRecord>,
    witness: Option<&Witness>,
) -> Classified {
    let witnessed = |boot: u64| witness.is_some_and(|w| w.boot == boot);
    let Some(boot) = last_boot else {
        return Classified {
            boot: None,
            previous: Previous::NoRecord,
            cause: Cause::Unknown,
        };
    };
    let cause_if_unrecorded = if witnessed(boot) {
        Cause::WitnessedKill
    } else {
        Cause::Unknown
    };
    let previous = match record {
        Some(record) if record.boot == boot => match (&record.outcome, record.received_at) {
            (Some(outcome), _) => {
                return Classified {
                    boot: Some(boot),
                    previous: Previous::Recorded(outcome.clone()),
                    cause: Cause::Recorded,
                };
            }
            (None, Some(_)) => Previous::IncompleteAfterSigterm,
            (None, None) => Previous::StoppedWithoutSignal,
        },
        _ => Previous::NoRecord,
    };
    Classified {
        boot: Some(boot),
        previous,
        cause: cause_if_unrecorded,
    }
}

/// The path of the stop record.
#[must_use]
pub fn stop_path(config: &Config) -> PathBuf {
    config.data_dir.join(STOP_FILE)
}

/// The path of the boot sequence.
#[must_use]
pub fn boot_seq_path(config: &Config) -> PathBuf {
    config.data_dir.join(BOOT_SEQ_FILE)
}

/// The path of the witness record.
#[must_use]
pub fn witness_path(config: &Config) -> PathBuf {
    config.data_dir.join(WITNESS_FILE)
}

/// Read the stop record, if one is there and parses.
#[must_use]
pub fn read_record(config: &Config) -> Option<StopRecord> {
    read_json(&stop_path(config))
}

/// Read the witness record, if one is there and parses.
#[must_use]
pub fn read_witness(config: &Config) -> Option<Witness> {
    read_json(&witness_path(config))
}

/// Read the last counted boot, if any.
#[must_use]
pub fn read_boot_seq(config: &Config) -> Option<u64> {
    std::fs::read_to_string(boot_seq_path(config))
        .ok()
        .and_then(|text| text.trim().parse().ok())
}

/// Write a witness naming `boot` as killed by `by`. A test harness and the
/// supervisor are the only writers.
///
/// # Errors
///
/// [`Error::Io`] when the record cannot be written.
pub fn write_witness(data_dir: &Path, boot: u64, by: &str) -> Result<()> {
    let witness = Witness {
        boot,
        by: by.to_owned(),
        at: now_millis(),
    };
    let bytes = serde_json::to_vec_pretty(&witness)
        .map_err(|err| Error::Io(format!("the witness cannot be encoded: {err}")))?;
    crate::write_replacing(&data_dir.join(WITNESS_FILE), &bytes)
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

/// One process's stop record, kept current on disk (B-10).
#[derive(Debug)]
pub struct Recorder {
    config: Config,
    record: std::sync::Mutex<StopRecord>,
    previous: Classified,
}

impl Recorder {
    /// Boot: classify what the previous boot left, count this boot, and
    /// rewrite the record as `{boot, started_at}`. Called after the gate
    /// (spec 043 B-4a) and before the store opens.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the boot sequence or the record cannot be written.
    pub fn begin(config: &Config, entry: &str) -> Result<Self> {
        let last = read_boot_seq(config);
        let previous = classify(
            last,
            read_record(config).as_ref(),
            read_witness(config).as_ref(),
        );
        let boot = last.unwrap_or(0).saturating_add(1);
        crate::write_replacing(&boot_seq_path(config), format!("{boot}\n").as_bytes())?;
        let record = StopRecord {
            boot,
            entry: entry.to_owned(),
            started_at: now_millis(),
            received_at: None,
            outcome: None,
            phases: Vec::new(),
            exited_at: None,
            exit_code: None,
        };
        let recorder = Self {
            config: config.clone(),
            record: std::sync::Mutex::new(record),
            previous,
        };
        recorder.flush()?;
        Ok(recorder)
    }

    /// The previous boot's stop, as this boot classified it.
    #[must_use]
    pub fn previous(&self) -> &Classified {
        &self.previous
    }

    /// This boot's number.
    #[must_use]
    pub fn boot(&self) -> u64 {
        self.lock().boot
    }

    /// SIGTERM arrived. A failed write is reported, not fatal: the stop goes
    /// on, and the next boot classifies the older record honestly.
    pub fn received(&self) {
        {
            let mut record = self.lock();
            if record.received_at.is_none() {
                record.received_at = Some(now_millis());
            }
        }
        if let Err(err) = self.flush() {
            eprintln!("stop: the SIGTERM cannot be recorded: {err}");
        }
    }

    /// The process is exiting with `exit_code` after observing `observed`.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the record cannot be written.
    pub fn finish(&self, observed: &Observed, exit_code: i32) -> Result<()> {
        {
            let mut record = self.lock();
            record.phases.clone_from(&observed.phases);
            record.outcome = Some(if observed.confirmed() {
                Outcome::Confirmed
            } else {
                Outcome::Unconfirmed {
                    reasons: observed.reasons.clone(),
                }
            });
            record.exited_at = Some(now_millis());
            record.exit_code = Some(exit_code);
        }
        self.flush()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, StopRecord> {
        self.record
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn flush(&self) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(&*self.lock())
            .map_err(|err| Error::Io(format!("the stop record cannot be encoded: {err}")))?;
        crate::write_replacing(&stop_path(&self.config), &bytes)
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, millis)
}

fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn record(boot: u64, received: bool, outcome: Option<Outcome>) -> StopRecord {
        StopRecord {
            boot,
            entry: "serve".to_owned(),
            started_at: 1,
            received_at: received.then_some(2),
            outcome,
            phases: Vec::new(),
            exited_at: None,
            exit_code: None,
        }
    }

    #[test]
    fn the_defaults_compose() {
        check_composition(
            [STREAM_DRAIN, CONNECTION_DRAIN, DENIAL_DRAIN, STORE_SHUTDOWN],
            SERVE_GRACE,
            RAUTHY_STOP,
            CONTAINER_GRACE,
        )
        .unwrap();
        assert_eq!(SERVE_GRACE, Duration::from_secs(40));
        assert_eq!(CONTAINER_GRACE, Duration::from_secs(50));
    }

    #[test]
    fn a_grace_below_its_sum_is_refused() {
        let phases = [STREAM_DRAIN, CONNECTION_DRAIN, DENIAL_DRAIN, STORE_SHUTDOWN];
        let err = check_composition(
            phases,
            Duration::from_secs(15),
            RAUTHY_STOP,
            CONTAINER_GRACE,
        )
        .unwrap_err();
        assert!(err.message().contains("S + C + D + H"), "{err}");
        let err = check_composition(phases, SERVE_GRACE, RAUTHY_STOP, Duration::from_secs(30))
            .unwrap_err();
        assert!(err.message().contains("SERVE_GRACE + R"), "{err}");
    }

    #[test]
    fn the_next_boot_classifies_what_it_finds() {
        let confirmed = record(4, true, Some(Outcome::Confirmed));
        let c = classify(Some(4), Some(&confirmed), None);
        assert_eq!(c.previous, Previous::Recorded(Outcome::Confirmed));
        assert_eq!(c.cause, Cause::Recorded);

        let started = record(4, false, None);
        let c = classify(Some(4), Some(&started), None);
        assert_eq!(c.previous, Previous::StoppedWithoutSignal);
        assert_eq!(c.cause, Cause::Unknown);

        let received = record(4, true, None);
        let c = classify(Some(4), Some(&received), None);
        assert_eq!(c.previous, Previous::IncompleteAfterSigterm);
        assert_eq!(c.cause, Cause::Unknown);

        let witness = Witness {
            boot: 4,
            by: "harness".to_owned(),
            at: 3,
        };
        let c = classify(Some(4), Some(&received), Some(&witness));
        assert_eq!(c.previous, Previous::IncompleteAfterSigterm);
        assert_eq!(c.cause, Cause::WitnessedKill);

        let stale = Witness { boot: 3, ..witness };
        let c = classify(Some(4), Some(&received), Some(&stale));
        assert_eq!(
            c.cause,
            Cause::Unknown,
            "a witness for another boot says nothing"
        );

        let older = record(3, true, Some(Outcome::Confirmed));
        let c = classify(Some(4), Some(&older), None);
        assert_eq!(c.previous, Previous::NoRecord);

        let c = classify(None, None, None);
        assert_eq!(c.previous, Previous::NoRecord);
        assert_eq!(c.cause, Cause::Unknown);
    }

    #[test]
    fn a_recorder_counts_boots_and_writes_whole_records() {
        let dir = tempfile::tempdir().unwrap();
        let env: std::collections::BTreeMap<String, String> = [
            ("RAHI_PUBLIC_URL", "http://localhost:8080".to_owned()),
            ("RAHI_DATA_DIR", dir.path().display().to_string()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect();
        let config = Config::from_env(&env).unwrap();
        let first = Recorder::begin(&config, "serve").unwrap();
        assert_eq!(first.boot(), 1);
        assert_eq!(first.previous().previous, Previous::NoRecord);
        first.received();
        let mut observed = Observed::default();
        observed.phase("store_shutdown", Duration::from_millis(5), STORE_SHUTDOWN);
        first.finish(&observed, observed.exit_code()).unwrap();
        drop(first);

        let second = Recorder::begin(&config, "serve").unwrap();
        assert_eq!(second.boot(), 2);
        assert_eq!(
            second.previous().previous,
            Previous::Recorded(Outcome::Confirmed)
        );
        second.received();
        drop(second);

        let third = Recorder::begin(&config, "supervise").unwrap();
        assert_eq!(third.previous().previous, Previous::IncompleteAfterSigterm);
        assert_eq!(third.previous().cause, Cause::Unknown);
        let on_disk = read_record(&config).unwrap();
        assert_eq!(on_disk.boot, 3);
        assert!(on_disk.received_at.is_none() && on_disk.outcome.is_none());
    }

    #[test]
    fn reasons_render_as_b10_names_them() {
        let mut observed = Observed::default();
        observed.reason(Reason::DenialsAbandoned { n: 3 });
        observed.reason(Reason::RauthyNonzero { code: 137 });
        observed.reason(Reason::StoreTimeout);
        assert_eq!(
            observed.render(),
            "unconfirmed: denials_abandoned{3}, rauthy_nonzero{137}, store_timeout"
        );
        assert_eq!(observed.exit_code(), 3);
    }
}
