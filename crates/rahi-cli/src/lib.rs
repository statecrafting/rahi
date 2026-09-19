//! The binary composer of the rahi chassis (spec 030).
//!
//! An app is a [`Cell`]; its `main` is one line:
//!
//! ```no_run
//! # use rahi_cli::EmptyCell;
//! fn main() {
//!     rahi_cli::run(EmptyCell)
//! }
//! ```
//!
//! [`run`] parses argv into a verb ([`verbs`]), builds the runtime, boots
//! as far as the verb needs ([`serve`]), and exits with the code
//! `rahi_types::Error::exit_code` gives the first error: `0` ok, `1`
//! failure, `2` stale, `3` infrastructure (spec 010 B-2). Every cell has
//! the same verbs, and every verb has the same exit codes.

#![forbid(unsafe_code)]

pub mod cell;
pub mod serve;
pub mod verbs;

use std::collections::BTreeMap;

use rahi_ledger::Depth;
use rahi_ops::backup::Destination;
use rahi_ops::restore::{KeySource, Outcome};
use rahi_types::{EnvReader, Error, Result};

pub use cell::{Cell, EmptyCell, OPERATOR_PREFIX};
pub use serve::{Booted, RauthyMode};
pub use verbs::{VERBS, Verb, usage};

/// The process environment, as the config reader sees it.
#[must_use]
pub fn process_env() -> BTreeMap<String, String> {
    std::env::vars().collect()
}

/// B-1: parse argv, run the verb for `C`, and exit with its code.
///
/// Never returns; the exit code is the outcome.
pub fn run<C: Cell>(cell: C) -> ! {
    drop(cell);
    let args: Vec<String> = std::env::args().skip(1).collect();
    let env = process_env();
    std::process::exit(run_with::<C>(&args, &env))
}

/// [`run`] without the exit: the code it would have used.
///
/// Prints what the verb prints. A usage error prints the usage to stderr
/// and is exit `1`; any other error prints `error: <kind>: <message>` to
/// stderr and exits with the error's code.
pub fn run_with<C: Cell>(args: &[String], env: &dyn EnvReader) -> i32 {
    let verb = match verbs::parse(args) {
        Ok(verb) => verb,
        Err(err) => {
            eprintln!("error: {err}\n\n{}", usage());
            return err.exit_code();
        }
    };
    match verb {
        Verb::Help => {
            print!("{}", usage());
            return 0;
        }
        Verb::Version => {
            println!("rahi {}", env!("CARGO_PKG_VERSION"));
            return 0;
        }
        _ => {}
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("error: io: the runtime cannot be built: {err}");
            return rahi_types::error::EXIT_INFRA;
        }
    };
    match runtime.block_on(dispatch::<C>(verb, env)) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err}");
            err.exit_code()
        }
    }
}

/// Run `verb`; the code is the process exit code, which only `supervise`
/// (spec 031 B-3, the child's code) makes anything but zero on `Ok`.
async fn dispatch<C: Cell>(verb: Verb, env: &dyn EnvReader) -> Result<i32> {
    match verb {
        Verb::Supervise => supervise::<C>(env).await,
        Verb::FirstBoot { export: true } => {
            print!("{}", rahi_ops::first_boot::export(env)?);
            Ok(0)
        }
        Verb::FirstBoot { export: false } => first_boot::<C>(env).await.map(|()| 0),
        other => verbs_030::<C>(other, env).await.map(|()| 0),
    }
}

/// Spec 031 B-2: mint keys once, or verify them.
async fn first_boot<C: Cell>(env: &dyn EnvReader) -> Result<()> {
    let manifest = rahi_kernel::Manifest::parse(C::manifest())?;
    match rahi_ops::first_boot::run(env, manifest.app.name.as_str()).await? {
        rahi_ops::first_boot::Outcome::Generated(credentials) => {
            println!("{}", rahi_ops::first_boot::announce(&credentials));
        }
        rahi_ops::first_boot::Outcome::Verified { env_rendered } => {
            println!(
                "first-boot: keys present and verified; nothing generated{}",
                if env_rendered {
                    "; rauthy's environment rendered"
                } else {
                    ""
                }
            );
        }
    }
    Ok(())
}

