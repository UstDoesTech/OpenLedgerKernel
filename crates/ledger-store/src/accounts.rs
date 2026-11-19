//! Opening and reading accounts at their home cell, for the API service.
//!
//! The account row and its first epoch are written in one SQL call. Which cell is home, and at
//! which epoch, is the control plane's to say, so the caller passes both in. See
//! `docs/design.md#cells-homes-and-epochs`.

use ledger_core::{AccountId, Currency, TenantId};
use tokio_postgres::Client;

use crate::LegError;
use crate::leg::classify;
use crate::validate_schema_name;

/// An account to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAccount {
    pub tenant: TenantId,
    pub account: AccountId,
    pub currency: Currency,
    /// How far below zero the account may go, in minor units. Zero for none.
    pub overdraft_limit: i64,
    /// The cell that is home for the account.
    pub home: String,
    /// The epoch the control plane gave it.
    pub epoch: i64,
}

/// What a cell holds for an account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountRecord {
    pub account: String,
    pub currency: String,
    pub balance: i64,
    pub overdraft_limit: i64,
}

/// Opens the account and its first epoch. True when this call made the account, false when it
/// was already there and nothing was written.
pub async fn open(client: &Client, schema: &str, a: &NewAccount) -> Result<bool, LegError> {
    validate_schema_name(schema)?;
    if a.overdraft_limit < 0 {
        return Err(LegError::NotPositive);
    }
    let row = client
        .query_one(
            &format!("select {schema}.open_account($1,$2,$3,$4,$5,$6)"),
            &[
                &a.tenant.as_str(),
                &a.account.as_str(),
                &a.currency.to_string(),
                &a.overdraft_limit,
                &a.home,
                &a.epoch,
            ],
        )
        .await
        .map_err(classify)?;
    Ok(row.get(0))
}

/// The account as this cell holds it, or none when the tenant has no such account here. Another
/// tenant's account is none too, so its existence does not show.
pub async fn get(
    client: &Client,
    schema: &str,
    tenant: &TenantId,
    account: &AccountId,
) -> Result<Option<AccountRecord>, LegError> {
    validate_schema_name(schema)?;
    let row = client
        .query_opt(
            &format!(
                "select id::text, currency::text, balance, overdraft_limit \
                 from {schema}.account where tenant_id = $1::text and id = $2::text and kind = 'customer'"
            ),
            &[&tenant.as_str(), &account.as_str()],
        )
        .await
        .map_err(classify)?;
    Ok(row.map(|r| AccountRecord {
        account: r.get(0),
        currency: r.get(1),
        balance: r.get(2),
        overdraft_limit: r.get(3),
    }))
}
