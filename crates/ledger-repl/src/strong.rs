//! The strong durability tier. See `docs/design.md#durability-tiers`.
//!
//! A strong write is an ordinary write at the home, followed by [`acknowledge`]: it returns only
//! once the remote cell holds every entry of the write. If the remote does not acknowledge in
//! time, the caller gets [`StrongError::DurabilityNotMet`] (`durability_not_met`, HTTP 504). The
//! write is committed at the home, so the caller retries with the same key: posting again
//! replays what was written (nothing is posted twice) and [`acknowledge`] completes the
//! acknowledgement.

use std::time::{Duration, Instant};

use tokio_postgres::Client;

use crate::{ReplError, catch_up, pull, validate_schema_name};

#[derive(Debug, thiserror::Error)]
pub enum StrongError {
    /// Committed at the home, but the remote cell did not apply it in time. Retry with the same key.
    #[error(
        "durability_not_met: committed at home, not applied by the remote cell within {waited:?}"
    )]
    DurabilityNotMet { waited: Duration },
    #[error(transparent)]
    Repl(#[from] ReplError),
}

impl StrongError {
    /// The error code in the API (`docs/api-design.md`).
    pub fn code(&self) -> &'static str {
        match self {
            StrongError::DurabilityNotMet { .. } => "durability_not_met",
            StrongError::Repl(_) => "internal",
        }
    }
}

/// What it took to acknowledge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Acknowledged {
    pub waited: Duration,
    /// Pulls it made. Zero when the remote already held the entries.
    pub pulls: usize,
}

async fn missing(
    remote: &Client,
    schema: &str,
    tenant: &str,
    entries: &[String],
) -> Result<usize, ReplError> {
    let held: i64 = remote
        .query_one(
            &format!(
                "select count(*) from {schema}.entry where tenant_id = $1 and id = any($2::text[])"
            ),
            &[&tenant, &entries],
        )
        .await?
        .get(0);
    Ok(entries.len().saturating_sub(held as usize))
}

/// Returns once the remote cell holds every one of `entries` in its log, having pulled them from
/// the home's outbox as needed. An entry the remote quarantined is not durable, so it is never
/// counted. If the remote has not applied them within `timeout`, returns `DurabilityNotMet`.
pub async fn acknowledge(
    home: &Client,
    remote: &Client,
    schema: &str,
    tenant: &str,
    source_cell: &str,
    entries: &[String],
    timeout: Duration,
) -> Result<Acknowledged, StrongError> {
    validate_schema_name(schema).map_err(ReplError::from)?;
    let started = Instant::now();
    let work = async {
        let mut pulls = 0;
        loop {
            if missing(remote, schema, tenant, entries).await? == 0 {
                return Ok::<_, ReplError>(pulls);
            }
            let r = pull(home, remote, schema, tenant, source_cell, 500).await?;
            pulls += 1;
            if missing(remote, schema, tenant, entries).await? == 0 {
                return Ok(pulls);
            }
            // A gap, or a row the cursor already passed: fetch what is missing by account.
            if r.applied.buffered > 0 || r.read == 0 {
                catch_up(home, remote, schema, tenant, 500).await?;
            }
            if missing(remote, schema, tenant, entries).await? == 0 {
                return Ok(pulls);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    };
    match tokio::time::timeout(timeout, work).await {
        Ok(pulls) => Ok(Acknowledged {
            waited: started.elapsed(),
            pulls: pulls?,
        }),
        Err(_) => Err(StrongError::DurabilityNotMet {
            waited: started.elapsed(),
        }),
    }
}