/// Spec 031 B-3: rauthy and serve as one lifetime.
async fn supervise<C: Cell>(env: &dyn EnvReader) -> Result<i32> {
    use rahi_ops::supervise as sup;
    let config = rahi_types::Config::from_env(env)?;
    let keys = rahi_ops::KeySet::of(&config);
    keys.check()?;
    let manifest = rahi_kernel::Manifest::parse(C::manifest())?;
    let app_name = manifest.app.name.as_str().to_owned();
    let (rauthy, supplied) = sup::prepare_rauthy(&config, env)?;
    let api = rahi_ops::rauthy_api::RauthyApi::new(config.rauthy_base_url(), keys.admin_token()?)?;
    let ready = async {
        sup::wait_healthy(&api, sup::HEALTH_BUDGET).await?;
        // Spec 037 B-3 and B-1, in that order: a restored rauthy has just
        // come up on the snapshot this start handed it, and the backup
        // admin the verb logs in as must exist before anything asks for a
        // backup.
        let steps = sup::ready_after_health(&config, &keys, &api, supplied.as_ref()).await?;
        sup::custody_client(&config, &keys, &app_name).await?;
        let said = steps.render();
        println!(
            "supervise: rauthy is healthy at {}, client {app_name} custodied{}{said}",
            api.base(),
            if said.is_empty() { "" } else { "; " }
        );
        Ok(())
    };
    let exit = sup::supervise(
        rauthy,
        ready,
        |stop| {
            serve::serve_until::<C>(env, async {
                let _ = stop.await;
            })
        },
        sup::shutdown_signal(),
    )
    .await;
    println!("supervise: exiting {} ({:?})", exit.code, exit.reason);
    Ok(exit.code)
}

/// The verbs of spec 030.
async fn verbs_030<C: Cell>(verb: Verb, env: &dyn EnvReader) -> Result<()> {
    match verb {
        Verb::Help | Verb::Version | Verb::Supervise | Verb::FirstBoot { .. } => Ok(()),
        Verb::Serve => serve::serve::<C>(env).await,
        Verb::Preflight => {
            let report = rahi_ops::preflight::run(env, C::manifest()).await;
            println!("{report}");
            if report.passed() {
                Ok(())
            } else {
                Err(Error::Validation("preflight failed".to_owned()))
            }
        }
        Verb::Migrate {
            backup,
            adopt_manifest,
        } => {
            let booted = Booted::open_or_attach::<C>(env).await?;
            let result = migrate::<C>(&booted, backup, adopt_manifest, env).await;
            booted.shutdown().await;
            result
        }
        Verb::Backup { to } => {
            let booted = Booted::open_or_attach::<C>(env).await?;
            let to = match to {
                Some(raw) => Destination::parse(&raw)?,
                None => Destination::default_for(&booted.config),
            };
            let result = backup(&booted, &to, env).await;
            booted.shutdown().await;
            result
        }
        Verb::Restore {
            archive,
            key,
            adopt,
        } => {
            let config = rahi_types::Config::from_env(env)?;
            let source = key.map_or(KeySource::KeySet, KeySource::File);
            // Spec 036 B-9: the running cell's manifest and migrations are
            // what the archive is checked against, and the check happens
            // inside `run`, before it writes anything.
            let manifest = rahi_kernel::Manifest::parse(C::manifest())?;
            let hash = manifest.hash()?.to_string();
            let cell = rahi_ops::restore::Compatibility {
                manifest_hash: &hash,
                migrations: C::migrations(),
                adopt,
            };
            match rahi_ops::restore::run(&config, &archive, &source, &cell).await? {
                Outcome::Restored(marker) => {
                    println!(
                        "restore: applied {} ({} parts); rauthy's snapshot is at {}; marker written to {}",
                        marker.archive,
                        marker.manifest.parts.len(),
                        marker.rauthy_snapshot,
                        rahi_ops::restore_marker(&config).display()
                    );
                    if !rahi_ops::restore::schema_checked(&marker.manifest) {
                        println!(
                            "restore: the archive predates spec 036 and records no migration \
                             history, so its schema could not be checked against this binary"
                        );
                    }
                    if marker.manifest.manifest_hash != hash {
                        println!(
                            "restore: the restored chain names manifest {}, this binary's is {}; \
                             run: rahi migrate --adopt-manifest",
                            marker.manifest.manifest_hash, hash
                        );
                    }
                }
                Outcome::AlreadyRestored(marker) => {
                    println!(
                        "restore: {} was already applied at {}; nothing to do",
                        marker.archive,
                        rahi_ops::utc_stamp(marker.restored)
                    );
                }
            }
            Ok(())
        }
        Verb::LedgerVerify { full } => {
            let booted = Booted::open::<C>(env).await?;
            let result = ledger_verify(&booted, full, env).await;
            booted.shutdown().await;
            result
        }
        Verb::LedgerExport { path } => {
            let booted = Booted::open::<C>(env).await?;
            let result = async {
                let ledger = booted.ledger().await?;
                let jsonl = ledger.export_jsonl().await?;
                let segments = ledger.segments().await?;
                let mut out = jsonl;
                for segment in &segments {
                    let line = serde_json::to_string(segment).map_err(|err| {
                        Error::Io(format!("a segment reference cannot be serialised: {err}"))
                    })?;
                    out.push_str(&line);
                    out.push('\n');
                }
                std::fs::write(&path, out).map_err(|err| {
                    Error::Io(format!("{} cannot be written: {err}", path.display()))
                })?;
                println!(
                    "ledger export: {} resident record(s) and {} segment reference(s) to {}",
                    ledger.count().await?,
                    segments.len(),
                    path.display()
                );
                Ok(())
            }
            .await;
            booted.shutdown().await;
            result
        }
    }
}

