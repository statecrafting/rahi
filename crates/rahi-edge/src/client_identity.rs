//! Who the client is when a proxy stands in front of the cell (spec 024 B-1).
//!
//! `X-Forwarded-For` is evidence, not identity. Anyone can send it, so the
//! only thing that turns it into an identity is the operator saying how many
//! hops in front of this cell are theirs (`RAHI_TRUSTED_PROXY_HOPS`, spec
//! 010). With that number the header can be read *from the right*, which is
//! the only direction that is safe.
//!
//! Reading from the right works because each trusted proxy appends the
//! address of the peer it saw. A client that sends `X-Forwarded-For: 9.9.9.9`
//! to a cell behind one proxy produces `9.9.9.9, <client>` at the edge, and
//! the first address from the right is the client. Every entry the client
//! forged sits to the left of the ones the operator's proxies wrote, and
//! shifts nothing: forging `k` entries makes the list `k` longer, and the
//! `n`-th from the right is still the address the outermost trusted proxy
//! observed. The leftmost entry is the one a naive implementation takes and
//! the one a client can always choose, so nothing here ever reads from the
//! left; it is reached only when the hop count exactly accounts for the list,
//! which is the case where the client sent no header of its own.
//!
//! With zero hops the header is not read at all. A cell with no proxy in
//! front of it has no reason to believe any of it.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::extract::ConnectInfo;
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderName};
use rahi_types::Config;

use crate::middleware::rate_limit::{ClientResolver, UNKNOWN_IDENTITY};

/// The header the trusted hops write the observed peer into.
pub const X_FORWARDED_FOR: HeaderName = HeaderName::from_static("x-forwarded-for");

/// The resolver B-1 fixes: the peer, or an address a trusted hop vouched for.
///
/// A unit type rather than a free function so that the rule has a name a call
/// site can point at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClientIdentity;

impl ClientIdentity {
    /// The address `peer` and `headers` describe under `trusted_proxy_hops`.
    ///
    /// With `0` hops this is `peer` and the header is not read. With `n` hops
    /// it is the `n`-th address from the right of `X-Forwarded-For`, and it
    /// falls back to `peer` when the header is shorter than `n`, when the
    /// entry does not parse, or when the header is absent.
    #[must_use]
    pub fn resolve(headers: &HeaderMap, peer: IpAddr, trusted_proxy_hops: u8) -> IpAddr {
        forwarded(headers, trusted_proxy_hops).unwrap_or(peer)
    }
}

/// The address a trusted hop vouched for, if `trusted_proxy_hops` names one.
///
/// `None` when no hop is trusted, when the header is absent or shorter than
/// the hop count, or when the entry it names is not an address. Every one of
/// those is a case where the caller has to fall back to the peer: a header
/// that does not agree with the declared topology is not evidence of
/// anything.
#[must_use]
pub fn forwarded(headers: &HeaderMap, trusted_proxy_hops: u8) -> Option<IpAddr> {
    let hops = usize::from(trusted_proxy_hops);
    if hops == 0 {
        return None;
    }
    let entries: Vec<&str> = headers
        .get_all(X_FORWARDED_FOR)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .collect();
    let index = entries.len().checked_sub(hops)?;
    entries.get(index).copied().and_then(address)
}

/// One `X-Forwarded-For` entry as an address.
///
/// A bare address is the common form; a proxy that wrote `addr:port` or a
/// bracketed IPv6 literal is read too, because the alternative is silently
/// counting every client behind it under the peer.
#[must_use]
pub fn address(entry: &str) -> Option<IpAddr> {
    let entry = entry.trim();
    if let Ok(ip) = entry.parse::<IpAddr>() {
        return Some(ip);
    }
    if let Ok(addr) = entry.parse::<SocketAddr>() {
        return Some(addr.ip());
    }
    entry
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .and_then(|inner| inner.parse().ok())
}

/// The peer address of the connection, when axum was given one.
///
/// A router served with `into_make_service_with_connect_info` carries a
/// [`ConnectInfo`]; one driven directly by a test or over a unix socket does
/// not.
#[must_use]
pub fn peer_ip(parts: &Parts) -> Option<IpAddr> {
    parts
        .extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip())
}

/// The identity string the rate limiter keys on.
///
/// A trusted hop's answer wins, then the peer, then
/// [`UNKNOWN_IDENTITY`](crate::middleware::rate_limit::UNKNOWN_IDENTITY).
/// The forwarded answer is consulted before the peer rather than after so
/// that a request with no connection info is still counted per client when
/// the operator has declared a proxy.
#[must_use]
pub fn identify(parts: &Parts, trusted_proxy_hops: u8) -> String {
    forwarded(&parts.headers, trusted_proxy_hops)
        .or_else(|| peer_ip(parts))
        .map_or_else(|| UNKNOWN_IDENTITY.to_owned(), |ip| ip.to_string())
}

/// A [`ClientResolver`] over `trusted_proxy_hops`.
#[must_use]
pub fn resolver(trusted_proxy_hops: u8) -> ClientResolver {
    Arc::new(move |parts: &Parts| identify(parts, trusted_proxy_hops))
}

/// The resolver the cell's own configuration describes.
///
/// This is what [`EdgeBuilder::build`](crate::router::EdgeBuilder::build)
/// gives the limiter unless the app injected one of its own.
#[must_use]
pub fn from_config(config: &Config) -> ClientResolver {
    resolver(config.trusted_proxy_hops)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(values: &[&str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for value in values {
            if let Ok(value) = value.parse() {
                headers.append(X_FORWARDED_FOR, value);
            }
        }
        headers
    }

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap_or(IpAddr::from([0, 0, 0, 0]))
    }

    #[test]
    fn no_trusted_hop_reads_no_header() {
        let headers = headers(&["9.9.9.9, 10.0.0.1, 10.0.0.2"]);
        assert_eq!(
            ClientIdentity::resolve(&headers, ip("192.0.2.7"), 0),
            ip("192.0.2.7")
        );
    }

    #[test]
    fn a_header_split_across_two_lines_is_one_list() {
        let headers = headers(&["9.9.9.9, 10.0.0.1", "10.0.0.2"]);
        assert_eq!(
            ClientIdentity::resolve(&headers, ip("192.0.2.7"), 2),
            ip("10.0.0.1")
        );
    }

    #[test]
    fn an_entry_with_a_port_is_still_an_address() {
        let headers = headers(&["203.0.113.5:41234"]);
        assert_eq!(
            ClientIdentity::resolve(&headers, ip("192.0.2.7"), 1),
            ip("203.0.113.5")
        );
    }

    #[test]
    fn a_bracketed_v6_literal_is_still_an_address() {
        let headers = headers(&["[2001:db8::1]"]);
        assert_eq!(
            ClientIdentity::resolve(&headers, ip("192.0.2.7"), 1),
            ip("2001:db8::1")
        );
    }

    #[test]
    fn an_entry_that_is_not_an_address_falls_back_to_the_peer() {
        let headers = headers(&["unknown"]);
        assert_eq!(
            ClientIdentity::resolve(&headers, ip("192.0.2.7"), 1),
            ip("192.0.2.7")
        );
    }

    #[test]
    fn forging_entries_shifts_nothing() {
        let honest = headers(&["198.51.100.4"]);
        let forged = headers(&["9.9.9.9, 9.9.9.8, 198.51.100.4"]);
        assert_eq!(
            ClientIdentity::resolve(&honest, ip("192.0.2.7"), 1),
            ClientIdentity::resolve(&forged, ip("192.0.2.7"), 1),
        );
    }
}
