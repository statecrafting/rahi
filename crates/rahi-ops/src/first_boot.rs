//! The first-boot verb (spec 031 B-2): idempotent, and the only place a
//! key is ever minted.
//!
//! An empty key directory is a first boot: every key is generated, rauthy's
//! environment is rendered from the public URL, and the admin credentials
//! are printed exactly once. A key directory with files in it is a restart
//! or a restore: the modes are verified, nothing is regenerated, and the
//! two files rauthy needs but a backup does not carry (its empty config and
//! its rendered environment) are written only if absent, so a restored
//! volume boots without a second first boot (spec 030 B-6).

use std::path::Path;

use rahi_types::{Config, EnvReader, Error, Result};

use crate::keys::{self, AdminCredentials};
use crate::rauthy_env::{self, HqlPorts, RauthySecrets};
use crate::{BACKUPS_DIR, KEY_DIR_MODE, KeySet, RAUTHY_SECRETS_FILE};

/// How first boot ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The key set was empty and has been minted; print these once.
    Generated(AdminCredentials),
    /// The key set was present and its modes hold; nothing was minted.
    Verified {
        /// rauthy's environment was absent and has been rendered.
        env_rendered: bool,
    },
}

/// Run first boot for the cell named `app_name` against `env`.
///
/// # Errors
///
/// [`Error::Config`] when the environment does not parse, when an existing
/// key set has a wrong mode or lacks rauthy's secrets; [`Error::Io`] when
/// the volume cannot be written or entropy is refused.
pub async fn run(env: &dyn EnvReader, app_name: &str) -> Result<Outcome> {
    let config = Config::from_env(env)?;
    let ports = HqlPorts::from_env(env)?;
    layout(&config)?;
    let keys = KeySet::of(&config);

    if is_empty_dir(keys.dir()) {
        let credentials = keys::generate(&keys)?;
        let secrets = keys.rauthy_secrets()?;
        write_rauthy_files(&config, &secrets, app_name, ports, true)?;
        return Ok(Outcome::Generated(credentials));
    }

    keys.check()?;
    let secrets = keys.rauthy_secrets()?;
    let env_rendered = write_rauthy_files(&config, &secrets, app_name, ports, false)?;
    Ok(Outcome::Verified { env_rendered })
}

/// The volume layout (B-1): the four directories, the key directory at
/// [`KEY_DIR_MODE`].
///
/// # Errors
///
/// [`Error::Io`] when a directory cannot be created.
pub fn layout(config: &Config) -> Result<()> {
    for dir in [
        config.data_dir.clone(),
        config.hiqlite_dir(),
        crate::rauthy_dir(config),
        config.keys_dir(),
        config.data_dir.join(BACKUPS_DIR),
    ] {
        std::fs::create_dir_all(&dir)
            .map_err(|err| Error::Io(format!("{} cannot be created: {err}", dir.display())))?;
    }
    // A key set that is a read-only Secret mount (spec 032 B-2) cannot be
    // re-moded and does not need to be: `KeySet::check` accepts it as it is.
    if !crate::is_read_only_dir(&config.keys_dir()) {
        crate::set_mode(&config.keys_dir(), KEY_DIR_MODE)?;
    }
    crate::set_mode(&crate::rauthy_dir(config), KEY_DIR_MODE)
}

impl KeySet {
    /// rauthy's secrets from [`RAUTHY_SECRETS_FILE`].
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the file cannot be read; [`Error::Config`] when it
    /// is not the JSON shape [`RauthySecrets`] serialises to.
    pub fn rauthy_secrets(&self) -> Result<RauthySecrets> {
        let text = self.read_text(RAUTHY_SECRETS_FILE)?;
        serde_json::from_str(&text).map_err(|err| {
            Error::Config(format!(
                "key file {} is not a rauthy secrets document: {err}",
                self.path(RAUTHY_SECRETS_FILE).display()
            ))
        })
    }
}

/// Write rauthy's empty config and rendered environment. With `force`,
/// both are written; without, only absent ones. Returns whether the
/// environment was written.
fn write_rauthy_files(
    config: &Config,
    secrets: &RauthySecrets,
    app_name: &str,
    ports: HqlPorts,
    force: bool,
) -> Result<bool> {
    let config_path = rauthy_env::config_path(config);
    if force || !config_path.exists() {
        write_private(&config_path, b"")?;
    }
    let env_path = rauthy_env::env_path(config);
    if force || !env_path.exists() {
        let rendered = rauthy_env::render(config, secrets, app_name, ports)?;
        write_private(&env_path, rendered.as_bytes())?;
        return Ok(true);
    }
    Ok(false)
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(path, bytes)
        .map_err(|err| Error::Io(format!("{} cannot be written: {err}", path.display())))?;
    crate::set_mode(path, crate::KEY_FILE_MODE)
}

fn is_empty_dir(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|mut entries| entries.next().is_none())
}

/// Render the credentials the way the verb prints them (B-2).
#[must_use]
pub fn announce(credentials: &AdminCredentials) -> String {
    format!(
        "first-boot: keys generated\n\
         rauthy admin:      {}\n\
         rauthy password:   {}\n\
         rauthy api token:  {}\n\
         These are printed once. The password is rauthy's bootstrap password; \
         change it at first login.",
        credentials.email, credentials.password, credentials.api_token
    )
}

/// The Secret name `export` renders (spec 032 B-2).
pub const EXPORT_SECRET_NAME: &str = "rahi-keys";

/// `first-boot --export` (spec 032 B-2): mint one key set into a private
/// temporary directory and render it as a Kubernetes Secret the operator
/// custodies and mounts read-only at `/data/keys` on every replica. The
/// credentials are inside the document (rauthy's bootstrap password and
/// the admin token are keys like any other), which is why the document
/// goes to stdout once and nowhere else.
///
/// # Errors
///
/// [`Error::Io`] when entropy is refused or the temporary directory cannot
/// be written.
pub fn export() -> Result<String> {
    use base64::Engine as _;
    let dir = tempfile::tempdir().map_err(|err| {
        Error::Io(format!(
            "a private temporary directory cannot be made: {err}"
        ))
    })?;
    let keys = KeySet::at(dir.path().join("keys"));
    keys::generate(&keys)?;
    let mut out = String::new();
    out.push_str(&format!(
        "# Rendered once by `rahi first-boot --export` (spec 032 B-2). Custody this\n\
         # document: it is every key of the deployment, including rauthy's bootstrap\n\
         # admin password. Apply it before the first rollout; first boot detects the\n\
         # mounted keys and generates nothing.\n\
         apiVersion: v1\nkind: Secret\nmetadata:\n  name: {EXPORT_SECRET_NAME}\ntype: Opaque\ndata:\n"
    ));
    for (name, bytes) in keys.export()? {
        out.push_str(&format!(
            "  {name}: {}\n",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        ));
    }
    Ok(out)
}
