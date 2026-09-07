//! What the identity crate needs, all of it derived from one public URL
//! (spec 021 B-1, spec 010 B-7).
//!
//! Nothing here is a second source of truth. The origin comes from
//! `rahi_types::Config`, rauthy's address comes from the same place, and
//! every path below is fixed by this spec rather than configurable: an issuer
//! an operator can move is an issuer that will not match the tokens in the
//! field.

use std::path::PathBuf;

use rahi_types::{Config, Error, Result};

/// The subtree the proxy owns, and the only route into rauthy.
pub const AUTH_PREFIX: &str = "/auth";
/// rauthy's API root under that subtree; the issuer is the public URL plus
/// this.
pub const ISSUER_PATH: &str = "/auth/v1";
/// Where rauthy sends the browser back after an authorization code.
/// Deliberately outside `AUTH_PREFIX`: that subtree is forwarded raw to
/// rauthy (B-2), so a callback inside it is handed straight back to rauthy,
/// which has no such route. This is the app's own route (spec 022 B-1), and
/// `bootstrap_client` registers this exact string with rauthy (D-9).
pub const CALLBACK_PATH: &str = "/session/callback";
/// The discovery document, under rauthy's API root.
pub const DISCOVERY_PATH: &str = "/auth/v1/.well-known/openid-configuration";
/// rauthy's admin API root for the client bootstrap (spec 021 B-5).
pub const CLIENTS_PATH: &str = "/auth/v1/clients";
/// The file under the key set that holds the app's OIDC client secret.
pub const CLIENT_SECRET_FILE: &str = "rauthy_client_secret";

/// The `Authorization` scheme rauthy's admin API expects for an API key.
pub const API_KEY_SCHEME: &str = "API-Key";

/// Everything the proxy, the discovery driver, and the bootstrap need.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdpConfig {
    /// Where rauthy listens, on loopback, inside the deployment unit.
    pub loopback_base: String,
    /// The issuer every token must carry: the public URL plus [`ISSUER_PATH`].
    pub issuer: String,
    /// The app's OIDC client id: the manifest's app name.
    pub client_id: String,
    /// Where the authorization code comes back to.
    pub redirect_uri: String,
    /// Where rauthy sends the browser after a logout.
    pub post_logout_redirect_uri: String,
    /// The file the client secret is custodied in (written by spec 031).
    pub client_secret_path: PathBuf,
    /// Whether the public origin is `https`.
    pub https: bool,
}

impl IdpConfig {
    /// Derive the whole tree from the cell's configuration and the app's name.
    ///
    /// `client_id` is the manifest's app name (spec 015 B-1). It is passed in
    /// rather than read here so that the identity crate depends on the
    /// configuration and on nothing else in the chassis (spec 021 D-1).
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when `client_id` is empty or carries a character
    /// rauthy's client id rule refuses (`[a-zA-Z0-9._-]{2,256}`); a client id
    /// rauthy will reject is worth catching before first boot rather than in
    /// the bootstrap's error body.
    pub fn derive(config: &Config, client_id: &str) -> Result<Self> {
        validate_client_id(client_id)?;
        let origin = config.public_url.as_str();
        Ok(Self {
            loopback_base: config.rauthy_base_url(),
            issuer: format!("{origin}{ISSUER_PATH}"),
            client_id: client_id.to_owned(),
            redirect_uri: format!("{origin}{CALLBACK_PATH}"),
            post_logout_redirect_uri: origin.to_owned(),
            client_secret_path: config.keys_dir().join(CLIENT_SECRET_FILE),
            https: config.public_url.is_https(),
        })
    }

    /// The discovery document's URL on the loopback base.
    #[must_use]
    pub fn discovery_url(&self) -> String {
        format!("{}{DISCOVERY_PATH}", self.loopback_base)
    }

    /// The admin API's URL for the app's own client.
    #[must_use]
    pub fn client_url(&self) -> String {
        format!("{}{CLIENTS_PATH}/{}", self.loopback_base, self.client_id)
    }

    /// The admin API's collection URL.
    #[must_use]
    pub fn clients_url(&self) -> String {
        format!("{}{CLIENTS_PATH}", self.loopback_base)
    }

