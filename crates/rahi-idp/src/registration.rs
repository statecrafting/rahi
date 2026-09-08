//! Dynamic client registration is rauthy's, and this is only how it is told
//! (spec 025 B-7, B-8).
//!
//! A command line tool or an agent runtime that has never seen this
//! deployment needs a client id before it can start an authorization code
//! flow, and RFC 7591 is how it gets one. rauthy implements that endpoint;
//! this crate does not reimplement it, does not proxy it a second time, and
//! does not mint anything of its own on the way. What lives here is the one
//! setting that decides whether the endpoint is open, gated by a token, or
//! off, plus the two derived facts that follow from it: what
//! [`crate::resource`] advertises, and what the packaging spec must put in
//! rauthy's environment.
//!
//! The default is [`Registration::Token`] and that is a security position,
//! not a convenience: an open registration endpoint on a public origin
//! accepts a client from anybody who can reach it, and the abuse that
//! follows is registered clients nobody asked for, sitting in the IdP's
//! database, each one a name a user might be asked to trust.
//!
//! **The device code grant is rauthy's too** (B-8). It is published in
//! rauthy's discovery document at [`DEVICE_AUTHORIZATION_PATH`] under this
//! cell's own origin, reachable through the raw proxy of spec 021, and it is
//! the supported flow for a client that cannot open a browser redirect.
//! Nothing in this chassis implements a leg of it.

use rahi_types::{EnvReader, Error, Result};
use serde::{Deserialize, Serialize};

/// rauthy's dynamic client registration endpoint, under the issuer path.
pub const REGISTRATION_PATH: &str = "/auth/v1/clients_dyn";
/// rauthy's device authorization endpoint, under the issuer path (B-8).
pub const DEVICE_AUTHORIZATION_PATH: &str = "/auth/v1/oidc/device";
/// The variable an operator sets to move off the default.
pub const ENV_REGISTRATION: &str = "RAHI_IDP_REGISTRATION";
/// rauthy's own switch for the endpoint.
pub const RAUTHY_ENABLE_VAR: &str = "ENABLE_DYN_CLIENT_REG";
/// rauthy's own variable for the registration token.
pub const RAUTHY_TOKEN_VAR: &str = "DYN_CLIENT_REG_TOKEN";

/// Whether a client may register itself, and on what terms (B-7).
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Registration {
    /// No registration endpoint. Clients are registered by an operator.
    Off,
    /// Registration requires a bearer token communicated in advance. The
    /// default, because an open endpoint on a public origin is an abuse
    /// surface.
    #[default]
    Token,
    /// Anybody who can reach the origin may register a client.
    Open,
}

