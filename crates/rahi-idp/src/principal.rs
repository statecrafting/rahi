//! Turning the IdP's claims into a [`Principal`] (spec 022 B-4).
//!
//! **The subject is the identity, and nothing else is.** rauthy answered the
//! question of who this is; every field below is that answer transcribed, and
//! there is no branch in this file that manufactures an identifier the IdP did
//! not send. enrahitu's first auth system minted its own subject and kept its
//! own account row, and the two of them were places where the app's answer
//! could differ from the IdP's (enrahitu://004 §1). There is no row here and
//! no minting here.
//!
//! Three rules make the transcription safe, and all three are the same rule
//! read three ways: an unverified assertion is not a verified one.
//!
//! - `email_verified` **absent means false**. A claim that was not made is not
//!   a claim that was made affirmatively, and defaulting the other way turns
//!   every IdP that omits the field into an IdP that vouches for every address.
//! - `email` is carried, but [`Principal::verified_email`] is the only reader
//!   that treats it as identifying. An unverified address is display text.
//! - `preferred_username` is **never** an email. It is user-chosen, it is not
//!   unique at every IdP, and it frequently looks like an address; treating it
//!   as one is how an account gets matched to a person who does not own it.
//!
//! Roles come from rauthy's `roles` claim and are re-read on every renewal
//! ([`crate::refresh`]), never carried forward from a previous assertion.

use std::collections::BTreeSet;

use rahi_types::{Email, Error, Principal, Result, Role, Sub, UnixSeconds};
use serde::{Deserialize, Serialize};

/// The claim rauthy carries a principal's roles in.
pub const CLAIM_ROLES: &str = "roles";

/// The claims this chassis reads, from an id token or from userinfo.
///
/// Deliberately partial. rauthy sends more than this and is free to send more
/// still; a field nobody reads is a field nobody has to keep true, and every
/// field here has exactly one reader below.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdpClaims {
    /// The subject: the only identifier.
    #[serde(default)]
    pub sub: Option<String>,
    /// The email address, verified or not.
    #[serde(default)]
    pub email: Option<String>,
    /// Whether the IdP has verified `email`. **Absent means false.**
    #[serde(default)]
    pub email_verified: Option<bool>,
    /// Read so that it can be ignored: it is never an email and never an id.
    #[serde(default)]
    pub preferred_username: Option<String>,
    /// The roles the IdP granted.
    #[serde(default)]
    pub roles: Vec<String>,
}

impl IdpClaims {
    /// The subject, as a [`Sub`].
    ///
    /// # Errors
    ///
    /// [`Error::Unauthorized`] when the claim is absent or empty. A response
    /// with no subject describes nobody, and the alternative to refusing it is
    /// inventing an identifier, which is the thing this module exists to
    /// prevent.
    pub fn subject(&self) -> Result<Sub> {
        match self.sub.as_deref().map(str::trim) {
            Some(sub) if !sub.is_empty() => Ok(Sub::new(sub)),
            _ => Err(Error::Unauthorized(
                "the IdP's answer carries no subject, so it names no principal".to_owned(),
            )),
        }
    }

    /// Whether the IdP affirmatively verified the email. Absent is `false`.
    #[must_use]
    pub fn is_email_verified(&self) -> bool {
        self.email_verified.unwrap_or(false)
    }

    /// The roles, deduplicated and ordered.
    #[must_use]
    pub fn role_set(&self) -> BTreeSet<Role> {
        self.roles
            .iter()
            .map(|role| role.trim())
            .filter(|role| !role.is_empty())
            .map(Role::new)
            .collect()
    }
}

/// Build the principal these claims describe, under `sub`.
///
/// `sub` is passed in rather than read from `claims` because a renewal must
/// answer for the subject the envelope pinned (B-5): a userinfo response that
/// came back describing somebody else is a failure, not a new principal. Use
/// [`pinned`] to make that check, then this to transcribe.
#[must_use]
pub fn principal(sub: Sub, claims: &IdpClaims, issued_at: UnixSeconds) -> Principal {
    let email_verified = claims.is_email_verified();
    Principal {
        sub,
        // The address is carried whatever its verification state; what changes
        // with the flag is which reader will hand it out (`verified_email`).
        // `preferred_username` is not consulted here and never will be.
        email: claims
            .email
            .as_deref()
            .map(str::trim)
            .filter(|email| !email.is_empty())
            .map(Email::new),
        email_verified,
        roles: claims.role_set(),
        issued_at,
    }
}

