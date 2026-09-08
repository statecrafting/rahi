//! What this cell is, said in RFC 9728 (spec 025 B-2, B-6).
//!
//! A browser learns where to log in by being redirected there. A command line
//! tool, an agent runtime, or an MCP client has no redirect to follow and no
//! cookie to present, so it has to be *told*, and the telling has to work for
//! a client that has never seen this deployment. Two documents do that, and
//! between them they are the whole bootstrap:
//!
//! - The challenge ([`Resource::challenge`]) on a 401 names this resource and
//!   the URL of the metadata document.
//! - The metadata document ([`ResourceMetadata`]) names the authorization
//!   server, and the client reads rauthy's discovery document from there.
//!
//! Every value in it is derived from the one public URL (spec 010 B-7), so
//! the document cannot disagree with the deployment it describes: the
//! `resource` is the origin, which is also the audience every access token
//! must carry (B-3), and the authorization server is that origin plus
//! rauthy's issuer path, which is the issuer spec 021 fixed.
//!
//! `scopes_supported` is not a list somebody maintains. It is the union of
//! the scopes the gates of [`crate::scope`] were built with, read at the
//! moment the document is served, so a route that requires a scope publishes
//! it by existing.

use axum::Router;
use axum::extract::State;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use rahi_types::{Error, Result};
use serde::{Deserialize, Serialize};

use crate::config::{ISSUER_PATH, IdpConfig};
use crate::registration::Registration;
use crate::scope;

/// Where the metadata document is served, at the root of the origin.
pub const METADATA_PATH: &str = "/.well-known/oauth-protected-resource";
/// How long a client may cache the document, in seconds (B-2).
pub const METADATA_MAX_AGE: u64 = 3600;
/// Where the deployment documents its API, unless the app names another.
pub const DOCUMENTATION_PATH: &str = "/docs/api";
/// The one way a credential may be presented: an `Authorization` header.
pub const BEARER_METHOD_HEADER: &str = "header";

/// The RFC 9728 protected resource metadata document (B-2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceMetadata {
    /// This resource, which is the public URL and the mandatory audience.
    pub resource: String,
    /// Where a client goes to be issued a token for it.
    pub authorization_servers: Vec<String>,
    /// `header`, and nothing else: no query parameter, no form field.
    pub bearer_methods_supported: Vec<String>,
    /// Every scope a mounted route requires.
    pub scopes_supported: Vec<String>,
    /// Where the deployment documents what these scopes buy.
    pub resource_documentation: String,
    /// rauthy's registration endpoint, when registration is not `off` (B-7).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registration_endpoint: Option<String>,
}

/// The resource server's own identity: one origin, three derived strings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resource {
    origin: String,
    authorization_server: String,
    documentation: String,
    registration: Registration,
}

impl Resource {
    /// Derive the resource from the identity configuration.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when the configured issuer does not end in
    /// [`ISSUER_PATH`], because then no origin can be recovered from it and
    /// every URL in this document would be a guess.
    pub fn derive(idp: &IdpConfig) -> Result<Self> {
        let origin = idp.issuer.strip_suffix(ISSUER_PATH).ok_or_else(|| {
            Error::Config(format!(
                "the issuer {:?} does not end in {ISSUER_PATH}, so no resource identifier \
                 can be derived from it",
                idp.issuer
            ))
        })?;
        Ok(Self {
            origin: origin.to_owned(),
            authorization_server: idp.issuer.clone(),
            documentation: format!("{origin}{DOCUMENTATION_PATH}"),
            registration: Registration::default(),
        })
    }

    /// Advertise `registration` in the document (B-7).
    #[must_use]
    pub const fn with_registration(mut self, registration: Registration) -> Self {
        self.registration = registration;
        self
    }

    /// Point `resource_documentation` at `url`.
    #[must_use]
    pub fn with_documentation(mut self, url: impl Into<String>) -> Self {
        self.documentation = url.into();
        self
    }

    /// The resource identifier: the public URL.
    ///
    /// This is the same string every access token must carry in `aud`
    /// (B-3, RFC 8707), which is why there is one accessor and not two.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.origin
    }

    /// The value an access token's `aud` claim must contain.
    #[must_use]
    pub fn audience(&self) -> &str {
        &self.origin
    }

    /// The authorization server: the origin plus rauthy's issuer path.
    #[must_use]
    pub fn authorization_server(&self) -> &str {
        &self.authorization_server
    }

    /// How registration is offered (B-7).
    #[must_use]
    pub const fn registration(&self) -> Registration {
        self.registration
    }

    /// Where the metadata document is served.
    #[must_use]
    pub fn metadata_url(&self) -> String {
        format!("{}{METADATA_PATH}", self.origin)
    }

    /// The document, with `scopes_supported` read now (B-2).
    #[must_use]
    pub fn metadata(&self) -> ResourceMetadata {
        ResourceMetadata {
            resource: self.origin.clone(),
            authorization_servers: vec![self.authorization_server.clone()],
            bearer_methods_supported: vec![BEARER_METHOD_HEADER.to_owned()],
            scopes_supported: scope::supported(),
            resource_documentation: self.documentation.clone(),
            registration_endpoint: self.registration.endpoint(&self.origin),
        }
    }

    /// The `WWW-Authenticate` value of a 401 (B-6).
    ///
    /// This header is the entire bootstrap: a client that has never seen this
    /// deployment reads the metadata URL out of it, reads the authorization
    /// server out of that document, and is one discovery fetch from being
    /// able to ask for a token.
    #[must_use]
    pub fn challenge(&self) -> String {
        format!(
            "Bearer realm=\"{}\", resource_metadata=\"{}\"",
            self.origin,
            self.metadata_url()
        )
    }

    /// [`Resource::challenge`] naming the RFC 6750 error code `error`.
    ///
    /// The code is an ASCII token by RFC 6750 and this writes no other kind:
    /// a value with a quote in it would end the header field early, so the
    /// codes are the fixed strings this crate raises and nothing derived from
    /// a request.
    #[must_use]
    pub fn challenge_with(&self, error: &str) -> String {
        format!("{}, error=\"{error}\"", self.challenge())
    }
}

