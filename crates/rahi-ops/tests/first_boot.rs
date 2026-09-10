//! First boot (spec 031 FR-001, FR-003): every key once, rauthy's
//! environment derived from one URL, and a second run that changes nothing.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use rahi_ops::archive::sha256_hex;
use rahi_ops::first_boot::{self, Outcome};
use rahi_ops::rauthy_env;
use rahi_ops::{KEY_DIR_MODE, KEY_FILE_MODE, KeySet};
use rahi_types::Config;

fn env(data_dir: &Path, public_url: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("RAHI_PUBLIC_URL".to_owned(), public_url.to_owned()),
        ("RAHI_DATA_DIR".to_owned(), data_dir.display().to_string()),
    ])
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// Every regular file under `root`, by relative path, with its hash.
fn tree(root: &Path) -> BTreeMap<String, String> {
    fn walk(dir: &Path, root: &Path, into: &mut BTreeMap<String, String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, root, into);
            } else {
                let rel = path.strip_prefix(root).unwrap().display().to_string();
                into.insert(rel, sha256_hex(&std::fs::read(&path).unwrap()));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

#[tokio::test]
async fn first_boot_mints_every_key_once_and_a_second_run_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let env = env(dir.path(), "http://localhost:8080");
    let config = Config::from_env(&env).unwrap();

    let Outcome::Generated(credentials) = first_boot::run(&env, "hello-cell").await.unwrap() else {
        panic!("an empty volume is a first boot");
    };
    assert_eq!(credentials.email, "admin@localhost");
    assert!(credentials.password.len() >= 24);
    assert!(credentials.api_token.starts_with("rahi$"));

    for sub in ["hiqlite", "rauthy", "keys", "backups"] {
        assert!(dir.path().join(sub).is_dir(), "{sub}/ exists (B-1)");
    }
    assert_eq!(mode(&config.keys_dir()), KEY_DIR_MODE);
    let keys = KeySet::of(&config);
    keys.check()
        .expect("every required key is present with mode 0600");
    let mut names: Vec<String> = keys.export().unwrap().into_iter().map(|(n, _)| n).collect();
    names.sort();
    assert_eq!(
        names,
        vec![
            "backup.key",
            "hiqlite.json",
            "ledger.key",
            "rauthy.json",
            "rauthy_admin_token",
            "session.key"
        ]
    );
    for name in &names {
        assert_eq!(mode(&keys.path(name)), KEY_FILE_MODE, "{name} is 0600");
    }
    keys.ledger_signer().expect("the ledger seed parses");
    keys.store_secrets().expect("the store secrets parse");
    keys.backup_identity().expect("the backup identity parses");
    let rauthy = keys.rauthy_secrets().expect("rauthy's secrets parse");
    assert_eq!(keys.admin_token().unwrap(), rauthy.api_token());
    assert!(
        rauthy.api_key_secret.len() >= 64,
        "rauthy's floor for an API key secret"
    );

    let env_path = rauthy_env::env_path(&config);
    assert_eq!(mode(&env_path), KEY_FILE_MODE);
    assert_eq!(
        std::fs::read(rauthy_env::config_path(&config)).unwrap(),
        b"",
        "an empty config file"
    );
    let rendered = std::fs::read_to_string(&env_path).unwrap();
    let pairs: BTreeMap<String, String> = rauthy_env::parse(&rendered).into_iter().collect();
    assert_eq!(pairs["PUB_URL"], "localhost:8080");
    assert_eq!(pairs["LISTEN_ADDRESS"], "127.0.0.1");
    assert_eq!(pairs["LISTEN_PORT_HTTP"], "8080");
    assert_eq!(pairs["RP_ID"], "localhost");
    assert_eq!(pairs["RP_ORIGIN"], "http://localhost:8080");
    assert_eq!(pairs["RP_NAME"], "hello-cell");
    assert_eq!(pairs["HQL_NODE_ID"], "1");
    assert_eq!(pairs["HQL_NODES"], "1 127.0.0.1:8100 127.0.0.1:8200");
    assert_eq!(
        pairs["HQL_DATA_DIR"],
        dir.path().join("rauthy").display().to_string()
    );
    assert_eq!(pairs["ENC_KEY_ACTIVE"], "k1");
    assert_eq!(pairs["ENC_KEYS"], format!("k1/{}", rauthy.enc_key));
    assert_eq!(
        pairs["BOOTSTRAP_ADMIN_PASSWORD_PLAIN"],
        credentials.password
    );
    assert_eq!(pairs["BOOTSTRAP_API_KEY_SECRET"], rauthy.api_key_secret);
    assert_eq!(pairs["BOOTSTRAP_API_KEY"], rauthy.bootstrap_api_key());
    assert!(
        !rendered.contains("${"),
        "no placeholder survives rendering"
    );

    let before = tree(dir.path());
    let second = first_boot::run(&env, "hello-cell").await.unwrap();
    assert_eq!(
        second,
        Outcome::Verified {
            env_rendered: false
        }
    );
    assert_eq!(
        tree(dir.path()),
        before,
        "a second run changes no file (FR-001)"
    );
}

