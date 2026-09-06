//! spec 010 FR-002: `Config::from_env` for `http` and `https` public URLs and
//! for each override.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;

use rahi_types::config::{
    DEFAULT_DATA_DIR, DEFAULT_HIQLITE_API_ADDR, DEFAULT_HIQLITE_RAFT_ADDR, DEFAULT_RAUTHY_ADDR,
    ENV_DATA_DIR, ENV_HIQLITE_API_ADDR, ENV_HIQLITE_RAFT_ADDR, ENV_OTLP_ENDPOINT, ENV_PUBLIC_URL,
    ENV_RAUTHY_ADDR, ENV_TRUSTED_PROXY_HOPS,
};
use rahi_types::{Config, CookieScheme, Error, PublicUrl};

fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

fn addr(s: &str) -> SocketAddr {
    s.parse().unwrap()
}

#[test]
fn https_public_url_yields_secure_cookies_and_the_defaults() {
    let cfg = Config::from_env(&env(&[(ENV_PUBLIC_URL, "https://cell.example.test")])).unwrap();
    assert_eq!(cfg.public_url.as_str(), "https://cell.example.test");
    assert!(cfg.public_url.is_https());
    assert_eq!(cfg.cookie_scheme, CookieScheme::Secure);
    assert!(cfg.cookie_scheme.is_secure());
    assert_eq!(cfg.data_dir, PathBuf::from(DEFAULT_DATA_DIR));
    assert_eq!(cfg.hiqlite.api_addr, addr(DEFAULT_HIQLITE_API_ADDR));
    assert_eq!(cfg.hiqlite.raft_addr, addr(DEFAULT_HIQLITE_RAFT_ADDR));
    assert_eq!(cfg.rauthy_addr, addr(DEFAULT_RAUTHY_ADDR));
    assert_eq!(cfg.rauthy_base_url(), "http://127.0.0.1:8080");
    assert_eq!(cfg.trusted_proxy_hops, 0);
    assert_eq!(cfg.otlp_endpoint, None);
    assert_eq!(cfg.hiqlite_dir(), PathBuf::from("/data/hiqlite"));
    assert_eq!(cfg.keys_dir(), PathBuf::from("/data/keys"));
}

#[test]
fn the_default_ports_leave_8100_and_8200_to_rauthy() {
    let cfg = Config::from_env(&env(&[(ENV_PUBLIC_URL, "https://cell.example.test")])).unwrap();
    for port in [cfg.hiqlite.api_addr.port(), cfg.hiqlite.raft_addr.port()] {
        assert!(
            port != 8100 && port != 8200,
            "port {port} belongs to rauthy's hiqlite"
        );
    }
    assert_eq!(cfg.hiqlite.api_addr.port(), 8300);
    assert_eq!(cfg.hiqlite.raft_addr.port(), 8400);
}

#[test]
fn http_public_url_yields_plain_cookies() {
    let cfg = Config::from_env(&env(&[(ENV_PUBLIC_URL, "http://localhost:3000")])).unwrap();
    assert!(!cfg.public_url.is_https());
    assert_eq!(cfg.public_url.scheme(), "http");
    assert_eq!(cfg.public_url.authority(), "localhost:3000");
    assert_eq!(cfg.cookie_scheme, CookieScheme::Plain);
    assert!(!cfg.cookie_scheme.is_secure());
}

#[test]
fn the_public_url_is_required() {
    let err = Config::from_env(&env(&[])).unwrap_err();
    assert!(matches!(err, Error::Config(_)), "{err}");
    assert_eq!(err.exit_code(), 3);
    let blank = Config::from_env(&env(&[(ENV_PUBLIC_URL, "  ")])).unwrap_err();
    assert!(matches!(blank, Error::Config(_)));
}

#[test]
fn override_data_dir() {
    let cfg = Config::from_env(&env(&[
        (ENV_PUBLIC_URL, "https://cell.example.test"),
        (ENV_DATA_DIR, "/var/lib/rahi"),
    ]))
    .unwrap();
    assert_eq!(cfg.data_dir, PathBuf::from("/var/lib/rahi"));
    assert_eq!(cfg.hiqlite_dir(), PathBuf::from("/var/lib/rahi/hiqlite"));
}

#[test]
fn override_hiqlite_addresses() {
    let cfg = Config::from_env(&env(&[
        (ENV_PUBLIC_URL, "https://cell.example.test"),
        (ENV_HIQLITE_API_ADDR, "0.0.0.0:9300"),
        (ENV_HIQLITE_RAFT_ADDR, "0.0.0.0:9400"),
    ]))
    .unwrap();
    assert_eq!(cfg.hiqlite.api_addr, addr("0.0.0.0:9300"));
    assert_eq!(cfg.hiqlite.raft_addr, addr("0.0.0.0:9400"));
}