/// Check that `claims` describe `expected`, the subject the envelope pinned.
///
/// # Errors
///
/// [`Error::Unauthorized`] when the claims carry no subject or carry a
/// different one. A renewal that came back for another principal is the one
/// failure mode that would silently hand a session to the wrong person, so it
/// ends the session rather than adopting the new subject.
pub fn pinned(expected: &Sub, claims: &IdpClaims) -> Result<()> {
    let answered = claims.subject()?;
    if &answered == expected {
        return Ok(());
    }
    Err(Error::Unauthorized(format!(
        "the IdP answered for subject {:?} and this session is pinned to {:?}",
        answered.as_str(),
        expected.as_str()
    )))
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    fn claims(json: serde_json::Value) -> IdpClaims {
        serde_json::from_value(json).expect("the fixture claims parse")
    }

    /// Spec 022 FR-003.
    #[test]
    fn an_email_with_no_verified_claim_is_not_a_verified_email() {
        let claims = claims(serde_json::json!({
            "sub": "s-1",
            "email": "someone@example.com",
        }));
        assert!(!claims.is_email_verified());

        let principal = principal(Sub::new("s-1"), &claims, UnixSeconds::new(10));
        assert!(!principal.email_verified);
        assert_eq!(principal.verified_email(), None);
        assert_eq!(
            principal.email_unverified().map(Email::as_str),
            Some("someone@example.com")
        );
    }

    #[test]
    fn a_verified_email_is_handed_out() {
        let claims = claims(serde_json::json!({
            "sub": "s-1",
            "email": "someone@example.com",
            "email_verified": true,
        }));
        let principal = principal(Sub::new("s-1"), &claims, UnixSeconds::new(10));
        assert!(principal.email_verified);
        assert_eq!(
            principal.verified_email().map(Email::as_str),
            Some("someone@example.com")
        );
    }

    #[test]
    fn email_verified_false_is_the_same_as_absent() {
        let claims = claims(serde_json::json!({
            "sub": "s-1",
            "email": "someone@example.com",
            "email_verified": false,
        }));
        let principal = principal(Sub::new("s-1"), &claims, UnixSeconds::new(10));
        assert_eq!(principal.verified_email(), None);
    }

    #[test]
    fn a_preferred_username_never_becomes_an_email() {
        let claims = claims(serde_json::json!({
            "sub": "s-1",
            "preferred_username": "someone@example.com",
            "email_verified": true,
        }));
        let principal = principal(Sub::new("s-1"), &claims, UnixSeconds::new(10));
        assert_eq!(principal.email, None);
        assert_eq!(principal.verified_email(), None);
    }

    #[test]
    fn the_subject_is_the_only_identifier() {
        let claims = claims(serde_json::json!({
            "email": "someone@example.com",
            "preferred_username": "someone",
        }));
        let err = claims.subject().expect_err("no subject, no principal");
        assert_eq!(err.kind(), "unauthorized");
    }

    #[test]
    fn roles_are_taken_from_the_roles_claim() {
        let claims = claims(serde_json::json!({
            "sub": "s-1",
            "roles": ["admin", "reader", "admin", "  "],
        }));
        let principal = principal(Sub::new("s-1"), &claims, UnixSeconds::new(10));
        assert!(principal.has_role(&Role::new("admin")));
        assert!(principal.has_role(&Role::new("reader")));
        assert_eq!(
            principal.roles.len(),
            2,
            "duplicates and blanks are dropped"
        );
    }

    #[test]
    fn a_renewal_that_answers_for_another_subject_is_refused() {
        let claims = claims(serde_json::json!({ "sub": "someone-else" }));
        let err = pinned(&Sub::new("s-1"), &claims).expect_err("the pin holds");
        assert_eq!(err.kind(), "unauthorized");
        assert!(err.message().contains("someone-else"), "{err}");

        pinned(&Sub::new("someone-else"), &claims).expect("the same subject passes");
    }
}
