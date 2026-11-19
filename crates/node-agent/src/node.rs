//! Starting a node: the steps that touch the machine.

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::config::{NodeConfig, Role};
use crate::plan::{self, DataState, Paths};
use crate::store::{SecretStore, StoreError};

#[derive(Debug, thiserror::Error)]
pub enum NodeError {
    #[error("{0}")]
    Refused(String),
    #[error("{0}: {1}")]
    Io(String, String),
    #[error("{0} failed: {1}")]
    Command(String, String),
    #[error(transparent)]
    Store(#[from] StoreError),
}

fn io(what: impl Into<String>) -> impl FnOnce(std::io::Error) -> NodeError {
    let what = what.into();
    move |e| NodeError::Io(what, e.to_string())
}

fn note(line: &str) {
    eprintln!("ledger-node: {line}");
}

/// A password for the administrator: 256 random bits as hexadecimal, from the kernel.
pub fn new_password() -> Result<String, NodeError> {
    crate::util::random_hex(32).map_err(io("reading random bytes"))
}

/// Copies the mounted TLS files into the writable area with the permissions PostgreSQL insists
/// on: the key readable by its owner only.
pub fn stage_tls(config: &NodeConfig, paths: &Paths) -> Result<(), NodeError> {
    let out = paths.tls();
    fs::create_dir_all(&out).map_err(io("creating the TLS directory"))?;
    fs::set_permissions(&out, fs::Permissions::from_mode(0o700))
        .map_err(io("securing the TLS directory"))?;
    for name in plan::tls_files(config.role) {
        let from = paths.tls_in.join(name);
        if !from.is_file() {
            return Err(NodeError::Refused(format!(
                "the TLS file {} is missing",
                from.display()
            )));
        }
        let to = out.join(name);
        fs::copy(&from, &to).map_err(io(format!("copying {name}")))?;
        let mode = if name.ends_with(".key") { 0o600 } else { 0o644 };
        fs::set_permissions(&to, fs::Permissions::from_mode(mode))
            .map_err(io(format!("securing {name}")))?;
    }
    Ok(())
}

fn run(
    paths: &Paths,
    tool: &str,
    args: &[String],
    env: &[(String, String)],
    stdin: Option<&str>,
) -> Result<(), NodeError> {
    let mut command = Command::new(paths.tool(tool));
    command
        .args(args)
        .envs(env.iter().cloned())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    if stdin.is_some() {
        command.stdin(Stdio::piped());
    }
    let mut child = command.spawn().map_err(io(format!("starting {tool}")))?;
    if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
        pipe.write_all(text.as_bytes())
            .map_err(io(format!("writing to {tool}")))?;
    }
    let status = child.wait().map_err(io(format!("waiting for {tool}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(NodeError::Command(tool.to_string(), status.to_string()))
    }
}

fn s(x: impl AsRef<str>) -> String {
    x.as_ref().to_string()
}

fn secure_dir(path: &Path) -> Result<(), NodeError> {
    fs::create_dir_all(path).map_err(io(format!("creating {}", path.display())))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(io(format!("securing {}", path.display())))
}

/// Writes the credential and reads it back, so that a store that lost the write cannot leave a
/// database with a password that is kept nowhere.
pub fn store_credential(
    store: &dyn SecretStore,
    id: &str,
    password: &str,
) -> Result<(), NodeError> {
    store.put(id, password)?;
    if store.get(id)? != password {
        return Err(NodeError::Refused(
            "the secret store did not return the credential that was just written".into(),
        ));
    }
    Ok(())
}

/// First start of a primary: make the cluster, give the administrator a password, hand that to
/// the secret store, and create the replication role.
fn bootstrap_primary(
    config: &NodeConfig,
    paths: &Paths,
    store: &dyn SecretStore,
) -> Result<(), NodeError> {
    note("new primary: creating the database cluster");
    let result = (|| {
        run(
            paths,
            "initdb",
            &[
                s("-D"),
                paths.data.display().to_string(),
                s("-U"),
                s(plan::ADMIN_ROLE),
                s("--encoding=UTF8"),
                s("--data-checksums"),
                s("--auth-local=trust"),
                s("--auth-host=reject"),
            ],
            &[],
            None,
        )?;
        let socket = paths.run.display().to_string();
        let options = format!(
            "-c listen_addresses='' -c unix_socket_directories={socket} -c port={} -c max_replication_slots=0",
            config.port
        );
        run(
            paths,
            "pg_ctl",
            &[
                s("-D"),
                paths.data.display().to_string(),
                s("-w"),
                s("-t"),
                s("120"),
                s("-l"),
                paths.run.join("bootstrap.log").display().to_string(),
                s("-o"),
                options,
                s("start"),
            ],
            &[],
            None,
        )?;
        let configured = (|| {
            let password = new_password()?;
            // The store first: if it fails, nothing has been set and the next start tries again.
            store_credential(store, &config.secret_id, &password)?;
            run(
                paths,
                "psql",
                &[
                    s("-X"),
                    s("-q"),
                    s("-v"),
                    s("ON_ERROR_STOP=1"),
                    s("-h"),
                    socket.clone(),
                    s("-p"),
                    config.port.to_string(),
                    s("-U"),
                    s(plan::ADMIN_ROLE),
                    s("-d"),
                    s("postgres"),
                    s("-f"),
                    s("-"),
                ],
                &[],
                Some(&plan::bootstrap_sql(&password)),
            )
        })();
        let stopped = run(
            paths,
            "pg_ctl",
            &[
                s("-D"),
                paths.data.display().to_string(),
                s("-m"),
                s("fast"),
                s("-w"),
                s("stop"),
            ],
            &[],
            None,
        );
        configured.and(stopped)
    })();
    if result.is_err() {
        // The directory was empty when we began, so clearing it loses nothing.
        let _ = clear(&paths.data);
    }
    result
}

fn clear(dir: &Path) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            fs::remove_dir_all(path)?
        } else {
            fs::remove_file(path)?
        }
    }
    Ok(())
}

