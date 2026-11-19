//! The outbox relay: a destination cell pulls a source cell's outbox and applies it, in batches,
//! over plain SQL. See `docs/design.md#replication`.
//!
//! * [`pull`] reads one batch of the source's outbox after this cell's cursor and applies it with
//!   one `apply_entries` call, which also moves the cursor. Applying is idempotent by entry id, so
//!   redelivery applies nothing twice.
//! * An entry that arrives before its turn is buffered by the database, never skipped.
//!   [`catch_up`] fetches what is missing from the source by account, in (epoch, seq) order, so a
//!   gap closes even when the outbox position it hid behind was passed.
//! * [`publish_watermark`] publishes how far a cell's outbox has reached, even when idle, and
//!   [`lag`] reports it honestly from the published figure.
//!
//! The rules that decide an entry live in migration 5, so the relay cannot get around them.

#![forbid(unsafe_code)]

pub mod strong;

use std::collections::BTreeMap;

use ledger_store::validate_schema_name;
use tokio_postgres::Client;

#[derive(Debug, thiserror::Error)]
pub enum ReplError {
    #[error(transparent)]
    Schema(#[from] ledger_store::MigrationError),
    #[error(transparent)]
    Database(#[from] tokio_postgres::Error),
}

/// One entry as it travels. Ids are the schema's own opaque identifiers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    pub journal: String,
    pub account: String,
    pub epoch: i64,
    pub seq: i64,
    pub leg: i16,
    pub kind: String,
    /// Signed by its effect on the balance, as stored.
    pub amount: i64,
    pub counterparty: Option<String>,
    pub origin: String,
    /// RFC 3339 text, so the entry keeps the time the home wrote it.
    pub created_at: String,
}

/// What one batch did at the destination.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Applied {
    pub applied: usize,
    pub dup: usize,
    pub quarantined: usize,
    /// Held in the buffer, waiting for what comes before them or for their epoch's fence.
    pub buffered: usize,
    /// Buffered earlier and applied now.
    pub drained: usize,
}

const ENTRY_COLUMNS: &str = "e.id, e.journal_id, e.account_id, e.epoch, e.seq, e.leg, e.kind, \
                             e.amount, e.counterparty, e.origin, e.created_at::text";

fn entry_of(r: &tokio_postgres::Row, at: usize) -> Entry {
    Entry {
        id: r.get(at),
        journal: r.get(at + 1),
        account: r.get(at + 2),
        epoch: r.get(at + 3),
        seq: r.get(at + 4),
        leg: r.get(at + 5),
        kind: r.get(at + 6),
        amount: r.get(at + 7),
        counterparty: r.get(at + 8),
        origin: r.get(at + 9),
        created_at: r.get(at + 10),
    }
}

/// Applies entries, in the order given, in one call. `source` and `last_position` move the
/// cursor for a pull and are `None` when filling a gap. Failures roll the whole batch back.
pub async fn apply(
    dest: &Client,
    schema: &str,
    tenant: &str,
    source: Option<(&str, i64)>,
    entries: &[Entry],
) -> Result<Applied, ReplError> {
    validate_schema_name(schema)?;
    let col = |f: fn(&Entry) -> String| entries.iter().map(f).collect::<Vec<String>>();
    let rows = dest
        .query(
            &format!(
                "select entry_id, outcome from {schema}.apply_entries($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)"
            ),
            &[
                &tenant,
                &source.map(|s| s.0),
                &source.map(|s| s.1),
                &col(|e| e.id.clone()),
                &col(|e| e.journal.clone()),
                &col(|e| e.account.clone()),
                &entries.iter().map(|e| e.epoch).collect::<Vec<i64>>(),
                &entries.iter().map(|e| e.seq).collect::<Vec<i64>>(),
                &entries.iter().map(|e| e.leg).collect::<Vec<i16>>(),
                &col(|e| e.kind.clone()),
                &entries.iter().map(|e| e.amount).collect::<Vec<i64>>(),
                &entries.iter().map(|e| e.counterparty.clone()).collect::<Vec<Option<String>>>(),
                &col(|e| e.origin.clone()),
                &col(|e| e.created_at.clone()),
            ],
        )
        .await?;
    let mut out = Applied::default();
    for r in rows {
        match r.get::<_, &str>(1) {
            "applied" => out.applied += 1,
            "dup" => out.dup += 1,
            "quarantined" => out.quarantined += 1,
            "gap" | "deferred" => out.buffered += 1,
            o if o.starts_with("drained_") => out.drained += 1,
            _ => {}
        }
    }
    Ok(out)
}

