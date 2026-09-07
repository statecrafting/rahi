//! The double-submit CSRF check (spec 020 B-4).
//!
//! A token is minted into a readable cookie on any safe request that arrives
//! without one, and every unsafe request must echo that exact value in
//! `X-CSRF-Token`. The cookie is deliberately not `HttpOnly`: the page has to
//! read it to echo it, and what makes the pair a proof is that a cross-origin
//! caller can send the cookie but cannot read it to build the header.
//!
//! `/auth/*` is exempt. That prefix is rauthy's own proxy (spec 021), it is
//! forwarded raw, and rauthy brings its own CSRF handling; a chassis token on
//! those requests would be a second opinion about a question the IdP already
//! answers (constitution VII).

use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rahi_types::{Config, CookieScheme, Error};

use crate::error;

/// The cookie name over `https`. The `__Host-` prefix binds the cookie to the
/// origin: no `Domain`, `Path=/`, and `Secure` are all mandatory for it.
pub const COOKIE_SECURE: &str = "__Host-csrf";
/// The cookie name over plain `http`, where `__Host-` is not a legal prefix
/// because it requires `Secure` (spec 010 D-4's cookie scheme rule).
pub const COOKIE_PLAIN: &str = "csrf";
/// The header the page echoes the cookie value in.
pub const HEADER: &str = "x-csrf-token";
/// The prefix rauthy's raw proxy owns, exempt from this check.
pub const AUTH_PREFIX: &str = "/auth";
/// How many bytes of entropy a token carries.
pub const TOKEN_BYTES: usize = 32;

/// The layer's state: which cookie name the cell's scheme allows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Csrf {
    scheme: CookieScheme,
}

impl Csrf {
    /// The check for a cell with `scheme`.
    #[must_use]
    pub const fn new(scheme: CookieScheme) -> Self {
        Self { scheme }
    }

    /// Derive the scheme from the one public URL (spec 010 B-7).
    #[must_use]
    pub const fn from_config(config: &Config) -> Self {
        Self::new(config.cookie_scheme)
    }

    /// The cookie name this cell uses.
    #[must_use]
    pub const fn cookie_name(self) -> &'static str {
        if self.scheme.is_secure() {
            COOKIE_SECURE
        } else {
            COOKIE_PLAIN
        }
    }

    /// The `Set-Cookie` value that issues `token`.
    #[must_use]
    pub fn set_cookie(self, token: &str) -> String {
        let mut cookie = format!("{}={token}; Path=/; SameSite=Lax", self.cookie_name());
        if self.scheme.is_secure() {
            cookie.push_str("; Secure");
        }
        cookie
    }
}

/// Mint a fresh token: [`TOKEN_BYTES`] from the operating system, in
/// URL-safe base64 without padding.
///
/// # Errors
///
/// [`Error::Io`] when the operating system will not produce entropy. The
/// caller answers 500 rather than issuing a guessable token.
pub fn issue_token() -> Result<String, Error> {
    let mut bytes = [0u8; TOKEN_BYTES];
    getrandom::fill(&mut bytes).map_err(|err| {
        Error::Io(format!(
            "the system refused entropy for a CSRF token: {err}"
        ))
    })?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// Whether `method` is one the check lets through unproven.
#[must_use]
pub fn is_safe(method: &Method) -> bool {
    matches!(
        *method,
        Method::GET | Method::HEAD | Method::OPTIONS | Method::TRACE
    )
}

/// Whether `path` belongs to rauthy's raw proxy and is exempt (B-4).
#[must_use]
pub fn is_exempt(path: &str) -> bool {
    path == AUTH_PREFIX || path.starts_with("/auth/")
}

/// The value of `name` in a `Cookie` header, if it is there.
#[must_use]
pub fn cookie_value<'h>(header: &'h str, name: &str) -> Option<&'h str> {
    header.split(';').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key.trim() == name).then(|| value.trim())
    })
}

/// Compare two tokens without leaking where they first differ.
#[must_use]
pub fn tokens_match(left: &str, right: &str) -> bool {
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

/// Enforce the double submit, and issue a token when a safe request has none.
pub async fn enforce(State(csrf): State<Csrf>, request: Request, next: Next) -> Response {
    if is_exempt(request.uri().path()) {
        return next.run(request).await;
    }

    let present = request
        .headers()
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|header| cookie_value(header, csrf.cookie_name()))
        .map(str::to_owned);

    if !is_safe(request.method()) {
        let submitted = request
            .headers()
            .get(HEADER)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let matched = present
            .as_deref()
            .is_some_and(|cookie| tokens_match(cookie, &submitted));
        if !matched {
            return error::refusal(
                StatusCode::FORBIDDEN,
                "csrf",
                "the request carries no matching CSRF cookie and X-CSRF-Token pair",
            );
        }
        return next.run(request).await;
    }

    if present.is_some() {
        return next.run(request).await;
    }

    let token = match issue_token() {
        Ok(token) => token,
        Err(err) => return error::response(&err),
    };
    let mut response = next.run(request).await;
    match HeaderValue::from_str(&csrf.set_cookie(&token)) {
        Ok(cookie) => {
            response.headers_mut().append(header::SET_COOKIE, cookie);
            response
        }
        Err(err) => error::response(&Error::Io(format!(
            "the CSRF cookie is not a header value: {err}"
        ))),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn the_cookie_name_follows_the_scheme() {
        assert_eq!(Csrf::new(CookieScheme::Secure).cookie_name(), COOKIE_SECURE);
        assert_eq!(Csrf::new(CookieScheme::Plain).cookie_name(), COOKIE_PLAIN);
    }

    #[test]
    fn the_host_prefix_cookie_is_secure_path_root_and_undomained() {
        let cookie = Csrf::new(CookieScheme::Secure).set_cookie("t");
        assert!(cookie.starts_with("__Host-csrf=t"), "{cookie}");
        assert!(cookie.contains("; Path=/"), "{cookie}");
        assert!(cookie.contains("; Secure"), "{cookie}");
        assert!(!cookie.contains("Domain"), "{cookie}");
        assert!(
            !Csrf::new(CookieScheme::Plain)
                .set_cookie("t")
                .contains("Secure"),
            "plain http carries no Secure attribute"
        );
    }

    #[test]
    fn a_token_is_fresh_every_time() {
        let (a, b) = (
            issue_token().expect("entropy"),
            issue_token().expect("entropy"),
        );
        assert_ne!(a, b);
        assert!(!a.is_empty());
    }

    #[test]
    fn the_auth_prefix_is_exempt_and_its_lookalikes_are_not() {
        assert!(is_exempt("/auth"));
        assert!(is_exempt("/auth/callback"));
        assert!(!is_exempt("/authors"));
        assert!(!is_exempt("/api/auth"));
    }

    #[test]
    fn a_cookie_header_yields_the_named_value() {
        let header = "other=1; __Host-csrf=abc; last=2";
        assert_eq!(cookie_value(header, "__Host-csrf"), Some("abc"));
        assert_eq!(cookie_value(header, "csrf"), None);
    }

    #[test]
    fn empty_tokens_never_match() {
        assert!(!tokens_match("", ""));
        assert!(tokens_match("abc", "abc"));
        assert!(!tokens_match("abc", "abd"));
        assert!(!tokens_match("abc", "ab"));
    }
}
