//! The security headers every response carries (spec 020 B-3).
//!
//! The layer sits directly under observation and over everything else, so the
//! headers reach probe answers, error bodies, static files, and the router's
//! own 404 alike (FR-003). It overwrites rather than fills in: the header set
//! is a floor the app cannot lower, and B-1 fixes the layer order precisely
//! so that it cannot be negotiated per route.

use axum::extract::{Request, State};
use axum::http::{HeaderName, HeaderValue, header};
use axum::middleware::Next;
use axum::response::Response;
use rahi_types::Config;

/// `Content-Security-Policy`: same-origin everything, framed by nobody.
pub const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; frame-ancestors 'none'";
/// `X-Content-Type-Options`: no MIME sniffing.
pub const CONTENT_TYPE_OPTIONS: &str = "nosniff";
/// `Referrer-Policy`: the origin only, and only to the same scheme.
pub const REFERRER_POLICY: &str = "strict-origin-when-cross-origin";
/// `Permissions-Policy`: camera, microphone, and geolocation denied outright.
pub const PERMISSIONS_POLICY: &str = "camera=(), microphone=(), geolocation=()";
/// `Strict-Transport-Security`, sent only when the public URL is `https`.
pub const STRICT_TRANSPORT_SECURITY: &str = "max-age=31536000; includeSubDomains";

/// The `Permissions-Policy` header name, which `http` has no constant for.
pub const PERMISSIONS_POLICY_HEADER: HeaderName = HeaderName::from_static("permissions-policy");

/// The layer's state: whether the cell's public origin is `https`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SecurityHeaders {
    https: bool,
}

impl SecurityHeaders {
    /// The header set for a cell served over `https` or over plain `http`.
    #[must_use]
    pub const fn new(https: bool) -> Self {
        Self { https }
    }

    /// Derive the header set from the one public URL (spec 010 B-7).
    #[must_use]
    pub fn from_config(config: &Config) -> Self {
        Self::new(config.public_url.is_https())
    }

    /// Whether `Strict-Transport-Security` is sent.
    #[must_use]
    pub const fn sends_hsts(self) -> bool {
        self.https
    }
}

/// Set the header floor on every response.
pub async fn apply(
    State(headers): State<SecurityHeaders>,
    request: Request,
    next: Next,
) -> Response {
    let mut response = next.run(request).await;
    let out = response.headers_mut();
    out.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CONTENT_SECURITY_POLICY),
    );
    out.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static(CONTENT_TYPE_OPTIONS),
    );
    out.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static(REFERRER_POLICY),
    );
    out.insert(
        PERMISSIONS_POLICY_HEADER,
        HeaderValue::from_static(PERMISSIONS_POLICY),
    );
    if headers.https {
        out.insert(
            header::STRICT_TRANSPORT_SECURITY,
            HeaderValue::from_static(STRICT_TRANSPORT_SECURITY),
        );
    } else {
        out.remove(header::STRICT_TRANSPORT_SECURITY);
    }
    response
}
