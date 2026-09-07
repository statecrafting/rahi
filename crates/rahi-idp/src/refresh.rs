//! Renewal as a round-trip to rauthy (spec 022 B-5).
//!
//! This is the module that makes the rest of the design true. Because the
//! assertion is short-lived and because renewing it means asking rauthy again,
//! **a role removed at the IdP takes effect here within one access lifetime**
//! without anybody telling the app. There is no local refresh table, no local
//! account row, and no path by which the app decides that a session is still
//! good: the app forwards the refresh token, and rauthy's answer is the
//! session's fate.
//!
//! Three things are re-read on every renewal and none of them is carried
//! forward from the previous assertion: the roles, `email_verified`, and the
//! email itself. Carrying any of them forward would be the app holding an
//! opinion about a person that the IdP has since changed.
//!
//! Two things are *not* re-read, and both are pins rather than reads. The
//! subject comes from the envelope, and a userinfo response that answers for
//! another subject ends the session ([`crate::principal::pinned`]): a renewal
//! cannot change who a session belongs to. And the session id comes from the
//! envelope too, so the assertion a renewal writes replaces the one it read.

use rahi_types::{Error, Result};

use crate::envelope::Envelope;
use crate::principal::pinned;
use crate::session::{Session, Sessions};

/// The `grant_type` a renewal presents.
pub const GRANT_REFRESH_TOKEN: &str = "refresh_token";

/// What a successful renewal produced: a new assertion and a new envelope.
#[derive(Clone, Debug)]
pub struct Renewed {
    /// The assertion, already cached under the envelope's session id.
    pub session: Session,
    /// The envelope to re-seal into the cookie, carrying the rotated token.
    pub envelope: Envelope,
}

/// Renew the session `envelope` describes (B-5).
///
/// The sequence is fixed: forward the refresh token, read userinfo with the
/// access token that came back, hold the answer to the envelope's pinned
/// subject, mint the assertion from what userinfo says *now*, and rotate the
/// envelope onto the new refresh token.
///
/// # Errors
///
/// [`Error::Unauthorized`] when rauthy refuses the grant, when userinfo
/// refuses the access token, or when the answer names another subject. Every
/// one of them ends the session: the caller clears both cookies and answers
/// 401. [`Error::Upstream`] when rauthy cannot be reached at all, which is a
/// failure of the cell rather than of the session and does not clear anything.
pub async fn renew(sessions: &Sessions, envelope: &Envelope) -> Result<Renewed> {
    let granted = sessions
        .token(&[
            ("grant_type", GRANT_REFRESH_TOKEN),
            ("refresh_token", envelope.refresh_token.expose()),
        ])
        .await?;

    let claims = sessions.userinfo(&granted.access_token).await?;
    pinned(&envelope.sub, &claims)?;

    let session = sessions
        .mint(
            &envelope.sid,
            envelope.sub.clone(),
            &claims,
            granted.expires_in,
        )
        .await?;

    // rauthy rotates the refresh token on every grant, but the standard does
    // not require it: an authorization server that answers without one has
    // said the presented token is still current, so it is carried through.
    let rotated = granted
        .refresh_token
        .unwrap_or_else(|| envelope.refresh_token.expose().clone());
    let envelope = envelope.rotated(rotated, sessions.now());

    Ok(Renewed { session, envelope })
}

/// Whether `error` means the session is over rather than the cell is unwell.
///
/// A refused grant ends the session; an unreachable rauthy does not, because
/// logging every user out of a cell whose IdP restarted would turn a blip into
/// an outage. The two answer with different statuses and only one of them
/// clears cookies.
#[must_use]
pub const fn ends_the_session(error: &Error) -> bool {
    matches!(error, Error::Unauthorized(_))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_refusal_ends_the_session() {
        assert!(ends_the_session(&Error::Unauthorized("refused".to_owned())));
        assert!(!ends_the_session(&Error::Upstream("no route".to_owned())));
        assert!(!ends_the_session(&Error::Io("disk".to_owned())));
    }
}
