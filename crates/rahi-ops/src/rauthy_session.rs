//! The backup admin and its software passkey (spec 037 B-1).
//!
//! rauthy's backup routes accept an admin *session* and nothing else, and
//! `validate_admin_session` refuses a session without MFA while
//! `ADMIN_FORCE_MFA` is on, which is one instance-wide setting this
//! chassis never turns off (037 D-1). So the cell carries a principal of
//! its own for the backup verb: a dedicated rauthy admin whose only
//! credential is a passkey, its private key custodied in the key set
//! beside every other key, and its public half registered with rauthy
//! through rauthy's own ceremonies.
//!
//! Nothing here reads rauthy's directory (constitution VIII): this is an
//! HTTP client for routes rauthy publishes, and every route it calls is a
//! route read from rauthy's source. It never probes: rauthy blacklists the
//! calling address for hours on a request to a path its scanner list
//! knows, and that blacklist survives a restart (037 D-3).
//!
//! # The account is passkey only
//!
//! The dedicated admin holds no password at all. It is created with one,
//! because rauthy's `User::convert_to_passkey` refuses any account type but
//! `Password`, and the conversion clears `password` and `password_expires`
//! in the same write; the transient password lives in memory for the length
//! of one registration and is never written anywhere. A passkey-only
//! account therefore has no credential that can expire behind the verb
//! (037 D-3).
//!
//! # The authenticator
//!
//! [`Passkey`] is a software authenticator: one resident ES256 credential,
//! `none` attestation, user verification always asserted. Its signature
//! counter is permanently zero, which WebAuthn 6.1.1 defines as "not
//! supported"; a counter that resets between processes reads to rauthy as a
//! cloned credential and is refused.

use std::collections::BTreeMap;
use std::time::Duration;

use base64::Engine as _;
use rahi_types::{Config, Error, Result};
use ring::digest::{SHA256, digest};
use ring::rand::{SecureRandom as _, SystemRandom};
use ring::signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, KeyPair as _};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The dedicated backup admin's login name. Fixed rather than derived from
/// the public URL, so that changing the URL does not orphan the account
/// rauthy's own database (and every backup of it) already holds.
pub const BACKUP_ADMIN_EMAIL: &str = "rahi-backup@localhost";

/// rauthy's built-in administrator role, which the backup routes demand.
pub const BACKUP_ADMIN_ROLE: &str = "rauthy_admin";

/// What the passkey is called in rauthy's own listing. Alphanumeric:
/// rauthy validates the name against a restrictive pattern.
pub const PASSKEY_LABEL: &str = "rahibackup";

/// rauthy pages its user listing once a deployment has more users than its
/// server-side-pagination threshold; the search follows at most this many
/// pages before it says so rather than looping.
pub const MAX_USER_PAGES: usize = 512;

/// The header rauthy returns the next page's cursor in.
const CONTINUATION_HEADER: &str = "x-continuation-token";

/// The client rauthy itself publishes, which the backup admin logs in
/// through. No client of the cell's is involved: this login mints no
/// session for the app, only for rauthy's admin API.
pub const RAUTHY_CLIENT_ID: &str = "rauthy";

/// rauthy's own callback, relative to the public origin.
pub const RAUTHY_CALLBACK_PATH: &str = "/auth/v1/oidc/callback";

/// How long one call to rauthy may take.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// The `User-Agent` every call carries. rauthy refuses a login with an
/// empty one and reports it to the caller as "Invalid user credentials"
/// (037 P-3).
pub const USER_AGENT: &str = concat!("rahi-ops/", env!("CARGO_PKG_VERSION"));

fn b64u(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn b64u_decode(text: &str) -> Result<Vec<u8>> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(text.trim())
        .map_err(|err| Error::Config(format!("a passkey field is not url-safe base64: {err}")))
}

// ------------------------------------------------------------------ CBOR

/// The two CBOR shapes an authenticator emits: the COSE public key and the
/// attestation object. Definite lengths, no floats, no tags.
mod cbor {
    fn head(major: u8, n: u64, out: &mut Vec<u8>) {
        let major = major << 5;
        match n {
            0..=23 => out.push(major | u8::try_from(n).unwrap_or(0)),
            24..=0xff => {
                out.push(major | 24);
                out.push(u8::try_from(n).unwrap_or(0));
            }
            0x100..=0xffff => {
                out.push(major | 25);
                out.extend_from_slice(&u16::try_from(n).unwrap_or(0).to_be_bytes());
            }
            _ => {
                out.push(major | 26);
                out.extend_from_slice(&u32::try_from(n).unwrap_or(0).to_be_bytes());
            }
        }
    }

    /// An integer, negative ones as major type 1.
    pub fn int(v: i64, out: &mut Vec<u8>) {
        if v >= 0 {
            head(0, v.unsigned_abs(), out);
        } else {
            head(1, (-(v + 1)).unsigned_abs(), out);
        }
    }

    /// A byte string.
    pub fn bytes(v: &[u8], out: &mut Vec<u8>) {
        head(2, v.len() as u64, out);
        out.extend_from_slice(v);
    }

    /// A text string.
    pub fn text(v: &str, out: &mut Vec<u8>) {
        head(3, v.len() as u64, out);
        out.extend_from_slice(v.as_bytes());
    }

    /// A map header of `n` pairs; the pairs follow.
    pub fn map(n: u64, out: &mut Vec<u8>) {
        head(5, n, out);
    }
}

