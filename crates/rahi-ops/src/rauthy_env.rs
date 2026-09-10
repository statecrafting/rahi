//! rauthy's environment (spec 031 B-2): rendered from one template.
//!
//! rauthy 0.36 reads a TOML file and lets an environment variable override
//! every key in it, so the app hands rauthy an empty file and a rendered
//! environment. The template is `docker/rauthy.env.template`, compiled into
//! this crate so the file in the repository and the file first boot writes
//! cannot drift; the rendering is a pure function of the public URL, the
//! ports, and the secrets minted into the key set.

use std::collections::BTreeMap;
use std::path::PathBuf;

use base64::Engine as _;
use serde::{Deserialize, Serialize};

use rahi_types::{Config, Error, Result};

/// The template, as committed.
pub const TEMPLATE: &str = include_str!("../../../docker/rauthy.env.template");

/// The rendered environment, relative to the data directory.
pub const ENV_FILE: &str = "rauthy/rauthy.env";

/// The empty configuration file rauthy is pointed at, relative to the data
/// directory.
pub const CONFIG_FILE: &str = "rauthy/config.toml";

/// rauthy's hiqlite Raft port on loopback (B-2).
pub const DEFAULT_HQL_RAFT_PORT: u16 = 8100;

/// rauthy's hiqlite API port on loopback (B-2).
pub const DEFAULT_HQL_API_PORT: u16 = 8200;

/// Overrides for rauthy's hiqlite ports, for a harness that boots more than
/// one deployment on one host (spec 033 B-2).
pub const ENV_HQL_RAFT_PORT: &str = "RAHI_RAUTHY_HQL_RAFT_PORT";
/// See [`ENV_HQL_RAFT_PORT`].
pub const ENV_HQL_API_PORT: &str = "RAHI_RAUTHY_HQL_API_PORT";

/// The loopback proxy rauthy trusts in `https` mode: the app in the same
/// container.
pub const TRUSTED_PROXY: &str = "127.0.0.1/32";

/// This replica's hiqlite node id, for both clusters (spec 032 B-1). Unset
/// means node `1` of a single-node deployment; [`ENV_POD_INDEX`] plus one
/// when only the pod ordinal is known.
pub const ENV_NODE_ID: &str = "RAHI_HIQ_NODE_ID";

/// The pod's ordinal (Kubernetes' `apps.kubernetes.io/pod-index`), from
/// which the node id is the ordinal plus one (spec 032 B-1).
pub const ENV_POD_INDEX: &str = "RAHI_POD_INDEX";

/// rauthy's hiqlite peers, one `<id> <raft_addr> <api_addr>` per line or
/// separated by `;` (spec 032 B-1). Unset means the one loopback node.
pub const ENV_RAUTHY_HQL_NODES: &str = "RAHI_RAUTHY_HQL_NODES";

/// The address rauthy's hiqlite listens on: loopback by default, the pod
/// network (`0.0.0.0`) in a cluster (spec 032 B-1).
pub const ENV_RAUTHY_HQL_LISTEN_ADDR: &str = "RAHI_RAUTHY_HQL_LISTEN_ADDR";

/// This replica's node id from the environment (spec 032 B-1).
///
/// # Errors
///
/// [`Error::Config`] when the value is not a positive integer.
pub fn node_id(env: &dyn rahi_types::EnvReader) -> Result<u64> {
    if let Some(raw) = env.get(ENV_NODE_ID) {
        let id: u64 = raw
            .parse()
            .map_err(|_| Error::Config(format!("{ENV_NODE_ID} {raw:?} is not a node id")))?;
        if id == 0 {
            return Err(Error::Config(format!("{ENV_NODE_ID} must be at least 1")));
        }
        return Ok(id);
    }
    if let Some(raw) = env.get(ENV_POD_INDEX) {
        let index: u64 = raw
            .parse()
            .map_err(|_| Error::Config(format!("{ENV_POD_INDEX} {raw:?} is not an ordinal")))?;
        return Ok(index.saturating_add(1));
    }
    Ok(1)
}

/// One `<id> <raft_addr> <api_addr>` triple.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeLine {
    /// The node id.
    pub id: u64,
    /// The Raft address, `host:port`.
    pub raft_addr: String,
    /// The API address, `host:port`.
    pub api_addr: String,
}

