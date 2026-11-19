//! Posting one leg of a journal: the write path's single SQL call.
//!
//! [`LegWriter::post`] sends one statement, `post_leg(...)` from migration 2, which locks the account,
//! answers a repeat, checks the epoch fence and the funds, writes the entry, updates the balance
//! and writes the outbox row, then returns the outbox position as the watermark. The checks live
//! in the database so that no writer, however stale or buggy, can get around them.
//! See `docs/design.md#write-path-performance`.

use ledger_core::{AccountId, EntryId, JournalId, Money, TenantId};
use tokio_postgres::error::SqlState;
use tokio_postgres::types::Type;
use tokio_postgres::{Client, Statement};

use crate::validate_schema_name;

/// Which side of the account a leg is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// Takes money out of the account.
    Debit,
    /// Puts money into the account.
    Credit,
}

impl Side {
    fn as_sql(self) -> &'static str {
        match self {
            Side::Debit => "debit",
            Side::Credit => "credit",
        }
    }
}

/// One leg to post. The amount is positive: the side says which way it moves the balance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Leg {
    pub tenant: TenantId,
    pub journal: JournalId,
    pub account: AccountId,
    /// Which leg of the journal this is, from 1. A journal may have several on one account.
    pub leg: i16,
    pub side: Side,
    pub amount: Money,
    pub counterparty: Option<AccountId>,
    /// The cell writing the entry, such as `aws-lon-03`.
    pub origin: String,
    /// The epoch the writer holds. The database refuses it unless it is the account's latest.
    pub epoch: i64,
    /// The id the entry gets. A repeat of the same leg returns the entry already written, whatever
    /// id the repeat carries.
    pub entry: EntryId,
}

/// What posting a leg did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Posted {
    pub entry: EntryId,
    pub epoch: i64,
    pub seq: i64,
    /// The account's balance after the leg.
    pub balance: i64,
    /// The outbox position of the entry: how far this cell's log has reached.
    pub watermark: i64,
    /// True when this was a repeat and nothing was written.
    pub replayed: bool,
}

/// Why a leg was refused.
#[derive(Debug, thiserror::Error)]
pub enum LegError {
    #[error("the amount of a leg must be positive")]
    NotPositive,
    #[error("insufficient funds: {0}")]
    InsufficientFunds(String),
    #[error("fenced: {0}")]
    Fenced(String),
    #[error("idempotency conflict: {0}")]
    IdempotencyConflict(String),
    #[error("unknown account: {0}")]
    UnknownAccount(String),
    #[error("currency mismatch: {0}")]
    CurrencyMismatch(String),
    #[error("hold in the wrong state: {0}")]
    HoldState(String),
    #[error("unknown hold: {0}")]
    UnknownHold(String),
    #[error("the database returned an id this build cannot read: {0}")]
    BadId(String),
    #[error(transparent)]
    Schema(#[from] crate::MigrationError),
    #[error(transparent)]
    Database(#[from] tokio_postgres::Error),
}

const INSUFFICIENT_FUNDS: &str = "LE001";
const FENCED: &str = "LE002";
const IDEMPOTENCY_CONFLICT: &str = "LE003";
const UNKNOWN_ACCOUNT: &str = "LE004";
const CURRENCY_MISMATCH: &str = "LE005";
const NOT_POSITIVE: &str = "LE006";
const HOLD_STATE: &str = "LE008";
const UNKNOWN_HOLD: &str = "LE009";

pub(crate) fn classify(e: tokio_postgres::Error) -> LegError {
    let Some(db) = e.as_db_error() else {
        return LegError::Database(e);
    };
    let message = db.message().to_owned();
    match db.code() {
        c if *c == SqlState::from_code(INSUFFICIENT_FUNDS) => LegError::InsufficientFunds(message),
        c if *c == SqlState::from_code(FENCED) => LegError::Fenced(message),
        c if *c == SqlState::from_code(IDEMPOTENCY_CONFLICT) => {
            LegError::IdempotencyConflict(message)
        }
        c if *c == SqlState::from_code(UNKNOWN_ACCOUNT) => LegError::UnknownAccount(message),
        c if *c == SqlState::from_code(CURRENCY_MISMATCH) => LegError::CurrencyMismatch(message),
        c if *c == SqlState::from_code(NOT_POSITIVE) => LegError::NotPositive,
        c if *c == SqlState::from_code(HOLD_STATE) => LegError::HoldState(message),
        c if *c == SqlState::from_code(UNKNOWN_HOLD) => LegError::UnknownHold(message),
        _ => LegError::Database(e),
    }
}

/// Posts legs with one prepared statement, so each leg is one round trip to the database.
///
/// Prepare once for a connection and a schema, then call [`LegWriter::post`] as often as needed.
/// `schema` is the ledger schema the migrations were applied to.
pub struct LegWriter<'a> {
    client: &'a Client,
    statement: Statement,
}

impl<'a> LegWriter<'a> {
    pub async fn prepare(client: &'a Client, schema: &str) -> Result<Self, LegError> {
        validate_schema_name(schema)?;
        let statement = client
            .prepare_typed(
                &format!(
                    "select * from {schema}.post_leg($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)"
                ),
                &[
                    Type::TEXT,
                    Type::TEXT,
                    Type::TEXT,
                    Type::INT2,
                    Type::TEXT,
                    Type::INT8,
                    Type::TEXT,
                    Type::TEXT,
                    Type::TEXT,
                    Type::INT8,
                    Type::TEXT,
                ],
            )
            .await?;
        Ok(LegWriter { client, statement })
    }

    /// Posts one leg. The database does it all in one statement, so if it fails nothing is
    /// written.
    pub async fn post(&self, leg: &Leg) -> Result<Posted, LegError> {
        if leg.amount.minor() <= 0 {
            return Err(LegError::NotPositive);
        }
        let row = self
            .client
            .query_one(
                &self.statement,
                &[
                    &leg.tenant.as_str(),
                    &leg.journal.as_str(),
                    &leg.account.as_str(),
                    &leg.leg,
                    &leg.side.as_sql(),
                    &leg.amount.minor(),
                    &leg.amount.currency().to_string(),
                    &leg.counterparty.as_ref().map(AccountId::as_str),
                    &leg.origin,
                    &leg.epoch,
                    &leg.entry.as_str(),
                ],
            )
            .await
            .map_err(classify)?;
        let entry: String = row.get("entry_id");
        Ok(Posted {
            entry: entry
                .parse()
                .map_err(|e: ledger_core::IdError| LegError::BadId(e.to_string()))?,
            epoch: row.get("epoch"),
            seq: row.get("seq"),
            balance: row.get("balance"),
            watermark: row.get("watermark"),
            replayed: row.get("replayed"),
        })
    }
}