// --------------------------------------------------------------- passkey

/// How the key set holds the backup admin's credential.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct PasskeyFile {
    /// The WebAuthn relying party id: rauthy's `RP_ID`, the public host.
    rp_id: String,
    /// The origin the assertion is signed for: rauthy's `RP_ORIGIN`.
    origin: String,
    /// The login name the credential belongs to.
    email: String,
    /// The credential id, url-safe base64.
    credential_id: String,
    /// The private key, PKCS#8, url-safe base64.
    private_key: String,
}

/// One resident ES256 credential: a software authenticator, as the key set
/// custodies it.
pub struct Passkey {
    rp_id: String,
    origin: String,
    email: String,
    credential_id: Vec<u8>,
    pkcs8: Vec<u8>,
    key: EcdsaKeyPair,
    rng: SystemRandom,
}

impl std::fmt::Debug for Passkey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Passkey")
            .field("rp_id", &self.rp_id)
            .field("origin", &self.origin)
            .field("email", &self.email)
            .field("credential_id", &b64u(&self.credential_id))
            .finish_non_exhaustive()
    }
}

/// rauthy's `RP_ID` and `RP_ORIGIN` for this deployment, derived from the
/// public URL exactly as `rauthy_env` renders them, so the origin a
/// `clientDataJSON` carries is the origin rauthy was configured with.
///
/// # Errors
///
/// [`Error::Config`] when the public URL names no host.
pub fn relying_party(config: &Config) -> Result<(String, String)> {
    let authority = config.public_url.authority();
    let host = authority
        .rsplit_once(':')
        .filter(|(_, port)| port.chars().all(|c| c.is_ascii_digit()))
        .map_or(authority, |(host, _)| host);
    if host.is_empty() {
        return Err(Error::Config("the public URL names no host".to_owned()));
    }
    let origin = if authority.contains(':') {
        config.public_url.origin()
    } else {
        format!(
            "{}://{authority}:{}",
            config.public_url.scheme(),
            if config.public_url.is_https() {
                443
            } else {
                80
            }
        )
    };
    Ok((host.to_owned(), origin))
}