/// Parse a peer list: triples separated by newlines or `;`.
///
/// # Errors
///
/// [`Error::Config`] naming the line that is not a triple.
pub fn parse_nodes(raw: &str) -> Result<Vec<NodeLine>> {
    let mut nodes = Vec::new();
    for line in raw
        .split(['\n', ';'])
        .map(str::trim)
        .filter(|l| !l.is_empty())
    {
        let mut parts = line.split_whitespace();
        let (Some(id), Some(raft), Some(api), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(Error::Config(format!(
                "node line {line:?} is not `<id> <raft_addr> <api_addr>`"
            )));
        };
        let id: u64 = id
            .parse()
            .map_err(|_| Error::Config(format!("node id {id:?} is not a number")))?;
        nodes.push(NodeLine {
            id,
            raft_addr: raft.to_owned(),
            api_addr: api.to_owned(),
        });
    }
    if nodes.is_empty() {
        return Err(Error::Config("the node list is empty".to_owned()));
    }
    Ok(nodes)
}

/// The peer list rendered the way hiqlite reads it: one triple per line.
#[must_use]
pub fn render_nodes(nodes: &[NodeLine]) -> String {
    nodes
        .iter()
        .map(|n| format!("{} {} {}", n.id, n.raft_addr, n.api_addr))
        .collect::<Vec<_>>()
        .join("\n")
}

/// What rauthy's bootstrap API key may do: the client registration and
/// update spec 021 B-5 performs, the secret read the supervisor performs,
/// and the user administration spec 033's harness performs (create a
/// test user, set its password). Nothing wider: rauthy re-applies this
/// access from the rendered environment at every start, so a later spec
/// that needs more widens it here by re-rendering, not by minting a broad
/// key ahead of its use (spec 031 D-8).
pub const API_KEY_ACCESS: &str = r#"[{"group":"Clients","access_rights":["read","create","update"]},{"group":"Secrets","access_rights":["read"]},{"group":"Users","access_rights":["read","create","update"]},{"group":"Roles","access_rights":["read","create"]}]"#;

/// rauthy's secrets, custodied as `keys/rauthy.json`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RauthySecrets {
    /// The encryption key's id.
    pub enc_key_id: String,
    /// The encryption key, base64 of 32 bytes.
    pub enc_key: String,
    /// hiqlite's Raft secret.
    pub secret_raft: String,
    /// hiqlite's API secret.
    pub secret_api: String,
    /// The bootstrap admin's email.
    pub admin_email: String,
    /// The bootstrap admin's initial password.
    pub admin_password: String,
    /// The API key's name.
    pub api_key_name: String,
    /// The API key's secret (at least 64 characters, rauthy's floor).
    pub api_key_secret: String,
}

impl std::fmt::Debug for RauthySecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RauthySecrets")
            .field("enc_key_id", &self.enc_key_id)
            .field("admin_email", &self.admin_email)
            .field("api_key_name", &self.api_key_name)
            .finish_non_exhaustive()
    }
}

impl RauthySecrets {
    /// The token the app presents: `name$secret`.
    #[must_use]
    pub fn api_token(&self) -> String {
        format!("{}${}", self.api_key_name, self.api_key_secret)
    }

    /// `BOOTSTRAP_API_KEY`: base64 of the JSON key request.
    #[must_use]
    pub fn bootstrap_api_key(&self) -> String {
        let request = format!(
            r#"{{"name":"{}","exp":null,"access":{API_KEY_ACCESS}}}"#,
            self.api_key_name
        );
        base64::engine::general_purpose::STANDARD.encode(request)
    }
}

/// rauthy's hiqlite topology: the ports, and at N=3 the node id, the peers,
/// and the listen address (spec 032 B-1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HqlPorts {
    /// The Raft port.
    pub raft: u16,
    /// The API port.
    pub api: u16,
    /// This replica's node id.
    pub node_id: u64,
    /// The peers, when the deployment names them; `None` is the one node.
    pub nodes: Option<Vec<NodeLine>>,
    /// The address the listeners bind.
    pub listen_addr: String,
}

impl HqlPorts {
    /// The defaults, or the overrides from the environment.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when an override is not a port.
    pub fn from_env(env: &dyn rahi_types::EnvReader) -> Result<Self> {
        let port = |key: &str, default: u16| -> Result<u16> {
            match env.get(key) {
                None => Ok(default),
                Some(raw) => raw
                    .parse()
                    .map_err(|_| Error::Config(format!("{key} {raw:?} is not a port"))),
            }
        };
        let nodes = match env.get(ENV_RAUTHY_HQL_NODES) {
            Some(raw) => Some(parse_nodes(&raw)?),
            None => None,
        };
        Ok(Self {
            raft: port(ENV_HQL_RAFT_PORT, DEFAULT_HQL_RAFT_PORT)?,
            api: port(ENV_HQL_API_PORT, DEFAULT_HQL_API_PORT)?,
            node_id: node_id(env)?,
            nodes,
            listen_addr: env
                .get(ENV_RAUTHY_HQL_LISTEN_ADDR)
                .unwrap_or_else(|| "127.0.0.1".to_owned()),
        })
    }
}

