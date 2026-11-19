//! The ledger's invariants, checked against real cells. See
//! `docs/design.md#invariants-checked-continuously`. This is the Rust counterpart of
//! `ledger/checks.py`, with the cell trial balance and in-transit age the design adds.
//!
//! [`check_cell`] runs the invariants that need one cell. [`compare_chains`] runs the one that
//! compares cells, because it can only hold once replication has caught up. Both only read.
//! Every violation names the cell, and the account, journal or tenant that broke the rule, and
//! never carries more than identifiers and numbers.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fmt;

use sha2::{Digest, Sha256};
use tokio_postgres::Client;

/// Limits for the invariants that depend on time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// A journal pending longer than this is a failure: its in-transit value should have drained.
    pub in_transit_max_age_secs: i64,
    /// An unresolved quarantine entry at least this old is a failure. Zero means any.
    pub quarantine_max_age_secs: i64,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            in_transit_max_age_secs: 300,
            quarantine_max_age_secs: 0,
        }
    }
}

/// One broken invariant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// Which invariant, such as `projection`.
    pub invariant: &'static str,
    /// The cell where it was found. For [`compare_chains`], the cells that disagree.
    pub cell: String,
    /// What broke it, such as `account=acc_a` or `journal=jr_x`.
    pub subject: String,
    pub detail: String,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "FAIL {} cell={} {}: {}",
            self.invariant, self.cell, self.subject, self.detail
        )
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CheckError {
    #[error(transparent)]
    Schema(#[from] ledger_store::MigrationError),
    #[error(transparent)]
    Database(#[from] tokio_postgres::Error),
}

fn v(invariant: &'static str, cell: &str, subject: String, detail: String) -> Violation {
    Violation {
        invariant,
        cell: cell.to_owned(),
        subject,
        detail,
    }
}

/// Checks every invariant that needs one cell: projection, sequence holes, journals netting to
/// zero, the cell's trial balance, overdrawn accounts, in-transit age and quarantine.
pub async fn check_cell(
    client: &Client,
    schema: &str,
    cell: &str,
    options: &Options,
) -> Result<Vec<Violation>, CheckError> {
    ledger_store::validate_schema_name(schema)?;
    let mut out = Vec::new();

    // Each account's balance equals the sum of its entries.
    for r in client
        .query(
            &format!(
                "select a.tenant_id, a.id, a.balance, coalesce(sum(e.amount), 0)::bigint as entries
                   from {schema}.account a
                   left join {schema}.entry e on e.tenant_id = a.tenant_id and e.account_id = a.id
                  group by a.tenant_id, a.id, a.balance
                 having a.balance <> coalesce(sum(e.amount), 0)
                  order by a.tenant_id, a.id"
            ),
            &[],
        )
        .await?
    {
        let (t, a, bal, sum): (String, String, i64, i64) = (r.get(0), r.get(1), r.get(2), r.get(3));
        out.push(v(
            "projection",
            cell,
            format!("account={a}"),
            format!("tenant {t}: balance {bal} but entries sum to {sum}"),
        ));
    }

    // Sequence numbers have no holes within an epoch.
    for r in client
        .query(
            &format!(
                "select tenant_id, account_id, epoch, count(*) as n, max(seq) as top
                   from {schema}.entry
                  group by tenant_id, account_id, epoch
                 having count(*) <> max(seq)
                  order by tenant_id, account_id, epoch"
            ),
            &[],
        )
        .await?
    {
        let (t, a, e, n, top): (String, String, i64, i64, i64) =
            (r.get(0), r.get(1), r.get(2), r.get(3), r.get(4));
        out.push(v(
            "sequence_gap",
            cell,
            format!("account={a}"),
            format!("tenant {t}, epoch {e}: {n} entries but the highest sequence is {top}"),
        ));
    }

    // Every journal nets to zero per currency.
    for r in client
        .query(
            &format!(
                "select e.tenant_id, e.journal_id, a.currency, sum(e.amount)::bigint as net
                   from {schema}.entry e
                   join {schema}.account a on a.tenant_id = e.tenant_id and a.id = e.account_id
                  group by e.tenant_id, e.journal_id, a.currency
                 having sum(e.amount) <> 0
                  order by e.tenant_id, e.journal_id"
            ),
            &[],
        )
        .await?
    {
        let (t, j, c, net): (String, String, String, i64) =
            (r.get(0), r.get(1), r.get(2), r.get(3));
        out.push(v(
            "journal_unbalanced",
            cell,
            format!("journal={j}"),
            format!("tenant {t}: nets to {net} {c}, not zero"),
        ));
    }

    // The cell's trial balance is zero per tenant and currency, by entries and by balances.
    for r in client
        .query(
            &format!(
                "select a.tenant_id, a.currency,
                        coalesce(sum(e.amount), 0)::bigint as entries
                   from {schema}.account a
                   join {schema}.entry e on e.tenant_id = a.tenant_id and e.account_id = a.id
                  group by a.tenant_id, a.currency
                 having sum(e.amount) <> 0
                  order by a.tenant_id, a.currency"
            ),
            &[],
        )
        .await?
    {
        let (t, c, sum): (String, String, i64) = (r.get(0), r.get(1), r.get(2));
        out.push(v(
            "trial_balance",
            cell,
            format!("tenant={t}"),
            format!("entries in {c} sum to {sum}, not zero"),
        ));
    }
    for r in client
        .query(
            &format!(
                "select tenant_id, currency, sum(balance)::bigint
                   from {schema}.account
                  group by tenant_id, currency
                 having sum(balance) <> 0
                  order by tenant_id, currency"
            ),
            &[],
        )
        .await?
    {
        let (t, c, sum): (String, String, i64) = (r.get(0), r.get(1), r.get(2));
        out.push(v(
            "trial_balance",
            cell,
            format!("tenant={t}"),
            format!("balances in {c} sum to {sum}, not zero"),
        ));
    }

    // No account without an overdraft allowance is negative.
    for r in client
        .query(
            &format!(
                "select tenant_id, id, balance, overdraft_limit from {schema}.account
                  where not allow_negative and balance < -overdraft_limit
                  order by tenant_id, id"
            ),
            &[],
        )
        .await?
    {
        let (t, a, bal, limit): (String, String, i64, i64) =
            (r.get(0), r.get(1), r.get(2), r.get(3));
        out.push(v(
            "overdrawn",
            cell,
            format!("account={a}"),
            format!("tenant {t}: balance {bal} is below the overdraft allowance of {limit}"),
        ));
    }

    // Every journal between legs finishes within its time limit.
    for r in client
        .query(
            &format!(
                "select j.tenant_id, j.id, extract(epoch from now() - j.created_at)::bigint as age
                   from {schema}.journal j
                  where j.state = 'pending'
                    and not exists (select 1 from {schema}.settlement s
                                     where s.tenant_id = j.tenant_id and s.journal_id = j.id)
                    and now() - j.created_at > make_interval(secs => $1::bigint)
                  order by j.created_at, j.id"
            ),
            &[&options.in_transit_max_age_secs],
        )
        .await?
    {
        let (t, j, age): (String, String, i64) = (r.get(0), r.get(1), r.get(2));
        out.push(v(
            "in_transit_age",
            cell,
            format!("journal={j}"),
            format!(
                "tenant {t}: pending for {age}s, over the limit of {}s",
                options.in_transit_max_age_secs
            ),
        ));
    }

    // Unresolved quarantine is not left to age.
    for r in client
        .query(
            &format!(
                "select tenant_id, id, account_id, extract(epoch from now() - quarantined_at)::bigint
                   from {schema}.quarantine
                  where not resolved
                    and now() - quarantined_at >= make_interval(secs => $1::bigint)
                  order by quarantined_at, id"
            ),
            &[&options.quarantine_max_age_secs],
        )
        .await?
    {
        let (t, e, a, age): (String, String, String, i64) =
            (r.get(0), r.get(1), r.get(2), r.get(3));
        out.push(v(
            "quarantine",
            cell,
            format!("account={a}"),
            format!("tenant {t}: entry {e} unresolved for {age}s"),
        ));
    }
    Ok(out)
}

/// A hash chain per account over the entries in (epoch, seq) order, as `ledger/checks.py` does.
async fn chains(
    client: &Client,
    schema: &str,
) -> Result<BTreeMap<(String, String), String>, CheckError> {
    let rows = client
        .query(
            &format!(
                "select tenant_id, account_id, id, epoch, seq, amount from {schema}.entry
                  order by tenant_id, account_id, epoch, seq"
            ),
            &[],
        )
        .await?;
    let mut hashers: BTreeMap<(String, String), Sha256> = BTreeMap::new();
    for r in rows {
        let (t, a, id, epoch, seq, amount): (String, String, String, i64, i64, i64) =
            (r.get(0), r.get(1), r.get(2), r.get(3), r.get(4), r.get(5));
        hashers
            .entry((t, a))
            .or_default()
            .update(format!("{id}|{epoch}|{seq}|{amount};").as_bytes());
    }
    Ok(hashers
        .into_iter()
        .map(|(k, h)| {
            let hex = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
            (k, hex)
        })
        .collect())
}

/// Hash chains over (epoch, sequence) match across cells for every account. It holds once
/// replication has caught up, so run it only where the cells are meant to hold the same log.
pub async fn compare_chains(
    cells: &[(&str, &Client)],
    schema: &str,
) -> Result<Vec<Violation>, CheckError> {
    ledger_store::validate_schema_name(schema)?;
    let mut all = Vec::new();
    for (name, client) in cells {
        all.push((*name, chains(client, schema).await?));
    }
    let mut accounts: BTreeMap<(String, String), ()> = BTreeMap::new();
    for (_, m) in &all {
        for k in m.keys() {
            accounts.insert(k.clone(), ());
        }
    }
    let mut out = Vec::new();
    for ((t, a), ()) in accounts {
        let hashes: Vec<Option<&String>> = all
            .iter()
            .map(|(_, m)| m.get(&(t.clone(), a.clone())))
            .collect();
        if hashes.windows(2).any(|w| w[0] != w[1]) {
            let detail = all
                .iter()
                .zip(&hashes)
                .map(|((n, _), h)| format!("{n}={}", h.map_or("none", |s| &s[..12])))
                .collect::<Vec<_>>()
                .join(" ");
            out.push(v(
                "chain_divergence",
                &all.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(","),
                format!("account={a}"),
                format!("tenant {t}: the cells hold different logs ({detail})"),
            ));
        }
    }
    Ok(out)
}
