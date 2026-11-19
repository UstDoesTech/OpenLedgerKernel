//! Holds and available balance. See `docs/design.md#data-model`.
//!
//! A hold reserves an amount against an account without moving its posted balance. Available
//! balance is posted, minus active holds, plus the arranged overdraft, and every debit
//! authorises against it inside the database (`post_leg`, migration 4). A hold ends in one of
//! three ways: [`release`], [`convert`] into the debit of a journal, or [`expire`]. Each is safe
//! to repeat, and a hold that has already ended another way is refused.

use ledger_core::{AccountId, CommandId, EntryId, HoldId, JournalId, Money, TenantId};
use tokio_postgres::Client;

use crate::leg::classify;
use crate::{LegError, Posted, validate_schema_name};

/// Where a hold is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldState {
    Active,
    Released,
    Converted,
    Expired,
}

impl HoldState {
    fn parse(s: &str) -> Result<Self, LegError> {
        Ok(match s {
            "active" => HoldState::Active,
            "released" => HoldState::Released,
            "converted" => HoldState::Converted,
            "expired" => HoldState::Expired,
            other => return Err(LegError::BadId(other.to_owned())),
        })
    }
}

/// A hold to place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewHold {
    pub tenant: TenantId,
    pub hold: HoldId,
    pub account: AccountId,
    pub amount: Money,
    /// A repeat of the same command returns the hold it made.
    pub command: Option<CommandId>,
    /// When the hold lapses, in RFC 3339. A sweep by [`expire`] ends it.
    pub expires_at: Option<String>,
    pub epoch: i64,
}

/// A hold as the database holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoldRecord {
    pub state: HoldState,
    pub amount: i64,
    pub journal: Option<String>,
    /// True when this was a repeat and nothing changed.
    pub replayed: bool,
}

fn record(row: &tokio_postgres::Row) -> Result<HoldRecord, LegError> {
    Ok(HoldRecord {
        state: HoldState::parse(row.get::<_, &str>("state"))?,
        amount: row.get("amount"),
        journal: row.get("journal_id"),
        replayed: row.get("replayed"),
    })
}

/// Places a hold after checking the account's available balance can cover it.
pub async fn place(client: &Client, schema: &str, h: &NewHold) -> Result<HoldRecord, LegError> {
    validate_schema_name(schema)?;
    if h.amount.minor() <= 0 {
        return Err(LegError::NotPositive);
    }
    let row = client
        .query_one(
            &format!("select * from {schema}.place_hold($1,$2,$3,$4,$5,$6,$7)"),
            &[
                &h.tenant.as_str(),
                &h.hold.as_str(),
                &h.account.as_str(),
                &h.amount.minor(),
                &h.command.as_ref().map(CommandId::as_str),
                &h.expires_at,
                &h.epoch,
            ],
        )
        .await
        .map_err(classify)?;
    record(&row)
}

/// Releases an active hold.
pub async fn release(
    client: &Client,
    schema: &str,
    tenant: &TenantId,
    hold: &HoldId,
    epoch: i64,
) -> Result<HoldRecord, LegError> {
    validate_schema_name(schema)?;
    let row = client
        .query_one(
            &format!("select * from {schema}.release_hold($1,$2,$3)"),
            &[&tenant.as_str(), &hold.as_str(), &epoch],
        )
        .await
        .map_err(classify)?;
    record(&row)
}

/// The debit a hold becomes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conversion {
    pub tenant: TenantId,
    pub hold: HoldId,
    pub journal: JournalId,
    /// The amount debited. `None` means the hold's own amount.
    pub amount: Option<i64>,
    pub counterparty: Option<AccountId>,
    pub origin: String,
    pub epoch: i64,
    pub entry: EntryId,
}

/// Turns an active hold into the debit of a journal, in one call. If the debit is refused the
/// hold is still active.
pub async fn convert(
    client: &Client,
    schema: &str,
    c: &Conversion,
) -> Result<(HoldState, Posted), LegError> {
    validate_schema_name(schema)?;
    let row = client
        .query_one(
            &format!("select * from {schema}.convert_hold($1,$2,$3,$4,$5,$6,$7,$8)"),
            &[
                &c.tenant.as_str(),
                &c.hold.as_str(),
                &c.journal.as_str(),
                &c.amount,
                &c.counterparty.as_ref().map(AccountId::as_str),
                &c.origin,
                &c.epoch,
                &c.entry.as_str(),
            ],
        )
        .await
        .map_err(classify)?;
    let entry: String = row.get("entry_id");
    Ok((
        HoldState::parse(row.get::<_, &str>("state"))?,
        Posted {
            entry: entry
                .parse()
                .map_err(|e: ledger_core::IdError| LegError::BadId(e.to_string()))?,
            epoch: row.get("epoch"),
            seq: row.get("seq"),
            balance: row.get("balance"),
            watermark: row.get("watermark"),
            replayed: row.get("replayed"),
        },
    ))
}

/// Expires every active hold whose time is at or before `now` (RFC 3339), and returns the ids it
/// expired. A repeat returns none.
pub async fn expire(
    client: &Client,
    schema: &str,
    tenant: &TenantId,
    now: &str,
) -> Result<Vec<String>, LegError> {
    validate_schema_name(schema)?;
    let rows = client
        .query(
            &format!("select hold_id from {schema}.expire_holds($1,$2)"),
            &[&tenant.as_str(), &now],
        )
        .await
        .map_err(classify)?;
    Ok(rows.iter().map(|r| r.get(0)).collect())
}

/// Available balance: posted, minus active holds, plus the arranged overdraft.
pub async fn available(
    client: &Client,
    schema: &str,
    tenant: &TenantId,
    account: &AccountId,
) -> Result<i64, LegError> {
    validate_schema_name(schema)?;
    let row = client
        .query_one(
            &format!("select {schema}.available_balance($1,$2)"),
            &[&tenant.as_str(), &account.as_str()],
        )
        .await
        .map_err(classify)?;
    row.try_get::<_, Option<i64>>(0)?
        .ok_or_else(|| LegError::UnknownAccount(account.as_str().to_owned()))
}
