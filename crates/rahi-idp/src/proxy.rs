//! The one route into rauthy (spec 021 B-2).
//!
//! `/auth/*` is forwarded to the loopback listener raw: the method, the path,
//! the query, and every header that is not hop-by-hop, with the body streamed
//! in both directions and the upstream's status and headers returned
//! unchanged. It is raw on purpose. Filtering it would mean re-implementing
//! rauthy's surface here, and rauthy already authenticates and rate-limits its
//! own endpoints; a second opinion about a question the IdP has answered is a
//! place for the two answers to differ (constitution VII).
//!
//! Two headers are added rather than forwarded: `X-Forwarded-Proto` and
//! `X-Forwarded-Host`, both from the cell's public URL, so that rauthy builds
//! redirects that point at the origin the browser actually used instead of at
//! loopback.
//!
//! The client follows no redirects and decompresses nothing: a 302 and a
//! gzipped body are answers to be relayed, not answers to be acted on.

use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use rahi_types::{Error, Result};

use crate::config::{AUTH_PREFIX, IdpConfig};

/// Headers that belong to one hop and are never forwarded (RFC 9110 7.6.1),
/// plus `host`, which the client sets for the upstream it is dialling.
pub const HOP_BY_HOP: [&str; 9] = [
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "host",
];

/// The scheme the browser reached the cell over.
pub const X_FORWARDED_PROTO: HeaderName = HeaderName::from_static("x-forwarded-proto");
/// The host the browser reached the cell over.
pub const X_FORWARDED_HOST: HeaderName = HeaderName::from_static("x-forwarded-host");

/// The forwarder: one HTTP client, the loopback base, and the two forwarded
/// values.
#[derive(Clone, Debug)]
pub struct Proxy {
    client: reqwest::Client,
    loopback_base: String,
    proto: HeaderValue,
    host: HeaderValue,
}

impl Proxy {
    /// Build the forwarder for `config`.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when the public URL's authority is not a legal
    /// header value, or when the HTTP client cannot be built.
    pub fn new(config: &IdpConfig) -> Result<Self> {
        let host = HeaderValue::from_str(authority_of(&config.issuer)).map_err(|err| {
            Error::Config(format!("the public URL is not a legal header value: {err}"))
        })?;
        let proto = HeaderValue::from_static(config.forwarded_proto());
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|err| Error::Config(format!("the loopback client cannot be built: {err}")))?;
        Ok(Self {
            client,
            loopback_base: config.loopback_base.clone(),
            proto,
            host,
        })
    }

    /// The upstream URL for `uri`: the loopback base plus the original path
    /// and query, unchanged.
    #[must_use]
    pub fn upstream_url(&self, uri: &Uri) -> String {
        let path_and_query = uri
            .path_and_query()
            .map_or_else(|| uri.path().to_owned(), ToString::to_string);
        format!("{}{path_and_query}", self.loopback_base)
    }
}

/// The `/auth/*` subtree, mounted on the app's own router (spec 020 B-1).
///
/// The edge exempts this prefix from its CSRF check (spec 020 B-4): rauthy
/// carries its own, and a chassis token on rauthy's endpoints would be that
/// second opinion again.
pub fn proxy_router(proxy: Proxy) -> Router {
    Router::new()
        .route(AUTH_PREFIX, any(forward))
        .route(&format!("{AUTH_PREFIX}/{{*rest}}"), any(forward))
        .with_state(proxy)
}

/// Forward one request and relay one answer.
async fn forward(State(proxy): State<Proxy>, request: Request) -> Response {
    let url = proxy.upstream_url(request.uri());
    let (parts, body) = request.into_parts();

    let mut upstream = proxy
        .client
        .request(parts.method, &url)
        .headers(forwarded_headers(&parts.headers));
    upstream = upstream.header(X_FORWARDED_PROTO, proxy.proto.clone());
    upstream = upstream.header(X_FORWARDED_HOST, proxy.host.clone());
    let upstream = upstream.body(reqwest::Body::wrap_stream(body.into_data_stream()));

    match upstream.send().await {
        Ok(response) => relay(response),
        Err(err) => upstream_failure(&Error::Upstream(format!(
            "rauthy did not answer at {url}: {err}"
        ))),
    }
}

/// The upstream answer, status and headers unchanged, body still streaming.
fn relay(response: reqwest::Response) -> Response {
    let status = response.status();
    let headers = forwarded_headers(response.headers());
    let mut relayed = Response::new(Body::from_stream(response.bytes_stream()));
    *relayed.status_mut() = status;
    *relayed.headers_mut() = headers;
    relayed
}

/// Every header but the hop-by-hop ones.
///
/// The request direction also drops `content-length`: the body is re-framed
/// by the client as it streams, and a length that describes the old framing
/// would describe the new one wrongly.
fn forwarded_headers(headers: &HeaderMap) -> HeaderMap {
    let mut out = HeaderMap::with_capacity(headers.len());
    for (name, value) in headers {
        if HOP_BY_HOP.contains(&name.as_str()) || name == header::CONTENT_LENGTH {
            continue;
        }
        out.append(name.clone(), value.clone());
    }
    out
}

/// The authority of a URL that has already been validated as one.
fn authority_of(url: &str) -> &str {
    let rest = url.split("://").nth(1).unwrap_or(url);
    rest.split('/').next().unwrap_or(rest)
}

/// The answer when rauthy cannot be reached at all.
///
/// One condition, one status: an unreachable co-deployed dependency is
/// `Error::Upstream`, which spec 020 D-2 maps to 502. The envelope is the
/// shape spec 020 emits so a client parses one thing, and this is the only
/// status this crate names: it is not a second table (spec 021 D-6).
fn upstream_failure(error: &Error) -> Response {
    let body = serde_json::json!({ "error": error.kind(), "message": error.message() });
    (
        StatusCode::BAD_GATEWAY,
        [(header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response()
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::config::IdpConfig;

    fn proxy(public_url: &str) -> Proxy {
        let env = BTreeMap::from([("RAHI_PUBLIC_URL", public_url)]);
        let config = rahi_types::Config::from_env(&env).expect("the fixture environment");
        Proxy::new(&IdpConfig::derive(&config, "hello-cell").expect("derives")).expect("builds")
    }

    #[test]
    fn the_path_and_query_reach_the_upstream_unchanged() {
        let proxy = proxy("https://cell.example.com");
        let uri: Uri = "/auth/v1/authorize?client_id=hello-cell&state=abc%20def"
            .parse()
            .expect("a uri");
        assert_eq!(
            proxy.upstream_url(&uri),
            "http://127.0.0.1:8080/auth/v1/authorize?client_id=hello-cell&state=abc%20def"
        );
    }

    #[test]
    fn hop_by_hop_headers_do_not_travel() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("cell.example.com"));
        headers.insert(header::CONNECTION, HeaderValue::from_static("keep-alive"));
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from_static("12"));
        headers.insert(header::COOKIE, HeaderValue::from_static("session=abc"));
        let out = forwarded_headers(&headers);
        assert!(out.get(header::HOST).is_none());
        assert!(out.get(header::CONNECTION).is_none());
        assert!(out.get(header::CONTENT_LENGTH).is_none());
        assert_eq!(
            out.get(header::COOKIE).and_then(|v| v.to_str().ok()),
            Some("session=abc")
        );
    }

    #[test]
    fn the_forwarded_host_is_the_public_authority() {
        assert_eq!(
            authority_of("https://cell.example.com/auth/v1"),
            "cell.example.com"
        );
        assert_eq!(
            authority_of("http://localhost:8080/auth/v1"),
            "localhost:8080"
        );
    }
}
