//! The cell's ledger key (spec 013 B-5).
//!
//! One Ed25519 key signs every record in a cell's chain. It is created once,
//! by the container's first boot (spec 031), and read from disk here. The
//! private half never leaves this module: [`LedgerSigner`] has a hand-written
//! `Debug` that prints no key material and derives no serde impl, so it
//! cannot be logged, ledgered, or backed up by accident.
//!
//! Verification takes the public half only. [`LedgerVerifier`] is what
//! [`crate::verify_chain`] holds and what an offline verifier needs, so a
//! process that only checks a chain never has to touch the private key.

use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::{Signature, Signer as _, SigningKey, Verifier as _, VerifyingKey};
use rahi_types::Error;

/// Where the container keeps the cell's ledger key (spec 031 writes it).
pub const DEFAULT_KEY_PATH: &str = "/data/keys/ledger.key";

/// The number of bytes in an Ed25519 seed.
const SEED_LEN: usize = 32;

/// The number of bytes in an Ed25519 signature.
const SIGNATURE_LEN: usize = 64;

/// The cell's ledger key: the private half.
///
/// Cloneable because the ledger is cloned into every task that appends, and
/// cloning a key already in this process's memory adds no exposure.
#[derive(Clone)]
pub struct LedgerSigner {
    key: SigningKey,
}

impl std::fmt::Debug for LedgerSigner {
    /// Prints the type name and nothing else. The key is the one value in
    /// this crate that must never reach a log line.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LedgerSigner")
    }
}

impl LedgerSigner {
    /// Load the key from `path`.
    ///
    /// The file holds the base64 of a 32-byte Ed25519 seed, the format the
    /// rest of the `attest-ledger` family reads, with surrounding whitespace
    /// ignored. Absence is an error rather than a key generation: the key is
    /// custodied with the backup (constitution XII), so minting one silently
    /// would start a second chain nobody could verify against the first.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the file cannot be read; [`Error::Config`] when its
    /// contents are not a base64 32-byte seed.
    pub fn load(path: &Path) -> Result<Self, Error> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::Io(format!("ledger key {}: {e}", path.display())))?;
        let seed = decode_seed(text.trim())
            .map_err(|e| Error::Config(format!("ledger key {}: {e}", path.display())))?;
        Ok(Self::from_seed(seed))
    }

    /// Load the key from [`DEFAULT_KEY_PATH`].
    ///
    /// # Errors
    ///
    /// As [`LedgerSigner::load`].
    pub fn load_default() -> Result<Self, Error> {
        Self::load(&PathBuf::from(DEFAULT_KEY_PATH))
    }

    /// Wrap a seed the caller already holds.
    #[must_use]
    pub fn from_seed(seed: [u8; SEED_LEN]) -> Self {
        Self {
            key: SigningKey::from_bytes(&seed),
        }
    }

    /// The base64 public key this signer stamps on every record.
    #[must_use]
    pub fn public_key(&self) -> String {
        B64.encode(self.key.verifying_key().to_bytes())
    }

    /// The public half, for verifying what this signer wrote.
    #[must_use]
    pub fn verifier(&self) -> LedgerVerifier {
        LedgerVerifier {
            key: self.key.verifying_key(),
        }
    }

    /// Base64 Ed25519 signature over `bytes`.
    #[must_use]
    pub fn sign(&self, bytes: &[u8]) -> String {
        let signature: Signature = self.key.sign(bytes);
        B64.encode(signature.to_bytes())
    }
}

/// The public half of a cell's ledger key.
///
/// Verification pins the key rather than trusting the one a record carries:
/// an adversary who can write rows can also write their own public key beside
/// a signature they minted, so a chain is only as good as the key the
/// verifier brought with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LedgerVerifier {
    key: VerifyingKey,
}

impl LedgerVerifier {
    /// Build a verifier from a base64 32-byte Ed25519 public key.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when the text is not base64, is the wrong length, or
    /// is not a valid Ed25519 point.
    pub fn from_public_key(public_key: &str) -> Result<Self, Error> {
        let bytes: [u8; SEED_LEN] = B64
            .decode(public_key.trim())
            .map_err(|e| Error::Config(format!("ledger public key base64: {e}")))?
            .try_into()
            .map_err(|v: Vec<u8>| {
                Error::Config(format!("ledger public key is {} bytes, not 32", v.len()))
            })?;
        let key = VerifyingKey::from_bytes(&bytes).map_err(|e| {
            Error::Config(format!("ledger public key is not an Ed25519 point: {e}"))
        })?;
        Ok(Self { key })
    }