/// How far this cell has read the source's outbox.
pub async fn cursor(
    dest: &Client,
    schema: &str,
    tenant: &str,
    source_cell: &str,
) -> Result<i64, ReplError> {
    validate_schema_name(schema)?;
    Ok(dest
        .query_opt(
            &format!(
                "select last_position from {schema}.outbox_cursor where tenant_id = $1 and source_cell = $2"
            ),
            &[&tenant, &source_cell],
        )
        .await?
        .map_or(0, |r| r.get(0)))
}

/// What a pull did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PullReport {
    /// Outbox rows read.
    pub read: usize,
    pub applied: Applied,
    /// The cursor after the batch.
    pub cursor: i64,
}

/// Reads one batch of the source's outbox after the cursor and applies it in one call, which
/// also moves the cursor. Outbox rows whose entries left the log (void, moved to quarantine) move
/// the cursor but ship nothing.
pub async fn pull(
    source: &Client,
    dest: &Client,
    schema: &str,
    tenant: &str,
    source_cell: &str,
    batch: i64,
) -> Result<PullReport, ReplError> {
    validate_schema_name(schema)?;
    let after = cursor(dest, schema, tenant, source_cell).await?;
    let rows = source
        .query(
            &format!(
                "select o.position, {ENTRY_COLUMNS}
                   from {schema}.outbox o
                   left join {schema}.entry e on e.tenant_id = o.tenant_id and e.id = o.entry_id
                  where o.tenant_id = $1 and o.position > $2
                  order by o.position limit $3"
            ),
            &[&tenant, &after, &batch],
        )
        .await?;
    let Some(last) = rows.last().map(|r| r.get::<_, i64>(0)) else {
        return Ok(PullReport {
            cursor: after,
            ..PullReport::default()
        });
    };
    let entries: Vec<Entry> = rows
        .iter()
        .filter(|r| r.get::<_, Option<String>>(1).is_some())
        .map(|r| entry_of(r, 1))
        .collect();
    let applied = apply(dest, schema, tenant, Some((source_cell, last)), &entries).await?;
    Ok(PullReport {
        read: rows.len(),
        applied,
        cursor: last,
    })
}

/// Pulls batches until the source's outbox has nothing after the cursor.
pub async fn pull_all(
    source: &Client,
    dest: &Client,
    schema: &str,
    tenant: &str,
    source_cell: &str,
    batch: i64,
) -> Result<PullReport, ReplError> {
    let mut total = PullReport::default();
    loop {
        let r = pull(source, dest, schema, tenant, source_cell, batch).await?;
        total.read += r.read;
        total.cursor = r.cursor;
        total.applied.applied += r.applied.applied;
        total.applied.dup += r.applied.dup;
        total.applied.quarantined += r.applied.quarantined;
        total.applied.buffered += r.applied.buffered;
        total.applied.drained += r.applied.drained;
        if (r.read as i64) < batch {
            return Ok(total);
        }
    }
}

/// What a catch-up did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CaughtUp {
    /// Accounts where the source was ahead of the destination.
    pub accounts: usize,
    pub applied: Applied,
}

/// The highest (epoch, seq) each account has on a cell.
async fn high_water(
    c: &Client,
    schema: &str,
    tenant: &str,
) -> Result<BTreeMap<String, (i64, i64)>, ReplError> {
    let rows = c
        .query(
            &format!(
                "select distinct on (account_id) account_id, epoch, seq from {schema}.entry
                  where tenant_id = $1 order by account_id, epoch desc, seq desc"
            ),
            &[&tenant],
        )
        .await?;
    Ok(rows
        .iter()
        .map(|r| (r.get(0), (r.get(1), r.get(2))))
        .collect())
}

