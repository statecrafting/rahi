//! The session cookie: a signed envelope that grants nothing (spec 022 B-2).
//!
//! What the browser holds is not a credential. It is rauthy's refresh token
//! and the subject that token belongs to, sealed under a key only this cell
//! has, and the seal is integrity alone: it says the cell wrote these bytes,
//! not that the bearer may do anything. Every question about what the bearer
//! may do is answered by asking rauthy again ([`crate::refresh`]), which is
//! what makes a role removed at the IdP take effect here without the app
//! being told.
//!
//! Nothing about the principal is in the cookie. No roles, no email, no
//! verification flag: those are the IdP's answers and they go stale, and a
//! stale answer in a cookie is an answer the app would have to be persuaded
//! to stop believing. The subject is pinned because a renewal must not be
//! able to change whose session this is (B-5), and the issue time is there so
//! an envelope can be aged out without a round-trip.
//!
//! The same seal carries the short-lived login envelope ([`crate::login`]),
//! which is why [`seal`] and [`open`] are generic: one construction, one place
//! where a signature is checked, one place to get constant-time comparison
//! right.

use std::fmt;
use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rahi_types::{CookieScheme, Error, Result, Sub, UnixSeconds};
use ring::hmac;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// The session cookie's name over `https`. The `__Host-` prefix binds it to
/// the origin: no `Domain`, `Path=/`, and `Secure` are all mandatory for it.
pub const COOKIE_SECURE: &str = "__Host-session";
/// The session cookie's name over plain `http`, where `__Host-` is not a legal
/// prefix because it requires `Secure` (spec 010 D-4's cookie scheme rule).
pub const COOKIE_PLAIN: &str = "session";
/// The file under the key set that holds the sealing key.
pub const SESSION_KEY_FILE: &str = "session.key";
/// How many bytes of entropy a session id carries.
pub const SESSION_ID_BYTES: usize = 32;
/// The shortest sealing key this crate accepts, in bytes.
pub const MIN_KEY_BYTES: usize = 32;
/// What separates the sealed payload from its tag.
const SEPARATOR: char = '.';

/// A value that must not appear in a log line, a `Debug` render, or an error.
///
/// The wrapper is the whole point: a refresh token that is only ever reachable
/// through [`Secret::expose`] is a refresh token whose every leak site is
/// greppable.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Secret<T>(T);

impl<T> Secret<T> {
    /// Wrap a value.
    pub const fn new(value: T) -> Self {
        Self(value)
    }

    /// Read the value, naming the risk at the call site.
    pub const fn expose(&self) -> &T {
        &self.0
    }

    /// Take the value out.
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// The key of one server-side assertion, minted at login and rotated never.
///
/// Random rather than derived: the assertion it names lives in the cache group
/// (B-3), which is shared with every other derived value the cell keeps, and a
/// key an attacker can guess is a key an attacker can read.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(String);

impl SessionId {
    /// Mint one from the operating system's entropy.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the system will not produce entropy. The caller
    /// answers 500 rather than issuing a guessable id.
    pub fn fresh() -> Result<Self> {
        let mut bytes = [0u8; SESSION_ID_BYTES];
        getrandom::fill(&mut bytes)
            .map_err(|err| Error::Io(format!("the system refused entropy for a session: {err}")))?;
        Ok(Self(URL_SAFE_NO_PAD.encode(bytes)))
    }

    /// Wrap an id that was already minted.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The id.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What the session cookie carries, and all it carries (B-2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    /// The key of the server-side assertion this envelope's session holds
    /// (B-3). Carried here because the cookie is the only thing the browser
    /// sends back.
    pub sid: SessionId,
    /// rauthy's subject, pinned: a renewal cannot change who this session
    /// belongs to (B-5, constitution VII).
    pub sub: Sub,
    /// rauthy's refresh token. The one credential in the envelope, and it is
    /// rauthy's to accept or refuse.
    pub refresh_token: Secret<String>,
    /// When the cell sealed this envelope.
    pub issued: UnixSeconds,
}

impl Envelope {
    /// The envelope a fresh login seals.
    #[must_use]
    pub fn new(sid: SessionId, sub: Sub, refresh_token: String, issued: UnixSeconds) -> Self {
        Self {
            sid,
            sub,
            refresh_token: Secret::new(refresh_token),
            issued,
        }
    }

