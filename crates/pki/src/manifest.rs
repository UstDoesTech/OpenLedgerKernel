//! What to rotate: the authority, and each cell's secret and address. The workflow builds it from
//! the infrastructure's outputs and keeps it on the runner. It holds identifiers, so it is never
//! printed and never kept.

use serde::Deserialize;

use crate::error::PkiError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Cloud {
    Aws,
    Azure,
    Gcp,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Authority {
    /// The KMS key that signs: an ECC NIST P-256 signing key.
    pub key_id: String,
    /// The secret that holds the authority's public certificate. It may be empty the first time.
    pub certificate_secret: String,
}

/// Where a cell's own database is, and where its administrator's credential is held.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Database {
    /// The name the database's certificate carries, which is what the connection string uses.
    pub host: String,
    pub port: u16,
    pub kind: DatabaseKind,
    /// The reference to the secret that holds the administrator's credential. Never the credential.
    pub credential_secret: String,
    /// A self-hosted cell's machines, each with the secret its certificates go in. Left out for a
    /// cell someone else runs, and then no certificate is issued for it.
    #[serde(default)]
    pub nodes: Vec<Node>,
}

/// One machine of a self-hosted cell's database.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Node {
    pub name: String,
    pub role: NodeRole,
    /// The machine's private IP address, which its certificate names.
    pub address: String,
    /// The reference to the secret that holds this node's certificates. Never a certificate.
    pub secret: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeRole {
    Primary,
    Standby,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DatabaseKind {
    /// Postgres on machines the node agent runs. Its secret holds the administrator's password.
    SelfHosted,
    /// A managed service. Its secret is the service's own, and is not copied here.
    Managed,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CellEntry {
    /// The cell's name, as the service calls itself (`LEDGER_NODE`).
    pub name: String,
    pub cloud: Cloud,
    /// The reference to the cell's API secret, as the infrastructure gives it.
    pub secret: String,
    /// The machine's private IP address, which the certificate names.
    pub address: String,
    pub internal_port: u16,
    /// The certificate of the authority that made this cell's database certificate, so the service
    /// trusts it. It is public. Left out, the secret keeps whatever it already holds.
    #[serde(default)]
    pub db_ca: Option<String>,
    /// This cell's own database. Needed only for a proving environment.
    #[serde(default)]
    pub database: Option<Database>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub authority: Authority,
    /// The tenants a cell credential covers.
    pub tenants: Vec<String>,
    pub cells: Vec<CellEntry>,
    /// A proving environment holds no real money or customer data (D-19). Only then may a run fill a
    /// missing `db.conn` from the administrator's credential and make tokens for the tenants. Left
    /// out, a run fills neither, and a person does.
    #[serde(default)]
    pub proving: bool,
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Manifest, PkiError> {
        let m: Manifest = serde_json::from_str(text)
            .map_err(|_| PkiError::Manifest("not valid JSON of the expected shape"))?;
        m.check()?;
        Ok(m)
    }

    fn check(&self) -> Result<(), PkiError> {
        if self.cells.is_empty() {
            return Err(PkiError::Manifest("it lists no cell"));
        }
        if self.tenants.is_empty() || self.tenants.iter().any(|t| !t.starts_with("ten_")) {
            return Err(PkiError::Manifest(
                "tenants are ten_ identifiers, and at least one",
            ));
        }
        let mut names: Vec<&str> = self.cells.iter().map(|c| c.name.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        if names.len() != self.cells.len() {
            return Err(PkiError::Manifest("two cells share a name"));
        }
        for c in &self.cells {
            if c.address.parse::<std::net::IpAddr>().is_err() {
                return Err(PkiError::Manifest("a cell's address is an IP address"));
            }
            if c.name.is_empty() || c.secret.is_empty() {
                return Err(PkiError::Manifest("a cell has a name and a secret"));
            }
            if self.proving {
                let Some(d) = &c.database else {
                    return Err(PkiError::Manifest(
                        "a proving environment gives each cell's database",
                    ));
                };
                let plain = |s: &str| {
                    !s.is_empty()
                        && s.chars().all(|ch| {
                            ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | ':' | '_')
                        })
                };
                if !plain(&d.host) || d.credential_secret.is_empty() {
                    return Err(PkiError::Manifest(
                        "a cell's database has a plain host and a credential secret",
                    ));
                }
            }
            if let Some(d) = &c.database
                && !d.nodes.is_empty()
            {
                let bad = |why: &'static str| Err(PkiError::Manifest(why));
                if d.kind != DatabaseKind::SelfHosted {
                    return bad("only a self-hosted cell has nodes");
                }
                if d.nodes
                    .iter()
                    .filter(|n| n.role == NodeRole::Primary)
                    .count()
                    != 1
                {
                    return bad("a cell has one primary node");
                }
                let mut names: Vec<&str> = d.nodes.iter().map(|n| n.name.as_str()).collect();
                names.sort_unstable();
                names.dedup();
                if names.len() != d.nodes.len() {
                    return bad("two nodes of a cell share a name");
                }
                for n in &d.nodes {
                    if n.name.is_empty() || n.secret.is_empty() {
                        return bad("a node has a name and a secret");
                    }
                    if n.address.parse::<std::net::IpAddr>().is_err() {
                        return bad("a node's address is an IP address");
                    }
                }
            }
            if let Some(ca) = &c.db_ca
                && !(ca.contains("-----BEGIN CERTIFICATE-----")
                    && ca.contains("-----END CERTIFICATE-----"))
            {
                return Err(PkiError::Manifest(
                    "a cell's db_ca is a certificate in PEM form",
                ));
            }
        }
        Ok(())
    }
}
