//! The authorization-code flow with PKCE, and the logout that ends it
//! (spec 022 B-1, B-7).
//!
//! Three routes, mounted by the app under [`SESSION_PREFIX`] and not under
//! `/auth`: that subtree is rauthy's own and is forwarded raw (spec 021 B-2),
//! so a route the app answered inside it is a route the proxy would swallow.
//!
//! The browser is sent to the endpoints rauthy published, which are on the
//! cell's public origin, so the authorization page the user sees is the one
//! same-origin proxy serves. The code exchange goes the other way, over
//! loopback, because that call is the cell talking to the process beside it.
//!
//! **What the login cookie holds is state, not authority.** `state` proves the
//! callback belongs to the request this browser started, `nonce` binds the id
//! token to it, and the PKCE verifier proves the code is being redeemed by
//! whoever asked for it. All three are sealed under the same key as the
//! session envelope, so a forged login cookie is refused for the same reason a
//! forged session cookie is, and the cookie is short-lived because a login
//! that has been sitting half-finished for an hour is not a login anybody is
//! still waiting on.

use axum::Router;
use axum::extract::{Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rahi_types::{CookieScheme, Error, Result, UnixSeconds};
use ring::digest;
use serde::{Deserialize, Serialize};

use crate::envelope::{Cookie, Envelope, Secret, SessionId, cookie_value, open, seal};
use crate::principal::pinned;
use crate::session::{CALLBACK_PATH, LOGIN_PATH, LOGOUT_PATH, SCOPES, Sessions, answer};

/// The login cookie's name over `https`.
pub const LOGIN_COOKIE_SECURE: &str = "__Host-login";
/// The login cookie's name over plain `http`, where `__Host-` is not legal.
pub const LOGIN_COOKIE_PLAIN: &str = "login";
/// How many bytes of entropy `state`, the nonce, and the verifier each carry.
pub const LOGIN_ENTROPY_BYTES: usize = 32;
/// The PKCE challenge method this chassis uses, and the only one (021 B-5).
pub const CHALLENGE_S256: &str = "S256";
/// The `grant_type` a code exchange presents.
pub const GRANT_AUTHORIZATION_CODE: &str = "authorization_code";

/// The login cookie's scoping for a cell with `scheme`.
#[must_use]
pub const fn login_cookie(scheme: CookieScheme) -> Cookie {
    Cookie::named(scheme, LOGIN_COOKIE_SECURE, LOGIN_COOKIE_PLAIN)
}

/// What the short-lived login cookie carries between the two legs of a login.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginState {
    /// Echoed by rauthy on the callback; proves the callback is this login's.
    pub state: String,
    /// Sent in the authorization request and asserted in the id token.
    pub nonce: String,
    /// The PKCE verifier, whose S256 challenge went out with the request.
    pub verifier: Secret<String>,
    /// When this login started.
    pub issued: UnixSeconds,
}

impl LoginState {
    /// Mint the three values a login needs.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the operating system will not produce entropy. The
    /// caller answers 500 rather than starting a login with guessable state.
    pub fn fresh(issued: UnixSeconds) -> Result<Self> {
        Ok(Self {
            state: entropy()?,
            nonce: entropy()?,
            verifier: Secret::new(entropy()?),
            issued,
        })
    }

    /// The S256 challenge of this login's verifier.
    #[must_use]
    pub fn challenge(&self) -> String {
        let hashed = digest::digest(&digest::SHA256, self.verifier.expose().as_bytes());
        URL_SAFE_NO_PAD.encode(hashed.as_ref())
    }
}

/// [`LOGIN_ENTROPY_BYTES`] from the operating system, in URL-safe base64.
fn entropy() -> Result<String> {
    let mut bytes = [0u8; LOGIN_ENTROPY_BYTES];
    getrandom::fill(&mut bytes)
        .map_err(|err| Error::Io(format!("the system refused entropy for a login: {err}")))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// The session routes, mounted by the app at [`SESSION_PREFIX`].
///
/// ```no_run
/// # use rahi_idp::{SESSION_PREFIX, Sessions, session_router};
/// # fn compose(sessions: Sessions, app: axum::Router) -> axum::Router {
/// app.nest(SESSION_PREFIX, session_router(sessions))
/// # }
/// ```
pub fn session_router(sessions: Sessions) -> Router {
    Router::new()
        .route(LOGIN_PATH, get(begin))
        .route(CALLBACK_PATH, get(callback))
        .route(LOGOUT_PATH, post(logout))
        .with_state(sessions)
}

/// `GET /session/login`: mint the state and send the browser to rauthy (B-1).
async fn begin(State(sessions): State<Sessions>) -> Response {
    match authorize(&sessions) {
        Ok(response) => response,
        Err(err) => answer(&err),
    }
}

/// Build the redirect and the cookie that remembers what it sent.
fn authorize(sessions: &Sessions) -> Result<Response> {
    let login = LoginState::fresh(sessions.now())?;
    let mut url = url::Url::parse(&sessions.discovery().authorization_endpoint).map_err(|err| {
        Error::Upstream(format!(
            "rauthy's authorization endpoint is not a URL: {err}"
        ))
    })?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &sessions.idp().client_id)
        .append_pair("redirect_uri", sessions.redirect_uri())
        .append_pair("scope", SCOPES)
        .append_pair("state", &login.state)
        .append_pair("nonce", &login.nonce)
        .append_pair("code_challenge", &login.challenge())
        .append_pair("code_challenge_method", CHALLENGE_S256);

    let sealed = seal(&login, sessions.key())?;
    let cookie = login_cookie(sessions.cookie_scheme())
        .set_for(&sealed, Some(sessions.login_ttl().as_secs()));
    redirect(url.as_str(), &[cookie])
}

