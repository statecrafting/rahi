//! The rauthy side of a login (spec 033 B-3): create the user through the
//! admin API with the bootstrap API key, then drive the cell's own
//! authorization-code flow against rauthy's login endpoint, through the
//! cell's `/auth` proxy, without a browser.
//!
//! What a browser does invisibly is done here by hand: an anonymous
//! session from `POST /auth/v1/oidc/session` (a cookie and a CSRF token),
//! a solved proof of work from `POST /auth/v1/pow`, and a JSON login to
//! `POST /auth/v1/oidc/authorize`, which answers `202` with the
//! `Location` a browser would have followed to the cell's `/callback`.

use reqwest::StatusCode;
use reqwest::header::{AUTHORIZATION, COOKIE, LOCATION, SET_COOKIE};
use ring::digest::{SHA256, digest};
use serde_json::{Value, json};

use crate::{Client, Error, Result};

/// A user the harness creates and logs in as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct User {
    /// The email, which is rauthy's login name.
    pub email: String,
    /// The password the harness sets.
    pub password: String,
    /// rauthy roles to grant (`admin`, or a cell's operator role).
    pub roles: Vec<String>,
}

impl User {
    /// A user with no roles.
    #[must_use]
    pub fn new(email: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            email: email.into(),
            password: password.into(),
            roles: Vec::new(),
        }
    }

    /// With `role` granted.
    #[must_use]
    pub fn with_role(mut self, role: impl Into<String>) -> Self {
        self.roles.push(role.into());
        self
    }
}

/// rauthy's admin API on its loopback base, with the bootstrap API key.
#[derive(Debug)]
pub struct Rauthy {
    base: String,
    token: String,
    http: reqwest::Client,
}

impl Rauthy {
    /// For rauthy at `base` (`http://127.0.0.1:<port>`) with `token`, the
    /// key `first-boot` minted.
    #[must_use]
    pub fn new(base: &str, token: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_owned(),
            token: token.to_owned(),
            http: reqwest::Client::builder()
                .user_agent("rahi-harness")
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
        }
    }

    fn auth(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        builder.header(AUTHORIZATION, format!("API-Key {}", self.token))
    }

    /// The user record for `email`, if any.
    ///
    /// # Errors
    ///
    /// [`Error::Rauthy`] when the admin API refuses.
    pub async fn find_user(&self, email: &str) -> Result<Option<Value>> {
        let response = self
            .auth(self.http.get(format!("{}/auth/v1/users", self.base)))
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(Error::Rauthy(format!(
                "GET /users answered {status}: {body}"
            )));
        }
        let users: Vec<Value> = serde_json::from_str(&body)
            .map_err(|err| Error::Rauthy(format!("the user list is not json: {err}")))?;
        Ok(users
            .into_iter()
            .find(|u| u.get("email").and_then(Value::as_str) == Some(email)))
    }

    /// Create `role` if rauthy does not have it. rauthy keeps of a user's
    /// roles only those that exist (`Role::sanitize`), so a role granted
    /// before it is created is silently dropped (spec 034 D-8).
    ///
    /// # Errors
    ///
    /// [`Error::Rauthy`] when the admin API refuses.
    pub async fn ensure_role(&self, role: &str) -> Result<()> {
        let response = self
            .auth(self.http.get(format!("{}/auth/v1/roles", self.base)))
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(Error::Rauthy(format!(
                "GET /roles answered {status}: {body}"
            )));
        }
        let roles: Vec<Value> = serde_json::from_str(&body)
            .map_err(|err| Error::Rauthy(format!("the role list is not json: {err}")))?;
        if roles
            .iter()
            .any(|r| r.get("name").and_then(Value::as_str) == Some(role))
        {
            return Ok(());
        }
        let response = self
            .auth(self.http.post(format!("{}/auth/v1/roles", self.base)))
            .json(&json!({ "role": role }))
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(Error::Rauthy(format!(
                "POST /roles answered {status}: {body}"
            )));
        }
        Ok(())
    }

    /// Create `user` if absent, then set its password, roles, and mark it
    /// enabled and verified so it can log in at once. Every role named is
    /// created first if rauthy lacks it.
    ///
    /// # Errors
    ///
    /// [`Error::Rauthy`] when any call refuses.
    pub async fn ensure_user(&self, user: &User) -> Result<String> {
        for role in &user.roles {
            self.ensure_role(role).await?;
        }
        let (local, _) = user.email.split_once('@').unwrap_or((&user.email, ""));
        let id = match self.find_user(&user.email).await? {
            Some(existing) => existing
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| Error::Rauthy("a user without an id".to_owned()))?,
            None => {
                let response = self
                    .auth(self.http.post(format!("{}/auth/v1/users", self.base)))
                    .json(&json!({
                        "email": user.email,
                        "given_name": local,
                        "family_name": "Harness",
                        "language": "en",
                        "roles": user.roles,
                        "groups": [],
                        "user_expires": null,
                    }))
                    .send()
                    .await?;
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                if !status.is_success() {
                    return Err(Error::Rauthy(format!(
                        "POST /users answered {status}: {body}"
                    )));
                }
                let created: Value = serde_json::from_str(&body)
                    .map_err(|err| Error::Rauthy(format!("the user is not json: {err}")))?;
                created
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| Error::Rauthy("the created user has no id".to_owned()))?
            }
        };
        let response = self
            .auth(self.http.put(format!("{}/auth/v1/users/{id}", self.base)))
            .json(&json!({
                "email": user.email,
                "given_name": local,
                "family_name": "Harness",
                "language": "en",
                "password": user.password,
                "roles": user.roles,
                "groups": [],
                "enabled": true,
                "email_verified": true,
                "user_expires": null,
            }))
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(Error::Rauthy(format!(
                "PUT /users/{id} answered {status}: {body}"
            )));
        }
        Ok(id)
    }
}

