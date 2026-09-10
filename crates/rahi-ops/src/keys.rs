//! Key generation for first boot (spec 031 B-2).
//!
//! Every key a deployment carries is minted here, once, from the operating
//! system's entropy, and written with the names spec 030 fixed in
//! [`KeySet`]. Nothing here reads a key back: generation is a function of
//! entropy and nothing else, and a second first boot generates nothing.

use base64::Engine as _;
use rahi_store::{EncKey, EncKeys, StoreSecrets};
use rahi_types::{Error, Result};

use crate::rauthy_env::RauthySecrets;
use crate::{
    ADMIN_TOKEN_FILE, BACKUP_KEY_FILE, KeySet, LEDGER_KEY_FILE, RAUTHY_SECRETS_FILE,
    SESSION_KEY_FILE, STORE_SECRETS_FILE,
};

/// The name of the admin API key rauthy is bootstrapped with, and the
/// prefix of the token every admin call presents (`name$secret`).
pub const ADMIN_KEY_NAME: &str = "rahi";

/// The id of the one encryption key each store starts with.
pub const ENC_KEY_ID: &str = "k1";

/// rauthy's bootstrap admin email (its own default).
pub const ADMIN_EMAIL: &str = "admin@localhost";

/// How many bytes of entropy back each secret string.
pub const SECRET_BYTES: usize = 48;

/// What first boot printed exactly once (B-2).
#[derive(Clone, PartialEq, Eq)]
pub struct AdminCredentials {
    /// The rauthy admin's email.
    pub email: String,
    /// The rauthy admin's initial password.
    pub password: String,
    /// The API key token, `name$secret`.
    pub api_token: String,
}

impl std::fmt::Debug for AdminCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdminCredentials")
            .field("email", &self.email)
            .finish_non_exhaustive()
    }
}

/// `n` random bytes.
///
/// # Errors
///
/// [`Error::Io`] when the system refuses entropy.
pub fn random_bytes(n: usize) -> Result<Vec<u8>> {
    let mut bytes = vec![0u8; n];
    getrandom::fill(&mut bytes)
        .map_err(|err| Error::Io(format!("the system refused entropy: {err}")))?;
    Ok(bytes)
}

/// A random secret as URL-safe base64 without padding: safe in an
/// environment file, a header, and a shell.
///
/// # Errors
///
/// As [`random_bytes`].
pub fn random_secret(bytes: usize) -> Result<String> {
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random_bytes(bytes)?))
}

/// Mint every key into `keys` and return the credentials to print.
///
/// Writes, in order: the ledger seed, the session key, the store secrets,
/// the backup identity, rauthy's secrets, and the admin token. Every file
/// lands with [`crate::KEY_FILE_MODE`] under a [`crate::KEY_DIR_MODE`]
/// directory.
///
/// # Errors
///
/// [`Error::Io`] when entropy is refused or a file cannot be written.
pub fn generate(keys: &KeySet) -> Result<AdminCredentials> {
    let ledger_seed = base64::engine::general_purpose::STANDARD.encode(random_bytes(32)?);
    keys.write(LEDGER_KEY_FILE, ledger_seed.as_bytes())?;

    keys.write(SESSION_KEY_FILE, &random_bytes(32)?)?;

    let store = StoreSecrets {
        secret_raft: random_secret(SECRET_BYTES)?,
        secret_api: random_secret(SECRET_BYTES)?,
        enc_keys: EncKeys {
            active: ENC_KEY_ID.to_owned(),
            keys: vec![EncKey {
                id: ENC_KEY_ID.to_owned(),
                key: random_bytes(32)?,
            }],
        },
    };
    keys.write(STORE_SECRETS_FILE, to_json(&store)?.as_bytes())?;

    keys.write(
        BACKUP_KEY_FILE,
        crate::generate_backup_identity().as_bytes(),
    )?;

    let rauthy = RauthySecrets {
        enc_key_id: ENC_KEY_ID.to_owned(),
        enc_key: base64::engine::general_purpose::STANDARD.encode(random_bytes(32)?),
        secret_raft: random_secret(SECRET_BYTES)?,
        secret_api: random_secret(SECRET_BYTES)?,
        admin_email: ADMIN_EMAIL.to_owned(),
        admin_password: random_secret(24)?,
        api_key_name: ADMIN_KEY_NAME.to_owned(),
        api_key_secret: random_secret(SECRET_BYTES)?,
    };
    keys.write(RAUTHY_SECRETS_FILE, to_json(&rauthy)?.as_bytes())?;

    let api_token = rauthy.api_token();
    keys.write(ADMIN_TOKEN_FILE, api_token.as_bytes())?;

    Ok(AdminCredentials {
        email: rauthy.admin_email,
        password: rauthy.admin_password,
        api_token,
    })
}

fn to_json<T: serde::Serialize>(value: &T) -> Result<String> {
    serde_json::to_string_pretty(value)
        .map_err(|err| Error::Io(format!("a key document cannot be serialised: {err}")))
}
