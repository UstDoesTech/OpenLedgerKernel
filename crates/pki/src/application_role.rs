//! The application role of a managed cell (K-24ca). The statements live in the API crate, because the
//! API machine runs them too (K-24cc). This re-exports them and turns their errors into this
//! crate's, so `ledger-pki managed-role` and its tests are as they were.

pub use ledger_api::application_role::*;

use crate::error::PkiError;

impl From<RoleError> for PkiError {
    fn from(e: RoleError) -> Self {
        match e {
            RoleError::Name(why) => PkiError::Manifest(why),
            RoleError::Step(doing) => PkiError::Command {
                code: 1,
                doing,
                tool: "the database",
                cause: "the statement was refused",
            },
        }
    }
}
