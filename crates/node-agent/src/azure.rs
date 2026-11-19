//! Azure Key Vault, with a token for the machine's own managed identity from the instance
//! metadata service. Nothing is configured or stored.

use serde_json::{Value, json};

use crate::http::{Http, Request};
use crate::reference::AzureRef;
use crate::store::{SecretStore, StoreError};
use crate::util::brief;

const API_VERSION: &str = "7.4";

pub struct AzureEndpoints {
    /// The instance metadata service.
    pub metadata: String,
    /// Replaces `https://<vault>.vault.azure.net`. Normally unset. Set in tests.
    pub vault: Option<String>,
}

impl Default for AzureEndpoints {
    fn default() -> Self {
        AzureEndpoints {
            metadata: "http://169.254.169.254".to_string(),
            vault: None,
        }
    }
}

pub struct AzureKeyVault {
    http: Box<dyn Http>,
    reference: AzureRef,
    endpoints: AzureEndpoints,
}

impl AzureKeyVault {
    pub fn new(http: Box<dyn Http>, reference: AzureRef) -> Self {
        Self::with_endpoints(http, reference, AzureEndpoints::default())
    }

    pub fn with_endpoints(
        http: Box<dyn Http>,
        reference: AzureRef,
        endpoints: AzureEndpoints,
    ) -> Self {
        AzureKeyVault {
            http,
            reference,
            endpoints,
        }
    }

    fn token(&self) -> Result<String, String> {
        let url = format!(
            "{}/metadata/identity/oauth2/token?api-version=2018-02-01&resource=https%3A%2F%2Fvault.azure.net",
            self.endpoints.metadata
        );
        let response = self
            .http
            .send(&Request::new("GET", url).header("Metadata", "true"))
            .map_err(|e| format!("instance metadata: {e}"))?;
        if response.status != 200 {
            return Err(format!("instance metadata answered {}", response.status));
        }
        serde_json::from_slice::<Value>(&response.body)
            .ok()
            .and_then(|v| {
                v.get("access_token")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .ok_or_else(|| "instance metadata gave no access token".to_string())
    }

    fn secret_url(&self) -> String {
        let name = self.reference.url.rsplit('/').next().unwrap_or_default();
        let base = self.endpoints.vault.clone().unwrap_or_else(|| {
            let host_end = self.reference.url["https://".len()..]
                .find('/')
                .map_or(self.reference.url.len(), |i| i + "https://".len());
            self.reference.url[..host_end].to_string()
        });
        format!("{base}/secrets/{name}?api-version={API_VERSION}")
    }

    fn call(&self, request: Request) -> Result<Value, String> {
        let token = self.token()?;
        let response = self
            .http
            .send(&request.header("Authorization", format!("Bearer {token}")))
            .map_err(|e| e.to_string())?;
        if response.status != 200 {
            let detail = serde_json::from_slice::<Value>(&response.body)
                .ok()
                .and_then(|v| {
                    v.pointer("/error/code")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .map(|c| brief(c.as_bytes()))
                .unwrap_or_default();
            return Err(format!("Key Vault answered {} {detail}", response.status)
                .trim()
                .to_string());
        }
        serde_json::from_slice(&response.body)
            .map_err(|_| "Key Vault answered with something that is not JSON".to_string())
    }
}

impl SecretStore for AzureKeyVault {
    fn put(&self, id: &str, value: &str) -> Result<(), StoreError> {
        debug_assert_eq!(id, self.reference.url);
        let body = serde_json::to_vec(&json!({ "value": value }))
            .map_err(|e| StoreError::Write(e.to_string()))?;
        let request = Request::new("PUT", self.secret_url())
            .header("Content-Type", "application/json")
            .body(body);
        self.call(request).map(|_| ()).map_err(StoreError::Write)
    }

    fn get(&self, id: &str) -> Result<String, StoreError> {
        debug_assert_eq!(id, self.reference.url);
        let answer = self
            .call(Request::new("GET", self.secret_url()))
            .map_err(StoreError::Read)?;
        answer
            .get("value")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| StoreError::Read("the secret has no value".to_string()))
    }
}