impl Passkey {
    /// Mint a credential for `config`'s relying party.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the system refuses entropy or the key cannot be
    /// generated.
    pub fn generate(config: &Config) -> Result<Self> {
        let (rp_id, origin) = relying_party(config)?;
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng)
            .map_err(|err| Error::Io(format!("a backup passkey cannot be generated: {err}")))?;
        let mut credential_id = vec![0u8; 32];
        rng.fill(&mut credential_id)
            .map_err(|err| Error::Io(format!("the system refused entropy: {err}")))?;
        Self::assemble(
            rp_id,
            origin,
            BACKUP_ADMIN_EMAIL.to_owned(),
            credential_id,
            pkcs8.as_ref().to_vec(),
        )
    }

    fn assemble(
        rp_id: String,
        origin: String,
        email: String,
        credential_id: Vec<u8>,
        pkcs8: Vec<u8>,
    ) -> Result<Self> {
        let rng = SystemRandom::new();
        let key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &pkcs8, &rng)
            .map_err(|err| Error::Config(format!("the backup passkey does not parse: {err}")))?;
        Ok(Self {
            rp_id,
            origin,
            email,
            credential_id,
            pkcs8,
            key,
            rng,
        })
    }

    /// The document the key set holds.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when it cannot be serialised.
    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string_pretty(&PasskeyFile {
            rp_id: self.rp_id.clone(),
            origin: self.origin.clone(),
            email: self.email.clone(),
            credential_id: b64u(&self.credential_id),
            private_key: b64u(&self.pkcs8),
        })
        .map_err(|err| Error::Io(format!("the backup passkey cannot be serialised: {err}")))
    }

    /// Read one back.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when the document is not a passkey this crate
    /// wrote.
    pub fn from_json(text: &str) -> Result<Self> {
        let file: PasskeyFile = serde_json::from_str(text).map_err(|err| {
            Error::Config(format!(
                "the backup passkey is not a passkey document: {err}"
            ))
        })?;
        Self::assemble(
            file.rp_id,
            file.origin,
            file.email,
            b64u_decode(&file.credential_id)?,
            b64u_decode(&file.private_key)?,
        )
    }

    /// The login name this credential belongs to.
    #[must_use]
    pub fn email(&self) -> &str {
        &self.email
    }

    /// The relying party id this credential was minted for.
    #[must_use]
    pub fn rp_id(&self) -> &str {
        &self.rp_id
    }

    /// The origin its assertions are signed for.
    #[must_use]
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// The COSE_Key an authenticator reports for ES256 over P-256.
    fn cose_key(&self) -> Vec<u8> {
        // 0x04 || X || Y, as ring reports an uncompressed point.
        let point = self.key.public_key().as_ref();
        let (x, y) = point
            .get(1..33)
            .zip(point.get(33..65))
            .unwrap_or((&[], &[]));
        let mut out = Vec::with_capacity(80);
        cbor::map(5, &mut out);
        cbor::int(1, &mut out); // kty
        cbor::int(2, &mut out); // EC2
        cbor::int(3, &mut out); // alg
        cbor::int(-7, &mut out); // ES256
        cbor::int(-1, &mut out); // crv
        cbor::int(1, &mut out); // P-256
        cbor::int(-2, &mut out);
        cbor::bytes(x, &mut out);
        cbor::int(-3, &mut out);
        cbor::bytes(y, &mut out);
        out
    }

    /// `rpIdHash || flags || signCount || [attestedCredentialData]`.
    ///
    /// The signature counter is zero and stays zero: WebAuthn 6.1.1 reads
    /// zero as "counters are not supported", while a counter that resets
    /// between processes reads as a cloned credential and is refused
    /// (037 D-3).
    fn authenticator_data(&self, flags: u8, with_credential: bool) -> Vec<u8> {
        let mut out = Vec::with_capacity(128);
        out.extend_from_slice(digest(&SHA256, self.rp_id.as_bytes()).as_ref());
        out.push(flags);
        out.extend_from_slice(&0u32.to_be_bytes());
        if with_credential {
            out.extend_from_slice(&[0u8; 16]); // the AAGUID of a software key
            out.extend_from_slice(&(self.credential_id.len() as u16).to_be_bytes());
            out.extend_from_slice(&self.credential_id);
            out.extend_from_slice(&self.cose_key());
        }
        out
    }

    fn client_data(&self, kind: &str, challenge: &str) -> Vec<u8> {
        json!({
            "type": kind,
            "challenge": challenge,
            "origin": self.origin,
            "crossOrigin": false,
        })
        .to_string()
        .into_bytes()
    }

    /// The credential a browser would hand back from `navigator
    /// .credentials.create`: user presence, user verification, attested
    /// credential data, and a `none` attestation.
    #[must_use]
    pub fn registration(&self, challenge: &str) -> Value {
        let client_data = self.client_data("webauthn.create", challenge);
        let auth_data = self.authenticator_data(0x01 | 0x04 | 0x40, true);
        let mut attestation = Vec::with_capacity(auth_data.len() + 32);
        cbor::map(3, &mut attestation);
        cbor::text("fmt", &mut attestation);
        cbor::text("none", &mut attestation);
        cbor::text("attStmt", &mut attestation);
        cbor::map(0, &mut attestation);
        cbor::text("authData", &mut attestation);
        cbor::bytes(&auth_data, &mut attestation);
        json!({
            "id": b64u(&self.credential_id),
            "rawId": b64u(&self.credential_id),
            "type": "public-key",
            "extensions": {},
            "response": {
                "attestationObject": b64u(&attestation),
                "clientDataJSON": b64u(&client_data),
            },
        })
    }

    /// The credential `navigator.credentials.get` would hand back: an
    /// ES256 signature over `authenticatorData || SHA-256(clientDataJSON)`.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the key refuses to sign.
    pub fn assertion(&self, challenge: &str) -> Result<Value> {
        let client_data = self.client_data("webauthn.get", challenge);
        let auth_data = self.authenticator_data(0x01 | 0x04, false);
        let mut signed = auth_data.clone();
        signed.extend_from_slice(digest(&SHA256, &client_data).as_ref());
        let signature = self
            .key
            .sign(&self.rng, &signed)
            .map_err(|err| Error::Io(format!("the backup passkey cannot sign: {err}")))?;
        Ok(json!({
            "id": b64u(&self.credential_id),
            "rawId": b64u(&self.credential_id),
            "type": "public-key",
            "extensions": {},
            "response": {
                "authenticatorData": b64u(&auth_data),
                "clientDataJSON": b64u(&client_data),
                "signature": b64u(signature.as_ref()),
                "userHandle": null,
            },
        }))
    }
}

// --------------------------------------------------------------- session

