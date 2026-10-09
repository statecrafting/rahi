//! The back channel to Rauthy, and what it trusts (spec 044 B-1, B-2).
//!
//! At N=1 Rauthy is inside the deployment unit and the back channel is
//! loopback `http` (spec 031). At N=3 Rauthy is its own StatefulSet, and
//! rahi reaches it only through one internal Service over Rauthy's native
//! TLS, verifying the certificate against a CA mounted into the pod
//! (044 D-2, P-1). Every client this crate and `rahi-ops` build toward
//! Rauthy is built here, so no call can take the plaintext path once a
//! remote back channel is installed.
//!
//! The setting is process-wide because the process has one Rauthy: it is
//! read once from the environment at startup ([`install_from_env`]) and
//! never changes. A process that installs nothing keeps the N=1 loopback
//! behavior exactly.

use std::path::PathBuf;
use std::sync::OnceLock;

use rahi_types::{Config, EnvReader, Error, Result};

/// Rauthy's base URL when it is remote: the internal Service
/// `https://rauthy-internal.<namespace>.svc.cluster.local:<port>` (B-1).
pub const ENV_RAUTHY_URL: &str = "RAHI_RAUTHY_URL";

/// The PEM file of the CA that signed Rauthy's certificate (B-2).
pub const ENV_RAUTHY_CA: &str = "RAHI_RAUTHY_CA";

/// A remote back channel: where Rauthy is and the one CA it is trusted under.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Remote {
    /// The `https` base URL, without a trailing slash.
    pub base: String,
    /// The CA, PEM.
    ca_pem: Vec<u8>,
}

static REMOTE: OnceLock<Remote> = OnceLock::new();

/// Read [`ENV_RAUTHY_URL`] and [`ENV_RAUTHY_CA`].
///
/// `None` when the URL is unset: the loopback back channel of spec 031.
///
/// # Errors
///
/// [`Error::Config`] when the URL is not `https` (plaintext on this path is
/// a validation failure, B-2), when the CA is not named, or when the file it
/// names is not a PEM certificate.
pub fn remote_from_env(env: &dyn EnvReader) -> Result<Option<Remote>> {
    let Some(url) = env.get(ENV_RAUTHY_URL).filter(|u| !u.trim().is_empty()) else {
        return Ok(None);
    };
    let base = url.trim().trim_end_matches('/').to_owned();
    if !base.starts_with("https://") {
        return Err(Error::Config(format!(
            "{ENV_RAUTHY_URL} {base:?} is not https: rahi reaches a remote Rauthy only over its \
             TLS (spec 044 B-2)"
        )));
    }
    let ca = env
        .get(ENV_RAUTHY_CA)
        .filter(|p| !p.trim().is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| {
            Error::Config(format!(
                "{ENV_RAUTHY_URL} is set and {ENV_RAUTHY_CA} names no CA: the certificate is \
                 verified against a mounted CA (spec 044 B-2)"
            ))
        })?;
    let ca_pem = std::fs::read(&ca).map_err(|err| {
        Error::Config(format!(
            "{ENV_RAUTHY_CA} {} cannot be read: {err}",
            ca.display()
        ))
    })?;
    if certificates(&ca_pem).is_err() {
        return Err(Error::Config(format!(
            "{ENV_RAUTHY_CA} {} holds no PEM certificate",
            ca.display()
        )));
    }
    Ok(Some(Remote { base, ca_pem }))
}

/// Install the back channel the environment names, once per process.
///
/// # Errors
///
/// As [`remote_from_env`]; [`Error::Conflict`] when a different remote back
/// channel was installed earlier in this process.
pub fn install_from_env(env: &dyn EnvReader) -> Result<()> {
    let Some(remote) = remote_from_env(env)? else {
        return Ok(());
    };
    let installed = REMOTE.get_or_init(|| remote.clone());
    if *installed != remote {
        return Err(Error::Conflict(
            "a different remote Rauthy back channel is already installed in this process"
                .to_owned(),
        ));
    }
    Ok(())
}

/// The installed remote back channel, if any.
#[must_use]
pub fn remote() -> Option<&'static Remote> {
    REMOTE.get()
}

/// Rauthy's base URL for this process: the remote one when installed, else
/// the loopback address of spec 031.
#[must_use]
pub fn base(config: &Config) -> String {
    remote().map_or_else(|| config.rauthy_base_url(), |r| r.base.clone())
}

/// A client builder for a call to Rauthy (B-2).
///
/// With a remote back channel it trusts only the mounted CA and refuses
/// anything but `https`; without one it is reqwest's default builder, the
/// loopback client of spec 031.
///
/// # Errors
///
/// [`Error::Config`] when the installed CA no longer parses, which cannot
/// happen after [`install_from_env`] accepted it.
pub fn builder() -> Result<reqwest::ClientBuilder> {
    let builder = reqwest::Client::builder();
    let Some(remote) = remote() else {
        return Ok(builder);
    };
    let ca = certificates(&remote.ca_pem)
        .map_err(|err| Error::Config(format!("the installed Rauthy CA does not parse: {err}")))?;
    Ok(builder.tls_certs_only(ca).https_only(true))
}

/// Every certificate in a PEM bundle; at least one, or an error.
fn certificates(pem: &[u8]) -> std::result::Result<Vec<reqwest::Certificate>, String> {
    let certs = reqwest::Certificate::from_pem_bundle(pem).map_err(|err| err.to_string())?;
    if certs.is_empty() {
        return Err("no certificate in the bundle".to_owned());
    }
    Ok(certs)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn unset_is_the_loopback_back_channel() {
        assert_eq!(remote_from_env(&env(&[])).unwrap(), None);
        assert_eq!(
            remote_from_env(&env(&[(ENV_RAUTHY_URL, " ")])).unwrap(),
            None
        );
    }

    #[test]
    fn a_remote_back_channel_is_https_with_a_ca_or_refused() {
        let err =
            remote_from_env(&env(&[(ENV_RAUTHY_URL, "http://rauthy-internal:8443")])).unwrap_err();
        assert!(
            matches!(&err, Error::Config(m) if m.contains("not https")),
            "{err}"
        );
        let err =
            remote_from_env(&env(&[(ENV_RAUTHY_URL, "https://rauthy-internal:8443")])).unwrap_err();
        assert!(
            matches!(&err, Error::Config(m) if m.contains(ENV_RAUTHY_CA)),
            "{err}"
        );
        let dir = tempfile::tempdir().unwrap();
        let junk = dir.path().join("ca.pem");
        std::fs::write(&junk, "not a certificate").unwrap();
        let err = remote_from_env(&env(&[
            (ENV_RAUTHY_URL, "https://rauthy-internal:8443"),
            (ENV_RAUTHY_CA, junk.to_str().unwrap()),
        ]))
        .unwrap_err();
        assert!(
            matches!(&err, Error::Config(m) if m.contains("PEM")),
            "{err}"
        );
    }
}