#[test]
fn override_rauthy_addr() {
    let cfg = Config::from_env(&env(&[
        (ENV_PUBLIC_URL, "https://cell.example.test"),
        (ENV_RAUTHY_ADDR, "127.0.0.1:8081"),
    ]))
    .unwrap();
    assert_eq!(cfg.rauthy_addr, addr("127.0.0.1:8081"));
    assert_eq!(cfg.rauthy_base_url(), "http://127.0.0.1:8081");
}

#[test]
fn override_trusted_proxy_hops() {
    let cfg = Config::from_env(&env(&[
        (ENV_PUBLIC_URL, "https://cell.example.test"),
        (ENV_TRUSTED_PROXY_HOPS, "2"),
    ]))
    .unwrap();
    assert_eq!(cfg.trusted_proxy_hops, 2);
}

#[test]
fn override_otlp_endpoint() {
    let cfg = Config::from_env(&env(&[
        (ENV_PUBLIC_URL, "https://cell.example.test"),
        (ENV_OTLP_ENDPOINT, "http://otel-collector:4317"),
    ]))
    .unwrap();
    assert_eq!(
        cfg.otlp_endpoint.as_deref(),
        Some("http://otel-collector:4317")
    );
}

#[test]
fn empty_overrides_fall_back_to_defaults() {
    let cfg = Config::from_env(&env(&[
        (ENV_PUBLIC_URL, "https://cell.example.test"),
        (ENV_DATA_DIR, ""),
        (ENV_TRUSTED_PROXY_HOPS, ""),
        (ENV_OTLP_ENDPOINT, ""),
    ]))
    .unwrap();
    assert_eq!(cfg.data_dir, PathBuf::from(DEFAULT_DATA_DIR));
    assert_eq!(cfg.trusted_proxy_hops, 0);
    assert_eq!(cfg.otlp_endpoint, None);
}

#[test]
fn malformed_overrides_are_config_errors() {
    for (key, bad) in [
        (ENV_HIQLITE_API_ADDR, "not-an-address"),
        (ENV_HIQLITE_RAFT_ADDR, "localhost:8400"),
        (ENV_RAUTHY_ADDR, "8080"),
        (ENV_TRUSTED_PROXY_HOPS, "-1"),
        (ENV_TRUSTED_PROXY_HOPS, "256"),
    ] {
        let err = Config::from_env(&env(&[
            (ENV_PUBLIC_URL, "https://cell.example.test"),
            (key, bad),
        ]))
        .unwrap_err();
        assert!(matches!(err, Error::Config(_)), "{key}={bad}: {err}");
        assert!(err.message().contains(key), "{key}={bad}: {err}");
    }
}

#[test]
fn public_url_is_normalised() {
    let url = PublicUrl::parse(" https://cell.example.test/app/ ").unwrap();
    assert_eq!(url.as_str(), "https://cell.example.test/app");
    assert_eq!(url.origin(), "https://cell.example.test");
    assert_eq!(url.authority(), "cell.example.test");
}

#[test]
fn public_url_rejects_other_schemes_queries_and_empty_hosts() {
    for bad in [
        "ftp://cell.example.test",
        "cell.example.test",
        "https://",
        "https:///path",
        "https://:8443",
        "https://user@cell.example.test",
        "https://cell.example.test/?x=1",
        "https://cell.example.test/#frag",
    ] {
        let err = PublicUrl::parse(bad).unwrap_err();
        assert!(matches!(err, Error::Config(_)), "{bad}: {err}");
    }
}

#[test]
fn config_round_trips_through_serde() {
    let cfg = Config::from_env(&env(&[
        (ENV_PUBLIC_URL, "https://cell.example.test"),
        (ENV_OTLP_ENDPOINT, "http://otel-collector:4317"),
    ]))
    .unwrap();
    let json = serde_json::to_string(&cfg).unwrap();
    let back: Config = serde_json::from_str(&json).unwrap();
    assert_eq!(back, cfg);
}

#[test]
fn a_str_map_is_also_an_env_reader() {
    let map: BTreeMap<&str, &str> = BTreeMap::from([(ENV_PUBLIC_URL, "http://localhost")]);
    let cfg = Config::from_env(&map).unwrap();
    assert_eq!(cfg.public_url.as_str(), "http://localhost");
}
