//! The connection string a cell's service uses for its own database (`db.conn`).
//!
//! Where the credential comes from is behind [`DatabaseSource`] (decision D-19). The end state is
//! that the node agent makes a dedicated role and writes its connection string itself (K-19d), so
//! no run or person reads the administrator's credential. Until then a proving environment may use
//! [`AdministratorCopy`]. Nothing here prints a host, a user or a password.

use crate::error::PkiError;
use crate::manifest::{CellEntry, DatabaseKind};
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
            .read(cell.cloud, &db.credential_secret)
            .map_err(|_| PkiError::Secret("read: a cell's database credential"))?
            .unwrap_or_default();
        let password = secret.trim();
        if password.is_empty() {
            return Err(PkiError::Secret(
                "read: a cell's database has no administrator credential yet",
            ));
        }
        Ok(connection_string(&db.host, db.port, ADMIN_ROLE, password))
    }
}

/// A source that holds no credential, for the rehearsal and for tests.
pub struct Simulated;

impl DatabaseSource for Simulated {
    fn connection(&self, cell: &CellEntry) -> Result<String, PkiError> {
        let db = cell
            .database
            .as_ref()
            .ok_or(PkiError::Manifest("a cell has no database to connect to"))?;
        Ok(connection_string(
            &db.host,
            db.port,
            ADMIN_ROLE,
            "simulated",
        ))
    }
}
