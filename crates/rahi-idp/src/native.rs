//! The declared native clients, provisioned in rauthy (spec 038 B-2, B-3).
//!
//! A command-line client cannot keep a secret, so it logs in as a *public*
//! client: no secret, PKCE `S256`, and the device grant or a loopback
//! authorization code. rauthy will hold such a client happily, but three of
//! its defaults make a token from one unusable here, and none of the three
//! is something a CLI can fix from its own side:
//!
//! - a new client signs with `EdDSA`, and the resource server accepts
//!   `RS256` only (025 B-3);
//! - the device grant takes no `resource` parameter (RFC 8707), so the cell's
//!   origin reaches `aud` only through the client's `default_aud`, which is
//!   an admin field;
//! - the access token lives for rauthy's default rather than the lifetime
//!   this cell's manifest fixes (B-4).
//!
//! So the supervisor's custody step registers each declared client, or brings
//! the one rauthy already holds to these settings, with all three settled;
//! after the cell's own client and with the same admin key (031 B-4). (B-3 has a
//! shorter word for register-or-update; spec 022 FR-004 scans this crate for
//! SQL write keywords and that word is one of them, so it is spelled out
//! rather than weakening the guard.) What it never does is delete: a client this manifest does not
//! declare is somebody else's, and a chassis that tidied it away would be
//! deleting a decision it cannot read.
//!
//! ## Declared settings replace, they do not widen (D-12)
//!
//! [`crate::bootstrap`] refuses to overwrite the cell's own client: an
//! operator who widened it did so on purpose. A native client is the
//! opposite case. It is *manifest content* (038 D-4), the manifest is the
//! ceiling, and a flow or scope that was removed from the document has to
//! come off the client or the ceiling was a description rather than a
//! bound. So the fields this spec fixes are written as declared, and every
//! field rauthy sent that this spec does not fix is sent back untouched.

use std::collections::BTreeSet;

use rahi_kernel::{NativeClient, NativeFlow};
use rahi_types::{Error, Result};

use crate::bootstrap::{ALG_RS256, CHALLENGE_S256};
use crate::config::{API_KEY_SCHEME, IdpConfig};

/// rauthy's admin API root for the scope catalog.
pub const SCOPES_PATH: &str = "/auth/v1/scopes";

/// The scopes every OIDC client is given, which rauthy holds already and
/// which a declared scope list never has to repeat.
pub const BASE_SCOPES: [&str; 3] = ["openid", "profile", "email"];

/// What provisioning one client did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Provisioned {
    /// The client id, as declared.
    pub id: String,
    /// Whether rauthy had no such client and this created it.
    pub created: bool,
    /// The scopes this created in rauthy's catalog, in order.
    pub scopes_created: Vec<String>,
}

/// The settings spec 038 B-3 fixes for one declared native client.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeSettings {
    /// The client id.
    pub client_id: String,
    /// The grants, as rauthy names them on the wire.
    pub flows_enabled: Vec<String>,
    /// Every scope this client may be granted, base scopes included.
    pub scopes: Vec<String>,
    /// Where an authorization code comes back to; empty for a device client.
    pub redirect_uris: Vec<String>,
    /// The audience every token this client is issued must carry: the cell's
    /// origin (B-3).
    pub default_aud: Vec<String>,
    /// The access token lifetime, in seconds (B-4).
    pub access_token_lifetime: u64,
}

impl NativeSettings {
    /// The settings for `client` on the cell `config` describes.
    ///
    /// `audience` is the cell's origin, which is what
    /// [`crate::Resource::audience`] holds tokens to; passing it in rather
    /// than deriving it here keeps one definition of what this cell is.
    #[must_use]
    pub fn for_client(client: &NativeClient, audience: &str, access_token_lifetime: u64) -> Self {
        let mut scopes: Vec<String> = BASE_SCOPES.iter().map(|s| (*s).to_owned()).collect();
        for scope in &client.scopes {
            if !scopes.contains(scope) {
                scopes.push(scope.clone());
            }
        }
        Self {
            client_id: client.id.clone(),
            flows_enabled: client.grants(),
            scopes,
            redirect_uris: client.redirect_uris.clone(),
            default_aud: vec![audience.to_owned()],
            access_token_lifetime,
        }
    }

