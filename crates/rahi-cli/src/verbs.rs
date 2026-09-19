//! argv, parsed (B-1): the verbs and their few flags, and the help text
//! `--help` prints, which lists exactly the verbs of B-1 (AC-2).

use std::path::PathBuf;

use rahi_types::{Error, Result};

/// The verbs, in the order `--help` lists them.
pub const VERBS: [&str; 10] = [
    "serve",
    "preflight",
    "migrate",
    "backup",
    "restore",
    "ledger verify",
    "ledger export",
    "ledger reindex",
    "supervise",
    "first-boot",
];

/// What argv asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verb {
    /// Print the usage and exit 0.
    Help,
    /// Print the version and exit 0.
    Version,
    /// B-2.
    Serve,
    /// B-3.
    Preflight,
    /// B-4; `backup` is `--backup`, `adopt_manifest` is
    /// `--adopt-manifest` (spec 036 B-3).
    Migrate {
        /// Take a backup before applying anything.
        backup: bool,
        /// Adopt the booted manifest: append a transition when it differs
        /// from the chain's current one (spec 036 B-3).
        adopt_manifest: bool,
    },
    /// B-5; `to` is `--to`, a directory or `s3://bucket/prefix`.
    Backup {
        /// Where the archive goes; the volume's backups directory when
        /// absent.
        to: Option<String>,
    },
    /// B-6; `key` is `--key`, the backup identity file when the volume has
    /// no key set yet; `adopt` is `--adopt` (spec 036 B-9).
    Restore {
        /// The archive.
        archive: PathBuf,
        /// The identity file.
        key: Option<PathBuf>,
        /// Restore an archive whose chain names a manifest this binary does
        /// not, leaving the transition to the next deploy step (spec 036
        /// B-9).
        adopt: bool,
    },
    /// B-7; `full` is `--full`.
    LedgerVerify {
        /// Fetch every archived segment body too.
        full: bool,
    },
    /// B-7.
    LedgerExport {
        /// Where the JSON lines go.
        path: PathBuf,
    },
    /// Spec 042 B-8, annotated `(042)` in spec 030 B-1 by that spec's D-9.
    ///
    /// The only mutating verb under `ledger`: it writes identity rows
    /// rebuilt from archived bodies, which is why it is a verb of its own
    /// rather than a flag on the read-only `ledger verify` (042 D-1).
    LedgerReindex {
        /// The archive holding the sealed segment bodies.
        archive: PathBuf,
    },
    /// Spec 031.
    Supervise,
    /// Spec 031; `export` is `--export` (spec 032 B-2).
    FirstBoot {
        /// Render a Kubernetes Secret holding a freshly minted key set
        /// instead of writing the volume.
        export: bool,
    },
}

/// The usage text.
#[must_use]
pub fn usage() -> String {
    let mut out = String::from(
        "rahi: one binary, the chassis verbs (spec 030)\n\
         \n\
         usage: rahi <verb> [options]\n\
         \n\
         verbs:\n",
    );
    let lines = [
        (
            "serve",
            "load config, open the store and the chain, boot the kernel, listen",
        ),
        (
            "preflight",
            "check the deployment by name; exit 1 on any failure; never mutates",
        ),
        (
            "migrate [--backup] [--adopt-manifest]",
            "apply the cell's migrations on the leader, and adopt its manifest (036)",
        ),
        (
            "backup [--to <dir|s3://bucket/prefix>]",
            "one encrypted archive: both stores, the keys, a manifest",
        ),
        (
            "restore <archive> [--key <file>] [--adopt]",
            "a cluster reset from one archive; single-shot by marker",
        ),
        (
            "ledger verify [--full]",
            "verify the decision chain at resident depth, or with every segment",
        ),
        (
            "ledger export <path>",
            "write the resident chain as attest-ledger JSON lines",
        ),
        (
            "ledger reindex <archive>",
            "MUTATES: rebuild the lifetime identity index from archived segments",
        ),
        ("supervise", "run rauthy and serve as one unit (spec 031)"),
        (
            "first-boot",
            "generate keys and rauthy's environment once (spec 031)",
        ),
    ];
    for (verb, what) in lines {
        out.push_str(&format!("  {verb:<44} {what}\n"));
    }
    out.push_str(
        "\n\
         exit codes: 0 ok, 1 failure, 2 stale (behind on migrations), 3 infrastructure\n\
         \n\
         environment: RAHI_PUBLIC_URL is required; RAHI_DATA_DIR, RAHI_LISTEN_ADDR,\n\
         RAHI_HIQLITE_API_ADDR, RAHI_HIQLITE_RAFT_ADDR, RAHI_RAUTHY_ADDR, RAHI_RAUTHY_MODE,\n\
         RAHI_TRUSTED_PROXY_HOPS, RAHI_OTLP_ENDPOINT, RAHI_LEDGER_ARCHIVE_DIR have defaults.\n",
    );
    out
}

