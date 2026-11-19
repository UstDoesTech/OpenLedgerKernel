//! Journals across cells: a transfer between accounts homed on two cells, in steps that leave
//! every cell's books balanced at every instant. See `docs/design.md#journals-across-cells`.
//!
//! 1. [`send`] at the payer's cell: debit the payer, credit the sending in-transit account, in
//!    one SQL call. A decline (funds, fence) writes nothing.
//! 2. [`receive`] at the payee's cell: debit the receiving in-transit account, credit the payee,
//!    in one SQL call.
//! 3. [`settle`] at the payer's cell: mark the journal posted.
//!
//! Every step is safe to repeat, so [`recover`] finishes a transfer interrupted between steps by
//! doing steps 2 and 3 again, whoever else is doing the same. The journal id keys all of it.

use ledger_core::{AccountId, JournalId, Money, TenantId};
use tokio_postgres::Client;

use crate::LegError;
use crate::leg::classify;
use crate::validate_schema_name;

/// A transfer between a payer on one cell and a payee on another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transfer {
    pub tenant: TenantId,
    pub journal: JournalId,
    /// Who asked, such as `usr_ada`.
    pub actor: String,
    pub payer: AccountId,
    pub payee: AccountId,
    /// The in-transit account homed at the payer's cell for this pair of cells.
    pub sending: AccountId,
    /// The in-transit account homed at the payee's cell for this pair of cells.
    pub receiving: AccountId,
    pub amount: Money,
    pub payer_cell: String,
    pub payee_cell: String,
    pub payer_epoch: i64,
    pub sending_epoch: i64,
    pub receiving_epoch: i64,
    pub payee_epoch: i64,
}

/// What one step did at one cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// The cell's watermark after the step.
    pub watermark: i64,
    /// The balance of the customer's account after the step.
    pub balance: i64,
    /// True when the step had been done before and nothing was written.
    pub replayed: bool,
}

/// A journal found at the payer's cell with its first half written and no settlement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unfinished {
    pub tenant: String,
    pub journal: String,
    pub payer: String,
    pub payee: String,
    pub sending: String,
    pub amount: i64,
    pub currency: String,
    pub payer_cell: String,
    pub payee_cell: String,
}

/// What a recovery pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Recovered {
    /// Journals this pass settled. A journal another pass settled first is not counted.
    pub settled: Vec<String>,
    /// Journals found unfinished.
    pub found: usize,
}

/// The id of the entry for a leg: the journal's own id and the leg number, so a retry always
/// names the same entry.
pub(crate) fn entry_id(journal: &JournalId, leg: u8) -> String {
    let suffix = journal.as_str().trim_start_matches("jr_");
    format!("ent_{suffix}{leg}")
}

fn step(row: &tokio_postgres::Row) -> Step {
    Step {
        watermark: row.get("watermark"),
        balance: row.get("balance"),
        replayed: row.get("replayed"),
    }
}

/// Step 1, at the payer's cell. Records the journal as pending, debits the payer and credits
/// the sending in-transit account, in one call. If the payer cannot afford it, or the epoch is
/// stale, nothing is written.
pub async fn send(client: &Client, schema: &str, t: &Transfer) -> Result<Step, LegError> {
    validate_schema_name(schema)?;
    if t.amount.minor() <= 0 {
        return Err(LegError::NotPositive);
    }
    let row = client
        .query_one(
            &format!(
                "select * from {schema}.post_send($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)"
            ),
            &[
                &t.tenant.as_str(),
                &t.journal.as_str(),
                &t.actor,
                &t.payer.as_str(),
                &t.payee.as_str(),
                &t.sending.as_str(),
                &t.amount.minor(),
                &t.amount.currency().to_string(),
                &t.payer_cell,
                &t.payer_epoch,
                &t.sending_epoch,
                &entry_id(&t.journal, 1),
                &entry_id(&t.journal, 2),
            ],
        )
        .await
        .map_err(classify)?;
    Ok(step(&row))
}

/// Step 2, at the payee's cell. Debits the receiving in-transit account and credits the payee,
/// in one call. A repeat writes nothing.
pub async fn receive(client: &Client, schema: &str, t: &Transfer) -> Result<Step, LegError> {
    validate_schema_name(schema)?;
    let row = client
        .query_one(
            &format!("select * from {schema}.post_receive($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)"),
            &[
                &t.tenant.as_str(),
                &t.journal.as_str(),
                &t.payer.as_str(),
                &t.payee.as_str(),
                &t.receiving.as_str(),
                &t.amount.minor(),
                &t.amount.currency().to_string(),
                &t.payee_cell,
                &t.receiving_epoch,
                &t.payee_epoch,
                &entry_id(&t.journal, 3),
                &entry_id(&t.journal, 4),
            ],
        )
        .await
        .map_err(classify)?;
    Ok(step(&row))
}

/// Step 3, at the payer's cell. Marks the journal posted. Returns true if this call did it.
pub async fn settle(
    client: &Client,
    schema: &str,
    tenant: &TenantId,
    journal: &JournalId,
) -> Result<bool, LegError> {
    validate_schema_name(schema)?;
    let row = client
        .query_one(
            &format!("select {schema}.settle_journal($1, $2)"),
            &[&tenant.as_str(), &journal.as_str()],
        )
        .await
        .map_err(classify)?;
    Ok(row.get(0))
}

/// The whole transfer, for a caller that is not going to crash: the three steps in order.
pub async fn transfer(
    payer_cell: &Client,
    payee_cell: &Client,
    schema: &str,
    t: &Transfer,
) -> Result<bool, LegError> {
    send(payer_cell, schema, t).await?;
    receive(payee_cell, schema, t).await?;
    settle(payer_cell, schema, &t.tenant, &t.journal).await
}