    /// The same session with a rotated refresh token (B-5).
    ///
    /// The subject and the session id are carried through unchanged; a
    /// renewal replaces the credential and nothing else.
    #[must_use]
    pub fn rotated(&self, refresh_token: String, issued: UnixSeconds) -> Self {
        Self {
            sid: self.sid.clone(),
            sub: self.sub.clone(),
            refresh_token: Secret::new(refresh_token),
            issued,
        }
    }
}

/// The key the envelope is sealed under, read from the cell's key set.
#[derive(Clone)]
pub struct SessionKey(hmac::Key);

impl fmt::Debug for SessionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionKey").finish_non_exhaustive()
    }
}

impl SessionKey {
    /// Take a key from bytes already in hand.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when there are fewer than [`MIN_KEY_BYTES`]. A short
    /// key is a weak seal, and a weak seal on a cookie holding a refresh token
    /// is worth refusing at boot rather than discovering later.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < MIN_KEY_BYTES {
            return Err(Error::Config(format!(
                "the session key is {} bytes and at least {MIN_KEY_BYTES} are required",
                bytes.len()
            )));
        }
        Ok(Self(hmac::Key::new(hmac::HMAC_SHA256, bytes)))
    }

    /// Read the key from `path`, which is `<keys_dir>/session.key`.
    ///
    /// Trailing whitespace is trimmed, so a key an operator wrote with an
    /// editor works; nothing else about the bytes is interpreted.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the file cannot be read; [`Error::Config`] when what
    /// it holds is too short.
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).map_err(|err| {
            Error::Io(format!(
                "the session key at {} cannot be read: {err}",
                path.display()
            ))
        })?;
        let trimmed: &[u8] = match bytes.iter().rposition(|b| !b.is_ascii_whitespace()) {
            Some(last) => bytes.get(..=last).unwrap_or(&bytes),
            None => &[],
        };
        Self::from_bytes(trimmed)
    }

    /// Mint a key from the operating system's entropy.
    ///
    /// For a test and for the first boot of a cell whose key set is empty;
    /// spec 031 is what custodies one across restarts.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the system will not produce entropy.
    pub fn generate() -> Result<Self> {
        let mut bytes = [0u8; MIN_KEY_BYTES];
        getrandom::fill(&mut bytes).map_err(|err| {
            Error::Io(format!(
                "the system refused entropy for a session key: {err}"
            ))
        })?;
        Self::from_bytes(&bytes)
    }
}

/// Seal `value` under `key`: the payload, a dot, and the tag over it.
///
/// # Errors
///
/// [`Error::Integrity`] when the value cannot be serialised, which for the
/// types this crate seals means a bug rather than an input.
pub fn seal<T: Serialize>(value: &T, key: &SessionKey) -> Result<String> {
    let json = serde_json::to_vec(value)
        .map_err(|err| Error::Integrity(format!("a session envelope will not serialise: {err}")))?;
    let payload = URL_SAFE_NO_PAD.encode(json);
    let tag = hmac::sign(&key.0, payload.as_bytes());
    let mac = URL_SAFE_NO_PAD.encode(tag.as_ref());
    Ok(format!("{payload}{SEPARATOR}{mac}"))
}

