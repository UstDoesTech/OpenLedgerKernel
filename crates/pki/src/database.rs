//! The connection string a cell's service uses for its own database (`db.conn`).
//!
//! Where the credential comes from is behind [`DatabaseSource`] (decision D-19). The end state is
//! that the node agent makes a dedicated role and writes its connection string itself (K-19d), so
//! no run or person reads the administrator's credential. Until then a proving environment may use
//! [`AdministratorCopy`]. Nothing here prints a host, a user or a password.

use crate::error::PkiError;
use crate::manifest::{CellEntry, Cloud, DatabaseKind};
use crate::store::SecretStore;

/// The role a self-hosted node's agent makes for the administrator.
pub const ADMIN_ROLE: &str = "ledger_admin";

pub trait DatabaseSource: Send + Sync {
    /// The connection string for this cell's own database, and no other.
    fn connection(&self, cell: &CellEntry) -> Result<String, PkiError>;
}

/// A libpq connection string, with the password quoted so any character in it is kept.
pub fn connection_string(host: &str, port: u16, user: &str, password: &str) -> String {
    let quoted = password.replace('\\', "\\\\").replace('\'', "\\'");
    format!(
        "host={host} port={port} user={user} password='{quoted}' dbname=postgres sslmode=require"
    )
}

/// Copies the administrator's credential from the secret the node agent wrote it to. For proving
/// environments only: it widens what the run can read, and the run then handles the credential.
pub struct AdministratorCopy<'a> {
    store: &'a dyn SecretStore,
}

impl<'a> AdministratorCopy<'a> {
    pub fn new(store: &'a dyn SecretStore) -> Self {
        AdministratorCopy { store }
    }
}

impl DatabaseSource for AdministratorCopy<'_> {
    fn connection(&self, cell: &CellEntry) -> Result<String, PkiError> {
        let db = cell
            .database
            .as_ref()
            .ok_or(PkiError::Manifest("a cell has no database to connect to"))?;
        if db.kind != DatabaseKind::SelfHosted {
            return Err(PkiError::Manifest(
                "only a self-hosted cell's credential is copied here",
            ));
        }
        let secret = self
            .store
            .read(cell.cloud, &db.credential_secret)?
            .unwrap_or_default();
        let password = secret.trim();
        // Azure's Key Vault cannot hold an empty secret, so OpenTofu makes the secret with this text
        // until the primary's first start replaces it (infra/modules/postgres/azure-self-hosted).
        // It is not a credential, and a connection built from it would only fail later, far away.
        if password.is_empty() || password == AZURE_PLACEHOLDER {
            return Err(PkiError::Store(match cell.cloud {
                Cloud::Aws => "read: a cell's database in aws has no administrator credential yet",
                Cloud::Azure => {
                    "read: a cell's database in azure has no administrator credential yet"
                }
                Cloud::Gcp => "read: a cell's database in gcp has no administrator credential yet",
            }));
        }
        Ok(connection_string(&db.host, db.port, ADMIN_ROLE, password))
    }
}

/// The role the service signs in as on a managed cell, unless the manifest says another (K-24ca).
pub const APPLICATION_ROLE: &str = "ledger_app";

/// A managed cell's `db.conn`: the host, port, role and database, and no password. The service asks
/// its token source for a short-lived token each time it connects (K-24a), so nothing here is a
/// secret, and nothing is read from the managed service's administrator secret.
pub fn identity_connection(db: &crate::manifest::Database) -> String {
    format!(
        "host={} port={} user={} dbname={} sslmode=require",
        db.host,
        db.port,
        db.role.as_deref().unwrap_or(APPLICATION_ROLE),
        db.name.as_deref().unwrap_or("postgres"),
    )
}

/// The value OpenTofu gives the Azure administrator secret until the primary writes the real one.
const AZURE_PLACEHOLDER: &str = "set-by-the-primary-on-first-start";

/// A source that holds no credential: the string it gives names the host and the role and has no
/// password. For the rehearsal and for tests.
pub struct Simulated;

impl DatabaseSource for Simulated {
    fn connection(&self, cell: &CellEntry) -> Result<String, PkiError> {
        let db = cell
            .database
            .as_ref()
            .ok_or(PkiError::Manifest("a cell has no database to connect to"))?;
        Ok(format!(
            "host={} port={} user={ADMIN_ROLE} dbname=postgres sslmode=require",
            db.host, db.port
        ))
    }
}