/// Where a cell's login starts (spec 022): under the session prefix.
pub const LOGIN_PATH: &str = "/session/login";

/// Drive the cell's login to a session for `user` (B-3).
///
/// 1. `GET /session/login` on the cell: a redirect to rauthy's authorize
///    URL and the cell's login cookie (state, PKCE verifier).
/// 2. On rauthy, through the cell's `/auth` proxy: a session, a proof of
///    work, and the JSON login carrying the authorize URL's parameters.
/// 3. `GET` the `Location` rauthy answers with: the cell's `/callback`,
///    which exchanges the code and sets the session cookie.
///
/// # Errors
///
/// [`Error::Rauthy`] at whichever step does not answer as expected, with
/// the status and body.
pub async fn login(client: &Client, base: &str, user: &User) -> Result<()> {
    let start = client.get(LOGIN_PATH).await?;
    let status = start.status();
    let authorize_url = start
        .headers()
        .get(LOCATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let Some(authorize_url) = authorize_url else {
        return Err(Error::Rauthy(format!(
            "GET {LOGIN_PATH} answered {status} without a Location: {}",
            start.text().await.unwrap_or_default()
        )));
    };
    let authorize = url::Url::parse(&authorize_url)
        .map_err(|err| Error::Rauthy(format!("the authorize URL does not parse: {err}")))?;
    let param = |name: &str| -> Option<String> {
        authorize
            .query_pairs()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.into_owned())
    };
    let Some(client_id) = param("client_id") else {
        return Err(Error::Rauthy(format!("no client_id in {authorize_url}")));
    };
    let Some(redirect_uri) = param("redirect_uri") else {
        return Err(Error::Rauthy(format!("no redirect_uri in {authorize_url}")));
    };
    let scopes: Vec<String> = param("scope")
        .unwrap_or_else(|| "openid".to_owned())
        .split_whitespace()
        .map(str::to_owned)
        .collect();

    // rauthy, through the proxy: the origin of the authorize URL is the
    // cell's, since the proxy is the only way rauthy is reached.
    let proxy = format!("{}/auth/v1", base.trim_end_matches('/'));
    let http = reqwest::Client::builder()
        .user_agent("rahi-harness")
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_default();
    let session = http.post(format!("{proxy}/oidc/session")).send().await?;
    if session.status() != StatusCode::CREATED && session.status() != StatusCode::OK {
        return Err(Error::Rauthy(format!(
            "POST /oidc/session answered {}",
            session.status()
        )));
    }
    let session_cookie = session
        .headers()
        .get(SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .map(str::to_owned)
        .ok_or_else(|| Error::Rauthy("the session set no cookie".to_owned()))?;
    let info: Value = session
        .json()
        .await
        .map_err(|err| Error::Rauthy(format!("the session is not json: {err}")))?;
    let csrf = info
        .get("csrf_token")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Rauthy("the session carries no csrf token".to_owned()))?
        .to_owned();
    let challenge = http
        .post(format!("{proxy}/pow"))
        .send()
        .await?
        .text()
        .await?;
    let pow = solve_pow(challenge.trim())?;

    let mut body = json!({
        "email": user.email,
        "password": user.password,
        "pow": pow,
        "client_id": client_id,
        "redirect_uri": redirect_uri,
        "scopes": scopes,
    });
    for name in [
        "state",
        "nonce",
        "code_challenge",
        "code_challenge_method",
        "resource",
    ] {
        if let (Some(value), Some(fields)) = (param(name), body.as_object_mut()) {
            fields.insert(name.to_owned(), Value::String(value));
        }
    }
    let answer = http
        .post(format!("{proxy}/oidc/authorize"))
        .header(COOKIE, session_cookie)
        .header("x-csrf-token", csrf)
        .json(&body)
        .send()
        .await?;
    let status = answer.status();
    let location = answer
        .headers()
        .get(LOCATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    if !status.is_success() {
        return Err(Error::Rauthy(format!(
            "POST /oidc/authorize answered {status}: {}",
            answer.text().await.unwrap_or_default()
        )));
    }
    let Some(callback) = location else {
        return Err(Error::Rauthy(
            "the login completed without a Location to the callback".to_owned(),
        ));
    };

    let done = client.get(&callback).await?;
    let status = done.status();
    if !(status.is_redirection() || status.is_success()) {
        return Err(Error::Rauthy(format!(
            "GET {callback} answered {status}: {}",
            done.text().await.unwrap_or_default()
        )));
    }
    Ok(())
}

/// Solve rauthy's proof of work: the challenge is
/// `version:difficulty:expiry:salt:hash:` and the answer appends the
/// smallest counter whose SHA-256 opens with `difficulty` zero bits.
///
/// # Errors
///
/// [`Error::Rauthy`] when the challenge does not state its difficulty.
pub fn solve_pow(challenge: &str) -> Result<String> {
    let difficulty: u32 = challenge
        .split(':')
        .nth(1)
        .and_then(|field| field.parse().ok())
        .ok_or_else(|| {
            Error::Rauthy(format!("the challenge {challenge:?} states no difficulty"))
        })?;
    for counter in 0u64.. {
        let attempt = format!("{challenge}{counter}");
        if leading_zero_bits(digest(&SHA256, attempt.as_bytes()).as_ref()) >= difficulty {
            return Ok(attempt);
        }
    }
    Err(Error::Rauthy("the counter space ran out".to_owned()))
}

fn leading_zero_bits(bytes: &[u8]) -> u32 {
    let mut bits = 0;
    for byte in bytes {
        bits += byte.leading_zeros();
        if *byte != 0 {
            break;
        }
    }
    bits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_solved_challenge_opens_with_enough_zero_bits() {
        let answer = solve_pow("1:08:1700000000:salt:hash:").unwrap_or_default();
        assert!(answer.starts_with("1:08:1700000000:salt:hash:"));
        let hash = digest(&SHA256, answer.as_bytes());
        assert!(leading_zero_bits(hash.as_ref()) >= 8);
        assert!(solve_pow("nonsense").is_err());
    }
}

impl Rauthy {
    /// Set one client's access token lifetime, in seconds (spec 038 B-4).
    ///
    /// For a test that has to observe a renewal: rauthy gives a refresh
    /// token `nbf = now + access_token_lifetime - 60`, and using it before
    /// that invalidates the token and every session linked to it, so a
    /// renewal at the manifest's 600 seconds is 540 seconds of waiting. A
    /// test shortens the lifetime at the IdP instead of asking the chassis
    /// to turn the `nbf` off (038 D-13).
    ///
    /// # Errors
    ///
    /// [`Error::Rauthy`] when rauthy does not hold the client or refuses the
    /// update, with the status and body.
    pub async fn set_access_token_lifetime(&self, client_id: &str, seconds: u64) -> Result<()> {
        let url = format!("{}/auth/v1/clients/{client_id}", self.base);
        let current = self.auth(self.http.get(&url)).send().await?;
        let status = current.status();
        if !status.is_success() {
            return Err(Error::Rauthy(format!(
                "GET {url} answered {status}: {}",
                current.text().await.unwrap_or_default()
            )));
        }
        let mut client: Value = current
            .json()
            .await
            .map_err(|err| Error::Rauthy(format!("the client document is not json: {err}")))?;
        if let Some(object) = client.as_object_mut() {
            object.insert("access_token_lifetime".to_owned(), json!(seconds));
        }
        let updated = self.auth(self.http.put(&url)).json(&client).send().await?;
        let status = updated.status();
        if !status.is_success() {
            return Err(Error::Rauthy(format!(
                "PUT {url} answered {status}: {}",
                updated.text().await.unwrap_or_default()
            )));
        }
        Ok(())
    }
}

// ------------------------------------------------- the device grant (038)

/// What a token endpoint answered: the credential, and what a client is
/// meant to renew on (spec 038 B-4, D-1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tokens {
    /// The access token, to be presented as `Authorization: Bearer`.
    pub access_token: String,
    /// The refresh token, when the grant included one.
    pub refresh_token: Option<String>,
    /// The lifetime the response states. A client renews on this and never
    /// on a number it read from a manifest (038 D-1).
    pub expires_in: u64,
}

