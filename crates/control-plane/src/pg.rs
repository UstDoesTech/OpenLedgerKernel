//! A cell as a Postgres database, for the orchestrator.

use std::sync::Arc;

use ledger_core::AccountId;
use tokio_postgres::Client;

use crate::{Cell, CellError, Epoch, Fence};

/// One tenant's ledger schema on one Postgres node. The fence functions are the ones from
/// migration 6, so installing a fence also sweeps the account (see `docs/design.md#failover`).
#[derive(Clone)]
pub struct PgCell {
    client: Arc<Client>,
    schema: String,
    tenant: String,
}

impl PgCell {
    pub fn new(client: Arc<Client>, schema: impl Into<String>, tenant: impl Into<String>) -> Self {
        PgCell {
            client,
            schema: schema.into(),
            tenant: tenant.into(),
        }
    }
}

fn cell_error(e: impl std::fmt::Display) -> CellError {
    CellError(e.to_string())
}

impl Cell for PgCell {
    async fn high_water(&self, account: &AccountId) -> Result<(Epoch, u64), CellError> {
        let (epoch, seq) = ledger_repl::high_water_of(
            &self.client,
            &self.schema,
            &self.tenant,
            &account.to_string(),
        )
        .await
        .map_err(cell_error)?;
        Ok((Epoch(epoch as u64), seq as u64))
    }

    async fn install_fence(&self, fence: &Fence) -> Result<(), CellError> {
        ledger_repl::install_fence(
            &self.client,
            &self.schema,
            &ledger_repl::Fence {
                tenant: self.tenant.clone(),
                account: fence.account.to_string(),
                epoch: fence.epoch.0 as i64,
                home: fence.home.to_string(),
                prev_epoch: fence.prev_epoch.0 as i64,
                prev_cutoff_seq: fence.prev_cutoff_seq as i64,
            },
        )
        .await
        .map(|_| ())
        .map_err(cell_error)
    }
}