#[tokio::test]
async fn the_scheme_decides_rauthys_cookie_or_proxy_mode() {
    let http = tempfile::tempdir().unwrap();
    let env_http = env(http.path(), "http://localhost:8080");
    first_boot::run(&env_http, "cell").await.unwrap();
    let rendered =
        std::fs::read_to_string(rauthy_env::env_path(&Config::from_env(&env_http).unwrap()))
            .unwrap();
    assert!(
        rendered.contains("COOKIE_MODE=danger-insecure"),
        "{rendered}"
    );
    assert!(!rendered.contains("PROXY_MODE"));

    let https = tempfile::tempdir().unwrap();
    let env_https = env(https.path(), "https://cell.example.com");
    first_boot::run(&env_https, "cell").await.unwrap();
    let rendered =
        std::fs::read_to_string(rauthy_env::env_path(&Config::from_env(&env_https).unwrap()))
            .unwrap();
    let pairs: BTreeMap<String, String> = rauthy_env::parse(&rendered).into_iter().collect();
    assert_eq!(pairs["PROXY_MODE"], "true");
    assert_eq!(pairs["TRUSTED_PROXIES"], "127.0.0.1/32");
    assert!(!pairs.contains_key("COOKIE_MODE"));
    assert_eq!(pairs["PUB_URL"], "cell.example.com");
    assert_eq!(pairs["RP_ID"], "cell.example.com");
    assert_eq!(
        pairs["RP_ORIGIN"], "https://cell.example.com:443",
        "rauthy wants an explicit port"
    );
}

#[tokio::test]
async fn a_present_key_set_is_verified_not_regenerated_and_a_bad_mode_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let env = env(dir.path(), "http://localhost:8080");
    let config = Config::from_env(&env).unwrap();
    first_boot::run(&env, "cell").await.unwrap();

    // A restored volume: keys present, rauthy's rendered files absent.
    std::fs::remove_file(rauthy_env::env_path(&config)).unwrap();
    std::fs::remove_file(rauthy_env::config_path(&config)).unwrap();
    let keys_before = tree(&config.keys_dir());
    let outcome = first_boot::run(&env, "cell").await.unwrap();
    assert_eq!(outcome, Outcome::Verified { env_rendered: true });
    assert_eq!(
        tree(&config.keys_dir()),
        keys_before,
        "no key was regenerated"
    );
    assert!(rauthy_env::env_path(&config).is_file());

    let ledger_key = config.keys_dir().join(rahi_ops::LEDGER_KEY_FILE);
    std::fs::set_permissions(&ledger_key, std::fs::Permissions::from_mode(0o644)).unwrap();
    let err = first_boot::run(&env, "cell").await.unwrap_err();
    assert!(matches!(err, rahi_types::Error::Config(_)), "{err}");
    assert!(err.to_string().contains("has mode 0644"));
}

#[test]
fn the_template_and_the_renderer_agree_on_every_placeholder() {
    let placeholders: Vec<&str> = rauthy_env::TEMPLATE
        .split("${")
        .skip(1)
        .map(|rest| rest.split('}').next().unwrap())
        .collect();
    assert!(!placeholders.is_empty());
    let env = env(Path::new("/data"), "https://cell.example.com:8443");
    let config = Config::from_env(&env).unwrap();
    let secrets = rauthy_env::RauthySecrets {
        enc_key_id: "k1".to_owned(),
        enc_key: "AAAA".to_owned(),
        secret_raft: "r".to_owned(),
        secret_api: "a".to_owned(),
        admin_email: "admin@localhost".to_owned(),
        admin_password: "p".to_owned(),
        api_key_name: "rahi".to_owned(),
        api_key_secret: "s".repeat(64),
    };
    let values = rauthy_env::values(
        &config,
        &secrets,
        "cell",
        rauthy_env::HqlPorts {
            raft: 8100,
            api: 8200,
            node_id: 1,
            nodes: None,
            listen_addr: "127.0.0.1".to_owned(),
        },
    )
    .unwrap();
    for name in &placeholders {
        assert!(values.contains_key(name), "${{{name}}} is rendered");
    }
    assert_eq!(values["RP_ORIGIN"], "https://cell.example.com:8443");
    assert_eq!(values["PUB_URL"], "cell.example.com:8443");
    assert_eq!(values["RP_ID"], "cell.example.com");
}