impl Registration {
    /// The wire name, which is also what [`Registration::parse`] accepts.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Token => "token",
            Self::Open => "open",
        }
    }

    /// Parse one of `off`, `token`, `open`.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] for anything else. A misspelled setting is refused
    /// rather than defaulted: silently reading `toke` as the default would be
    /// indistinguishable from reading it as `open`.
    pub fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "off" => Ok(Self::Off),
            "token" => Ok(Self::Token),
            "open" => Ok(Self::Open),
            other => Err(Error::Config(format!(
                "{ENV_REGISTRATION} must be one of off, token, open; got {other:?}"
            ))),
        }
    }

    /// Read the setting from the environment, defaulting to
    /// [`Registration::Token`].
    ///
    /// # Errors
    ///
    /// As [`Registration::parse`].
    pub fn from_env(reader: &dyn EnvReader) -> Result<Self> {
        match reader
            .get(ENV_REGISTRATION)
            .map(|raw| raw.trim().to_owned())
            .filter(|raw| !raw.is_empty())
        {
            None => Ok(Self::default()),
            Some(raw) => Self::parse(&raw),
        }
    }

    /// Whether the metadata document names a registration endpoint (B-7).
    ///
    /// `off` advertises nothing, because a document that names an endpoint
    /// rauthy will refuse sends every new client into a 404 it cannot
    /// interpret.
    #[must_use]
    pub const fn is_advertised(self) -> bool {
        !matches!(self, Self::Off)
    }

    /// Whether the packaging must custody a registration token for rauthy.
    #[must_use]
    pub const fn needs_token(self) -> bool {
        matches!(self, Self::Token)
    }

    /// What rauthy is configured with for this setting (B-7).
    ///
    /// Spec 031 writes these into the container and, when
    /// [`Registration::needs_token`] is true, adds [`RAUTHY_TOKEN_VAR`] from
    /// the key set: the token itself is a secret and this crate never holds
    /// one.
    #[must_use]
    pub const fn rauthy_env(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::Off => &[(RAUTHY_ENABLE_VAR, "false")],
            Self::Token | Self::Open => &[(RAUTHY_ENABLE_VAR, "true")],
        }
    }

    /// The registration endpoint on `origin`, when there is one to advertise.
    #[must_use]
    pub fn endpoint(self, origin: &str) -> Option<String> {
        self.is_advertised()
            .then(|| format!("{origin}{REGISTRATION_PATH}"))
    }

    /// The device authorization endpoint on `origin` (B-8).
    ///
    /// Documented rather than implemented: a client with no browser uses
    /// rauthy's device code grant through the proxy, and this chassis has no
    /// leg of that flow to contribute.
    #[must_use]
    pub fn device_endpoint(origin: &str) -> String {
        format!("{origin}{DEVICE_AUTHORIZATION_PATH}")
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn the_default_is_the_gated_one() {
        assert_eq!(Registration::default(), Registration::Token);
        let empty: BTreeMap<&str, &str> = BTreeMap::new();
        assert_eq!(
            Registration::from_env(&empty).expect("the default needs no setting"),
            Registration::Token
        );
    }

    #[test]
    fn every_accepted_value_round_trips_and_the_rest_are_refused() {
        for value in [Registration::Off, Registration::Token, Registration::Open] {
            assert_eq!(
                Registration::parse(value.as_str()).expect("its own name parses"),
                value
            );
        }
        assert_eq!(
            Registration::parse("OPEN").expect("the case is not the setting"),
            Registration::Open
        );
        let err = Registration::parse("toke").expect_err("a typo is not a default");
        assert_eq!(err.kind(), "config");
    }

    #[test]
    fn the_environment_is_read_and_a_bad_value_is_a_config_error() {
        let env = BTreeMap::from([(ENV_REGISTRATION, "off")]);
        assert_eq!(
            Registration::from_env(&env).expect("off is a setting"),
            Registration::Off
        );
        let bad = BTreeMap::from([(ENV_REGISTRATION, "yes")]);
        assert_eq!(
            Registration::from_env(&bad)
                .expect_err("a value nobody defined")
                .kind(),
            "config"
        );
    }

    #[test]
    fn off_advertises_nothing_and_the_other_two_advertise_rauthys_endpoint() {
        let origin = "https://cell.example.com";
        assert_eq!(Registration::Off.endpoint(origin), None);
        assert_eq!(
            Registration::Token.endpoint(origin).as_deref(),
            Some("https://cell.example.com/auth/v1/clients_dyn")
        );
        assert_eq!(
            Registration::Open.endpoint(origin),
            Registration::Token.endpoint(origin),
            "the endpoint is rauthy's either way; what differs is who may use it"
        );
    }

    #[test]
    fn rauthy_is_told_the_setting_and_only_token_needs_a_secret() {
        assert_eq!(
            Registration::Off.rauthy_env(),
            &[(RAUTHY_ENABLE_VAR, "false")]
        );
        assert_eq!(
            Registration::Open.rauthy_env(),
            &[(RAUTHY_ENABLE_VAR, "true")]
        );
        assert!(Registration::Token.needs_token());
        assert!(!Registration::Open.needs_token());
        assert!(!Registration::Off.needs_token());
    }

    #[test]
    fn the_device_grant_is_named_and_not_implemented() {
        assert_eq!(
            Registration::device_endpoint("https://cell.example.com"),
            "https://cell.example.com/auth/v1/oidc/device"
        );
    }
}
