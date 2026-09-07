//! The app's own OIDC client, registered once (spec 021 B-5).
//!
//! First boot registers the client; every boot after that finds it and
//! changes nothing. What it must never do is overwrite: a client an operator
//! has widened on purpose, or narrowed after an incident, is a decision, and
//! a chassis that silently restored its own defaults over it would undo that
//! decision every restart. A client that differs is [`Error::Conflict`] with
//! the field named, and a human decides.
//!
//! The settings this spec fixes are the ones B-5 names: confidential,
//! authorization code plus refresh token, S256 PKCE, RS256 for both tokens,
//! the redirect URI derived from the public URL, and the public URL itself as
//! the post-logout destination.
//!
//! The update is a read-modify-write over rauthy's own client document rather
//! than a typed mirror of its update request (spec 021 D-5): a rauthy that
//! grows a field keeps it, and this crate never has to know what the field
//! was for.

use rahi_types::{Error, Result};
use serde::{Deserialize, Serialize};

use crate::config::{API_KEY_SCHEME, IdpConfig};

/// The authorization code grant, as rauthy names it on the wire.
pub const GRANT_AUTHORIZATION_CODE: &str = "authorization_code";
/// The refresh token grant, as rauthy names it on the wire.
pub const GRANT_REFRESH_TOKEN: &str = "refresh_token";
/// The PKCE challenge method this chassis requires.
pub const CHALLENGE_S256: &str = "S256";
/// The signing algorithm this chassis requires of both tokens.
pub const ALG_RS256: &str = "RS256";

/// What the bootstrap did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Bootstrap {
    /// The client was registered now. The secret is rauthy's to mint and the
    /// caller's to custody (spec 031 writes it under the key set).
    Created {
        /// The client secret, when rauthy returned one.
        secret: Option<String>,
    },
    /// The client was already there and already matched.
    Unchanged,
}

/// The settings spec 021 B-5 fixes for the app's client.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientSettings {
    /// The client id: the manifest's app name.
    pub client_id: String,
    /// Always true: the cell holds a secret and is not a public client.
    pub confidential: bool,
    /// Where the authorization code comes back to.
    pub redirect_uris: Vec<String>,
    /// Where a logout lands.
    pub post_logout_redirect_uris: Vec<String>,
    /// The grants: code and refresh, and nothing else.
    pub flows_enabled: Vec<String>,
    /// The PKCE challenge methods: S256, and nothing else.
    pub challenges: Vec<String>,
    /// The access token algorithm.
    pub access_token_alg: String,
    /// The id token algorithm.
    pub id_token_alg: String,
}

impl ClientSettings {
    /// The settings this cell's client must have.
    #[must_use]
    pub fn for_config(config: &IdpConfig) -> Self {
        Self {
            client_id: config.client_id.clone(),
            confidential: true,
            redirect_uris: vec![config.redirect_uri.clone()],
            post_logout_redirect_uris: vec![config.post_logout_redirect_uri.clone()],
            flows_enabled: vec![
                GRANT_AUTHORIZATION_CODE.to_owned(),
                GRANT_REFRESH_TOKEN.to_owned(),
            ],
            challenges: vec![CHALLENGE_S256.to_owned()],
            access_token_alg: ALG_RS256.to_owned(),
            id_token_alg: ALG_RS256.to_owned(),
        }
    }

    /// The first field of `client` that does not match these settings.
    ///
    /// `None` means the registered client is the one this cell expects. The
    /// comparison is one of coverage rather than equality: an operator who
    /// added a second redirect URI or a third grant has widened the client on
    /// purpose, and the chassis only insists that what it needs is present.
    #[must_use]
    pub fn first_mismatch(&self, client: &serde_json::Value) -> Option<String> {
        if client.get("confidential") != Some(&serde_json::Value::Bool(true)) {
            return Some("confidential".to_owned());
        }
        for (field, required) in [
            ("redirect_uris", &self.redirect_uris),
            ("post_logout_redirect_uris", &self.post_logout_redirect_uris),
            ("flows_enabled", &self.flows_enabled),
            ("challenges", &self.challenges),
        ] {
            let present = strings_at(client, field);
            if let Some(missing) = required.iter().find(|want| !present.contains(*want)) {
                return Some(format!("{field} (missing {missing})"));
            }
        }
        for (field, required) in [
            ("access_token_alg", &self.access_token_alg),
            ("id_token_alg", &self.id_token_alg),
        ] {
            if client.get(field).and_then(serde_json::Value::as_str) != Some(required.as_str()) {
                return Some(field.to_owned());
            }
        }
        None
    }