/// rauthy's proof of work: the challenge is
/// `version:difficulty:expiry:salt:hash:` and the answer appends the
/// smallest counter whose SHA-256 opens with `difficulty` zero bits.
///
/// # Errors
///
/// [`Error::Upstream`] when the challenge states no difficulty.
pub fn solve_pow(challenge: &str) -> Result<String> {
    let difficulty: u32 = challenge
        .split(':')
        .nth(1)
        .and_then(|field| field.parse().ok())
        .ok_or_else(|| {
            Error::Upstream(format!(
                "rauthy's proof of work {challenge:?} states no difficulty"
            ))
        })?;
    for counter in 0u64.. {
        let attempt = format!("{challenge}{counter}");
        if leading_zero_bits(digest(&SHA256, attempt.as_bytes()).as_ref()) >= difficulty {
            return Ok(attempt);
        }
    }
    Err(Error::Upstream(
        "rauthy's proof of work exhausted the counter space".to_owned(),
    ))
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

/// An MFA-satisfied admin session on rauthy, held as the cookies and CSRF
/// token a browser would hold.
pub struct AdminSession {
    base: String,
    client: reqwest::Client,
    cookies: BTreeMap<String, String>,
    csrf: Option<String>,
}

impl std::fmt::Debug for AdminSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdminSession")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

impl AdminSession {
    /// A session holder against rauthy at `base`, with no session yet.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when the HTTP client cannot be built.
    pub fn new(base: &str) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(CALL_TIMEOUT)
            .user_agent(USER_AGENT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|err| Error::Config(format!("the loopback client cannot be built: {err}")))?;
        Ok(Self {
            base: base.trim_end_matches('/').to_owned(),
            client,
            cookies: BTreeMap::new(),
            csrf: None,
        })
    }

    /// The loopback base this session talks to.
    #[must_use]
    pub fn base(&self) -> &str {
        &self.base
    }

    fn absorb(&mut self, response: &reqwest::Response) {
        for value in response.headers().get_all(reqwest::header::SET_COOKIE) {
            let Ok(text) = value.to_str() else { continue };
            let Some((pair, _)) = text.split_once(';').or(Some((text, ""))) else {
                continue;
            };
            if let Some((name, value)) = pair.split_once('=') {
                self.cookies
                    .insert(name.trim().to_owned(), value.trim().to_owned());
            }
        }
    }

    fn cookie_header(&self) -> String {
        self.cookies
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// One call, carrying whatever cookies and CSRF token the session
    /// holds, absorbing whatever it is given back.
    ///
    /// # Errors
    ///
    /// [`Error::Upstream`] when rauthy is unreachable.
    pub async fn call(
        &mut self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<(reqwest::StatusCode, String)> {
        let url = format!("{}{path}", self.base);
        let mut request = self.client.request(method, &url);
        if !self.cookies.is_empty() {
            request = request.header(reqwest::header::COOKIE, self.cookie_header());
        }
        if let Some(csrf) = &self.csrf {
            request = request.header("x-csrf-token", csrf);
        }
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request
            .send()
            .await
            .map_err(|err| Error::Upstream(format!("rauthy is unreachable at {url}: {err}")))?;
        self.absorb(&response);
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        Ok((status, text))
    }

    /// One call whose answer is bytes rather than text: a snapshot.
    ///
    /// # Errors
    ///
    /// [`Error::Upstream`] when rauthy is unreachable.
    pub async fn call_bytes(
        &mut self,
        method: reqwest::Method,
        path: &str,
    ) -> Result<(reqwest::StatusCode, Vec<u8>)> {
        let url = format!("{}{path}", self.base);
        let mut request = self.client.request(method, &url);
        if !self.cookies.is_empty() {
            request = request.header(reqwest::header::COOKIE, self.cookie_header());
        }
        if let Some(csrf) = &self.csrf {
            request = request.header("x-csrf-token", csrf);
        }
        let response = request
            .send()
            .await
            .map_err(|err| Error::Upstream(format!("rauthy is unreachable at {url}: {err}")))?;
        self.absorb(&response);
        let status = response.status();
        let bytes = response.bytes().await.map_err(|err| {
            Error::Upstream(format!("rauthy's answer at {url} cannot be read: {err}"))
        })?;
        Ok((status, bytes.to_vec()))
    }

    /// Log the passkey-only backup admin in and satisfy rauthy's MFA
    /// requirement, leaving the session ready for the admin routes (B-1).
    ///
    /// # Errors
    ///
    /// [`Error::Unauthorized`] when rauthy refuses the login or the
    /// assertion; [`Error::Upstream`] for anything else it answers.
    pub async fn login(&mut self, passkey: &Passkey) -> Result<()> {
        let (status, body) = self
            .call(reqwest::Method::POST, "/auth/v1/oidc/session", None)
            .await?;
        if !status.is_success() {
            return Err(Error::Upstream(format!(
                "rauthy answered {status} to the anonymous session: {}",
                clip(&body)
            )));
        }
        self.csrf = field(&body, "csrf_token");

        let (status, challenge) = self
            .call(reqwest::Method::POST, "/auth/v1/pow", None)
            .await?;
        if !status.is_success() {
            return Err(Error::Upstream(format!(
                "rauthy answered {status} to the proof of work: {}",
                clip(&challenge)
            )));
        }
        let pow = solve_pow(challenge.trim())?;

        // A passkey-only account logs in with no password field at all;
        // rauthy answers with the code the assertion is bound to.
        let verifier = b64u(digest(&SHA256, b"rahi-backup-verifier").as_ref());
        let code_challenge = b64u(digest(&SHA256, verifier.as_bytes()).as_ref());
        let login = json!({
            "email": passkey.email(),
            "pow": pow,
            "client_id": RAUTHY_CLIENT_ID,
            "redirect_uri": format!("{}{RAUTHY_CALLBACK_PATH}", passkey.origin()),
            "scopes": ["openid"],
            "code_challenge": code_challenge,
            "code_challenge_method": "S256",
        });
        let (status, body) = self
            .call(
                reqwest::Method::POST,
                "/auth/v1/oidc/authorize",
                Some(&login),
            )
            .await?;
        if !status.is_success() {
            return Err(refusal(
                status,
                &body,
                &format!(
                    "rauthy refused the backup admin {} at /auth/v1/oidc/authorize",
                    passkey.email()
                ),
            ));
        }
        let code = field(&body, "code").ok_or_else(|| {
            Error::Upstream(format!(
                "rauthy accepted the backup admin but asked for no assertion: {}",
                clip(&body)
            ))
        })?;

        let (status, body) = self
            .call(
                reqwest::Method::POST,
                "/auth/v1/users/webauthn_start",
                Some(&json!({ "purpose": { "Login": code } })),
            )
            .await?;
        if !status.is_success() {
            return Err(refusal(
                status,
                &body,
                "rauthy refused to start the backup admin's assertion",
            ));
        }
        let document: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
        let assertion_challenge = document
            .pointer("/rcr/publicKey/challenge")
            .or_else(|| document.pointer("/publicKey/challenge"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                Error::Upstream(format!(
                    "rauthy's assertion request carries no challenge: {}",
                    clip(&body)
                ))
            })?;
        let login_code = document
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or(&code)
            .to_owned();
        let data = passkey.assertion(assertion_challenge)?;

        let (status, body) = self
            .call(
                reqwest::Method::POST,
                "/auth/v1/users/webauthn_finish",
                Some(&json!({ "code": login_code, "data": data })),
            )
            .await?;
        if !status.is_success() {
            return Err(refusal(
                status,
                &body,
                &format!(
                    "rauthy refused the backup admin's passkey assertion. If this key set \
                     was replaced while rauthy's store survived, rauthy holds a credential \
                     this set did not mint and cannot be given another one without an MFA \
                     this process cannot satisfy: restore the key set that belongs to it, \
                     or delete {} in rauthy's admin UI and start the cell again",
                    passkey.email()
                ),
            ));
        }
        Ok(())
    }
}

/// rauthy reports a wrong `redirect_uri` and a missing `code_challenge` as
/// "Invalid user credentials" too, and only its own log says which
/// (037 D-3). Say so, so that a field failure is debuggable from the
/// message rather than only from rauthy's stderr.
fn refusal(status: reqwest::StatusCode, body: &str, what: &str) -> Error {
    let detail = format!("{what} ({status}): {}", clip(body));
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Error::Unauthorized(format!(
            "{detail}. rauthy reports a rejected redirect_uri or an unregistered \
             credential as an invalid credential too; its own log says which"
        ));
    }
    Error::Upstream(detail)
}

fn clip(body: &str) -> String {
    body.chars().take(200).collect()
}

fn field(body: &str, name: &str) -> Option<String> {
    serde_json::from_str::<Value>(body)
        .ok()?
        .get(name)?
        .as_str()
        .map(str::to_owned)
}

// ---------------------------------------------------------- provisioning

/// What `ensure_backup_admin` found or did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provisioned {
    /// The account existed as a passkey-only admin; nothing was done.
    AlreadyPresent,
    /// The account was created, the custodied passkey registered, and the
    /// account converted to passkey only.
    Created,
}

