//! Choosing the secret store from the configuration, and trying again when a cloud service is
//! not ready yet. A new machine's role or identity can take a minute to be recognised.

use std::time::Duration;

use crate::aws::AwsSecretsManager;
use crate::azure::AzureKeyVault;
use crate::config::StoreConfig;
use crate::gcp::GcpSecretManager;
use crate::http::UreqHttp;
use crate::reference;
use crate::store::{FileStore, SecretStore, StoreError};

/// Tries again, with a growing pause, until the attempts run out.
pub struct Retrying<S> {
    inner: S,
    attempts: u32,
    first_pause: Duration,
    longest_pause: Duration,
}

impl<S> Retrying<S> {
    pub fn new(inner: S, attempts: u32, first_pause: Duration, longest_pause: Duration) -> Self {
        Retrying {
            inner,
            attempts: attempts.max(1),
            first_pause,
            longest_pause,
        }
    }

    /// For tests: the store inside.
    pub fn inner_for_test(&self) -> &S {
        &self.inner
    }

    fn retry<T>(&self, mut op: impl FnMut() -> Result<T, StoreError>) -> Result<T, StoreError> {
        let mut pause = self.first_pause;
        let mut last = None;
        for attempt in 1..=self.attempts {
            match op() {
                Ok(v) => return Ok(v),
                Err(e) => {
                    eprintln!(
                        "ledger-node: secret store, attempt {attempt} of {}: {e}",
                        self.attempts
                    );
                    last = Some(e);
                    if attempt < self.attempts {
                        std::thread::sleep(pause);
                        pause = (pause * 2).min(self.longest_pause);
                    }
                }
            }
        }
        Err(last.expect("at least one attempt was made"))
    }
}

impl<S: SecretStore> SecretStore for Retrying<S> {
    fn put(&self, id: &str, value: &str) -> Result<(), StoreError> {
        self.retry(|| self.inner.put(id, value))
    }
    fn get(&self, id: &str) -> Result<String, StoreError> {
        self.retry(|| self.inner.get(id))
    }
}

/// The store the configuration asks for. The cloud stores are reached over HTTPS and wait up to
/// about two minutes for the machine's identity to be recognised.
pub fn open(config: &StoreConfig, secret_id: &str) -> Result<Box<dyn SecretStore>, String> {
    fn retrying<S: SecretStore + 'static>(s: S) -> Box<dyn SecretStore> {
        Box::new(Retrying::new(
            s,
            8,
            Duration::from_secs(2),
            Duration::from_secs(30),
        ))
    }
    Ok(match config {
        StoreConfig::File { dir } => Box::new(FileStore::new(dir)),
        StoreConfig::AwsSecretsManager {} => retrying(AwsSecretsManager::new(
            Box::new(UreqHttp::new()),
            reference::parse_aws(secret_id)?,
        )),
        StoreConfig::AzureKeyVault {} => retrying(AzureKeyVault::new(
            Box::new(UreqHttp::new()),
            reference::parse_azure(secret_id)?,
        )),
        StoreConfig::GcpSecretManager {} => retrying(GcpSecretManager::new(
            Box::new(UreqHttp::new()),
            reference::parse_gcp(secret_id)?,
        )),
    })
}
