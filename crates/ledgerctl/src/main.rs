//! `ledgerctl`, the operator command line. `ledgerctl check` verifies the ledger's invariants
//! on one or more cells and exits non-zero, naming the cell and the account or journal, when
//! any of them fails.
//!
//! Connection strings are secrets, so they are never arguments. Each cell is `NAME=ENV_VAR`:
//! the name appears in the output and the variable holds the connection string.
//!
//! Exit codes: 0 every invariant holds, 1 an invariant failed, 2 the command or a connection
//! failed, so a check that could not run is never mistaken for a pass.

#[cfg(feature = "failover")]
mod failover_cmd;

use std::process::ExitCode;
use std::time::Duration;

use ledger_check::{Options, Violation, check_cell, compare_chains};
use native_tls::TlsConnector;
use postgres_native_tls::MakeTlsConnector;
use tokio_postgres::config::SslMode;
use tokio_postgres::{Client, Config, NoTls};

const USAGE: &str = "\
usage: ledgerctl promote|drain|undrain ... (run `ledgerctl promote --help`)
       ledgerctl check --cell NAME=ENV_VAR [--cell NAME=ENV_VAR ...] [options]

Checks the ledger's invariants on each cell and exits 1 if any fails.

  --cell NAME=ENV_VAR              a cell: the name to print, and the environment variable that
                                   holds its connection string (never pass the string itself)
  --schema NAME                    the ledger schema (default public)
  --compare-chains                 also compare each account's hash chain across the cells; use
                                   it only where replication is meant to have caught up
  --in-transit-max-age-secs N      a journal pending longer than this fails (default 300)
  --quarantine-max-age-secs N      an unresolved quarantine entry this old fails (default 0: any)
";

#[derive(Debug, PartialEq, Eq)]
struct Check {
    cells: Vec<(String, String)>,
    schema: String,
    compare_chains: bool,
    options: Options,
}