    /// The scopes this client needs that are not rauthy's own.
    #[must_use]
    pub fn declared_scopes(&self) -> Vec<String> {
        self.scopes
            .iter()
            .filter(|scope| !BASE_SCOPES.contains(&scope.as_str()))
            .cloned()
            .collect()
    }

    /// The create request rauthy's admin API accepts (`POST /clients`).
    ///
    /// Deliberately thin, exactly as the cell's own client's is: rauthy's
    /// `NewClientRequest` takes an id, a name, the confidential flag, and the
    /// redirect URIs, and everything else this spec fixes is applied by the
    /// update that follows.
    #[must_use]
    pub fn create_request(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.client_id,
            "name": self.client_id,
            "confidential": false,
            "redirect_uris": self.redirect_uris,
        })
    }

    /// `client` with this spec's settings written over it (D-12).
    #[must_use]
    pub fn update_request(&self, client: &serde_json::Value) -> serde_json::Value {
        let mut update = client.clone();
        if let Some(object) = update.as_object_mut() {
            object.insert("enabled".to_owned(), serde_json::json!(true));
            // A public client, which is what makes the device grant reachable
            // without a secret a CLI would have to keep.
            object.insert("confidential".to_owned(), serde_json::json!(false));
            object.insert(
                "redirect_uris".to_owned(),
                serde_json::json!(self.redirect_uris),
            );
            object.insert(
                "flows_enabled".to_owned(),
                serde_json::json!(self.flows_enabled),
            );
            object.insert("challenges".to_owned(), serde_json::json!([CHALLENGE_S256]));
            object.insert("access_token_alg".to_owned(), serde_json::json!(ALG_RS256));
            object.insert("id_token_alg".to_owned(), serde_json::json!(ALG_RS256));
            object.insert("scopes".to_owned(), serde_json::json!(self.scopes));
            object.insert("default_scopes".to_owned(), serde_json::json!(self.scopes));
            object.insert(
                "default_aud".to_owned(),
                serde_json::json!(self.default_aud),
            );
            object.insert(
                "access_token_lifetime".to_owned(),
                serde_json::json!(self.access_token_lifetime),
            );
        }
        update
    }

    /// The first field of `client` that is not what this spec fixes.
    ///
    /// `None` means the registered client is already the one this cell needs,
    /// so the boot has nothing to write. Equality and not coverage, which is
    /// the difference from [`crate::ClientSettings::first_mismatch`]: a
    /// native client's settings are the manifest's, so a widened one is drift
    /// rather than a decision (D-12).
    #[must_use]
    pub fn first_mismatch(&self, client: &serde_json::Value) -> Option<String> {
        if client.get("confidential") != Some(&serde_json::Value::Bool(false)) {
            return Some("confidential".to_owned());
        }
        if client.get("enabled") != Some(&serde_json::Value::Bool(true)) {
            return Some("enabled".to_owned());
        }
        for (field, required) in [
            ("flows_enabled", &self.flows_enabled),
            ("redirect_uris", &self.redirect_uris),
            ("scopes", &self.scopes),
            ("default_scopes", &self.scopes),
            ("default_aud", &self.default_aud),
        ] {
            if &strings_at(client, field) != required {
                return Some(field.to_owned());
            }
        }
        for (field, required) in [("access_token_alg", ALG_RS256), ("id_token_alg", ALG_RS256)] {
            if client.get(field).and_then(serde_json::Value::as_str) != Some(required) {
                return Some(field.to_owned());
            }
        }
        if strings_at(client, "challenges") != vec![CHALLENGE_S256.to_owned()] {
            return Some("challenges".to_owned());
        }
        if client
            .get("access_token_lifetime")
            .and_then(serde_json::Value::as_u64)
            != Some(self.access_token_lifetime)
        {
            return Some("access_token_lifetime".to_owned());
        }
        None
    }
}

