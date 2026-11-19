//! Google Secret Manager, with a token for the machine's own service account from the metadata
//! server. Nothing is configured or stored.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};

use crate::http::{Http, Request};
use crate::reference::GcpRef;
use crate::store::{SecretStore, StoreError};
use crate::util::brief;

pub struct GcpEndpoints {
    /// The metadata server.
    pub metadata: String,
    /// Secret Manager's address. Private Google Access or Private Service Connect makes this
    /// name resolve to a private address.
    pub secrets: String,
}

impl Default for GcpEndpoints {
    fn default() -> Self {
        GcpEndpoints {
            metadata: "http://metadata.google.internal".to_string(),
            secrets: "https://secretmanager.googleapis.com".to_string(),
        }
    }
}

pub struct GcpSecretManager {
    http: Box<dyn Http>,
    reference: GcpRef,
    endpoints: GcpEndpoints,
}

impl GcpSecretManager {
    pub fn new(http: Box<dyn Http>, reference: GcpRef) -> Self {
        Self::with_endpoints(http, reference, GcpEndpoints::default())
    }

    pub fn with_endpoints(http: Box<dyn Http>, reference: GcpRef, endpoints: GcpEndpoints) -> Self {
        GcpSecretManager {
            http,
            reference,
            endpoints,
        }
    }

    fn token(&self) -> Result<String, String> {
        let url = format!(
            "{}/computeMetadata/v1/instance/service-accounts/default/token",
            self.endpoints.metadata
        );
        let response = self
            .http
            .send(&Request::new("GET", url).header("Metadata-Flavor", "Google"))
            .map_err(|e| format!("metadata server: {e}"))?;
        if response.status != 200 {
            return Err(format!("metadata server answered {}", response.status));
        }
        serde_json::from_slice::<Value>(&response.body)
            .ok()
            .and_then(|v| {
                v.get("access_token")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .ok_or_else(|| "metadata server gave no access token".to_string())
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
                    v.pointer("/error/status")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .map(|s| brief(s.as_bytes()))
                .unwrap_or_default();
            return Err(
                format!("Secret Manager answered {} {detail}", response.status)
                    .trim()
                    .to_string(),
            );
        }
        serde_json::from_slice(&response.body)
            .map_err(|_| "Secret Manager answered with something that is not JSON".to_string())
    }
}

impl SecretStore for GcpSecretManager {
    fn put(&self, id: &str, value: &str) -> Result<(), StoreError> {
        debug_assert_eq!(id, self.reference.resource);
        let url = format!(
            "{}/v1/{}:addVersion",
            self.endpoints.secrets, self.reference.resource
        );
        let body = serde_json::to_vec(&json!({ "payload": { "data": STANDARD.encode(value) } }))
            .map_err(|e| StoreError::Write(e.to_string()))?;
        let request = Request::new("POST", url)
            .header("Content-Type", "application/json")
            .body(body);
        self.call(request).map(|_| ()).map_err(StoreError::Write)
    }

    fn get(&self, id: &str) -> Result<String, StoreError> {
        debug_assert_eq!(id, self.reference.resource);
        let url = format!(
            "{}/v1/{}/versions/latest:access",
            self.endpoints.secrets, self.reference.resource
        );
        let answer = self
            .call(Request::new("GET", url))
            .map_err(StoreError::Read)?;
        let data = answer
            .pointer("/payload/data")
            .and_then(Value::as_str)
            .ok_or_else(|| StoreError::Read("the secret has no payload".to_string()))?;
        let bytes = STANDARD
            .decode(data)
            .map_err(|_| StoreError::Read("the payload is not valid base64".to_string()))?;
        String::from_utf8(bytes)
            .map_err(|_| StoreError::Read("the payload is not text".to_string()))
    }
}