#[tokio::test]
async fn a_replica_renders_its_ordinal_and_its_peers_into_rauthys_environment() {
    let dir = tempfile::tempdir().unwrap();
    let mut env = env(dir.path(), "https://cell.example.com");
    env.insert("RAHI_POD_INDEX".to_owned(), "1".to_owned());
    env.insert(
        "RAHI_RAUTHY_HQL_NODES".to_owned(),
        "1 rahi-0.rahi:8100 rahi-0.rahi:8200;2 rahi-1.rahi:8100 rahi-1.rahi:8200;3 rahi-2.rahi:8100 rahi-2.rahi:8200".to_owned(),
    );
    env.insert(
        "RAHI_RAUTHY_HQL_LISTEN_ADDR".to_owned(),
        "0.0.0.0".to_owned(),
    );
    first_boot::run(&env, "cell").await.unwrap();
    let rendered =
        std::fs::read_to_string(rauthy_env::env_path(&Config::from_env(&env).unwrap())).unwrap();
    let pairs: BTreeMap<String, String> = rauthy_env::parse(&rendered).into_iter().collect();
    assert_eq!(pairs["HQL_NODE_ID"], "2", "ordinal 1 is node 2");
    assert_eq!(pairs["HQL_LISTEN_ADDR_API"], "0.0.0.0");
    assert_eq!(pairs["HQL_LISTEN_ADDR_RAFT"], "0.0.0.0");
    let nodes: Vec<&str> = rendered
        .lines()
        .filter(|l| {
            l.starts_with("HQL_NODES=") || l.chars().next().is_some_and(|c| c.is_ascii_digit())
        })
        .collect();
    assert_eq!(nodes.len(), 3, "three peers, one per line: {nodes:?}");
    assert!(nodes[0].ends_with("1 rahi-0.rahi:8100 rahi-0.rahi:8200"));
    assert_eq!(nodes[2], "3 rahi-2.rahi:8100 rahi-2.rahi:8200");
    assert_eq!(rauthy_env::node_id(&env).unwrap(), 2);
    assert!(
        rauthy_env::parse_nodes("1 a:1").is_err(),
        "a pair is not a triple"
    );
}

#[tokio::test]
async fn a_read_only_key_mount_is_accepted_and_a_writable_wide_one_is_not() {
    let dir = tempfile::tempdir().unwrap();
    let env = env(dir.path(), "http://localhost:8080");
    let config = Config::from_env(&env).unwrap();
    first_boot::run(&env, "cell").await.unwrap();
    let keys = KeySet::of(&config);

    // A read-only mount: no write bits for the process, none for others.
    std::fs::set_permissions(keys.dir(), std::fs::Permissions::from_mode(0o555)).unwrap();
    keys.check()
        .expect("a read-only mount with 0600 files passes");
    let outcome = first_boot::run(&env, "cell").await.unwrap();
    assert_eq!(
        outcome,
        Outcome::Verified {
            env_rendered: false
        }
    );

    // A kubelet-owned file on that mount: root-owned, group-readable, no
    // write bit anywhere. Accepted there, refused on a writable directory.
    let ledger = keys.path(rahi_ops::LEDGER_KEY_FILE);
    std::fs::set_permissions(&ledger, std::fs::Permissions::from_mode(0o440)).unwrap();
    keys.check().expect("0440 on a read-only mount is private");
    std::fs::set_permissions(&ledger, std::fs::Permissions::from_mode(0o444)).unwrap();
    let err = keys.check().unwrap_err();
    assert!(err.to_string().contains("has mode 0444"), "{err}");
    std::fs::set_permissions(&ledger, std::fs::Permissions::from_mode(0o600)).unwrap();

    // Writable and readable by the group: refused.
    std::fs::set_permissions(keys.dir(), std::fs::Permissions::from_mode(0o775)).unwrap();
    let err = keys.check().unwrap_err();
    assert!(err.to_string().contains("has mode 0775"), "{err}");
    std::fs::set_permissions(keys.dir(), std::fs::Permissions::from_mode(0o700)).unwrap();
}