    /// The create request rauthy's admin API accepts (`POST /clients`).
    #[must_use]
    pub fn create_request(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.client_id,
            "name": self.client_id,
            "confidential": self.confidential,
            "redirect_uris": self.redirect_uris,
            "post_logout_redirect_uris": self.post_logout_redirect_uris,
        })
    }

    /// `client` with this spec's settings written over it.
    ///
    /// Everything rauthy sent that this spec does not fix is sent back
    /// unchanged, so an update never drops a field this crate has not heard
    /// of.
    #[must_use]
    pub fn update_request(&self, client: &serde_json::Value) -> serde_json::Value {
        let mut update = client.clone();
        if let Some(object) = update.as_object_mut() {
            object.insert("confidential".to_owned(), serde_json::json!(true));
            object.insert("enabled".to_owned(), serde_json::json!(true));
            object.insert(
                "redirect_uris".to_owned(),
                serde_json::json!(union_of(client, "redirect_uris", &self.redirect_uris)),
            );
            object.insert(
                "post_logout_redirect_uris".to_owned(),
                serde_json::json!(union_of(
                    client,
                    "post_logout_redirect_uris",
                    &self.post_logout_redirect_uris
                )),
            );
            object.insert(
                "flows_enabled".to_owned(),
                serde_json::json!(union_of(client, "flows_enabled", &self.flows_enabled)),
            );
            object.insert(
                "challenges".to_owned(),
                serde_json::json!(self.challenges.clone()),
            );
            object.insert(
                "access_token_alg".to_owned(),
                serde_json::json!(self.access_token_alg),
            );
            object.insert(
                "id_token_alg".to_owned(),
                serde_json::json!(self.id_token_alg),
            );
        }
        update
    }
}

/// Register the app's client with rauthy, or confirm the one that is there.
///
/// `admin_token` is rauthy's API key (spec 031 custodies it under the key
/// set); it travels as `Authorization: API-Key <token>`.
///
/// # Errors
///
/// [`Error::Conflict`] when a registered client differs from what this cell
/// needs, naming the field; [`Error::Unauthorized`] when rauthy refuses the
/// admin token; [`Error::Upstream`] when rauthy cannot be reached or answers
/// with a status the bootstrap does not expect.
pub async fn bootstrap_client(config: &IdpConfig, admin_token: &str) -> Result<Bootstrap> {
    let client = reqwest::Client::builder()
        .build()
        .map_err(|err| Error::Config(format!("the loopback client cannot be built: {err}")))?;
    let settings = ClientSettings::for_config(config);
    let authorization = format!("{API_KEY_SCHEME} {admin_token}");

    if let Some(existing) = read_client(&client, config, &authorization).await? {
        return match settings.first_mismatch(&existing) {
            None => Ok(Bootstrap::Unchanged),
            Some(field) => Err(Error::Conflict(format!(
                "the OIDC client {} is registered with a different {field}: this cell \
                 will not overwrite it",
                config.client_id
            ))),
        };
    }

    let created = send(
        client
            .post(config.clients_url())
            .header(reqwest::header::AUTHORIZATION, &authorization)
            .json(&settings.create_request()),
        "register the client",
    )
    .await?;
    let secret = created
        .get("client_secret")
        .or_else(|| created.get("secret"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);

    let registered = read_client(&client, config, &authorization)
        .await?
        .ok_or_else(|| {
            Error::Upstream(format!(
                "rauthy accepted the registration of {} and then did not return it",
                config.client_id
            ))
        })?;
    send(
        client
            .put(config.client_url())
            .header(reqwest::header::AUTHORIZATION, &authorization)
            .json(&settings.update_request(&registered)),
        "apply the client's flows and algorithms",
    )
    .await?;

    Ok(Bootstrap::Created { secret })
}

/// The registered client, or `None` when rauthy says there is none.
async fn read_client(
    client: &reqwest::Client,
    config: &IdpConfig,
    authorization: &str,
) -> Result<Option<serde_json::Value>> {
    let url = config.client_url();
    let response = client
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
            Err(Error::Unauthorized(
                "rauthy refused the admin API key the bootstrap was given".to_owned(),
            ))
        }
        status => Err(Error::Upstream(format!(
            "rauthy answered {status} when asked for the client {}",
            config.client_id
        ))),
    }
}

/// Send one admin request and read its JSON answer.
async fn send(request: reqwest::RequestBuilder, what: &str) -> Result<serde_json::Value> {
    let response = request
        .send()
        .await
        .map_err(|err| Error::Upstream(format!("rauthy is unreachable to {what}: {err}")))?;
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(Error::Unauthorized(
            "rauthy refused the admin API key the bootstrap was given".to_owned(),
        ));
    }
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(Error::Upstream(format!(
            "rauthy answered {status} when asked to {what}: {body}"
        )));
    }
    Ok(serde_json::from_str(&body).unwrap_or(serde_json::Value::Null))
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