/// Drive RFC 8628 against rauthy, through the cell's origin, as `user`
/// (spec 038 B-7).
///
/// What a person at a terminal does: the client asks for a device code, the
/// person approves it in a browser that is already logged in, and the client
/// polls until rauthy hands over the token set. Every leg goes through the
/// cell's `/auth` proxy, because that is the only way rauthy is reachable.
///
/// # Errors
///
/// [`Error::Rauthy`] at whichever leg does not answer as expected, with the
/// status and the body.
pub async fn device_login(base: &str, user: &User, client_id: &str, scope: &str) -> Result<Tokens> {
    let proxy = format!("{}/auth/v1", base.trim_end_matches('/'));
    let http = http_client();

    // The client's leg: a device code and the code the person types in.
    let asked = http
        .post(format!("{proxy}/oidc/device"))
        .form(&[("client_id", client_id), ("scope", scope)])
        .send()
        .await?;
    let status = asked.status();
    if !status.is_success() {
        return Err(Error::Rauthy(format!(
            "POST /oidc/device answered {status}: {}",
            asked.text().await.unwrap_or_default()
        )));
    }
    let granted: Value = asked
        .json()
        .await
        .map_err(|err| Error::Rauthy(format!("the device grant is not json: {err}")))?;
    let device_code = string_at(&granted, "device_code")?;
    let user_code = string_at(&granted, "user_code")?;

    // The person's leg: an authenticated rauthy session, then the approval.
    let (session_cookie, csrf) = authenticated_session(base, &proxy, &http, user).await?;
    let pow = solve_pow(
        http.post(format!("{proxy}/pow"))
            .send()
            .await?
            .text()
            .await?
            .trim(),
    )?;
    let approved = http
        .post(format!("{proxy}/oidc/device/verify"))
        .header(COOKIE, &session_cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "user_code": user_code,
            "pow": pow,
            "device_accepted": "accept",
        }))
        .send()
        .await?;
    let status = approved.status();
    if !status.is_success() {
        return Err(Error::Rauthy(format!(
            "POST /oidc/device/verify answered {status}: {}",
            approved.text().await.unwrap_or_default()
        )));
    }

    // The client's poll. rauthy answers `authorization_pending` until the
    // approval lands, which is the one legitimate reason to try again.
    for _ in 0..20u8 {
        let answer = http
            .post(format!("{proxy}/oidc/token"))
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", device_code.as_str()),
                ("client_id", client_id),
            ])
            .send()
            .await?;
        if answer.status().is_success() {
            return tokens(answer).await;
        }
        let body = answer.text().await.unwrap_or_default();
        if !body.contains("authorization_pending") && !body.contains("slow_down") {
            return Err(Error::Rauthy(format!(
                "the device token endpoint refused: {body}"
            )));
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    Err(Error::Rauthy(
        "the device grant never yielded a token set".to_owned(),
    ))
}

