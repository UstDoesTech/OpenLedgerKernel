//! Where each cell's API secret lives. A secret is one JSON object, with an entry for each file the
//! service reads. [`CliStore`] reads and writes it with the cloud's own command line tool, as the
//! run's federated identity. Each write makes a new version, so the old one stays until it is
//! replaced, and a write that fails changes nothing.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::error::PkiError;
use crate::manifest::Cloud;
use crate::runner::Runner;

pub trait SecretStore: Send + Sync {
    /// The current value, or None if the secret has no version yet.
    fn read(&self, cloud: Cloud, secret: &str) -> Result<Option<String>, PkiError>;
    /// Makes `value` the secret's newest version.
    fn write(&self, cloud: Cloud, secret: &str, value: &str) -> Result<(), PkiError>;
}

/// The cloud tools, through a [`Runner`].
pub struct CliStore<R: Runner> {
    runner: R,
    /// A private directory for the one file a write hands to the tool.
    work: PathBuf,
}

impl<R: Runner> CliStore<R> {
    pub fn new(runner: R, work: PathBuf) -> Self {
        CliStore { runner, work }
    }

    fn run(
        &self,
        program: &str,
        args: Vec<String>,
        doing: &'static str,
    ) -> Result<String, PkiError> {
        let out = self.runner.run(program, &args, doing)?;
        String::from_utf8(out).map_err(|_| PkiError::Unreadable(doing))
    }

    /// Writes the value to a file only this user can read, runs the tool with its path and removes it.
    fn with_file<T>(
        &self,
        value: &str,
        f: impl FnOnce(&str) -> Result<T, PkiError>,
    ) -> Result<T, PkiError> {
        use std::io::Write;
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.work)
            .map_err(|_| PkiError::Secret("written: no private place for it"))?;
        let path = self.work.join("secret-value");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
            .map_err(|_| PkiError::Secret("written: no private place for it"))?;
        let wrote = file.write_all(value.as_bytes());
        drop(file);
        let result = match wrote {
            Ok(()) => f(&path.to_string_lossy()),
            Err(_) => Err(PkiError::Secret("written: no private place for it")),
        };
        let _ = std::fs::remove_file(&path);
        result
    }
}

/// `https://<vault>.vault.azure.net/secrets/<name>` into the vault and the name.
fn azure_parts(secret: &str) -> Result<(String, String), PkiError> {
    let bad = PkiError::Secret("addressed: not a Key Vault secret");
    let rest = secret.strip_prefix("https://").ok_or(bad)?;
    let (host, path) = rest
        .split_once("/secrets/")
        .ok_or(PkiError::Secret("addressed: not a Key Vault secret"))?;
    let vault = host
        .strip_suffix(".vault.azure.net")
        .ok_or(PkiError::Secret("addressed: not a Key Vault secret"))?;
    let name = path.split('/').next().unwrap_or("");
    if vault.is_empty() || name.is_empty() {
        return Err(PkiError::Secret("addressed: not a Key Vault secret"));
    }
    Ok((vault.to_owned(), name.to_owned()))
}

/// `projects/<project>/secrets/<name>` into the project and the name.
fn gcp_parts(secret: &str) -> Result<(String, String), PkiError> {
    let parts: Vec<&str> = secret.split('/').collect();
    match parts.as_slice() {
        ["projects", project, "secrets", name] if !project.is_empty() && !name.is_empty() => {
            Ok(((*project).to_owned(), (*name).to_owned()))
        }
        _ => Err(PkiError::Secret("addressed: not a Secret Manager secret")),
    }
}

impl<R: Runner> SecretStore for CliStore<R> {
    fn read(&self, cloud: Cloud, secret: &str) -> Result<Option<String>, PkiError> {
        const DOING: &str = "reading a secret";
        match cloud {
            Cloud::Aws => {
                let count = self.run(
                    "aws",
                    [
                        "secretsmanager",
                        "list-secret-version-ids",
                        "--secret-id",
                        secret,
                        "--query",
                        "length(Versions)",
                        "--output",
                        "text",
                    ]
                    .map(String::from)
                    .to_vec(),
                    DOING,
                )?;
                if count.trim() == "0" {
                    return Ok(None);
                }
                let value = self.run(
                    "aws",
                    [
                        "secretsmanager",
                        "get-secret-value",
                        "--secret-id",
                        secret,
                        "--query",
                        "SecretString",
                        "--output",
                        "text",
                    ]
                    .map(String::from)
                    .to_vec(),
                    DOING,
                )?;
                Ok(Some(value.trim_end_matches('\n').to_owned()))
            }
            Cloud::Azure => {
                let (vault, name) = azure_parts(secret)?;
                let value = self.run(
                    "az",
                    [
                        "keyvault",
                        "secret",
                        "show",
                        "--vault-name",
                        &vault,
                        "--name",
                        &name,
                        "--query",
                        "value",
                        "--output",
                        "tsv",
                    ]
                    .map(String::from)
                    .to_vec(),
                    DOING,
                )?;
                Ok(Some(value.trim_end_matches('\n').to_owned()))
            }
            Cloud::Gcp => {
                let (project, name) = gcp_parts(secret)?;
                let listed = self.run(
                    "gcloud",
                    [
                        "secrets",
                        "versions",
                        "list",
                        &name,
                        "--project",
                        &project,
                        "--limit",
                        "1",
                        "--format",
                        "value(name)",
                    ]
                    .map(String::from)
                    .to_vec(),
                    DOING,
                )?;
                if listed.trim().is_empty() {
                    return Ok(None);
                }
                let value = self.run(
                    "gcloud",
                    [
                        "secrets",
                        "versions",
                        "access",
                        "latest",
                        "--secret",
                        &name,
                        "--project",
                        &project,
                    ]
                    .map(String::from)
                    .to_vec(),
                    DOING,
                )?;
                Ok(Some(value))
            }
        }
    }