async fn migrate<C: Cell>(
    booted: &Booted,
    with_backup: bool,
    adopt_manifest: bool,
    env: &dyn EnvReader,
) -> Result<()> {
    if with_backup {
        backup(booted, &Destination::default_for(&booted.config), env).await?;
    }
    if !adopt_manifest {
        let report = rahi_ops::migrate::run(&booted.store, C::migrations()).await?;
        println!("{}", rahi_ops::migrate::render(&report));
        return Ok(());
    }
    // Spec 036 B-3: a follower refuses as `migrate` does, and it refuses
    // before the chain is opened, so a follower's deploy step writes nothing
    // at all rather than creating the chain's schema on its way to exit 2.
    rahi_ops::migrate::refuse_follower(&booted.store).await?;
    // One deploy step moves the schema and the ceiling together. The chain is
    // opened here because the adoption reads the manifest it currently names
    // and appends to it; opening it verifies it first, as every other verb
    // that touches the chain does.
    let ledger = booted.ledger().await?;
    let (report, adoption) =
        rahi_ops::migrate::adopt(&booted.store, &ledger, &booted.manifest, C::migrations()).await?;
    println!("{}", rahi_ops::migrate::render(&report));
    println!("{}", rahi_ops::migrate::render_adoption(&adoption));
    Ok(())
}

async fn backup(booted: &Booted, to: &Destination, env: &dyn EnvReader) -> Result<()> {
    // The API key cannot take rauthy's backup; the dedicated backup admin's
    // passkey can (spec 037 B-1). A key set minted before spec 037 holds
    // none, and the verb says so by name rather than failing on a 401.
    let rauthy = rahi_ops::rauthy_api::RauthyApi::new(
        booted.config.rauthy_base_url(),
        booted.keys.admin_token()?,
    )?
    .with_passkey(booted.keys.backup_passkey()?);
    // Spec 036 B-9: the archive records the chain's *current* manifest, which
    // is what a restore has to be checked against. On an adopted cell that is
    // the booted manifest; on one whose deploy step has not run it is not,
    // and recording the booted hash there would make a restore check itself
    // against a ceiling the chain never named.
    let ledger = booted.ledger().await?;
    let current = ledger.current_manifest().await?;
    let outcome = rahi_ops::backup::run(
        &booted.store,
        &rauthy,
        &booted.keys,
        &current.to_string(),
        to,
        env,
    )
    .await?;
    println!(
        "backup: {} ({} bytes, {} parts) at {}",
        outcome.name,
        outcome.bytes,
        outcome.manifest.parts.len(),
        outcome.location
    );
    Ok(())
}

async fn ledger_verify(booted: &Booted, full: bool, env: &dyn EnvReader) -> Result<()> {
    let ledger = booted.ledger().await?;
    let depth = if full { "full" } else { "resident" };
    if full {
        let archive = booted.ledger_archive(env)?;
        ledger.verify_chain(Depth::Full(&archive)).await?;
    } else {
        ledger.verify_chain(Depth::Resident).await?;
    }
    println!(
        "ledger verify: ok at {depth} depth; {} resident record(s), {} sealed segment(s), head {}",
        ledger.count().await?,
        ledger.segment_count().await?,
        ledger.head().await?
    );
    Ok(())
}