/// Renew at the issuer with the refresh grant (spec 038 B-7, D-1).
///
/// The chassis has no leg of this: a native client renews at rauthy's own
/// token endpoint and presents the new access token like any other (025
/// B-1, 038 P-1).
///
/// # Errors
///
/// [`Error::Rauthy`] when the token endpoint refuses, with its body.
pub async fn refresh_tokens(base: &str, client_id: &str, refresh_token: &str) -> Result<Tokens> {
    let proxy = format!("{}/auth/v1", base.trim_end_matches('/'));
    let answer = http_client()
        .post(format!("{proxy}/oidc/token"))
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id),
        ])
        .send()
        .await?;
    let status = answer.status();
    if !status.is_success() {
        return Err(Error::Rauthy(format!(
            "the refresh grant answered {status}: {}",
            answer.text().await.unwrap_or_default()
        )));
    }
    tokens(answer).await
}

/// An anonymous rauthy session, logged in as `user` through the cell's own
/// authorization-code flow, kept as a cookie the caller can present.
async fn authenticated_session(
    base: &str,
    proxy: &str,
    http: &reqwest::Client,
    user: &User,
) -> Result<(String, String)> {
    let client = Client::new(base);
    let start = client.get(LOGIN_PATH).await?;
    let status = start.status();
    let authorize_url = start
        .headers()
        .get(LOCATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .ok_or_else(|| {
            Error::Rauthy(format!(
                "GET {LOGIN_PATH} answered {status} without a Location"
            ))
        })?;
    let authorize = url::Url::parse(&authorize_url)
        .map_err(|err| Error::Rauthy(format!("the authorize URL does not parse: {err}")))?;
    let param = |name: &str| -> Option<String> {
        authorize
            .query_pairs()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.into_owned())
    };

    let session = http.post(format!("{proxy}/oidc/session")).send().await?;
    if session.status() != StatusCode::CREATED && session.status() != StatusCode::OK {
        return Err(Error::Rauthy(format!(
            "POST /oidc/session answered {}",
            session.status()
        )));
    }
    let session_cookie = session
        .headers()
        .get(SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .map(str::to_owned)
        .ok_or_else(|| Error::Rauthy("the session set no cookie".to_owned()))?;
    let info: Value = session
        .json()
        .await
        .map_err(|err| Error::Rauthy(format!("the session is not json: {err}")))?;
    let csrf = string_at(&info, "csrf_token")?;

    let pow = solve_pow(
        http.post(format!("{proxy}/pow"))
            .send()
            .await?
            .text()
            .await?
            .trim(),
    )?;
    let mut body = json!({
        "email": user.email,
        "password": user.password,
        "pow": pow,
        "client_id": param("client_id").unwrap_or_default(),
        "redirect_uri": param("redirect_uri").unwrap_or_default(),
        "scopes": param("scope")
            .unwrap_or_else(|| "openid".to_owned())
            .split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>(),
    });
    for name in [
        "state",
        "nonce",
        "code_challenge",
        "code_challenge_method",
        "resource",
    ] {
        if let (Some(value), Some(fields)) = (param(name), body.as_object_mut()) {
            fields.insert(name.to_owned(), Value::String(value));
        }
    }
    let answer = http
        .post(format!("{proxy}/oidc/authorize"))
        .header(COOKIE, &session_cookie)
        .header("x-csrf-token", &csrf)
        .json(&body)
        .send()
        .await?;
    let status = answer.status();
    if !status.is_success() {
        return Err(Error::Rauthy(format!(
            "POST /oidc/authorize answered {status}: {}",
            answer.text().await.unwrap_or_default()
        )));
    }
    Ok((session_cookie, csrf))
}

/// The one http client every leg above uses: no redirects, so a `Location`
/// is read rather than followed.
fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent("rahi-harness")
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_default()
}

/// A token endpoint's answer, read as [`Tokens`].
async fn tokens(response: reqwest::Response) -> Result<Tokens> {
    let body: Value = response
        .json()
        .await
        .map_err(|err| Error::Rauthy(format!("the token set is not json: {err}")))?;
    Ok(Tokens {
        access_token: string_at(&body, "access_token")?,
        refresh_token: body
            .get("refresh_token")
            .and_then(Value::as_str)
            .map(str::to_owned),
        expires_in: body
            .get("expires_in")
            .and_then(Value::as_u64)
            .ok_or_else(|| Error::Rauthy("the token set states no expires_in".to_owned()))?,
    })
}

/// One string field, or a refusal naming it.
fn string_at(value: &Value, field: &str) -> Result<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| Error::Rauthy(format!("the answer carries no {field}: {value}")))
}