/// The journals at the payer's cell whose first half is written and which are not settled.
pub async fn unfinished(
    payer_cell: &Client,
    schema: &str,
    tenant: &TenantId,
) -> Result<Vec<Unfinished>, LegError> {
    validate_schema_name(schema)?;
    let rows = payer_cell
        .query(
            &format!(
                "select j.id as journal, d.account_id as payer, d.counterparty as payee,
                        c.account_id as sending, -d.amount as amount, a.currency,
                        t.from_cell, t.to_cell
                   from {schema}.journal j
                   join {schema}.entry d on d.tenant_id = j.tenant_id and d.journal_id = j.id and d.leg = 1
                   join {schema}.entry c on c.tenant_id = j.tenant_id and c.journal_id = j.id and c.leg = 2
                   join {schema}.account a on a.tenant_id = d.tenant_id and a.id = d.account_id
                   join {schema}.in_transit_account t on t.tenant_id = c.tenant_id and t.account_id = c.account_id
                  where j.tenant_id = $1 and j.state = 'pending'
                    and not exists (select 1 from {schema}.settlement s
                                     where s.tenant_id = j.tenant_id and s.journal_id = j.id)
                  order by j.created_at, j.id"
            ),
            &[&tenant.as_str()],
        )
        .await?;
    Ok(rows
        .iter()
        .map(|r| Unfinished {
            tenant: tenant.as_str().to_owned(),
            journal: r.get("journal"),
            payer: r.get("payer"),
            payee: r.get("payee"),
            sending: r.get("sending"),
            amount: r.get("amount"),
            currency: r.get("currency"),
            payer_cell: r.get("from_cell"),
            payee_cell: r.get("to_cell"),
        })
        .collect())
}

/// Finishes every unfinished journal: step 2 at the payee's cell, then step 3 at the payer's.
/// Everything it needs is read from the two cells, and every step is safe to repeat, so any
/// number of recoveries may run at once and each journal is settled exactly once.
pub async fn recover(
    payer_cell: &Client,
    payee_cell: &Client,
    schema: &str,
    tenant: &TenantId,
) -> Result<Recovered, LegError> {
    let todo = unfinished(payer_cell, schema, tenant).await?;
    let mut out = Recovered {
        found: todo.len(),
        ..Recovered::default()
    };
    for u in todo {
        let receiving: String = payee_cell
            .query_one(
                &format!(
                    "select account_id from {schema}.in_transit_receipt
                      where tenant_id = $1 and from_cell = $2 and to_cell = $3"
                ),
                &[&u.tenant, &u.payer_cell, &u.payee_cell],
            )
            .await?
            .get(0);
        let epoch = |account: String| {
            let tenant = u.tenant.clone();
            async move {
                payee_cell
                    .query_one(
                        &format!(
                            "select coalesce(max(epoch), 0)::bigint from {schema}.epoch_fence
                              where tenant_id = $1 and account_id = $2"
                        ),
                        &[&tenant, &account],
                    )
                    .await
                    .map(|r| r.get::<_, i64>(0))
            }
        };
        let t = Transfer {
            tenant: tenant.clone(),
            journal: u
                .journal
                .parse()
                .map_err(|e: ledger_core::IdError| LegError::BadId(e.to_string()))?,
            actor: String::new(),
            payer: u
                .payer
                .parse()
                .map_err(|e: ledger_core::IdError| LegError::BadId(e.to_string()))?,
            payee: u
                .payee
                .parse()
                .map_err(|e: ledger_core::IdError| LegError::BadId(e.to_string()))?,
            sending: u
                .sending
                .parse()
                .map_err(|e: ledger_core::IdError| LegError::BadId(e.to_string()))?,
            receiving: receiving
                .parse()
                .map_err(|e: ledger_core::IdError| LegError::BadId(e.to_string()))?,
            amount: Money::new(
                u.amount,
                u.currency
                    .parse()
                    .map_err(|_| LegError::BadId(u.currency.clone()))?,
            ),
            payer_cell: u.payer_cell.clone(),
            payee_cell: u.payee_cell.clone(),
            payer_epoch: 0,
            sending_epoch: 0,
            receiving_epoch: epoch(receiving.clone()).await?,
            payee_epoch: epoch(u.payee.clone()).await?,
        };
        receive(payee_cell, schema, &t).await?;
        if settle(payer_cell, schema, tenant, &t.journal).await? {
            out.settled.push(u.journal);
        }
    }
    Ok(out)
}

/// The in-transit accounts for an ordered pair of cells, in one currency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pair {
    pub tenant: TenantId,
    pub from_cell: String,
    pub to_cell: String,
    pub currency: String,
    pub sending: AccountId,
    pub receiving: AccountId,
}

/// Gets the cell `here` ready to carry value for the pair: registers both accounts and fences the
/// one homed at `here`. Run it at both cells before the first transfer. Safe to repeat.
pub async fn prepare(
    client: &Client,
    schema: &str,
    pair: &Pair,
    here: &str,
) -> Result<(), LegError> {
    validate_schema_name(schema)?;
    client
        .execute(
            &format!("select {schema}.prepare_in_transit($1,$2,$3,$4,$5,$6,$7)"),
            &[
                &pair.tenant.as_str(),
                &pair.from_cell,
                &pair.to_cell,
                &pair.currency,
                &pair.sending.as_str(),
                &pair.receiving.as_str(),
                &here,
            ],
        )
        .await
        .map_err(classify)?;
    Ok(())
}
