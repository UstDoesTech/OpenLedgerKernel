//! The node configuration file, written by the infrastructure modules (see I-01d) or by hand.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::reference;

/// What can be wrong with a configuration.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("cannot read the configuration {0}: {1}")]
    Read(String, String),
    #[error("the configuration is not valid: {0}")]
    Parse(String),
    #[error("{field}: {reason}")]
    Invalid { field: &'static str, reason: String },
}

fn invalid(field: &'static str, reason: impl Into<String>) -> ConfigError {
    ConfigError::Invalid {
        field,
        reason: reason.into(),
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Primary,
    Standby,
}

/// Where the administrator's credential goes.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StoreConfig {
    /// A file in a directory the customer mounts from their own secret store or encrypted volume.
    File { dir: PathBuf },
    /// AWS Secrets Manager. `secret_id` is the secret's ARN. The machine's role signs the calls.
    AwsSecretsManager {},
    /// Azure Key Vault. `secret_id` is `https://<vault>.vault.azure.net/secrets/<name>`. The
    /// machine's managed identity signs in.
    AzureKeyVault {},
    /// Google Secret Manager. `secret_id` is `projects/<project>/secrets/<name>`. The machine's
    /// service account signs in.
    GcpSecretManager {},
}

impl Default for StoreConfig {
    fn default() -> Self {
        StoreConfig::File {
            dir: PathBuf::from("/secrets/out"),
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NodeConfig {
    pub tenant: String,
    /// The node's name. Standbys use it as their application name, which is what the primary's
    /// `synchronous_standby_names` matches.
    pub node: String,
    pub role: Role,
    pub postgres_version: u32,
    /// The primary's address: a host name or an IP address that its certificate names.
    pub primary_address: String,
    /// `ANY k (name, name)` on the primary, empty on a standby.
    #[serde(default)]
    pub synchronous_standby_names: String,
    /// Names the credential in the secret store. Never the credential itself.
    pub secret_id: String,
    #[serde(default = "default_port")]
    pub port: u16,
    /// The primary's port, when it differs from this node's own. Defaults to `port`.
    #[serde(default)]
    pub primary_port: Option<u16>,
    #[serde(default)]
    pub secret_store: StoreConfig,
}

fn default_port() -> u16 {
    5432
}

fn valid_name(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && s.len() <= 63
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn valid_host(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 253
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':'))
}

/// Parses `ANY k (a, b, c)` and returns k and the names.
pub fn parse_sync_names(s: &str) -> Result<(usize, Vec<String>), String> {
    let rest = s
        .strip_prefix("ANY ")
        .ok_or("must look like ANY 1 (node-a, node-b)")?;
    let (k, rest) = rest
        .split_once(' ')
        .ok_or("must look like ANY 1 (node-a, node-b)")?;
    let k: usize = k.parse().map_err(|_| "the count must be a number")?;
    let inner = rest
        .strip_prefix('(')
        .and_then(|r| r.strip_suffix(')'))
        .ok_or("the names must be in brackets")?;
    let names: Vec<String> = inner.split(',').map(|n| n.trim().to_string()).collect();
    if let Some(bad) = names.iter().find(|n| !valid_name(n)) {
        return Err(format!("{bad:?} is not a valid node name"));
    }
    if k == 0 || k > names.len() {
        return Err(format!("the count must be between 1 and {}", names.len()));
    }
    Ok((k, names))
}

impl NodeConfig {
    /// The port a standby uses to reach the primary.
    pub fn upstream_port(&self) -> u16 {
        self.primary_port.unwrap_or(self.port)
    }

    pub fn from_json(text: &str) -> Result<Self, ConfigError> {
        let config: NodeConfig =
            serde_json::from_str(text).map_err(|e| ConfigError::Parse(e.to_string()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| ConfigError::Read(path.display().to_string(), e.to_string()))?;
        Self::from_json(&text)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        let tenant = self.tenant.strip_prefix("ten_").unwrap_or("");
        if tenant.is_empty()
            || tenant.len() > 24
            || !tenant
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        {
            return Err(invalid(
                "tenant",
                "must look like ten_ followed by 1 to 24 lower-case letters or digits",
            ));
        }
        if !valid_name(&self.node) {
            return Err(invalid(
                "node",
                "must be lower-case letters, digits and hyphens, starting with a letter",
            ));
        }
        if self.postgres_version != 16 {
            return Err(invalid(
                "postgres_version",
                "only PostgreSQL 16 is supported",
            ));
        }
        if !valid_host(&self.primary_address) {
            return Err(invalid(
                "primary_address",
                "must be a host name or an IP address",
            ));
        }
        if self.secret_id.is_empty() || self.secret_id.len() > 512 {
            return Err(invalid("secret_id", "must be 1 to 512 characters"));
        }
        let reference = match &self.secret_store {
            StoreConfig::File { .. } => Ok(()),
            StoreConfig::AwsSecretsManager {} => reference::parse_aws(&self.secret_id).map(|_| ()),
            StoreConfig::AzureKeyVault {} => reference::parse_azure(&self.secret_id).map(|_| ()),
            StoreConfig::GcpSecretManager {} => reference::parse_gcp(&self.secret_id).map(|_| ()),
        };
        reference.map_err(|e| invalid("secret_id", e))?;
        if self.port == 0 || self.primary_port == Some(0) {
            return Err(invalid("port", "must not be 0"));
        }
        match self.role {
            Role::Primary => {
                let (_, names) = parse_sync_names(&self.synchronous_standby_names)
                    .map_err(|e| invalid("synchronous_standby_names", e))?;
                if names.contains(&self.node) {
                    return Err(invalid(
                        "synchronous_standby_names",
                        "a node cannot wait for itself",
                    ));
                }
            }
            Role::Standby => {
                if !self.synchronous_standby_names.is_empty() {
                    return Err(invalid(
                        "synchronous_standby_names",
                        "must be empty on a standby",
                    ));
                }
            }
        }
        Ok(())
    }
}
