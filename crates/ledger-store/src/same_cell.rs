//! A transfer between two accounts homed at one cell: one SQL call writes the journal, both legs
//! and the settlement, so the cell never holds half of it. A decline writes nothing.

use ledger_core::{AccountId, JournalId, Money, TenantId};
use tokio_postgres::Client;

use crate::LegError;
use crate::cross_cell::{Step, entry_id};
use crate::leg::classify;
use crate::validate_schema_name;

/// A transfer between two accounts at one cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SameCell {
    pub tenant: TenantId,
    pub journal: JournalId,
    /// Who asked, such as `usr_ada`.
    pub actor: String,
    pub payer: AccountId,
    pub payee: AccountId,
    pub amount: Money,
    pub cell: String,
    pub payer_epoch: i64,
    pub payee_epoch: i64,
}

/// Posts the transfer. A repeat of the same journal writes nothing and says so.
pub async fn post(client: &Client, schema: &str, t: &SameCell) -> Result<Step, LegError> {
    validate_schema_name(schema)?;
    if t.amount.minor() <= 0 {
        return Err(LegError::NotPositive);
    }
    let row = client
        .query_one(
            &format!(
                "select * from {schema}.post_same_cell($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)"
            ),
            &[
                &t.tenant.as_str(),
                &t.journal.as_str(),
                &t.actor,
                &t.payer.as_str(),
                &t.payee.as_str(),
                &t.amount.minor(),
                &t.amount.currency().to_string(),
                &t.cell,
                &t.payer_epoch,
                &t.payee_epoch,
                &entry_id(&t.journal, 1),
                &entry_id(&t.journal, 2),
            ],
        )
        .await
        .map_err(classify)?;
    Ok(Step {
        watermark: row.get("watermark"),
        balance: row.get("balance"),
        replayed: row.get("replayed"),
    })
}
