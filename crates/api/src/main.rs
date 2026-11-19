//! `ledger-api`: runs the API next to one cell.
//!
//! Configuration is by environment. Anything secret is read from a file whose path the
//! environment names, never from the environment or the command line, so it cannot show in a
//! process list or a log.
//!
//!   LEDGER_NODE            the cell this runs next to, such as `aws-lon-03`
//!   LEDGER_DB_FILE         a file holding this cell's Postgres connection string
//!   LEDGER_DB_TLS          `off` to connect without TLS (tests only); anything else uses TLS
//!   LEDGER_SCHEMA          the ledger schema, default `ledger`
//!   LEDGER_LISTEN          the address to listen on, default `127.0.0.1:8080`
//!   LEDGER_TOKENS_FILE     a JSON list of `{token, tenant, principal}` (see `ledger_api::auth`)
//!   LEDGER_ETCD_ENDPOINTS  comma-separated etcd client URLs
//!   LEDGER_ETCD_PREFIX     the control plane's key prefix, default `/ledger`
//!   LEDGER_LEASE_*         the lease guard's timings (see `control_plane::GuardConfig`)
//!
//! Exit codes: 2 for a configuration or connection problem at start, 1 for a failure after.

use std::process::ExitCode;
use std::sync::Arc;

use control_plane::{EtcdConfig, EtcdControlPlane, GuardConfig, LeaseGuard, NodeId, SystemClock};
use ledger_api::auth::StaticTokens;
use ledger_api::{AppState, router};
use tokio_postgres::{Client, Config, NoTls};

fn var(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|_| format!("{name} is not set"))
}

fn file(name: &str) -> Result<String, String> {
    let path = var(name)?;
    std::fs::read_to_string(&path)
        .map(|s| s.trim().to_owned())
        .map_err(|e| format!("{name}: cannot read {path}: {e}"))
}

async fn connect(conn: &str, tls: bool) -> Result<Client, String> {
    let config: Config = conn
        .parse()
        .map_err(|_| "LEDGER_DB_FILE: not a connection string".to_owned())?;
    if tls {
        let connector = native_tls::TlsConnector::new().map_err(|e| e.to_string())?;
        let (client, connection) = config
            .connect(postgres_native_tls::MakeTlsConnector::new(connector))
            .await
            .map_err(|e| format!("cannot connect to the cell's database: {e}"))?;
        tokio::spawn(connection);
        Ok(client)
    } else {
        let (client, connection) = config
            .connect(NoTls)
            .await
            .map_err(|e| format!("cannot connect to the cell's database: {e}"))?;
        tokio::spawn(connection);
        Ok(client)
    }
}

async fn run() -> Result<(), String> {
    let node: NodeId = var("LEDGER_NODE")?
        .parse()
        .map_err(|e| format!("LEDGER_NODE: {e}"))?;
    let schema = std::env::var("LEDGER_SCHEMA").unwrap_or_else(|_| "ledger".to_owned());
    let listen = std::env::var("LEDGER_LISTEN").unwrap_or_else(|_| "127.0.0.1:8080".to_owned());
    let tls = std::env::var("LEDGER_DB_TLS")
        .map(|v| v != "off")
        .unwrap_or(true);
    let tokens =
        StaticTokens::from_json(&file("LEDGER_TOKENS_FILE")?).map_err(|e| e.to_string())?;
    let guard_config = GuardConfig::from_env().map_err(|e| e.to_string())?;
    let endpoints: Vec<String> = var("LEDGER_ETCD_ENDPOINTS")?
        .split(',')
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect();
    let prefix = std::env::var("LEDGER_ETCD_PREFIX").unwrap_or_else(|_| "/ledger".to_owned());

    let db = connect(&file("LEDGER_DB_FILE")?, tls).await?;
    // Fail at start, not at the first request, if the schema this needs is not there.
    ledger_store::validate_schema_name(&schema).map_err(|e| e.to_string())?;
    db.query_opt(&format!("select 1 from {schema}.api_request limit 1"), &[])
        .await
        .map_err(|_| {
            "the cell's database has no ledger schema at this version: migrate it first".to_owned()
        })?;

    let cp = EtcdControlPlane::connect(EtcdConfig::new(endpoints, prefix, guard_config.lease_ttl))
        .await
        .map_err(|e| format!("cannot reach the control plane: {e}"))?;
    let guard = LeaseGuard::new(node.clone(), guard_config, Arc::new(SystemClock::new()))
        .map_err(|e| e.to_string())?;
    let state = AppState::new(
        node.clone(),
        &schema,
        db,
        Arc::new(cp),
        guard,
        Arc::new(tokens),
    );

    // A healthy cell renews the leases of the accounts homed here, on a timer.
    let renewing = state.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(guard_config.renew_every);
        loop {
            tick.tick().await;
            if let Err(e) = renewing.renew().await {
                eprintln!("ledger-api: could not renew leases: {e}");
            }
        }
    });

    let listener = tokio::net::TcpListener::bind(&listen)
        .await
        .map_err(|e| format!("cannot listen on {listen}: {e}"))?;
    eprintln!("ledger-api: cell {node} listening on {listen}");
    let admission = state.clone();
    axum::serve(listener, router(state))
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            // Stop taking new requests, then give those in flight a moment.
            admission.admission().set_draining(true);
            admission
                .admission()
                .wait_idle(std::time::Duration::from_secs(10))
                .await;
        })
        .await
        .map_err(|e| e.to_string())
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ledger-api: {e}");
            ExitCode::from(2)
        }
    }
}