/// rauthy's admin API with the API key first boot minted: the credential
/// that can create a user but cannot take a backup (037 D-3).
struct AdminApiKey<'a> {
    base: &'a str,
    token: &'a str,
    client: reqwest::Client,
}

impl AdminApiKey<'_> {
    async fn call_with_headers(
        &self,
        method: reqwest::Method,
        path: &str,
    ) -> Result<(reqwest::StatusCode, reqwest::header::HeaderMap, String)> {
        let url = format!("{}{path}", self.base);
        let response = self
            .client
            .request(method, &url)
            .header(
                reqwest::header::AUTHORIZATION,
                format!("{} {}", rahi_idp::API_KEY_SCHEME, self.token),
            )
            .send()
            .await
            .map_err(|err| Error::Upstream(format!("rauthy is unreachable at {url}: {err}")))?;
        let status = response.status();
        let headers = response.headers().clone();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(Error::Unauthorized(format!(
                "rauthy refused the admin token at {url} ({status})"
            )));
        }
        let text = response.text().await.unwrap_or_default();
        Ok((status, headers, text))
    }

    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<(reqwest::StatusCode, String)> {
        let url = format!("{}{path}", self.base);
        let mut request = self.client.request(method, &url).header(
            reqwest::header::AUTHORIZATION,
            format!("{} {}", rahi_idp::API_KEY_SCHEME, self.token),
        );
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request
            .send()
            .await
            .map_err(|err| Error::Upstream(format!("rauthy is unreachable at {url}: {err}")))?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(Error::Unauthorized(format!(
                "rauthy refused the admin token at {url} ({status})"
            )));
        }
        Ok((status, text))
    }
}