/// Register every declared native client in rauthy, or bring the one that is
/// already there to the settings this spec fixes (B-3).
///
/// Idempotent by construction: a client that already carries these settings
/// is read and left alone, and one that does not is written to carry them.
/// Scopes the clients declare are created in rauthy's catalog first, because
/// a client cannot be given a scope that does not exist yet.
///
/// # Errors
///
/// [`Error::Unauthorized`] when rauthy refuses the admin key;
/// [`Error::Upstream`] when it cannot be reached or answers with a status
/// this step does not expect.
pub async fn provision_native_clients(
    config: &IdpConfig,
    admin_token: &str,
    clients: &[NativeClient],
    audience: &str,
    access_token_lifetime: u64,
) -> Result<Vec<Provisioned>> {
    if clients.is_empty() {
        return Ok(Vec::new());
    }
    let http = reqwest::Client::builder()
        .build()
        .map_err(|err| Error::Config(format!("the loopback client cannot be built: {err}")))?;
    let authorization = format!("{API_KEY_SCHEME} {admin_token}");

    let mut existing_scopes = read_scopes(&http, config, &authorization).await?;
    let mut done = Vec::with_capacity(clients.len());
    for client in clients {
        let settings = NativeSettings::for_client(client, audience, access_token_lifetime);

        let mut scopes_created = Vec::new();
        for scope in settings.declared_scopes() {
            if existing_scopes.contains(&scope) {
                continue;
            }
            create_scope(&http, config, &authorization, &scope).await?;
            existing_scopes.insert(scope.clone());
            scopes_created.push(scope);
        }

        let found = read_client(&http, config, &authorization, &settings.client_id).await?;
        let created = found.is_none();
        let current = match found {
            Some(current) => current,
            None => {
                send(
                    http.post(config.clients_url())
                        .header(reqwest::header::AUTHORIZATION, &authorization)
                        .json(&settings.create_request()),
                    &format!("register the native client {}", settings.client_id),
                )
                .await?;
                read_client(&http, config, &authorization, &settings.client_id)
                    .await?
                    .ok_or_else(|| {
                        Error::Upstream(format!(
                            "rauthy accepted the registration of {} and then did not return it",
                            settings.client_id
                        ))
                    })?
            }
        };

        if settings.first_mismatch(&current).is_some() {
            send(
                http.put(native_client_url(config, &settings.client_id))
                    .header(reqwest::header::AUTHORIZATION, &authorization)
                    .json(&settings.update_request(&current)),
                &format!(
                    "apply the declared settings of the native client {}",
                    settings.client_id
                ),
            )
            .await?;
        }

        done.push(Provisioned {
            id: settings.client_id,
            created,
            scopes_created,
        });
    }
    Ok(done)
}

/// Write `lifetime` onto one client, when rauthy is not already applying it
/// (B-4).
///
/// Narrow on purpose: it reads the client, changes one field, and writes it
/// back. Spec 021 B-5 refuses to overwrite a client an operator widened, and
/// this does not widen anything; a lifetime rauthy defaulted is not a
/// decision anybody made, and the manifest is where this cell's is stated.
///
/// Answers whether it had to write.
///
/// # Errors
///
/// As [`provision_native_clients`]; [`Error::NotFound`] when rauthy holds no
/// such client.
pub async fn apply_lifetime(
    config: &IdpConfig,
    admin_token: &str,
    client_id: &str,
    lifetime: u64,
) -> Result<bool> {
    let http = reqwest::Client::builder()
        .build()
        .map_err(|err| Error::Config(format!("the loopback client cannot be built: {err}")))?;
    let authorization = format!("{API_KEY_SCHEME} {admin_token}");
    let mut client = read_client(&http, config, &authorization, client_id)
        .await?
        .ok_or_else(|| Error::NotFound(format!("rauthy holds no client {client_id}")))?;
    if client
        .get("access_token_lifetime")
        .and_then(serde_json::Value::as_u64)
        == Some(lifetime)
    {
        return Ok(false);
    }
    if let Some(object) = client.as_object_mut() {
        object.insert(
            "access_token_lifetime".to_owned(),
            serde_json::json!(lifetime),
        );
    }
    send(
        http.put(native_client_url(config, client_id))
            .header(reqwest::header::AUTHORIZATION, &authorization)
            .json(&client),
        &format!("apply the access token lifetime of {client_id}"),
    )
    .await?;
    Ok(true)
}

/// Read one client's access token lifetime back from rauthy (B-6).
///
/// What `preflight` reports: the bound an operator reasons about during an
/// incident is the one rauthy is applying, not the one the manifest asked
/// for, and the two are only the same until somebody changes one of them.
///
/// # Errors
///
/// As [`provision_native_clients`]; [`Error::NotFound`] when rauthy holds no
/// such client.
pub async fn read_lifetime(config: &IdpConfig, admin_token: &str, client_id: &str) -> Result<u64> {
    let http = reqwest::Client::builder()
        .build()
        .map_err(|err| Error::Config(format!("the loopback client cannot be built: {err}")))?;
    let authorization = format!("{API_KEY_SCHEME} {admin_token}");
    let client = read_client(&http, config, &authorization, client_id)
        .await?
        .ok_or_else(|| Error::NotFound(format!("rauthy holds no client {client_id}")))?;
    client
        .get("access_token_lifetime")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| {
            Error::Upstream(format!(
                "rauthy's document for {client_id} names no access_token_lifetime"
            ))
        })
}

