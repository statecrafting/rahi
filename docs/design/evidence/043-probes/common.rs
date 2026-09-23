use std::borrow::Cow;
use hiqlite::{Node, NodeConfig};
#[derive(Clone, Copy, Debug)]
pub enum Cache { Kv }
impl hiqlite::CacheVariants for Cache {
    fn hiqlite_cache_index(&self) -> usize { 0 }
    fn hiqlite_cache_variants() -> &'static [(usize, &'static str)] { &[(0, "Kv")] }
}
pub fn cfg(dir: &str) -> NodeConfig {
    let mut c = NodeConfig {
        node_id: 1,
        nodes: vec![Node { id: 1, addr_raft: "127.0.0.1:28471".into(), addr_api: "127.0.0.1:28472".into() }],
        listen_addr_api: Cow::Borrowed("127.0.0.1"),
        listen_addr_raft: Cow::Borrowed("127.0.0.1"),
        data_dir: Cow::Owned(dir.to_string()),
        secret_raft: "0123456789abcdef0123".into(),
        secret_api: "0123456789abcdef0123".into(),
        ..NodeConfig::default()
    };
    c.enc_keys.enc_key_active = "k1".into();
    c.enc_keys.enc_keys = vec![("k1".into(), vec![7u8; 32])];
    c
}
/// Hold an exclusive non-blocking flock on `path` through a fresh open file description.
pub fn hold(path: &str) -> std::fs::File {
    use std::os::fd::AsRawFd;
    let f = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path).unwrap();
    let r = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    println!("HOLD {path} rc={r}");
    f
}