/// Make sure rauthy holds the dedicated backup admin this key set's
/// passkey belongs to, and that it is passkey only (B-1).
///
/// Idempotent: an account that is already `passkey` is left exactly as it
/// is, which is what every start after the first finds, and what a restored
/// cell finds, since rauthy's snapshot carries the registration and the
/// archive carries the private key.
///
/// # Errors
///
/// [`Error::Unauthorized`] when rauthy refuses the admin token;
/// [`Error::Upstream`] when any step of the provisioning does not answer as
/// rauthy's own source says it does.
pub async fn ensure_backup_admin(
    base: &str,
    admin_token: &str,
    passkey: &Passkey,
) -> Result<Provisioned> {
    let base = base.trim_end_matches('/');
    let api = AdminApiKey {
        base,
        token: admin_token,
        client: reqwest::Client::builder()
            .timeout(CALL_TIMEOUT)
            .user_agent(USER_AGENT)
            .build()
            .map_err(|err| Error::Config(format!("the loopback client cannot be built: {err}")))?,
    };

    let id = find_user(&api, passkey.email()).await?;
    if let Some(id) = &id {
        let user = read_user(&api, id).await?;
        let account_type = user.get("account_type").and_then(Value::as_str);
        if account_type == Some("passkey") {
            // The steady state, and what a restored cell finds: rauthy's
            // snapshot carries the registration and the archive carries the
            // private key, so the pair is already whole.
            return Ok(Provisioned::AlreadyPresent);
        }
        if user
            .get("webauthn_user_id")
            .and_then(Value::as_str)
            .is_some()
        {
            // A credential is registered that this key set did not mint, and
            // registering another one needs an MFA this process cannot
            // satisfy. Deleting rauthy's credentials is not this chassis's to
            // do, so say what is there and stop.
            return Err(Error::Conflict(format!(
                "rauthy's {} already holds a passkey this key set did not mint (account \
                 type {:?}). Either restore the key set that belongs to it, or delete the \
                 account in rauthy's admin UI and start the cell again",
                passkey.email(),
                account_type.unwrap_or("unknown"),
            )));
        }
    }

    // rauthy keeps of a user's roles only those that exist (spec 034 D-8),
    // so the role is created first when rauthy lacks it.
    ensure_role(&api, BACKUP_ADMIN_ROLE).await?;

    let id = match id {
        Some(id) => id,
        None => {
            let (status, body) = api
                .call(
                    reqwest::Method::POST,
                    "/auth/v1/users",
                    Some(&json!({
                        "email": passkey.email(),
                        "given_name": "Rahi",
                        "family_name": "Backup",
                        "language": "en",
                        "roles": [BACKUP_ADMIN_ROLE],
                        "groups": null,
                        "user_expires": null,
                    })),
                )
                .await?;
            if !status.is_success() {
                return Err(Error::Upstream(format!(
                    "rauthy answered {status} creating the backup admin: {}",
                    clip(&body)
                )));
            }
            field(&body, "id").ok_or_else(|| {
                Error::Upstream("rauthy created the backup admin without an id".to_owned())
            })?
        }
    };

    // The transient password exists for the length of one registration:
    // `User::convert_to_passkey` refuses any account type but `Password`,
    // and the conversion clears the password and its expiry. It is never
    // written to the volume.
    let transient = transient_password()?;
    let (status, body) = api
        .call(
            reqwest::Method::PUT,
            &format!("/auth/v1/users/{id}"),
            Some(&json!({
                "email": passkey.email(),
                "given_name": "Rahi",
                "family_name": "Backup",
                "language": "en",
                "password": transient,
                "roles": [BACKUP_ADMIN_ROLE],
                "groups": null,
                "enabled": true,
                "email_verified": true,
                "user_expires": null,
                "user_values": null,
            })),
        )
        .await?;
    if !status.is_success() {
        return Err(Error::Upstream(format!(
            "rauthy answered {status} setting up the backup admin: {}",
            clip(&body)
        )));
    }

    register_passkey(base, &id, passkey, &transient).await?;
    Ok(Provisioned::Created)
}

/// The id of the user with this login name, paging through rauthy's
/// listing. rauthy answers `GET /users` with a lighter record that carries
/// no account type, and pages it once the deployment has more users than
/// its server-side-pagination threshold, so the search follows the
/// continuation token rather than trusting one page.
async fn find_user(api: &AdminApiKey<'_>, email: &str) -> Result<Option<String>> {
    let mut path = "/auth/v1/users".to_owned();
    for _ in 0..MAX_USER_PAGES {
        let (status, headers, body) = api.call_with_headers(reqwest::Method::GET, &path).await?;
        if !status.is_success() && status != reqwest::StatusCode::PARTIAL_CONTENT {
            return Err(Error::Upstream(format!(
                "rauthy answered {status} to the user listing: {}",
                clip(&body)
            )));
        }
        let users: Vec<Value> = serde_json::from_str(&body).map_err(|err| {
            Error::Upstream(format!("rauthy's user listing does not parse: {err}"))
        })?;
        if let Some(found) = users
            .iter()
            .find(|u| u.get("email").and_then(Value::as_str) == Some(email))
            .and_then(|u| u.get("id"))
            .and_then(Value::as_str)
        {
            return Ok(Some(found.to_owned()));
        }
        let Some(token) = headers
            .get(CONTINUATION_HEADER)
            .and_then(|v| v.to_str().ok())
            .filter(|t| !t.is_empty())
        else {
            return Ok(None);
        };
        path = format!("/auth/v1/users?continuation_token={token}");
    }
    Err(Error::Upstream(format!(
        "rauthy's user listing did not end within {MAX_USER_PAGES} pages"
    )))
}

/// The full record for one user, which carries the account type and
/// whether a credential is registered. The listing does not.
async fn read_user(api: &AdminApiKey<'_>, id: &str) -> Result<Value> {
    let (status, body) = api
        .call(reqwest::Method::GET, &format!("/auth/v1/users/{id}"), None)
        .await?;
    if !status.is_success() {
        return Err(Error::Upstream(format!(
            "rauthy answered {status} reading the backup admin: {}",
            clip(&body)
        )));
    }
    serde_json::from_str(&body)
        .map_err(|err| Error::Upstream(format!("rauthy's user record does not parse: {err}")))
}

async fn ensure_role(api: &AdminApiKey<'_>, role: &str) -> Result<()> {
    let (status, body) = api
        .call(reqwest::Method::GET, "/auth/v1/roles", None)
        .await?;
    if !status.is_success() {
        return Err(Error::Upstream(format!(
            "rauthy answered {status} to the role listing: {}",
            clip(&body)
        )));
    }
    let roles: Vec<Value> = serde_json::from_str(&body)
        .map_err(|err| Error::Upstream(format!("rauthy's role listing does not parse: {err}")))?;
    if roles
        .iter()
        .any(|r| r.get("name").and_then(Value::as_str) == Some(role))
    {
        return Ok(());
    }
    let (status, body) = api
        .call(
            reqwest::Method::POST,
            "/auth/v1/roles",
            Some(&json!({ "role": role })),
        )
        .await?;
    if !status.is_success() {
        return Err(Error::Upstream(format!(
            "rauthy answered {status} creating the role {role}: {}",
            clip(&body)
        )));
    }
    Ok(())
}

