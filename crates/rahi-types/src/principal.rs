//! The authenticated principal (spec 010 B-5, constitution VII).
//!
//! rauthy's `sub` is the only principal identifier. Nothing here is derived
//! from an email, a username, or a local row; the IdP answered the question
//! and this type carries the answer.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// The IdP's subject: the one identifier of a principal.
///
/// Opaque by construction. The only way in is [`Sub::new`] with the value
/// rauthy issued; there is no conversion from an email, a username, or any
/// other identifier.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Sub(String);

impl Sub {
    /// Wrap the `sub` claim exactly as the IdP issued it.
    #[must_use]
    pub fn new(sub: impl Into<String>) -> Self {
        Self(sub.into())
    }

    /// The claim value.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// An email address as the IdP reported it, verified or not.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Email(String);

impl Email {
    /// Wrap the `email` claim.
    #[must_use]
    pub fn new(email: impl Into<String>) -> Self {
        Self(email.into())
    }

    /// The address.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A role the IdP granted. Re-read on every renewal, never carried forward.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Role(String);

impl Role {
    /// Wrap a role name.
    #[must_use]
    pub fn new(role: impl Into<String>) -> Self {
        Self(role.into())
    }

    /// The role name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Seconds since the Unix epoch, as a claim value.
///
/// This crate never reads a clock; the value is whatever the IdP's `iat`
/// said, or whatever the caller measured.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(transparent)]
#[serde(transparent)]
pub struct UnixSeconds(u64);

impl UnixSeconds {
    /// Wrap a claim value.
    #[must_use]
    pub const fn new(secs: u64) -> Self {
        Self(secs)
    }

    /// The raw seconds.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// An authenticated principal as the IdP described it at issue time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Principal {
    /// The IdP's subject: the only identifier.
    pub sub: Sub,
    /// The email claim, if the IdP sent one. Read it through
    /// [`Principal::verified_email`] or [`Principal::email_unverified`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<Email>,
    /// Whether the IdP has verified `email`. Absent means `false`.
    #[serde(default)]
    pub email_verified: bool,
    /// The roles the IdP granted at issue time.
    #[serde(default)]
    pub roles: BTreeSet<Role>,
    /// When the IdP issued these claims.
    pub issued_at: UnixSeconds,
}

impl Principal {
    /// The email, only when the IdP has verified it.
    #[must_use]
    pub fn verified_email(&self) -> Option<&Email> {
        if self.email_verified {
            self.email.as_ref()
        } else {
            None
        }
    }

    /// The email whether or not it is verified.
    ///
    /// The name is the warning: a caller that uses this for anything but
    /// display has named the risk it is taking.
    #[must_use]
    pub fn email_unverified(&self) -> Option<&Email> {
        self.email.as_ref()
    }

    /// Whether the IdP granted `role`.
    #[must_use]
    pub fn has_role(&self, role: &Role) -> bool {
        self.roles.contains(role)
    }
}