    /// The `X-Forwarded-Proto` the proxy sends upstream.
    #[must_use]
    pub const fn forwarded_proto(&self) -> &'static str {
        if self.https { "https" } else { "http" }
    }

    /// The environment rauthy is given for this origin (spec 021 B-6).
    ///
    /// Plain `http` is a local trial, and rauthy's default `host` cookies are
    /// refused by Safari over `http`, so the cookie mode is relaxed and said
    /// out loud. `https` is a real deployment, where rauthy sits behind this
    /// proxy and must trust the forwarded scheme and host. Spec 031 writes
    /// these into the container; nothing here touches a file.
    #[must_use]
    pub fn rauthy_env(&self) -> Vec<(&'static str, &'static str)> {
        if self.https {
            vec![("PROXY_MODE", "true")]
        } else {
            vec![("COOKIE_MODE", "danger-insecure")]
        }
    }
}

/// rauthy's client id rule: `^[a-zA-Z0-9._\-]{2,256}$`.
fn validate_client_id(client_id: &str) -> Result<()> {
    let ok = (2..=256).contains(&client_id.len())
        && client_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if ok {
        Ok(())
    } else {
        Err(Error::Validation(format!(
            "the client id {client_id:?} is not two to 256 characters of \
             [a-zA-Z0-9._-], which is what rauthy accepts"
        )))
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn config(public_url: &str) -> Config {
        let env = BTreeMap::from([("RAHI_PUBLIC_URL", public_url)]);
        Config::from_env(&env).expect("the fixture environment is well formed")
    }

    #[test]
    fn every_url_derives_from_the_one_public_url() {
        let idp = IdpConfig::derive(&config("https://cell.example.com"), "hello-cell")
            .expect("the fixture derives");
        assert_eq!(idp.issuer, "https://cell.example.com/auth/v1");
        assert_eq!(
            idp.redirect_uri,
            "https://cell.example.com/session/callback"
        );
        assert_eq!(idp.post_logout_redirect_uri, "https://cell.example.com");
        assert_eq!(idp.loopback_base, "http://127.0.0.1:8080");
        assert_eq!(
            idp.discovery_url(),
            "http://127.0.0.1:8080/auth/v1/.well-known/openid-configuration"
        );
        assert_eq!(
            idp.client_url(),
            "http://127.0.0.1:8080/auth/v1/clients/hello-cell"
        );
        assert_eq!(
            idp.client_secret_path,
            config("https://c.example.com")
                .keys_dir()
                .join(CLIENT_SECRET_FILE)
        );
    }

    #[test]
    fn a_trailing_slash_never_doubles_in_a_derived_url() {
        let idp = IdpConfig::derive(&config("https://cell.example.com/"), "hello-cell")
            .expect("the fixture derives");
        assert_eq!(idp.issuer, "https://cell.example.com/auth/v1");
    }

    #[test]
    fn the_scheme_decides_what_rauthy_is_told() {
        let secure =
            IdpConfig::derive(&config("https://cell.example.com"), "hello-cell").expect("derives");
        assert_eq!(secure.rauthy_env(), vec![("PROXY_MODE", "true")]);
        assert_eq!(secure.forwarded_proto(), "https");

        let plain =
            IdpConfig::derive(&config("http://localhost:8080"), "hello-cell").expect("derives");
        assert_eq!(plain.rauthy_env(), vec![("COOKIE_MODE", "danger-insecure")]);
        assert_eq!(plain.forwarded_proto(), "http");
    }

    #[test]
    fn a_client_id_rauthy_would_refuse_is_refused_here() {
        let config = config("https://cell.example.com");
        for bad in ["", "a", "hello cell", "hello/cell", "héllo"] {
            let err = IdpConfig::derive(&config, bad).expect_err("refused");
            assert_eq!(err.kind(), "validation", "{bad:?}");
        }
        for good in ["hello-cell", "hello.cell", "hello_cell", "HelloCell1"] {
            IdpConfig::derive(&config, good).expect("accepted");
        }
    }
}
