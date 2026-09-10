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

    /// Create `user` if absent, then set its password, roles, and mark it
    /// enabled and verified so it can log in at once.
    ///
    /// # Errors
    ///
    /// [`Error::Rauthy`] when either call refuses.
    pub async fn ensure_user(&self, user: &User) -> Result<String> {
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