/// Fetches, by account and in (epoch, seq) order, whatever the source holds beyond what this cell
/// holds, and applies it. This closes a gap by fetching what is missing, and also finds an outbox
/// row the cursor passed before it became visible. It does not move the cursor.
pub async fn catch_up(
    source: &Client,
    dest: &Client,
    schema: &str,
    tenant: &str,
    batch: i64,
) -> Result<CaughtUp, ReplError> {
    validate_schema_name(schema)?;
    let (theirs, ours) = (
        high_water(source, schema, tenant).await?,
        high_water(dest, schema, tenant).await?,
    );
    let mut out = CaughtUp::default();
    for (account, top) in theirs {
        let have = ours.get(&account).copied().unwrap_or((0, 0));
        if top <= have {
            continue;
        }
        out.accounts += 1;
        let mut after = have;
        loop {
            let rows = source
                .query(
                    &format!(
                        "select {ENTRY_COLUMNS} from {schema}.entry e
                          where e.tenant_id = $1 and e.account_id = $2 and (e.epoch, e.seq) > ($3, $4)
                          order by e.epoch, e.seq limit $5"
                    ),
                    &[&tenant, &account, &after.0, &after.1, &batch],
                )
                .await?;
            let entries: Vec<Entry> = rows.iter().map(|r| entry_of(r, 0)).collect();
            let Some(last) = entries.last() else { break };
            after = (last.epoch, last.seq);
            let a = apply(dest, schema, tenant, None, &entries).await?;
            out.applied.applied += a.applied;
            out.applied.dup += a.dup;
            out.applied.quarantined += a.quarantined;
            out.applied.buffered += a.buffered;
            out.applied.drained += a.drained;
            if (entries.len() as i64) < batch {
                break;
            }
        }
    }
    Ok(out)
}

/// Publishes how far this cell's outbox has reached, even when nothing was written. Returns the
/// published position.
pub async fn publish_watermark(
    client: &Client,
    schema: &str,
    tenant: &str,
    cell: &str,
) -> Result<i64, ReplError> {
    validate_schema_name(schema)?;
    Ok(client
        .query_one(
            &format!("select {schema}.publish_watermark($1, $2)"),
            &[&tenant, &cell],
        )
        .await?
        .get(0))
}

/// The position a cell last published, and how many seconds ago.
pub async fn watermark(
    client: &Client,
    schema: &str,
    tenant: &str,
    cell: &str,
) -> Result<Option<(i64, i64)>, ReplError> {
    validate_schema_name(schema)?;
    Ok(client
        .query_opt(
            &format!(
                "select position, extract(epoch from now() - published_at)::bigint
                   from {schema}.cell_watermark where tenant_id = $1 and cell = $2"
            ),
            &[&tenant, &cell],
        )
        .await?
        .map(|r| (r.get(0), r.get(1))))
}

/// Entries the source has published that this cell has not yet read: the source's published
/// watermark minus this cell's cursor. It is honest because an idle source still publishes.
/// `None` until the source has published once.
pub async fn lag(
    source: &Client,
    dest: &Client,
    schema: &str,
    tenant: &str,
    source_cell: &str,
) -> Result<Option<i64>, ReplError> {
    let Some((published, _)) = watermark(source, schema, tenant, source_cell).await? else {
        return Ok(None);
    };
    let read = cursor(dest, schema, tenant, source_cell).await?;
    Ok(Some((published - read).max(0)))
}

/// A fence the control plane issued for an account: from `epoch` on, `home` writes it, and
/// entries of `prev_epoch` beyond `prev_cutoff_seq` are void.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fence {
    pub tenant: String,
    pub account: String,
    pub epoch: i64,
    pub home: String,
    pub prev_epoch: i64,
    pub prev_cutoff_seq: i64,
}

/// Installs a fence, which sweeps the account at once: entries it voids move to quarantine and
/// leave the balance. Safe to repeat. Returns true when it installed the fence.
pub async fn install_fence(client: &Client, schema: &str, f: &Fence) -> Result<bool, ReplError> {
    validate_schema_name(schema)?;
    Ok(client
        .query_one(
            &format!("select {schema}.install_fence($1,$2,$3,$4,$5,$6)"),
            &[
                &f.tenant,
                &f.account,
                &f.epoch,
                &f.home,
                &f.prev_epoch,
                &f.prev_cutoff_seq,
            ],
        )
        .await?
        .get(0))
}

/// The highest (epoch, seq) the cell holds for the account, which a promoted standby reports as
/// the cutoff for the old epoch. (0, 0) when it holds nothing.
pub async fn high_water_of(
    client: &Client,
    schema: &str,
    tenant: &str,
    account: &str,
) -> Result<(i64, i64), ReplError> {
    validate_schema_name(schema)?;
    let r = client
        .query_one(
            &format!("select epoch, seq from {schema}.high_water($1, $2)"),
            &[&tenant, &account],
        )
        .await?;
    Ok((r.get(0), r.get(1)))
}