/// Open a sealed value, refusing anything the key did not seal.
///
/// The signature is checked before the payload is parsed, so a forged cookie
/// never reaches a deserialiser.
///
/// # Errors
///
/// [`Error::Unauthorized`] when the value has no tag, when the tag does not
/// verify, or when the payload the seal covers is not the expected shape. All
/// three are the same event to a caller: this cell did not write this cookie,
/// or it did and something rewrote it.
pub fn open<T: DeserializeOwned>(raw: &str, key: &SessionKey) -> Result<T> {
    let (payload, mac) = raw
        .rsplit_once(SEPARATOR)
        .ok_or_else(|| Error::Unauthorized("the session cookie carries no signature".to_owned()))?;
    let tag = URL_SAFE_NO_PAD.decode(mac).map_err(|_| {
        Error::Unauthorized("the session cookie's signature is not base64".to_owned())
    })?;
    hmac::verify(&key.0, payload.as_bytes(), &tag).map_err(|_| {
        Error::Unauthorized("the session cookie's signature does not verify".to_owned())
    })?;

    let json = URL_SAFE_NO_PAD.decode(payload).map_err(|_| {
        Error::Unauthorized("the session cookie's payload is not base64".to_owned())
    })?;
    serde_json::from_slice(&json).map_err(|err| {
        Error::Unauthorized(format!(
            "the session cookie's payload is not the shape this cell seals: {err}"
        ))
    })
}

/// How a cookie this crate sets is scoped, decided by the public URL's scheme.
///
/// One type for both cookies B-1 and B-2 name: the attributes are the same and
/// only the name differs, so a change to the scoping rule cannot apply to one
/// and miss the other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cookie {
    scheme: CookieScheme,
    secure_name: &'static str,
    plain_name: &'static str,
}

impl Cookie {
    /// The session cookie for a cell with `scheme` (B-2).
    #[must_use]
    pub const fn session(scheme: CookieScheme) -> Self {
        Self {
            scheme,
            secure_name: COOKIE_SECURE,
            plain_name: COOKIE_PLAIN,
        }
    }

    /// A cookie with the same attributes under another pair of names.
    #[must_use]
    pub const fn named(
        scheme: CookieScheme,
        secure_name: &'static str,
        plain_name: &'static str,
    ) -> Self {
        Self {
            scheme,
            secure_name,
            plain_name,
        }
    }

    /// The name this cell's scheme allows.
    #[must_use]
    pub const fn name(self) -> &'static str {
        if self.scheme.is_secure() {
            self.secure_name
        } else {
            self.plain_name
        }
    }

    /// The `Set-Cookie` value that issues `value`.
    ///
    /// `HttpOnly` because no page has any reason to read an envelope, and a
    /// page that cannot read it is a page whose compromise does not hand over
    /// a refresh token. `SameSite=Lax` because the login redirect arrives as a
    /// top-level navigation and `Strict` would drop the cookie on it.
    #[must_use]
    pub fn set(self, value: &str) -> String {
        self.set_for(value, None)
    }

    /// [`Cookie::set`] with a lifetime, for the short-lived login cookie.
    #[must_use]
    pub fn set_for(self, value: &str, max_age_secs: Option<u64>) -> String {
        let mut cookie = format!("{}={value}; Path=/; HttpOnly; SameSite=Lax", self.name());
        if let Some(seconds) = max_age_secs {
            cookie.push_str(&format!("; Max-Age={seconds}"));
        }
        if self.scheme.is_secure() {
            cookie.push_str("; Secure");
        }
        cookie
    }

    /// The `Set-Cookie` value that removes the cookie.
    #[must_use]
    pub fn clear(self) -> String {
        let mut cookie = format!(
            "{}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0",
            self.name()
        );
        if self.scheme.is_secure() {
            cookie.push_str("; Secure");
        }
        cookie
    }
}