/// The one route this module serves: the metadata document, unauthenticated
/// and cached for an hour (B-2).
///
/// The app merges it at the root, because RFC 9728 fixes the path:
///
/// ```no_run
/// # use rahi_idp::{Resource, resource_router};
/// # fn compose(resource: Resource, app: axum::Router) -> axum::Router {
/// app.merge(resource_router(resource))
/// # }
/// ```
pub fn resource_router(resource: Resource) -> Router {
    Router::new()
        .route(METADATA_PATH, get(metadata))
        .with_state(resource)
}

/// `GET /.well-known/oauth-protected-resource` (B-2).
async fn metadata(State(resource): State<Resource>) -> Response {
    (
        [
            (header::CONTENT_TYPE, "application/json".to_owned()),
            (
                header::CACHE_CONTROL,
                format!("public, max-age={METADATA_MAX_AGE}"),
            ),
        ],
        axum::Json(resource.metadata()),
    )
        .into_response()
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::collections::BTreeMap;

    use rahi_types::Config;

    use super::*;

    fn resource(public_url: &str) -> Resource {
        let env = BTreeMap::from([("RAHI_PUBLIC_URL", public_url)]);
        let config = Config::from_env(&env).expect("the fixture environment is well formed");
        let idp = IdpConfig::derive(&config, "hello-cell").expect("the fixture derives");
        Resource::derive(&idp).expect("the resource derives")
    }

    #[test]
    fn every_value_derives_from_the_one_public_url() {
        let resource = resource("https://cell.example.com");
        assert_eq!(resource.as_str(), "https://cell.example.com");
        assert_eq!(resource.audience(), resource.as_str());
        assert_eq!(
            resource.authorization_server(),
            "https://cell.example.com/auth/v1"
        );
        assert_eq!(
            resource.metadata_url(),
            "https://cell.example.com/.well-known/oauth-protected-resource"
        );

        let document = resource.metadata();
        assert_eq!(document.resource, "https://cell.example.com");
        assert_eq!(
            document.authorization_servers,
            vec!["https://cell.example.com/auth/v1".to_owned()],
            "the discovery document is one path below this (FR-002)"
        );
        assert_eq!(document.bearer_methods_supported, vec!["header".to_owned()]);
        assert_eq!(
            document.resource_documentation,
            "https://cell.example.com/docs/api"
        );
    }

    #[test]
    fn registration_decides_whether_an_endpoint_is_advertised() {
        let resource = resource("https://cell.example.com");
        assert_eq!(
            resource.metadata().registration_endpoint.as_deref(),
            Some("https://cell.example.com/auth/v1/clients_dyn"),
            "the default is token, which is advertised"
        );
        let off = resource.with_registration(Registration::Off);
        assert_eq!(off.metadata().registration_endpoint, None);
    }

    #[test]
    fn the_challenge_names_this_resource_and_where_to_read_about_it() {
        let resource = resource("https://cell.example.com");
        assert_eq!(
            resource.challenge(),
            "Bearer realm=\"https://cell.example.com\", \
             resource_metadata=\"https://cell.example.com/.well-known/oauth-protected-resource\""
        );
        assert!(
            resource
                .challenge_with("invalid_token")
                .ends_with(", error=\"invalid_token\"")
        );
    }

    #[test]
    fn an_issuer_that_is_not_this_chassiss_is_refused() {
        let idp = IdpConfig {
            loopback_base: "http://127.0.0.1:8080".to_owned(),
            issuer: "https://cell.example.com/realms/rahi".to_owned(),
            client_id: "hello-cell".to_owned(),
            redirect_uri: "https://cell.example.com/session/callback".to_owned(),
            post_logout_redirect_uri: "https://cell.example.com".to_owned(),
            client_secret_path: std::path::PathBuf::from("/data/keys/rauthy_client_secret"),
            https: true,
        };
        let err = Resource::derive(&idp).expect_err("the issuer path is fixed by spec 021");
        assert_eq!(err.kind(), "config");
    }

    #[test]
    fn the_document_serialises_without_an_absent_registration_endpoint() {
        let resource = resource("http://localhost:8080").with_registration(Registration::Off);
        let json = serde_json::to_value(resource.metadata()).expect("the document serialises");
        assert!(
            json.get("registration_endpoint").is_none(),
            "an absent endpoint is absent, not null: {json}"
        );
        assert_eq!(
            json.get("resource").and_then(serde_json::Value::as_str),
            Some("http://localhost:8080")
        );
    }
}