    fn write(&self, cloud: Cloud, secret: &str, value: &str) -> Result<(), PkiError> {
        const DOING: &str = "writing a secret";
        match cloud {
            Cloud::Aws => self.with_file(value, |path| {
                self.run(
                    "aws",
                    [
                        "secretsmanager",
                        "put-secret-value",
                        "--secret-id",
                        secret,
                        "--secret-string",
                        &format!("file://{path}"),
                        "--output",
                        "text",
                        "--query",
                        "VersionId",
                    ]
                    .map(String::from)
                    .to_vec(),
                    DOING,
                )
                .map(|_| ())
            }),
            Cloud::Azure => {
                let (vault, name) = azure_parts(secret)?;
                self.with_file(value, |path| {
                    self.run(
                        "az",
                        [
                            "keyvault",
                            "secret",
                            "set",
                            "--vault-name",
                            &vault,
                            "--name",
                            &name,
                            "--file",
                            path,
                            "--encoding",
                            "utf-8",
                            "--output",
                            "none",
                        ]
                        .map(String::from)
                        .to_vec(),
                        DOING,
                    )
                    .map(|_| ())
                })
            }
            Cloud::Gcp => {
                let (project, name) = gcp_parts(secret)?;
                self.with_file(value, |path| {
                    self.run(
                        "gcloud",
                        [
                            "secrets",
                            "versions",
                            "add",
                            &name,
                            "--project",
                            &project,
                            "--data-file",
                            path,
                            "--quiet",
                        ]
                        .map(String::from)
                        .to_vec(),
                        DOING,
                    )
                    .map(|_| ())
                })
            }
        }
    }
}

/// Secrets in memory, with every version kept. For tests and rehearsals.
#[derive(Default)]
pub struct MemoryStore {
    versions: Mutex<HashMap<(Cloud, String), Vec<String>>>,
    writes: Mutex<Vec<(Cloud, String)>>,
    fail_write: Mutex<Option<usize>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Puts a first or replacement version in, without counting it as a write by a run.
    pub fn put(&self, cloud: Cloud, secret: &str, value: &str) {
        self.versions
            .lock()
            .unwrap()
            .entry((cloud, secret.to_owned()))
            .or_default()
            .push(value.to_owned());
    }

    pub fn latest(&self, cloud: Cloud, secret: &str) -> Option<String> {
        self.versions
            .lock()
            .unwrap()
            .get(&(cloud, secret.to_owned()))
            .and_then(|v| v.last().cloned())
    }

    pub fn version_count(&self, cloud: Cloud, secret: &str) -> usize {
        self.versions
            .lock()
            .unwrap()
            .get(&(cloud, secret.to_owned()))
            .map_or(0, Vec::len)
    }

    /// Makes the n-th write from now on (counting from 1) fail.
    pub fn fail_write_number(&self, n: usize) {
        *self.fail_write.lock().unwrap() = Some(n);
    }

    /// Which secret each write went to, in order.
    pub fn write_log(&self) -> Vec<(Cloud, String)> {
        self.writes.lock().unwrap().clone()
    }
}

impl SecretStore for MemoryStore {
    fn read(&self, cloud: Cloud, secret: &str) -> Result<Option<String>, PkiError> {
        Ok(self.latest(cloud, secret))
    }

    fn write(&self, cloud: Cloud, secret: &str, value: &str) -> Result<(), PkiError> {
        let attempt = self.writes.lock().unwrap().len() + 1;
        if *self.fail_write.lock().unwrap() == Some(attempt) {
            self.writes.lock().unwrap().push((cloud, secret.to_owned()));
            return Err(PkiError::Secret("written"));
        }
        self.writes.lock().unwrap().push((cloud, secret.to_owned()));
        self.put(cloud, secret, value);
        Ok(())
    }
}