/// The query rauthy sends back to the callback.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Callback {
    /// The authorization code, on success.
    #[serde(default)]
    pub code: Option<String>,
    /// The state this cell sent, echoed.
    #[serde(default)]
    pub state: Option<String>,
    /// The error code, when rauthy refused.
    #[serde(default)]
    pub error: Option<String>,
}

/// `GET /session/callback`: validate, exchange, verify, establish (B-1).
async fn callback(
    State(sessions): State<Sessions>,
    Query(query): Query<Callback>,
    headers: axum::http::HeaderMap,
) -> Response {
    let cookies = header_str(&headers, header::COOKIE);
    match establish(&sessions, &query, cookies.as_deref()).await {
        Ok(response) => response,
        // The login cookie is cleared on every failure: a half-finished login
        // whose state was spent, refused, or forged is not one to resume, and
        // leaving it in place would let a second attempt reuse a verifier.
        Err(err) => with_cookies(
            answer(&err),
            &[login_cookie(sessions.cookie_scheme()).clear()],
        ),
    }
}

/// The whole second leg of the login.
async fn establish(
    sessions: &Sessions,
    query: &Callback,
    cookies: Option<&str>,
) -> Result<Response> {
    if let Some(error) = &query.error {
        return Err(Error::Unauthorized(format!(
            "rauthy refused the authorization with {error:?}"
        )));
    }
    let sealed = cookies
        .and_then(|header| cookie_value(header, login_cookie(sessions.cookie_scheme()).name()))
        .ok_or_else(|| {
            Error::Unauthorized("this callback carries no login cookie to match it to".to_owned())
        })?;
    let login: LoginState = open(sealed, sessions.key())?;

    if sessions.now().get().saturating_sub(login.issued.get()) > sessions.login_ttl().as_secs() {
        return Err(Error::Unauthorized(
            "this login started too long ago to be finished now".to_owned(),
        ));
    }
    let echoed = query.state.as_deref().unwrap_or_default();
    if !constant_time_eq(echoed, &login.state) {
        return Err(Error::Unauthorized(
            "the callback's state is not the one this login sent".to_owned(),
        ));
    }
    let code = query.code.as_deref().ok_or_else(|| {
        Error::Unauthorized("the callback carries no authorization code".to_owned())
    })?;

    let granted = sessions
        .token(&[
            ("grant_type", GRANT_AUTHORIZATION_CODE),
            ("code", code),
            ("redirect_uri", sessions.redirect_uri()),
            ("code_verifier", login.verifier.expose()),
        ])
        .await?;
    let id_token = granted
        .id_token
        .as_deref()
        .ok_or_else(|| Error::Unauthorized("the code exchange returned no id token".to_owned()))?;
    let asserted = sessions.verify_id_token(id_token, &login.nonce).await?;
    let sub = asserted.subject()?;

    // The assertion is minted from the id token *and* userinfo (B-3): the id
    // token proves who this is, and userinfo is the surface every renewal will
    // re-read, so reading it here means login and renewal agree from the first
    // request rather than from the second.
    let claims = sessions.userinfo(&granted.access_token).await?;
    pinned(&sub, &claims)?;

    let sid = SessionId::fresh()?;
    sessions
        .mint(&sid, sub.clone(), &claims, granted.expires_in)
        .await?;
    let refresh_token = granted.refresh_token.ok_or_else(|| {
        Error::Unauthorized(
            "the code exchange returned no refresh token, so this session could never renew"
                .to_owned(),
        )
    })?;
    let envelope = Envelope::new(sid, sub, refresh_token, sessions.now());

    redirect(
        sessions.origin(),
        &[
            sessions.cookie().set(&seal(&envelope, sessions.key())?),
            login_cookie(sessions.cookie_scheme()).clear(),
        ],
    )
}

