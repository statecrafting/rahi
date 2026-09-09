//! argv, parsed (B-1): the verbs and their few flags, and the help text
//! `--help` prints, which lists exactly the verbs of B-1 (AC-2).

use std::path::PathBuf;

use rahi_types::{Error, Result};

/// The verbs, in the order `--help` lists them.
pub const VERBS: [&str; 9] = [
    "serve",
    "preflight",
    "migrate",
    "backup",
    "restore",
    "ledger verify",
    "ledger export",
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
    /// B-4; `backup` is `--backup`.
    Migrate {
        /// Take a backup before applying anything.
        backup: bool,
    },
    /// B-5; `to` is `--to`, a directory or `s3://bucket/prefix`.
    Backup {
        /// Where the archive goes; the volume's backups directory when
        /// absent.
        to: Option<String>,
    },
    /// B-6; `key` is `--key`, the backup identity file when the volume has
    /// no key set yet.
    Restore {
        /// The archive.
        archive: PathBuf,
        /// The identity file.
        key: Option<PathBuf>,
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
    /// Spec 031.
    Supervise,
    /// Spec 031.
    FirstBoot,
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
            "migrate [--backup]",
            "apply the cell's migrations on the leader (a deploy step, never boot)",
        ),
        (
            "backup [--to <dir|s3://bucket/prefix>]",
            "one encrypted archive: both stores, the keys, a manifest",
        ),
        (
            "restore <archive> [--key <file>]",
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
        "first-boot" => no_options(verb, &rest, Verb::FirstBoot),
        "migrate" => match rest.as_slice() {
            [] => Ok(Verb::Migrate { backup: false }),
            ["--backup"] => Ok(Verb::Migrate { backup: true }),
            _ => Err(unexpected(verb, &rest)),
        },
        "backup" => match rest.as_slice() {
            [] => Ok(Verb::Backup { to: None }),
            ["--to", to] => Ok(Verb::Backup {
                to: Some((*to).to_owned()),
            }),
            _ => Err(unexpected(verb, &rest)),
        },
        "restore" => match rest.as_slice() {
            [archive] => Ok(Verb::Restore {
                archive: PathBuf::from(archive),
                key: None,
            }),
            [archive, "--key", key] => Ok(Verb::Restore {
                archive: PathBuf::from(archive),
                key: Some(PathBuf::from(key)),
            }),
            [] => Err(Error::Validation("restore needs an archive".to_owned())),
            _ => Err(unexpected(verb, &rest)),
        },
        "ledger" => match rest.as_slice() {
            ["verify"] => Ok(Verb::LedgerVerify { full: false }),
            ["verify", "--full"] => Ok(Verb::LedgerVerify { full: true }),
            ["export", path] => Ok(Verb::LedgerExport {
                path: PathBuf::from(path),
            }),
            ["export"] => Err(Error::Validation("ledger export needs a path".to_owned())),
            [] => Err(Error::Validation(
                "ledger needs a subverb: verify or export".to_owned(),
            )),
            _ => Err(unexpected("ledger", &rest)),
        },
        other => Err(Error::Validation(format!("unknown verb {other:?}"))),
    }
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