/// Parse argv (without the program name).
///
/// # Errors
///
/// [`Error::Validation`] naming what was wrong; the caller prints it with
/// the usage and exits 1.
pub fn parse<I, S>(args: I) -> Result<Verb>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let args: Vec<String> = args.into_iter().map(|s| s.as_ref().to_owned()).collect();
    let mut it = args.iter().map(String::as_str);
    let Some(verb) = it.next() else {
        return Err(Error::Validation("a verb is required".to_owned()));
    };
    let rest: Vec<&str> = it.collect();
    match verb {
        "--help" | "-h" | "help" => Ok(Verb::Help),
        "--version" | "-V" | "version" => Ok(Verb::Version),
        "serve" => no_options(verb, &rest, Verb::Serve),
        "preflight" => no_options(verb, &rest, Verb::Preflight),
        "supervise" => no_options(verb, &rest, Verb::Supervise),
        "first-boot" => match rest.as_slice() {
            [] => Ok(Verb::FirstBoot { export: false }),
            ["--export"] => Ok(Verb::FirstBoot { export: true }),
            _ => Err(unexpected(verb, &rest)),
        },
        "migrate" => migrate_flags(&rest),
        "backup" => match rest.as_slice() {
            [] => Ok(Verb::Backup { to: None }),
            ["--to", to] => Ok(Verb::Backup {
                to: Some((*to).to_owned()),
            }),
            _ => Err(unexpected(verb, &rest)),
        },
        "restore" => restore_flags(&rest),
        "ledger" => match rest.as_slice() {
            ["verify"] => Ok(Verb::LedgerVerify { full: false }),
            ["verify", "--full"] => Ok(Verb::LedgerVerify { full: true }),
            ["export", path] => Ok(Verb::LedgerExport {
                path: PathBuf::from(path),
            }),
            ["export"] => Err(Error::Validation("ledger export needs a path".to_owned())),
            ["reindex", archive] => Ok(Verb::LedgerReindex {
                archive: PathBuf::from(archive),
            }),
            ["reindex"] => Err(Error::Validation(
                "ledger reindex needs an archive".to_owned(),
            )),
            [] => Err(Error::Validation(
                "ledger needs a subverb: verify, export or reindex".to_owned(),
            )),
            _ => Err(unexpected("ledger", &rest)),
        },
        other => Err(Error::Validation(format!("unknown verb {other:?}"))),
    }
}

/// `migrate [--backup] [--adopt-manifest]`, in either order (spec 036 B-3).
fn migrate_flags(rest: &[&str]) -> Result<Verb> {
    let mut backup = false;
    let mut adopt_manifest = false;
    for arg in rest {
        match *arg {
            "--backup" if !backup => backup = true,
            "--adopt-manifest" if !adopt_manifest => adopt_manifest = true,
            _ => return Err(unexpected("migrate", rest)),
        }
    }
    Ok(Verb::Migrate {
        backup,
        adopt_manifest,
    })
}

/// `restore <archive> [--key <file>] [--adopt]` (spec 036 B-9).
fn restore_flags(rest: &[&str]) -> Result<Verb> {
    let Some((archive, flags)) = rest.split_first() else {
        return Err(Error::Validation("restore needs an archive".to_owned()));
    };
    if archive.starts_with('-') {
        return Err(Error::Validation("restore needs an archive".to_owned()));
    }
    let mut key = None;
    let mut adopt = false;
    let mut flags = flags.iter();
    while let Some(flag) = flags.next() {
        match *flag {
            "--adopt" if !adopt => adopt = true,
            "--key" if key.is_none() => {
                let Some(path) = flags.next() else {
                    return Err(Error::Validation("restore --key needs a file".to_owned()));
                };
                key = Some(PathBuf::from(*path));
            }
            _ => return Err(unexpected("restore", rest)),
        }
    }
    Ok(Verb::Restore {
        archive: PathBuf::from(*archive),
        key,
        adopt,
    })
}

fn no_options(verb: &str, rest: &[&str], ok: Verb) -> Result<Verb> {
    if rest.is_empty() {
        Ok(ok)
    } else {
        Err(unexpected(verb, rest))
    }
}

fn unexpected(verb: &str, rest: &[&str]) -> Error {
    Error::Validation(format!("{verb} does not take {}", rest.join(" ")))
}