/// `POST /session/logout`: revoke, clear, and send the browser to rauthy
/// (B-7).
async fn logout(State(sessions): State<Sessions>, headers: axum::http::HeaderMap) -> Response {
    let cookies = header_str(&headers, header::COOKIE);
    let cleared = [
        sessions.cookie().clear(),
        login_cookie(sessions.cookie_scheme()).clear(),
    ];

    if let Some(envelope) = opened_envelope(&sessions, cookies.as_deref()) {
        // Both failures are ignored on purpose. The session ends here whatever
        // rauthy says: a token this cell has thrown away is a token it will
        // never present again, and a logout that reported a 502 because the
        // IdP blinked would leave the user logged in on the only surface they
        // control.
        let _ = sessions.revoke(envelope.refresh_token.expose()).await;
        let _ = sessions.drop_assertion(&envelope.sid).await;
    }

    let mut url = sessions.discovery().end_session_endpoint.clone();
    if let Ok(mut parsed) = url::Url::parse(&url) {
        parsed
            .query_pairs_mut()
            .append_pair("client_id", &sessions.idp().client_id)
            .append_pair("post_logout_redirect_uri", sessions.origin());
        url = parsed.into();
    }
    redirect(&url, &cleared).unwrap_or_else(|err| answer(&err))
}

/// The envelope in `cookies`, when there is one this cell sealed.
fn opened_envelope(sessions: &Sessions, cookies: Option<&str>) -> Option<Envelope> {
    let sealed = cookie_value(cookies?, sessions.cookie().name())?;
    open::<Envelope>(sealed, sessions.key()).ok()
}

/// A 303 to `location`, carrying `cookies`.
fn redirect(location: &str, cookies: &[String]) -> Result<Response> {
    let location = HeaderValue::from_str(location).map_err(|err| {
        Error::Upstream(format!("the redirect target is not a header value: {err}"))
    })?;
    let mut response = StatusCode::SEE_OTHER.into_response();
    response.headers_mut().insert(header::LOCATION, location);
    Ok(with_cookies(response, cookies))
}

/// `response` with every cookie in `cookies` appended.
fn with_cookies(mut response: Response, cookies: &[String]) -> Response {
    for cookie in cookies {
        if let Ok(value) = HeaderValue::from_str(cookie) {
            response.headers_mut().append(header::SET_COOKIE, value);
        }
    }
    response
}

/// One request header as a string, when it is one.
fn header_str(headers: &axum::http::HeaderMap, name: header::HeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

/// Compare two values without leaking where they first differ.
fn constant_time_eq(left: &str, right: &str) -> bool {
    let (left, right) = (left.as_bytes(), right.as_bytes());
    if left.len() != right.len() || left.is_empty() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.iter().zip(right.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}

/// The login cookie's name for a cell with `scheme`.
///
/// A test that builds a login cookie by hand needs the name this cell will
/// look for, and so does an operator reading a browser's cookie jar.
#[must_use]
pub fn login_cookie_name(scheme: CookieScheme) -> &'static str {
    login_cookie(scheme).name()
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn the_challenge_is_the_s256_of_the_verifier() {
        // RFC 7636 appendix B's worked example: the verifier and the challenge
        // it must produce. If this ever drifts, every login stops working at
        // rauthy rather than here, so the vector is the cheap place to catch it.
        let login = LoginState {
            state: "s".to_owned(),
            nonce: "n".to_owned(),
            verifier: Secret::new("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".to_owned()),
            issued: UnixSeconds::new(0),
        };
        assert_eq!(
            login.challenge(),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn every_login_gets_its_own_state_nonce_and_verifier() {
        let a = LoginState::fresh(UnixSeconds::new(0)).expect("entropy");
        let b = LoginState::fresh(UnixSeconds::new(0)).expect("entropy");
        assert_ne!(a.state, b.state);
        assert_ne!(a.nonce, b.nonce);
        assert_ne!(a.verifier.expose(), b.verifier.expose());
        assert_ne!(a.state, a.nonce, "one draw per value, never one reused");
    }

    #[test]
    fn a_login_state_round_trips_under_the_session_key() {
        let key = crate::envelope::SessionKey::from_bytes(&[5u8; 32]).expect("a key");
        let login = LoginState::fresh(UnixSeconds::new(7)).expect("entropy");
        let sealed = seal(&login, &key).expect("it seals");
        let opened: LoginState = open(&sealed, &key).expect("it opens");
        assert_eq!(opened, login);
    }

    #[test]
    fn the_login_cookie_follows_the_scheme() {
        assert_eq!(login_cookie_name(CookieScheme::Secure), "__Host-login");
        assert_eq!(login_cookie_name(CookieScheme::Plain), "login");
        let cookie = login_cookie(CookieScheme::Secure).set_for("v", Some(600));
        assert!(cookie.contains("Max-Age=600"), "{cookie}");
        assert!(cookie.contains("HttpOnly"), "{cookie}");
    }

    #[test]
    fn state_is_compared_without_leaking_the_prefix() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "ab"));
        assert!(!constant_time_eq("", ""), "an empty state never matches");
    }

    #[test]
    fn the_routes_are_the_three_this_spec_names() {
        assert_eq!(crate::session::SESSION_PREFIX, "/session");
        assert_eq!(LOGIN_PATH, "/login");
        assert_eq!(CALLBACK_PATH, "/callback");
        assert_eq!(LOGOUT_PATH, "/logout");
    }
}