/// Whether `clients` declares one that uses the refresh grant (B-4, D-11).
#[must_use]
pub fn uses_refresh(clients: &[NativeClient]) -> bool {
    clients
        .iter()
        .any(|client| client.has_flow(NativeFlow::RefreshToken))
}

/// The admin API's URL for one named client.
fn native_client_url(config: &IdpConfig, client_id: &str) -> String {
    format!("{}/{client_id}", config.clients_url())
}

/// The named client, or `None` when rauthy says there is none.
async fn read_client(
    http: &reqwest::Client,
    config: &IdpConfig,
    authorization: &str,
    client_id: &str,
) -> Result<Option<serde_json::Value>> {
    let url = native_client_url(config, client_id);
    let response = http
        .get(&url)
        .header(reqwest::header::AUTHORIZATION, authorization)
        .send()
        .await
        .map_err(|err| Error::Upstream(format!("rauthy is unreachable at {url}: {err}")))?;
    match response.status() {
        status if status.is_success() => response
            .json::<serde_json::Value>()
            .await
            .map(Some)
            .map_err(|err| {
                Error::Upstream(format!("rauthy's client document does not parse: {err}"))
            }),
        reqwest::StatusCode::NOT_FOUND => Ok(None),
        reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
            Err(refused_the_key())
        }
        status => Err(Error::Upstream(format!(
            "rauthy answered {status} when asked for the client {client_id}"
        ))),
    }
}

/// Every scope rauthy's catalog holds, by name.
async fn read_scopes(
    http: &reqwest::Client,
    config: &IdpConfig,
    authorization: &str,
) -> Result<BTreeSet<String>> {
    let url = format!("{}{SCOPES_PATH}", config.loopback_base);
    let response = http
        .get(&url)
        .header(reqwest::header::AUTHORIZATION, authorization)
        .send()
        .await
        .map_err(|err| Error::Upstream(format!("rauthy is unreachable at {url}: {err}")))?;
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(refused_the_key());
    }
    if !status.is_success() {
        return Err(Error::Upstream(format!(
            "rauthy answered {status} when asked for its scope catalog"
        )));
    }
    let scopes: Vec<serde_json::Value> = response
        .json()
        .await
        .map_err(|err| Error::Upstream(format!("rauthy's scope catalog does not parse: {err}")))?;
    Ok(scopes
        .iter()
        .filter_map(|scope| scope.get("name").and_then(serde_json::Value::as_str))
        .map(str::to_owned)
        .collect())
}

/// Create one scope in rauthy's catalog.
async fn create_scope(
    http: &reqwest::Client,
    config: &IdpConfig,
    authorization: &str,
    scope: &str,
) -> Result<()> {
    send(
        http.post(format!("{}{SCOPES_PATH}", config.loopback_base))
            .header(reqwest::header::AUTHORIZATION, authorization)
            .json(&serde_json::json!({ "scope": scope })),
        &format!("create the scope {scope}"),
    )
    .await
    .map(|_| ())
}

/// Send one admin request and read its JSON answer.
async fn send(request: reqwest::RequestBuilder, what: &str) -> Result<serde_json::Value> {
    let response = request
        .send()
        .await
        .map_err(|err| Error::Upstream(format!("rauthy is unreachable to {what}: {err}")))?;
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(refused_the_key());
    }
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(Error::Upstream(format!(
            "rauthy answered {status} when asked to {what}: {body}"
        )));
    }
    Ok(serde_json::from_str(&body).unwrap_or(serde_json::Value::Null))
}

/// The one refusal every call above shares.
fn refused_the_key() -> Error {
    Error::Unauthorized(
        "rauthy refused the admin API key the native client provisioning was given; the key \
         needs the Clients and Scopes groups (spec 031 D-8)"
            .to_owned(),
    )
}