/// Log in with the transient password, buy an MFA modification token with
/// it, register the custodied credential, and convert the account to
/// passkey only in the same session.
async fn register_passkey(base: &str, id: &str, passkey: &Passkey, transient: &str) -> Result<()> {
    let mut session = AdminSession::new(base)?;
    let (status, body) = session
        .call(reqwest::Method::POST, "/auth/v1/oidc/session", None)
        .await?;
    if !status.is_success() {
        return Err(Error::Upstream(format!(
            "rauthy answered {status} to the anonymous session: {}",
            clip(&body)
        )));
    }
    session.csrf = field(&body, "csrf_token");
    let (status, challenge) = session
        .call(reqwest::Method::POST, "/auth/v1/pow", None)
        .await?;
    if !status.is_success() {
        return Err(Error::Upstream(format!(
            "rauthy answered {status} to the proof of work: {}",
            clip(&challenge)
        )));
    }
    let verifier = b64u(digest(&SHA256, b"rahi-backup-provision").as_ref());
    let (status, body) = session
        .call(
            reqwest::Method::POST,
            "/auth/v1/oidc/authorize",
            Some(&json!({
                "email": passkey.email(),
                "password": transient,
                "pow": solve_pow(challenge.trim())?,
                "client_id": RAUTHY_CLIENT_ID,
                "redirect_uri": format!("{}{RAUTHY_CALLBACK_PATH}", passkey.origin()),
                "scopes": ["openid"],
                "code_challenge": b64u(digest(&SHA256, verifier.as_bytes()).as_ref()),
                "code_challenge_method": "S256",
            })),
        )
        .await?;
    if !status.is_success() {
        return Err(refusal(
            status,
            &body,
            "rauthy refused the backup admin's first login",
        ));
    }

    let (status, body) = session
        .call(
            reqwest::Method::POST,
            &format!("/auth/v1/users/{id}/mfa_token"),
            Some(&json!({ "password": transient })),
        )
        .await?;
    if !status.is_success() {
        return Err(refusal(
            status,
            &body,
            "rauthy refused an MFA modification token for the backup admin",
        ));
    }
    let mod_token = field(&body, "id").ok_or_else(|| {
        Error::Upstream("rauthy's MFA modification token carries no id".to_owned())
    })?;

    let (status, body) = session
        .call(
            reqwest::Method::POST,
            &format!("/auth/v1/users/{id}/webauthn/register/start"),
            Some(&json!({
                "passkey_name": PASSKEY_LABEL,
                "mfa_mod_token_id": mod_token,
            })),
        )
        .await?;
    if !status.is_success() {
        return Err(refusal(
            status,
            &body,
            "rauthy refused to start the backup admin's passkey registration",
        ));
    }
    let document: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    let challenge = document
        .pointer("/publicKey/challenge")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            Error::Upstream(format!(
                "rauthy's registration request carries no challenge: {}",
                clip(&body)
            ))
        })?;

    let (status, body) = session
        .call(
            reqwest::Method::POST,
            &format!("/auth/v1/users/{id}/webauthn/register/finish"),
            Some(&json!({
                "passkey_name": PASSKEY_LABEL,
                "mfa_mod_token_id": mod_token,
                "data": passkey.registration(challenge),
            })),
        )
        .await?;
    if !status.is_success() {
        return Err(refusal(
            status,
            &body,
            "rauthy refused the backup admin's passkey registration",
        ));
    }

    let (status, body) = session
        .call(
            reqwest::Method::POST,
            &format!("/auth/v1/users/{id}/self/convert_passkey"),
            None,
        )
        .await?;
    if !status.is_success() {
        return Err(refusal(
            status,
            &body,
            "rauthy refused to drop the backup admin's transient password",
        ));
    }
    Ok(())
}