/// What `client` already has in `field`, plus everything `required` names.
fn union_of(client: &serde_json::Value, field: &str, required: &[String]) -> Vec<String> {
    let mut out = strings_at(client, field);
    for want in required {
        if !out.contains(want) {
            out.push(want.clone());
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn settings() -> ClientSettings {
        let env = BTreeMap::from([("RAHI_PUBLIC_URL", "https://cell.example.com")]);
        let config = rahi_types::Config::from_env(&env).expect("the fixture environment");
        ClientSettings::for_config(&IdpConfig::derive(&config, "hello-cell").expect("derives"))
    }

    fn registered() -> serde_json::Value {
        serde_json::json!({
            "id": "hello-cell",
            "confidential": true,
            "enabled": true,
            "redirect_uris": ["https://cell.example.com/auth/callback"],
            "post_logout_redirect_uris": ["https://cell.example.com"],
            "flows_enabled": ["authorization_code", "refresh_token"],
            "challenges": ["S256"],
            "access_token_alg": "RS256",
            "id_token_alg": "RS256",
            "scopes": ["openid", "profile", "email"],
        })
    }

    #[test]
    fn a_matching_client_is_left_alone() {
        assert_eq!(settings().first_mismatch(&registered()), None);
    }

    #[test]
    fn a_widened_client_still_matches() {
        let mut client = registered();
        client["redirect_uris"] = serde_json::json!([
            "https://cell.example.com/auth/callback",
            "https://staging.example.com/auth/callback"
        ]);
        client["flows_enabled"] =
            serde_json::json!(["authorization_code", "refresh_token", "client_credentials"]);
        assert_eq!(settings().first_mismatch(&client), None);
    }

    #[test]
    fn every_fixed_field_is_named_when_it_differs() {
        let cases = [
            ("confidential", serde_json::json!(false), "confidential"),
            (
                "redirect_uris",
                serde_json::json!(["https://elsewhere.example.com/cb"]),
                "redirect_uris",
            ),
            (
                "flows_enabled",
                serde_json::json!(["authorization_code"]),
                "flows_enabled",
            ),
            ("challenges", serde_json::json!(["plain"]), "challenges"),
            (
                "access_token_alg",
                serde_json::json!("EdDSA"),
                "access_token_alg",
            ),
            ("id_token_alg", serde_json::json!("EdDSA"), "id_token_alg"),
        ];
        for (field, value, named) in cases {
            let mut client = registered();
            client[field] = value;
            let mismatch = settings()
                .first_mismatch(&client)
                .expect("the difference is found");
            assert!(mismatch.starts_with(named), "{mismatch} names {named}");
        }
    }

    #[test]
    fn an_absent_array_is_a_mismatch_not_a_pass() {
        let mut client = registered();
        client
            .as_object_mut()
            .expect("an object")
            .remove("challenges");
        assert_eq!(
            settings().first_mismatch(&client),
            Some("challenges (missing S256)".to_owned())
        );
    }

    #[test]
    fn the_update_keeps_what_rauthy_sent_and_fixes_what_the_spec_names() {
        let mut client = registered();
        client["access_token_alg"] = serde_json::json!("EdDSA");
        client["scim"] = serde_json::json!({ "base_uri": "https://scim.example.com" });
        let update = settings().update_request(&client);
        assert_eq!(update["access_token_alg"], "RS256");
        assert_eq!(update["challenges"], serde_json::json!(["S256"]));
        assert_eq!(update["confidential"], true);
        assert_eq!(
            update["scim"]["base_uri"], "https://scim.example.com",
            "a field this crate does not know is sent back untouched"
        );
        assert_eq!(
            update["scopes"],
            serde_json::json!(["openid", "profile", "email"])
        );
    }

    #[test]
    fn the_update_widens_rather_than_replaces() {
        let mut client = registered();
        client["redirect_uris"] = serde_json::json!(["https://staging.example.com/auth/callback"]);
        let update = settings().update_request(&client);
        assert_eq!(
            update["redirect_uris"],
            serde_json::json!([
                "https://staging.example.com/auth/callback",
                "https://cell.example.com/auth/callback"
            ])
        );
    }

    #[test]
    fn the_create_request_is_what_rauthy_accepts() {
        let create = settings().create_request();
        assert_eq!(create["id"], "hello-cell");
        assert_eq!(create["confidential"], true);
        assert_eq!(
            create["redirect_uris"],
            serde_json::json!(["https://cell.example.com/auth/callback"])
        );
    }
}
