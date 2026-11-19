use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use node_agent::config::NodeConfig;
use node_agent::node;
use node_agent::plan::Paths;
use node_agent::stores;

fn env_path(name: &str, default: &str) -> PathBuf {
    PathBuf::from(std::env::var(name).unwrap_or_else(|_| default.to_string()))
}

fn main() -> ExitCode {
    let config_path = env_path("LEDGER_NODE_CONFIG", "/etc/ledger/node.json");
    let config = match NodeConfig::load(&config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("ledger-node: {e}");
            return ExitCode::from(2);
        }
    };
    let paths = Paths {
        bin: env_path("LEDGER_PG_BIN", "/usr/lib/postgresql/16/bin"),
        data: env_path("LEDGER_DATA", "/var/lib/postgresql/data/pgdata"),
        run: env_path("LEDGER_RUN", "/run/ledger"),
        tls_in: env_path("LEDGER_TLS", "/run/secrets/tls"),
    };
    let wait = std::env::var("LEDGER_CLONE_WAIT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(600);
    let store = match stores::open(&config.secret_store, &config.secret_id) {
        Ok(store) => store,
        Err(e) => {
            eprintln!("ledger-node: {e}");
            return ExitCode::from(2);
        }
    };
    let error = node::start(&config, &paths, store.as_ref(), Duration::from_secs(wait));
    eprintln!("ledger-node: {error}");
    ExitCode::from(1)
}
