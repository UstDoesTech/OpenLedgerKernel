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
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub authority: Authority,
    /// The tenants a cell credential covers.
    pub tenants: Vec<String>,
    pub cells: Vec<CellEntry>,
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