    /// The base64 public key.
    #[must_use]
    pub fn public_key(&self) -> String {
        B64.encode(self.key.to_bytes())
    }

    /// Check `signature` over `bytes`.
    ///
    /// # Errors
    ///
    /// [`Error::Integrity`] when the signature is malformed or does not
    /// verify. Both are the same fact to a caller: this key did not write
    /// these bytes.
    pub fn verify(&self, bytes: &[u8], signature: &str) -> Result<(), Error> {
        let raw: [u8; SIGNATURE_LEN] = B64
            .decode(signature.trim())
            .map_err(|e| Error::Integrity(format!("signature base64: {e}")))?
            .try_into()
            .map_err(|v: Vec<u8>| {
                Error::Integrity(format!("signature is {} bytes, not 64", v.len()))
            })?;
        self.key
            .verify(bytes, &Signature::from_bytes(&raw))
            .map_err(|e| Error::Integrity(format!("Ed25519 signature does not verify: {e}")))
    }
}

fn decode_seed(text: &str) -> Result<[u8; SEED_LEN], String> {
    B64.decode(text)
        .map_err(|e| format!("base64: {e}"))?
        .try_into()
        .map_err(|v: Vec<u8>| format!("seed is {} bytes, not {SEED_LEN}", v.len()))
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn a_signature_verifies_under_the_matching_public_half() {
        let signer = LedgerSigner::from_seed([3u8; SEED_LEN]);
        let signature = signer.sign(b"sha256:cafe");
        signer
            .verifier()
            .verify(b"sha256:cafe", &signature)
            .expect("the signer's own verifier accepts it");
        assert_eq!(
            LedgerVerifier::from_public_key(&signer.public_key()).expect("public key round-trips"),
            signer.verifier()
        );
    }

    #[test]
    fn another_key_and_altered_bytes_are_both_refused() {
        let signer = LedgerSigner::from_seed([3u8; SEED_LEN]);
        let other = LedgerSigner::from_seed([4u8; SEED_LEN]);
        let signature = signer.sign(b"sha256:cafe");
        assert!(other.verifier().verify(b"sha256:cafe", &signature).is_err());
        assert!(
            signer
                .verifier()
                .verify(b"sha256:beef", &signature)
                .is_err()
        );
    }

    #[test]
    fn a_malformed_signature_is_an_integrity_error_not_a_panic() {
        let signer = LedgerSigner::from_seed([3u8; SEED_LEN]);
        for bad in ["", "not base64!!", &B64.encode([0u8; 8])] {
            let err = signer
                .verifier()
                .verify(b"sha256:cafe", bad)
                .expect_err("refused");
            assert!(matches!(err, Error::Integrity(_)), "{bad:?}: {err}");
        }
    }

    #[test]
    fn a_missing_key_file_is_io_and_a_malformed_one_is_config() {
        let dir = tempfile::tempdir().expect("temp dir");
        let missing = dir.path().join("absent.key");
        assert!(matches!(LedgerSigner::load(&missing), Err(Error::Io(_))));

        let short = dir.path().join("short.key");
        std::fs::write(&short, B64.encode([1u8; 8])).expect("write");
        assert!(matches!(LedgerSigner::load(&short), Err(Error::Config(_))));

        let good = dir.path().join("ledger.key");
        std::fs::write(&good, format!("{}\n", B64.encode([5u8; SEED_LEN]))).expect("write");
        assert_eq!(
            LedgerSigner::load(&good).expect("loads").public_key(),
            LedgerSigner::from_seed([5u8; SEED_LEN]).public_key(),
        );
    }

    #[test]
    fn the_signer_never_prints_key_material() {
        let signer = LedgerSigner::from_seed([9u8; SEED_LEN]);
        assert_eq!(format!("{signer:?}"), "LedgerSigner");
    }
}
