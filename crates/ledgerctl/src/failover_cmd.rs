//! `ledgerctl promote`, `drain` and `undrain`: the operator's side of the failover
//! orchestrator. The control plane is etcd; the cells are Postgres nodes given as
//! `NODE_ID=ENV_VAR`, where the variable holds the connection string (never an argument).
//!
//! Approval is the caller's job. These commands do what they are told, so a person (or a
//! policy that a person owns) decides, and an agent never runs `promote` on a real system.
//!
//! Exit codes: 0 done, 1 done with cells that did not take the fence (they install it from the
//! control plane when they rejoin), 2 the command failed and nothing was promoted.

use std::collections::BTreeMap;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use control_plane::{EtcdConfig, EtcdControlPlane, NodeId, Orchestrator, PgCell};
use ledger_core::AccountId;

pub const USAGE: &str = "\
usage: ledgerctl promote --account ID --to NODE --tenant ID --cell NODE=ENV_VAR [--cell ...] [options]
       ledgerctl drain   --cell NODE [options]
       ledgerctl undrain --cell NODE [options]

promote moves one account's home to NODE: it reads that node's own high-water mark as the
cutoff for the old epoch, takes the next epoch from the control plane, then installs the fence
on NODE first and on every other cell. It does not wait for in-flight work; to move a home
gently, `drain` the old home first and wait for it to go idle.

  --etcd URL[,URL]         control plane members (default http://127.0.0.1:2379)
  --prefix KEY             etcd key prefix (default /ledger)
  --lease-ttl-secs N       lifetime of the new home's lease (default 10)
  --schema NAME            ledger schema on the cells (default public)
  --timeout-secs N         how long to wait for any one cell (default 5)
  --cell NODE=ENV_VAR      promote: a cell, by node id, and the variable holding its connection
                           string. drain/undrain: --cell NODE is the node id alone
";

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Promote {
        account: AccountId,
        to: NodeId,
        tenant: String,
    },
    Drain(NodeId),
    Undrain(NodeId),
}

#[derive(Debug, PartialEq, Eq)]
pub struct Parsed {
    pub command: Command,
    pub cells: Vec<(NodeId, String)>,
    pub etcd: Vec<String>,
    pub prefix: String,
    pub lease_ttl: Duration,
    pub schema: String,
    pub timeout: Duration,
}