fn parse(args: &[String]) -> Result<Check, String> {
    let mut it = args.iter();
    match it.next().map(String::as_str) {
        Some("check") => {}
        Some(other) => return Err(format!("unknown command {other:?}")),
        None => return Err("a command is needed".to_owned()),
    }
    let mut c = Check {
        cells: Vec::new(),
        schema: "public".to_owned(),
        compare_chains: false,
        options: Options::default(),
    };
    while let Some(a) = it.next() {
        let mut value = |name: &str| {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        let number = |name: &str, s: String| {
            s.parse::<i64>()
                .ok()
                .filter(|n| *n >= 0)
                .ok_or_else(|| format!("{name} needs a whole number of seconds, not {s:?}"))
        };
        match a.as_str() {
            "--cell" => {
                let v = value("--cell")?;
                let (name, var) = v
                    .split_once('=')
                    .filter(|(n, e)| !n.is_empty() && !e.is_empty())
                    .ok_or_else(|| "--cell is NAME=ENV_VAR".to_owned())?;
                if var.contains("://") || var.contains('@') {
                    return Err(
                        "--cell takes the name of an environment variable, not a connection string"
                            .to_owned(),
                    );
                }
                c.cells.push((name.to_owned(), var.to_owned()));
            }
            "--schema" => c.schema = value("--schema")?,
            "--compare-chains" => c.compare_chains = true,
            "--in-transit-max-age-secs" => {
                let s = value("--in-transit-max-age-secs")?;
                c.options.in_transit_max_age_secs = number("--in-transit-max-age-secs", s)?;
            }
            "--quarantine-max-age-secs" => {
                let s = value("--quarantine-max-age-secs")?;
                c.options.quarantine_max_age_secs = number("--quarantine-max-age-secs", s)?;
            }
            other => return Err(format!("unknown option {other:?}")),
        }
    }
    if c.cells.is_empty() {
        return Err("at least one --cell is needed".to_owned());
    }
    Ok(c)
}

async fn connect(name: &str, var: &str) -> Result<Client, String> {
    let dsn = std::env::var(var).map_err(|_| format!("cell {name}: {var} is not set"))?;
    let config: Config = dsn
        .parse()
        .map_err(|_| format!("cell {name}: {var} is not a valid connection string"))?;
    let timeout = Duration::from_secs(30);
    let client = if config.get_ssl_mode() == SslMode::Disable {
        let (client, conn) = tokio::time::timeout(timeout, config.connect(NoTls))
            .await
            .map_err(|_| format!("cell {name}: timed out connecting"))?
            .map_err(|e| format!("cell {name}: could not connect: {e}"))?;
        tokio::spawn(conn);
        client
    } else {
        let tls = MakeTlsConnector::new(TlsConnector::new().map_err(|e| e.to_string())?);
        let (client, conn) = tokio::time::timeout(timeout, config.connect(tls))
            .await
            .map_err(|_| format!("cell {name}: timed out connecting"))?
            .map_err(|e| format!("cell {name}: could not connect: {e}"))?;
        tokio::spawn(conn);
        client
    };
    Ok(client)
}

async fn run(c: Check) -> Result<Vec<Violation>, String> {
    let mut clients = Vec::new();
    for (name, var) in &c.cells {
        clients.push((name.as_str(), connect(name, var).await?));
    }
    let mut found = Vec::new();
    for (name, client) in &clients {
        found.extend(
            check_cell(client, &c.schema, name, &c.options)
                .await
                .map_err(|e| format!("cell {name}: {e}"))?,
        );
    }
    if c.compare_chains {
        let refs: Vec<(&str, &Client)> = clients.iter().map(|(n, cl)| (*n, cl)).collect();
        found.extend(
            compare_chains(&refs, &c.schema)
                .await
                .map_err(|e| format!("comparing chains: {e}"))?,
        );
    }
    Ok(found)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if matches!(
        args.first().map(String::as_str),
        Some("promote" | "drain" | "undrain")
    ) {
        #[cfg(feature = "failover")]
        return failover_cmd::main(&args).await;
        #[cfg(not(feature = "failover"))]
        {
            eprintln!("error: this build has no failover commands; build with --features failover");
            return ExitCode::from(2);
        }
    }
    let check = match parse(&args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let cells = check.cells.len();
    match run(check).await {
        Ok(found) if found.is_empty() => {
            println!("OK: every invariant holds on {cells} cell(s)");
            ExitCode::SUCCESS
        }
        Ok(found) => {
            for v in &found {
                println!("{v}");
            }
            println!("FAILED: {} violation(s) on {cells} cell(s)", found.len());
            ExitCode::from(1)
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn parses_cells_and_limits() {
        let c = parse(&args(
            "check --cell use1=DSN_A --cell use2=DSN_B --schema ledger --compare-chains --in-transit-max-age-secs 60",
        ))
        .unwrap();
        assert_eq!(
            c.cells,
            [
                ("use1".into(), "DSN_A".into()),
                ("use2".into(), "DSN_B".into())
            ]
        );
        assert_eq!(c.schema, "ledger");
        assert!(c.compare_chains);
        assert_eq!(c.options.in_transit_max_age_secs, 60);
        assert_eq!(c.options.quarantine_max_age_secs, 0);
    }

    #[test]
    fn refuses_a_connection_string_where_a_variable_name_goes() {
        for bad in ["a=postgres://u:p@h/db", "a=user@host"] {
            let e = parse(&args(&format!("check --cell {bad}"))).unwrap_err();
            assert!(e.contains("environment variable"), "{e}");
            assert!(!e.contains("u:p"), "the error must not echo a secret: {e}");
        }
    }

    #[test]
    fn refuses_bad_input() {
        assert!(parse(&args("")).is_err());
        assert!(parse(&args("check")).unwrap_err().contains("--cell"));
        assert!(parse(&args("check --cell nope")).is_err());
        assert!(parse(&args("check --cell a=B --quarantine-max-age-secs -1")).is_err());
        assert!(parse(&args("check --cell a=B --wat")).is_err());
        assert!(parse(&args("frobnicate")).is_err());
    }
}