/// The value of `name` in a `Cookie` header, if it is there.
#[must_use]
pub fn cookie_value<'h>(header: &'h str, name: &str) -> Option<&'h str> {
    header.split(';').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key.trim() == name).then(|| value.trim())
    })
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    fn envelope() -> Envelope {
        Envelope::new(
            SessionId::new("sid-1"),
            Sub::new("rauthy-subject"),
            "refresh-token-1".to_owned(),
            UnixSeconds::new(1_767_225_600),
        )
    }

    #[test]
    fn a_sealed_envelope_round_trips() {
        let key = SessionKey::from_bytes(&[3u8; 32]).expect("32 bytes is enough");
        let sealed = seal(&envelope(), &key).expect("it seals");
        let opened: Envelope = open(&sealed, &key).expect("it opens");
        assert_eq!(opened, envelope());
        assert_eq!(opened.refresh_token.expose(), "refresh-token-1");
    }

    #[test]
    fn a_tampered_payload_is_refused() {
        let key = SessionKey::from_bytes(&[3u8; 32]).expect("32 bytes is enough");
        let sealed = seal(&envelope(), &key).expect("it seals");
        let (payload, mac) = sealed.rsplit_once('.').expect("it has a tag");

        let forged = format!("{}x{SEPARATOR}{mac}", payload);
        let err = open::<Envelope>(&forged, &key).expect_err("a rewritten payload is refused");
        assert_eq!(err.kind(), "unauthorized");

        let stripped = payload.to_owned();
        let err = open::<Envelope>(&stripped, &key).expect_err("an unsigned value is refused");
        assert_eq!(err.kind(), "unauthorized");
    }

    #[test]
    fn another_cells_key_does_not_open_this_cells_cookie() {
        let mine = SessionKey::from_bytes(&[3u8; 32]).expect("a key");
        let theirs = SessionKey::from_bytes(&[4u8; 32]).expect("another key");
        let sealed = seal(&envelope(), &mine).expect("it seals");
        let err = open::<Envelope>(&sealed, &theirs).expect_err("refused");
        assert_eq!(err.kind(), "unauthorized");
    }

    #[test]
    fn a_short_key_is_refused_before_it_seals_anything() {
        let err = SessionKey::from_bytes(&[1u8; 16]).expect_err("16 bytes is not enough");
        assert_eq!(err.kind(), "config");
    }

    #[test]
    fn the_cookie_follows_the_scheme_and_is_never_readable_by_a_page() {
        let secure = Cookie::session(CookieScheme::Secure).set("v");
        assert!(secure.starts_with("__Host-session=v"), "{secure}");
        assert!(secure.contains("; HttpOnly"), "{secure}");
        assert!(secure.contains("; SameSite=Lax"), "{secure}");
        assert!(secure.contains("; Path=/"), "{secure}");
        assert!(secure.contains("; Secure"), "{secure}");
        assert!(!secure.contains("Domain"), "{secure}");

        let plain = Cookie::session(CookieScheme::Plain).set("v");
        assert!(plain.starts_with("session=v"), "{plain}");
        assert!(!plain.contains("Secure"), "{plain}");
    }

    #[test]
    fn clearing_expires_the_cookie_under_the_same_name() {
        let cleared = Cookie::session(CookieScheme::Secure).clear();
        assert!(cleared.starts_with("__Host-session="), "{cleared}");
        assert!(cleared.contains("Max-Age=0"), "{cleared}");
    }

    #[test]
    fn a_secret_never_renders_what_it_holds() {
        let secret = Secret::new("refresh-token-1".to_owned());
        assert_eq!(format!("{secret:?}"), "Secret(<redacted>)");
        assert!(!format!("{:?}", envelope()).contains("refresh-token-1"));
    }

    #[test]
    fn a_session_id_is_fresh_every_time() {
        let (a, b) = (
            SessionId::fresh().expect("entropy"),
            SessionId::fresh().expect("entropy"),
        );
        assert_ne!(a, b);
        assert!(!a.as_str().is_empty());
    }

    #[test]
    fn a_cookie_header_is_read_by_name() {
        let header = "__Host-csrf=abc; __Host-session=xyz; other=1";
        assert_eq!(cookie_value(header, "__Host-session"), Some("xyz"));
        assert_eq!(cookie_value(header, "session"), None);
    }
}