pub fn parse(args: &[String]) -> Result<Parsed, String> {
    let mut it = args.iter();
    let verb = it.next().cloned().ok_or("a command is needed")?;
    let (mut account, mut to, mut tenant) = (None, None, None);
    let mut raw_cells: Vec<String> = Vec::new();
    let mut p = Parsed {
        command: Command::Drain("placeholder".parse().expect("valid")),
        cells: Vec::new(),
        etcd: vec!["http://127.0.0.1:2379".to_owned()],
        prefix: "/ledger".to_owned(),
        lease_ttl: Duration::from_secs(10),
        schema: "public".to_owned(),
        timeout: Duration::from_secs(5),
    };
    while let Some(a) = it.next() {
        let mut value = |name: &str| {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        let secs = |name: &str, s: String| {
            s.parse::<u64>()
                .ok()
                .filter(|n| *n > 0)
                .map(Duration::from_secs)
                .ok_or_else(|| format!("{name} needs a whole number of seconds above 0"))
        };
        match a.as_str() {
            "--account" => account = Some(value("--account")?),
            "--to" => to = Some(value("--to")?),
            "--tenant" => tenant = Some(value("--tenant")?),
            "--cell" => raw_cells.push(value("--cell")?),
            "--etcd" => {
                p.etcd = value("--etcd")?
                    .split(',')
                    .map(str::to_owned)
                    .filter(|s| !s.is_empty())
                    .collect();
            }
            "--prefix" => p.prefix = value("--prefix")?,
            "--schema" => p.schema = value("--schema")?,
            "--lease-ttl-secs" => {
                p.lease_ttl = secs("--lease-ttl-secs", value("--lease-ttl-secs")?)?
            }
            "--timeout-secs" => p.timeout = secs("--timeout-secs", value("--timeout-secs")?)?,
            other => return Err(format!("unknown option {other:?}")),
        }
    }
    if p.etcd.is_empty() {
        return Err("--etcd needs at least one URL".to_owned());
    }
    let node = |s: &str| {
        s.parse::<NodeId>()
            .map_err(|e| format!("{s:?} is not a node id: {e}"))
    };
    match verb.as_str() {
        "promote" => {
            for c in &raw_cells {
                let (n, var) = c
                    .split_once('=')
                    .filter(|(n, v)| !n.is_empty() && !v.is_empty())
                    .ok_or("--cell is NODE=ENV_VAR for promote")?;
                if var.contains("://") || var.contains('@') {
                    return Err(
                        "--cell takes the name of an environment variable, not a connection string"
                            .to_owned(),
                    );
                }
                p.cells.push((node(n)?, var.to_owned()));
            }
            if p.cells.is_empty() {
                return Err("promote needs at least one --cell".to_owned());
            }
            let to = node(&to.ok_or("promote needs --to")?)?;
            if !p.cells.iter().any(|(n, _)| *n == to) {
                return Err(format!("--to {to} is not one of the --cell nodes"));
            }
            p.command = Command::Promote {
                account: account
                    .ok_or("promote needs --account")?
                    .parse()
                    .map_err(|e| format!("--account: {e}"))?,
                to,
                tenant: tenant.ok_or("promote needs --tenant")?,
            };
        }
        "drain" | "undrain" => {
            let [one] = raw_cells.as_slice() else {
                return Err(format!("{verb} takes exactly one --cell NODE"));
            };
            let n = node(one)?;
            p.command = if verb == "drain" {
                Command::Drain(n)
            } else {
                Command::Undrain(n)
            };
        }
        other => return Err(format!("unknown command {other:?}")),
    }
    Ok(p)
}

pub async fn main(args: &[String]) -> ExitCode {
    let p = match parse(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match run(p).await {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
    }
}

async fn run(p: Parsed) -> Result<u8, String> {
    let control = EtcdControlPlane::connect(EtcdConfig::new(p.etcd, p.prefix, p.lease_ttl))
        .await
        .map_err(|e| format!("control plane: {e}"))?;
    match p.command {
        Command::Drain(n) => {
            Orchestrator::<_, PgCell>::new(control, BTreeMap::new(), p.timeout)
                .drain(&n)
                .await
                .map_err(|e| e.to_string())?;
            println!("draining {n}: new requests are refused, in-flight work finishes");
            Ok(0)
        }
        Command::Undrain(n) => {
            Orchestrator::<_, PgCell>::new(control, BTreeMap::new(), p.timeout)
                .undrain(&n)
                .await
                .map_err(|e| e.to_string())?;
            println!("{n} accepts requests again");
            Ok(0)
        }
        Command::Promote {
            account,
            to,
            tenant,
        } => {
            let mut cells = BTreeMap::new();
            for (node, var) in &p.cells {
                let client = crate::connect(&node.to_string(), var).await?;
                cells.insert(
                    node.clone(),
                    PgCell::new(Arc::new(client), p.schema.clone(), tenant.clone()),
                );
            }
            let done = Orchestrator::new(control, cells, p.timeout)
                .promote(&account, &to)
                .await
                .map_err(|e| e.to_string())?;
            println!(
                "promoted {account} from {} to {to}: epoch {} (old epoch cut off at sequence {}) in {:?}",
                done.previous_home, done.fence.epoch.0, done.fence.prev_cutoff_seq, done.took
            );
            println!("fence installed on: {}", join(done.installed.iter()));
            if done.not_installed.is_empty() {
                Ok(0)
            } else {
                for (n, why) in &done.not_installed {
                    println!(
                        "NOT installed on {n}: {why} (it installs from the control plane when it rejoins)"
                    );
                }
                Ok(1)
            }
        }
    }
}

fn join<'a>(it: impl Iterator<Item = &'a NodeId>) -> String {
    it.map(ToString::to_string).collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn parses_a_promotion() {
        let p = parse(&args(
            "promote --account acc_p --to azure-lon-01 --tenant ten_acme --cell aws-lon-01=A --cell azure-lon-01=B --etcd http://a:2379,http://b:2379 --timeout-secs 3",
        ))
        .unwrap();
        assert_eq!(p.cells.len(), 2);
        assert_eq!(p.etcd, ["http://a:2379", "http://b:2379"]);
        assert_eq!(p.timeout, Duration::from_secs(3));
        assert!(matches!(p.command, Command::Promote { .. }));
    }

    #[test]
    fn parses_drain_and_undrain() {
        let d = parse(&args("drain --cell aws-lon-01")).unwrap();
        assert_eq!(d.command, Command::Drain("aws-lon-01".parse().unwrap()));
        let u = parse(&args("undrain --cell aws-lon-01 --prefix /x")).unwrap();
        assert_eq!(u.command, Command::Undrain("aws-lon-01".parse().unwrap()));
        assert_eq!(u.prefix, "/x");
    }

    #[test]
    fn promote_refuses_a_target_that_is_not_a_listed_cell_and_secrets_in_arguments() {
        let e = parse(&args(
            "promote --account acc_p --to gcp-lon-01 --tenant ten_a --cell aws-lon-01=A",
        ))
        .unwrap_err();
        assert!(e.contains("not one of"), "{e}");
        let e = parse(&args("promote --account acc_p --to aws-lon-01 --tenant ten_a --cell aws-lon-01=postgres://u:p@h/db")).unwrap_err();
        assert!(
            e.contains("environment variable") && !e.contains("u:p"),
            "{e}"
        );
    }

    #[test]
    fn refuses_missing_and_bad_input() {
        assert!(parse(&args("")).is_err());
        assert!(parse(&args("promote --cell a-1=A")).is_err());
        assert!(parse(&args("drain")).is_err());
        assert!(parse(&args("drain --cell a-1 --cell b-1")).is_err());
        assert!(parse(&args("drain --cell Bad_Name")).is_err());
        assert!(parse(&args("drain --cell a-1 --timeout-secs 0")).is_err());
        assert!(parse(&args("drain --cell a-1 --wat")).is_err());
    }
}