/// First start of a standby: copy the primary, waiting for it to be ready.
fn clone_primary(config: &NodeConfig, paths: &Paths, wait: Duration) -> Result<(), NodeError> {
    note(&format!("new standby: copying {}", config.primary_address));
    let start = Instant::now();
    let mut delay = Duration::from_secs(1);
    loop {
        match run(
            paths,
            "pg_basebackup",
            &plan::basebackup_args(config, paths),
            &plan::basebackup_env(paths),
            None,
        ) {
            Ok(()) => break,
            Err(e) if start.elapsed() + delay > wait => return Err(e),
            Err(_) => {
                note(&format!(
                    "the primary is not ready, trying again in {}s",
                    delay.as_secs()
                ));
                std::thread::sleep(delay);
                delay = (delay * 2).min(Duration::from_secs(15));
            }
        }
    }
    fs::File::create(paths.data.join("standby.signal")).map_err(io("creating standby.signal"))?;
    Ok(())
}

/// Prepares the node and returns the command that runs it. The caller replaces itself with it.
pub fn prepare(
    config: &NodeConfig,
    paths: &Paths,
    store: &dyn SecretStore,
    clone_wait: Duration,
) -> Result<Command, NodeError> {
    secure_dir(&paths.run)?;
    stage_tls(config, paths)?;
    fs::write(paths.hba(), plan::hba()).map_err(io("writing pg_hba.conf"))?;
    secure_dir(&paths.data)?;

    let state = plan::data_state(&paths.data).map_err(NodeError::Refused)?;
    plan::check_role(config, &state).map_err(NodeError::Refused)?;
    if state == DataState::Empty {
        match config.role {
            Role::Primary => bootstrap_primary(config, paths, store)?,
            Role::Standby => clone_primary(config, paths, clone_wait)?,
        }
    }
    let mut command = Command::new(paths.tool("postgres"));
    command.args(plan::server_args(config, paths));
    Ok(command)
}

/// Prepares the node, then becomes PostgreSQL. Only returns if something went wrong.
pub fn start(
    config: &NodeConfig,
    paths: &Paths,
    store: &dyn SecretStore,
    clone_wait: Duration,
) -> NodeError {
    match prepare(config, paths, store, clone_wait) {
        Ok(mut command) => {
            note(&format!("starting {} as {:?}", config.node, config.role));
            let e = command.exec();
            NodeError::Io("starting postgres".into(), e.to_string())
        }
        Err(e) => e,
    }
}