/// A password that satisfies any reasonable policy and is discarded the
/// moment the account stops being a password account.
fn transient_password() -> Result<String> {
    Ok(format!("{}aA1!", crate::keys::random_secret(24)?))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn config(public_url: &str) -> Config {
        let env = std::collections::BTreeMap::from([("RAHI_PUBLIC_URL", public_url)]);
        Config::from_env(&env).expect("the fixture environment")
    }

    /// The relying party this module signs for is the one first boot
    /// renders into rauthy's environment. Two derivations of one fact stay
    /// one fact only if something checks.
    #[test]
    fn the_relying_party_is_the_one_rauthy_is_configured_with() {
        for url in [
            "https://cell.example.com",
            "http://localhost:8443",
            "https://cell.example.com:8443",
        ] {
            let config = config(url);
            let secrets = crate::rauthy_env::RauthySecrets {
                enc_key_id: "k1".to_owned(),
                enc_key: "a".to_owned(),
                secret_raft: "b".to_owned(),
                secret_api: "c".to_owned(),
                admin_email: "admin@localhost".to_owned(),
                admin_password: "d".to_owned(),
                api_key_name: "rahi".to_owned(),
                api_key_secret: "e".to_owned(),
            };
            let rendered = crate::rauthy_env::values(
                &config,
                &secrets,
                "hello-cell",
                crate::rauthy_env::HqlPorts::from_env(
                    &std::collections::BTreeMap::<&str, &str>::new(),
                )
                .expect("the defaults"),
            )
            .expect("renders");
            let (rp_id, origin) = relying_party(&config).expect("derives");
            assert_eq!(Some(&rp_id), rendered.get("RP_ID"), "{url}");
            assert_eq!(Some(&origin), rendered.get("RP_ORIGIN"), "{url}");
        }
    }

    #[test]
    fn a_passkey_survives_the_key_set_round_trip() {
        let passkey = Passkey::generate(&config("https://cell.example.com")).expect("mints");
        let text = passkey.to_json().expect("serialises");
        let back = Passkey::from_json(&text).expect("reads back");
        assert_eq!(back.rp_id(), passkey.rp_id());
        assert_eq!(back.origin(), passkey.origin());
        assert_eq!(back.email(), BACKUP_ADMIN_EMAIL);
        assert_eq!(back.credential_id, passkey.credential_id);
        // The same credential signs the same challenge with the same key.
        let one = passkey.assertion("abc").expect("signs");
        let two = back.assertion("abc").expect("signs");
        assert_eq!(
            one.pointer("/response/clientDataJSON"),
            two.pointer("/response/clientDataJSON")
        );
        assert_eq!(
            one.pointer("/response/authenticatorData"),
            two.pointer("/response/authenticatorData")
        );
    }

    /// The authenticator data an assertion signs: the relying party hash,
    /// user presence and user verification, and a counter of zero that
    /// stays zero across processes (037 D-3).
    #[test]
    fn the_signature_counter_is_permanently_zero() {
        let passkey = Passkey::generate(&config("https://cell.example.com")).expect("mints");
        for _ in 0..3 {
            let assertion = passkey.assertion("abc").expect("signs");
            let auth_data = b64u_decode(
                assertion
                    .pointer("/response/authenticatorData")
                    .and_then(Value::as_str)
                    .expect("carries authenticator data"),
            )
            .expect("decodes");
            assert_eq!(
                auth_data.get(32),
                Some(&(0x01 | 0x04)),
                "user presence and user verification"
            );
            assert_eq!(auth_data.get(33..37), Some(&[0, 0, 0, 0][..]), "counter");
            assert_eq!(
                auth_data.get(..32),
                Some(digest(&SHA256, b"cell.example.com").as_ref()),
                "the relying party hash"
            );
        }
    }

    /// The registration carries attested credential data and a `none`
    /// attestation, with the COSE key an ES256 credential reports.
    #[test]
    fn the_registration_carries_a_cose_es256_key() {
        let passkey = Passkey::generate(&config("https://cell.example.com")).expect("mints");
        let registration = passkey.registration("abc");
        let attestation = b64u_decode(
            registration
                .pointer("/response/attestationObject")
                .and_then(Value::as_str)
                .expect("carries an attestation object"),
        )
        .expect("decodes");
        // a 3-pair map, "fmt", "none"
        assert_eq!(attestation.first(), Some(&0xa3));
        assert!(
            attestation.windows(4).any(|w| w == b"none"),
            "a none attestation"
        );
        // The authenticator data is in there whole, found by its first
        // field, so the test does not have to re-decode CBOR's length head.
        let rp_hash = digest(&SHA256, b"cell.example.com");
        let start = attestation
            .windows(32)
            .position(|w| w == rp_hash.as_ref())
            .expect("carries the relying party hash");
        assert_eq!(
            attestation.get(start + 32),
            Some(&(0x01 | 0x04 | 0x40)),
            "presence, verification, attested credential data"
        );
        assert_eq!(
            attestation.get(start + 33..start + 37),
            Some(&[0, 0, 0, 0][..]),
            "counter"
        );
        // the AAGUID of a software key, then the credential id
        assert_eq!(
            attestation.get(start + 37..start + 53),
            Some(&[0u8; 16][..])
        );
        assert_eq!(
            attestation.get(start + 53..start + 55),
            Some(&32u16.to_be_bytes()[..]),
            "the credential id is 32 bytes"
        );
        // and the COSE key follows it: a five-pair map, kty EC2, alg ES256
        let cose = start + 55 + 32;
        assert_eq!(
            attestation.get(cose..cose + 6),
            Some(&[0xa5, 0x01, 0x02, 0x03, 0x26, 0x20][..])
        );
    }

    #[test]
    fn a_solved_proof_of_work_opens_with_enough_zero_bits() {
        let answer = solve_pow("1:08:1700000000:salt:hash:").expect("solves");
        assert!(answer.starts_with("1:08:1700000000:salt:hash:"));
        assert!(leading_zero_bits(digest(&SHA256, answer.as_bytes()).as_ref()) >= 8);
        assert!(solve_pow("nonsense").is_err());
    }

    #[test]
    fn cbor_encodes_the_two_shapes_an_authenticator_emits() {
        let mut out = Vec::new();
        cbor::int(-7, &mut out);
        assert_eq!(out, vec![0x26]);
        out.clear();
        cbor::int(1, &mut out);
        assert_eq!(out, vec![0x01]);
        out.clear();
        cbor::bytes(&[1, 2, 3], &mut out);
        assert_eq!(out, vec![0x43, 1, 2, 3]);
        out.clear();
        cbor::text("fmt", &mut out);
        assert_eq!(out, vec![0x63, b'f', b'm', b't']);
        out.clear();
        cbor::map(0, &mut out);
        assert_eq!(out, vec![0xa0]);
    }
}
