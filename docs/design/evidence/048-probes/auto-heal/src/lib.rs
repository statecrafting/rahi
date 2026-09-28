//! A single-voter hiqlite node shaped like rahi's (`rahi_store::Store::open`),
//! with a small snapshot interval so a short run snapshots and purges its log
//! many times.

use std::borrow::Cow;

use hiqlite::{Node, NodeConfig};

/// Snapshot every this many log entries. rahi runs hiqlite's default,
/// 10 000; the probe uses a small value so every mechanism the default would
/// reach after hours runs in seconds.
pub const LOGS_UNTIL_SNAPSHOT: u64 = 50;

#[derive(Clone, Copy, Debug)]
pub enum Cache {
    Kv,
}

impl hiqlite::CacheVariants for Cache {
    fn hiqlite_cache_index(&self) -> usize {
        0
    }
    fn hiqlite_cache_variants() -> &'static [(usize, &'static str)] {
        &[(0, "Kv")]
    }
}

#[must_use]
pub fn node_config(dir: &str, raft_port: u16, api_port: u16) -> NodeConfig {
    let mut c = NodeConfig {
        node_id: 1,
        nodes: vec![Node {
            id: 1,
            addr_raft: format!("127.0.0.1:{raft_port}"),
            addr_api: format!("127.0.0.1:{api_port}"),
        }],
        listen_addr_api: Cow::Borrowed("127.0.0.1"),
        listen_addr_raft: Cow::Borrowed("127.0.0.1"),
        data_dir: Cow::Owned(dir.to_owned()),
        secret_raft: "0123456789abcdef0123".into(),
        secret_api: "0123456789abcdef0123".into(),
        raft_config: NodeConfig::default_raft_config(LOGS_UNTIL_SNAPSHOT),
        ..NodeConfig::default()
    };
    c.enc_keys.enc_key_active = "k1".into();
    c.enc_keys.enc_keys = vec![("k1".into(), vec![7u8; 32])];
    c
}
