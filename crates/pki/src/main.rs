//! `ledger-pki rotate --manifest <file> --kind all|certificates|credentials [--settle-secs N]`
//!
//! Run by the `rotate-credentials` workflow and by nothing else. It prints one line of counts and
//! nothing a person must keep private. `--rehearse` runs the same steps on made-up cells in memory,
//! to show what a run prints.

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use ledger_pki::AdministratorCopy;
use ledger_pki::application_role::{Plan, Tie, apply};
use ledger_pki::manifest::{Authority, CellEntry};
use ledger_pki::manifest::{Database, DatabaseKind};
use ledger_pki::runner::ProcessRunner;
use ledger_pki::{
    CliStore, Cloud, Kind, KmsSigner, Manifest, MemorySigner, MemoryStore, PkiError, Rotation,
    SecretStore, Signer, Simulated,
};

fn usage() -> ExitCode {
    eprintln!(
        "usage: ledger-pki rotate --manifest <file> --kind all|certificates|credentials [--settle-secs N] [--work <dir>]"
    );
    eprintln!("       ledger-pki rehearse");
    eprintln!(
        "       ledger-pki managed-role --cloud aws --admin-file <file> [--database postgres] [--role ledger_app] [--schema ledger] [--ca-file <file>]"
    );
    ExitCode::from(2)
}

/// `ledger-pki managed-role --cloud aws --admin-file <file> [--database postgres] [--role ledger_app]
/// [--schema ledger] [--ca-file <file>]`: makes a managed cell's application role (K-24ca). A person
/// runs it, once for each cell. The file holds the administrator's connection string, and this
/// command reads it, uses it for these statements and writes it nowhere.
fn managed_role(flag: &dyn Fn(&str) -> Option<String>) -> Result<String, PkiError> {
    let tie = match flag("--cloud").as_deref() {
        Some("aws") => Tie::AwsIam,
        Some(_) => return Err(PkiError::Manifest("only --cloud aws is supported so far")),
        None => return Err(PkiError::Manifest("--cloud is needed")),
    };
    let plan = Plan::new(
        tie,
        &flag("--role").unwrap_or_else(|| "ledger_app".into()),
        &flag("--database").unwrap_or_else(|| "postgres".into()),
        &flag("--schema").unwrap_or_else(|| "ledger".into()),
    )?;
    let conn = std::fs::read_to_string(
        flag("--admin-file").ok_or(PkiError::Manifest("--admin-file is needed"))?,
    )
    .map_err(|_| PkiError::Manifest("the administrator's file could not be read"))?;
    let ca = match flag("--ca-file") {
        Some(path) => Some(
            std::fs::read_to_string(path)
                .map_err(|_| PkiError::Manifest("the CA file could not be read"))?,
        ),
        None => None,
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| PkiError::CannotRun("starting the step"))?;
    runtime.block_on(async {
        // The sign-in error is dropped: its words can name the host.
        let admin = ledger_api::dbconn::connect(conn.trim(), true, ca.as_deref())
            .await
            .map_err(|_| PkiError::Command {
                code: 1,
                doing: "signing in as the administrator",
                tool: "the database",
                cause: "the sign-in was refused or the database could not be reached",
            })?;
        Ok(apply(&admin, &plan).await?.to_string())
    })
}

fn run(args: &[String]) -> Result<String, PkiError> {
    let flag = |name: &str| args.windows(2).find(|w| w[0] == name).map(|w| w[1].clone());
    let kind = match flag("--kind").as_deref() {
        Some("all") | None => Kind::All,
        Some("certificates") => Kind::Certificates,
        Some("credentials") => Kind::Credentials,
        Some(_) => {
            return Err(PkiError::Manifest(
                "--kind is all, certificates or credentials",
            ));
        }
    };
    let now = time::OffsetDateTime::now_utc();
    match args.first().map(String::as_str) {
        Some("rehearse") => {
            // `--proving` rehearses a proving environment: empty secrets, filled by a simulated source.
            let proving = args.iter().any(|a| a == "--proving");
            let manifest = Manifest {
                proving,
                etcd_members: vec![],
                authority: Authority {
                    key_id: "rehearsal".into(),
                    certificate_secret: "rehearsal".into(),
                },
                tenants: vec!["ten_rehearsal".into()],
                cells: [
                    ("cell-one", Cloud::Aws, "10.0.0.1"),
                    ("cell-two", Cloud::Azure, "10.0.0.2"),
                    ("cell-three", Cloud::Gcp, "10.0.0.3"),
                ]
                .iter()
                .map(|(n, c, a)| CellEntry {
                    name: (*n).into(),
                    cloud: *c,
                    secret: format!("secret-{n}"),
                    address: (*a).into(),
                    internal_port: 8444,
                    db_ca: None,
                    database: proving.then(|| Database {
                        host: format!("db-{n}.rehearsal"),
                        port: 5432,
                        kind: DatabaseKind::SelfHosted,
                        credential_secret: format!("credential-{n}"),
                        nodes: vec![],
                        sign_in: None,
                        role: None,
                        name: None,
                    }),
                })
                .collect(),
            };
            let store = MemoryStore::new();
            for c in &manifest.cells {
                let held = if proving {
                    "{}"
                } else {
                    r#"{"db.conn":"x","tokens.json":"[]"}"#
                };
                store.put(c.cloud, &c.secret, held);
            }
            let signer: Arc<dyn Signer> = Arc::new(MemorySigner::new()?);
            let report = Rotation {
                manifest: &manifest,
                signer,
                store: &store,
                now,
                settle: Duration::ZERO,
                sleep: &|_| {},
                database: if proving { Some(&Simulated) } else { None },
            }
            .run(kind)?;
            Ok(report.to_string())
        }
        Some("rotate") => {
            let path = flag("--manifest").ok_or(PkiError::Manifest("--manifest is needed"))?;
            let text = std::fs::read_to_string(path)
                .map_err(|_| PkiError::Manifest("it could not be read"))?;
            let manifest = Manifest::parse(&text)?;
            let work = std::path::PathBuf::from(flag("--work").unwrap_or_else(|| {
                std::env::var("RUNNER_TEMP").unwrap_or_else(|_| "/tmp".into()) + "/pki"
            }));
            let settle = Duration::from_secs(
                flag("--settle-secs")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(420),
            );
            let signer: Arc<dyn Signer> = Arc::new(KmsSigner::new(
                ProcessRunner,
                &manifest.authority.key_id,
                work.join("kms"),
            )?);
            let store: Box<dyn SecretStore> =
                Box::new(CliStore::new(ProcessRunner, work.join("secret")));
            let copy = AdministratorCopy::new(store.as_ref());
            let report = Rotation {
                manifest: &manifest,
                signer,
                store: store.as_ref(),
                now,
                settle,
                sleep: &std::thread::sleep,
                // Used only when the manifest says it is a proving environment.
                database: Some(&copy),
            }
            .run(kind)?;
            Ok(report.to_string())
        }
        Some("managed-role") => managed_role(&flag),
        _ => Err(PkiError::Manifest(
            "the first word is rotate, rehearse or managed-role",
        )),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        return usage();
    }
    match run(&args) {
        Ok(line) => {
            println!("{line}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            // The message names a step, never a cell, a secret or a value.
            eprintln!("ledger-pki: {e}");
            ExitCode::FAILURE
        }
    }
}