/// The rendered environment's path.
#[must_use]
pub fn env_path(config: &Config) -> PathBuf {
    config.data_dir.join(ENV_FILE)
}

/// The empty config file's path.
#[must_use]
pub fn config_path(config: &Config) -> PathBuf {
    config.data_dir.join(CONFIG_FILE)
}

/// The values the template's placeholders take for `config`.
///
/// # Errors
///
/// [`Error::Config`] when the public URL has no host, or when the data
/// directory is not UTF-8.
pub fn values(
    config: &Config,
    secrets: &RauthySecrets,
    app_name: &str,
    ports: HqlPorts,
) -> Result<BTreeMap<&'static str, String>> {
    let authority = config.public_url.authority();
    let host = authority
        .rsplit_once(':')
        .filter(|(_, port)| port.chars().all(|c| c.is_ascii_digit()))
        .map_or(authority, |(host, _)| host);
    if host.is_empty() {
        return Err(Error::Config("the public URL names no host".to_owned()));
    }
    let https = config.public_url.is_https();
    let origin_with_port = if authority.contains(':') {
        config.public_url.origin()
    } else {
        format!(
            "{}://{authority}:{}",
            config.public_url.scheme(),
            if https { 443 } else { 80 }
        )
    };
    let scheme_mode = if https {
        format!("PROXY_MODE=true\nTRUSTED_PROXIES={TRUSTED_PROXY}")
    } else {
        "COOKIE_MODE=danger-insecure".to_owned()
    };
    let data_dir = crate::rauthy_dir(config)
        .to_str()
        .ok_or_else(|| Error::Config("the data directory is not UTF-8".to_owned()))?
        .to_owned();
    let hql_nodes = match &ports.nodes {
        Some(nodes) => render_nodes(nodes),
        None => format!("1 127.0.0.1:{} 127.0.0.1:{}", ports.raft, ports.api),
    };
    Ok(BTreeMap::from([
        ("HQL_NODE_ID", ports.node_id.to_string()),
        ("HQL_NODES", hql_nodes),
        ("HQL_LISTEN_ADDR", ports.listen_addr.clone()),
        ("PUB_URL", authority.to_owned()),
        ("LISTEN_ADDRESS", config.rauthy_addr.ip().to_string()),
        ("LISTEN_PORT", config.rauthy_addr.port().to_string()),
        ("SCHEME_MODE", scheme_mode),
        ("RP_ID", host.to_owned()),
        ("RP_ORIGIN", origin_with_port),
        ("RP_NAME", app_name.to_owned()),
        ("HQL_DATA_DIR", data_dir),
        ("HQL_SECRET_RAFT", secrets.secret_raft.clone()),
        ("HQL_SECRET_API", secrets.secret_api.clone()),
        (
            "ENC_KEYS",
            format!("{}/{}", secrets.enc_key_id, secrets.enc_key),
        ),
        ("ENC_KEY_ACTIVE", secrets.enc_key_id.clone()),
        ("ADMIN_EMAIL", secrets.admin_email.clone()),
        ("ADMIN_PASSWORD", secrets.admin_password.clone()),
        ("BOOTSTRAP_API_KEY", secrets.bootstrap_api_key()),
        ("BOOTSTRAP_API_KEY_SECRET", secrets.api_key_secret.clone()),
    ]))
}

/// Render the template for `config`.
///
/// # Errors
///
/// As [`values`]; [`Error::Config`] when the template holds a placeholder
/// no value covers, which is a build defect rather than an input.
pub fn render(
    config: &Config,
    secrets: &RauthySecrets,
    app_name: &str,
    ports: HqlPorts,
) -> Result<String> {
    let values = values(config, secrets, app_name, ports)?;
    let mut out = TEMPLATE.to_owned();
    for (key, value) in &values {
        out = out.replace(&format!("${{{key}}}"), value);
    }
    if let Some(start) = out.find("${") {
        let rest = out.get(start..).unwrap_or("");
        let name: String = rest.chars().take_while(|c| *c != '}').skip(2).collect();
        return Err(Error::Config(format!(
            "the rauthy environment template names ${{{name}}}, which nothing renders"
        )));
    }
    Ok(out)
}

/// Parse a rendered environment back into pairs: `KEY=VALUE` lines, with
/// comments and blank lines skipped. A line with no `=` continues the
/// previous value on a new line, which is how a peer list (one node per
/// line, as hiqlite reads it) survives the file.
#[must_use]
pub fn parse(text: &str) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        match line.split_once('=') {
            Some((key, value)) if !key.contains(char::is_whitespace) => {
                pairs.push((key.trim().to_owned(), value.to_owned()));
            }
            _ => {
                if let Some((_, value)) = pairs.last_mut() {
                    value.push('\n');
                    value.push_str(line);
                }
            }
        }
    }
    pairs
}
