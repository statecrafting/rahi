//! A cookie jar for one origin (spec 033 B-3).
//!
//! The harness talks to one cell at one origin, so the jar keys cookies by
//! name and ignores `Domain`; `Path` is honoured as a prefix so a cookie
//! scoped to `/auth` is not sent to `/`. Expiry and `Max-Age=0` delete.
//! `Secure` cookies are kept and sent on `https` origins only, which is
//! what a browser would do and what the chassis's `__Host-` names need.

use std::collections::BTreeMap;

/// One stored cookie.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cookie {
    /// The name.
    pub name: String,
    /// The value, verbatim.
    pub value: String,
    /// The path the cookie is scoped to; `/` when the server said nothing.
    pub path: String,
    /// Whether the cookie was marked `Secure`.
    pub secure: bool,
}

/// The jar: cookies by name, for one origin.
#[derive(Clone, Debug, Default)]
pub struct CookieJar {
    cookies: BTreeMap<String, Cookie>,
}

impl CookieJar {
    /// An empty jar.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Store what a `Set-Cookie` header value says: a new value, or a
    /// deletion when `Max-Age` is zero or negative or `Expires` is in the
    /// past enough to be the epoch a server deletes with.
    pub fn store(&mut self, set_cookie: &str) {
        let mut parts = set_cookie.split(';').map(str::trim);
        let Some(pair) = parts.next() else {
            return;
        };
        let Some((name, value)) = pair.split_once('=') else {
            return;
        };
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        let mut cookie = Cookie {
            name: name.to_owned(),
            value: value.trim().to_owned(),
            path: "/".to_owned(),
            secure: false,
        };
        let mut delete = false;
        for attribute in parts {
            let (key, val) = attribute
                .split_once('=')
                .map_or((attribute, ""), |(k, v)| (k.trim(), v.trim()));
            if key.eq_ignore_ascii_case("path") && !val.is_empty() {
                cookie.path = val.to_owned();
            } else if key.eq_ignore_ascii_case("secure") {
                cookie.secure = true;
            } else if key.eq_ignore_ascii_case("max-age") {
                delete = val.parse::<i64>().is_ok_and(|n| n <= 0);
            } else if key.eq_ignore_ascii_case("expires") && val.contains("1970") {
                delete = true;
            }
        }
        if delete || cookie.value.is_empty() {
            self.cookies.remove(name);
        } else {
            self.cookies.insert(name.to_owned(), cookie);
        }
    }

    /// The `Cookie` header value for a request to `path` on an origin that
    /// is `https` or not, or `None` when nothing applies.
    #[must_use]
    pub fn header_for(&self, path: &str, https: bool) -> Option<String> {
        let pairs: Vec<String> = self
            .cookies
            .values()
            .filter(|c| path_matches(&c.path, path))
            .filter(|c| https || !c.secure)
            .map(|c| format!("{}={}", c.name, c.value))
            .collect();
        if pairs.is_empty() {
            None
        } else {
            Some(pairs.join("; "))
        }
    }

    /// The value of the cookie called `name`, if stored.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.cookies.get(name).map(|c| c.value.as_str())
    }

    /// Every stored cookie, by name.
    #[must_use]
    pub fn all(&self) -> Vec<&Cookie> {
        self.cookies.values().collect()
    }

    /// Forget everything.
    pub fn clear(&mut self) {
        self.cookies.clear();
    }
}

/// RFC 6265 path matching: equal, or a prefix at a `/` boundary.
fn path_matches(cookie_path: &str, request_path: &str) -> bool {
    if cookie_path == request_path {
        return true;
    }
    if let Some(rest) = request_path.strip_prefix(cookie_path) {
        return cookie_path.ends_with('/') || rest.starts_with('/');
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_sends_and_deletes() {
        let mut jar = CookieJar::new();
        jar.store("csrf=abc; Path=/; SameSite=Strict");
        jar.store("__Host-session=s1; Path=/; Secure; HttpOnly");
        jar.store("login=l1; Path=/auth; HttpOnly");
        assert_eq!(jar.header_for("/", false), Some("csrf=abc".to_owned()));
        assert_eq!(
            jar.header_for("/", true),
            Some("__Host-session=s1; csrf=abc".to_owned())
        );
        assert_eq!(
            jar.header_for("/auth/v1/x", false),
            Some("csrf=abc; login=l1".to_owned())
        );
        assert_eq!(jar.header_for("/authz", false), Some("csrf=abc".to_owned()));
        jar.store("csrf=; Max-Age=0; Path=/");
        assert_eq!(jar.get("csrf"), None);
        jar.store("login=; Path=/auth; Expires=Thu, 01 Jan 1970 00:00:00 GMT");
        assert_eq!(jar.header_for("/auth/v1/x", false), None);
    }
}