/// Every string in `client`'s `field` array.
fn strings_at(client: &serde_json::Value, field: &str) -> Vec<String> {
    client
        .get(field)
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn client() -> NativeClient {
        NativeClient {
            id: "hello-cli".to_owned(),
            flows: vec![NativeFlow::DeviceCode, NativeFlow::RefreshToken],
            scopes: vec!["notes:write".to_owned()],
            redirect_uris: Vec::new(),
        }
    }

    fn settings() -> NativeSettings {
        NativeSettings::for_client(&client(), "https://cell.example.com", 600)
    }

    #[test]
    fn the_settings_bind_the_audience_the_algorithm_and_the_lifetime() {
        let settings = settings();
        assert_eq!(settings.default_aud, vec!["https://cell.example.com"]);
        assert_eq!(settings.access_token_lifetime, 600);
        assert_eq!(
            settings.flows_enabled,
            vec![
                "urn:ietf:params:oauth:grant-type:device_code".to_owned(),
                "refresh_token".to_owned(),
            ]
        );
        assert_eq!(
            settings.scopes,
            vec!["openid", "profile", "email", "notes:write"]
        );
        assert_eq!(settings.declared_scopes(), vec!["notes:write"]);
    }

    #[test]
    fn the_create_request_asks_for_a_public_client() {
        let create = settings().create_request();
        assert_eq!(create["id"], "hello-cli");
        assert_eq!(
            create["confidential"], false,
            "a CLI cannot keep a secret, and rauthy's device endpoint asks a confidential \
             client for one"
        );
    }

    #[test]
    fn the_update_fixes_what_the_spec_names_and_keeps_what_it_does_not() {
        let current = serde_json::json!({
            "id": "hello-cli",
            "confidential": true,
            "access_token_alg": "EdDSA",
            "scim": { "base_uri": "https://scim.example.com" },
        });
        let update = settings().update_request(&current);
        assert_eq!(update["confidential"], false);
        assert_eq!(update["access_token_alg"], "RS256");
        assert_eq!(update["id_token_alg"], "RS256");
        assert_eq!(update["challenges"], serde_json::json!(["S256"]));
        assert_eq!(update["access_token_lifetime"], 600);
        assert_eq!(
            update["default_aud"],
            serde_json::json!(["https://cell.example.com"])
        );
        assert_eq!(
            update["scim"]["base_uri"], "https://scim.example.com",
            "a field this crate does not know is sent back untouched"
        );
    }

    #[test]
    fn a_declared_setting_replaces_rather_than_widens() {
        // D-12: the manifest is the ceiling, so a flow it no longer declares
        // comes off the client.
        let current = serde_json::json!({
            "flows_enabled": [
                "urn:ietf:params:oauth:grant-type:device_code",
                "refresh_token",
                "client_credentials",
            ],
        });
        let update = settings().update_request(&current);
        assert_eq!(
            update["flows_enabled"],
            serde_json::json!([
                "urn:ietf:params:oauth:grant-type:device_code",
                "refresh_token"
            ])
        );
        assert!(
            settings().first_mismatch(&current).is_some(),
            "a widened client is drift, not a decision"
        );
    }

    #[test]
    fn a_client_that_already_carries_the_settings_is_left_alone() {
        let settled = settings().update_request(&serde_json::json!({}));
        assert_eq!(settings().first_mismatch(&settled), None);
    }

    #[test]
    fn every_fixed_field_is_named_when_it_differs() {
        for (field, value) in [
            ("confidential", serde_json::json!(true)),
            ("enabled", serde_json::json!(false)),
            ("access_token_alg", serde_json::json!("EdDSA")),
            ("id_token_alg", serde_json::json!("EdDSA")),
            ("challenges", serde_json::json!(["plain"])),
            ("access_token_lifetime", serde_json::json!(1800)),
            ("default_aud", serde_json::json!([])),
            ("flows_enabled", serde_json::json!(["authorization_code"])),
        ] {
            let mut settled = settings().update_request(&serde_json::json!({}));
            settled[field] = value;
            assert_eq!(
                settings().first_mismatch(&settled),
                Some(field.to_owned()),
                "{field}"
            );
        }
    }

    #[test]
    fn a_refresh_flow_is_noticed_across_the_declared_clients() {
        assert!(uses_refresh(&[client()]));
        assert!(!uses_refresh(&[NativeClient {
            flows: vec![NativeFlow::DeviceCode],
            ..client()
        }]));
        assert!(!uses_refresh(&[]));
    }
}
