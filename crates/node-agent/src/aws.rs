//! AWS Secrets Manager, signed with the machine's own role credentials from the instance
//! metadata service (version 2, which needs a session token). Nothing is configured or stored.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::http::{Http, Request, host_of};
use crate::reference::AwsRef;
use crate::sigv4::{self, Credentials};
use crate::store::{SecretStore, StoreError};
use crate::util::{brief, random_hex};

pub struct AwsEndpoints {
    /// The instance metadata service.
    pub metadata: String,
    /// Where Secrets Manager is. Normally unset: the regional name, which a private endpoint with
    /// private DNS resolves to a private address. Set in tests.
    pub secrets: Option<String>,
}

impl Default for AwsEndpoints {
    fn default() -> Self {
        AwsEndpoints {
            metadata: "http://169.254.169.254".to_string(),
            secrets: None,
        }
    }
}

pub struct AwsSecretsManager {
    http: Box<dyn Http>,
    reference: AwsRef,
    endpoints: AwsEndpoints,
    now: fn() -> u64,
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl AwsSecretsManager {
    pub fn new(http: Box<dyn Http>, reference: AwsRef) -> Self {
        Self::with_endpoints(http, reference, AwsEndpoints::default(), unix_now)
    }

    pub fn with_endpoints(
        http: Box<dyn Http>,
        reference: AwsRef,
        endpoints: AwsEndpoints,
        now: fn() -> u64,
    ) -> Self {
        AwsSecretsManager {
            http,
            reference,
            endpoints,
            now,
        }
    }

    fn metadata(&self, request: Request) -> Result<String, String> {
        let response = self
            .http
            .send(&request)
            .map_err(|e| format!("instance metadata: {e}"))?;
        if response.status != 200 {
            return Err(format!("instance metadata answered {}", response.status));
        }
        String::from_utf8(response.body)
            .map_err(|_| "instance metadata answered with something that is not text".to_string())
    }

    fn credentials(&self) -> Result<Credentials, String> {
        let base = &self.endpoints.metadata;
        let token = self.metadata(
            Request::new("PUT", format!("{base}/latest/api/token"))
                .header("X-aws-ec2-metadata-token-ttl-seconds", "60"),
        )?;
        let get = |path: &str| {
            self.metadata(
                Request::new("GET", format!("{base}{path}"))
                    .header("X-aws-ec2-metadata-token", token.trim()),
            )
        };
        let roles = get("/latest/meta-data/iam/security-credentials/")?;
        let role = roles
            .lines()
            .next()
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .ok_or("the machine has no role attached")?;
        let body = get(&format!(
            "/latest/meta-data/iam/security-credentials/{role}"
        ))?;
        let v: Value = serde_json::from_str(&body)
            .map_err(|_| "the role credentials are not valid JSON".to_string())?;
        let field = |name: &str| v.get(name).and_then(Value::as_str).map(str::to_string);
        Ok(Credentials {
            access_key: field("AccessKeyId").ok_or("the role credentials have no AccessKeyId")?,
            secret_key: field("SecretAccessKey")
                .ok_or("the role credentials have no SecretAccessKey")?,
            session_token: field("Token"),
        })
    }

    fn call(&self, target: &str, body: &Value) -> Result<Value, String> {
        let credentials = self.credentials()?;
        let base = self.endpoints.secrets.clone().unwrap_or_else(|| {
            format!(
                "https://secretsmanager.{}.amazonaws.com",
                self.reference.region
            )
        });
        let payload = serde_json::to_vec(body).map_err(|e| e.to_string())?;
        let amz_date = sigv4::amz_date((self.now)());
        let mut headers = vec![
            (
                "content-type".to_string(),
                "application/x-amz-json-1.1".to_string(),
            ),
            ("host".to_string(), host_of(&base).to_string()),
            ("x-amz-date".to_string(), amz_date.clone()),
            ("x-amz-target".to_string(), target.to_string()),
        ];
        if let Some(token) = &credentials.session_token {
            headers.push(("x-amz-security-token".to_string(), token.clone()));
        }
        let signed = sigv4::sign(
            &credentials,
            &self.reference.region,
            "secretsmanager",
            "POST",
            "/",
            "",
            &headers,
            &payload,
            &amz_date,
        );
        let mut request = Request::new("POST", format!("{base}/")).body(payload);
        for (name, value) in headers.iter().filter(|(n, _)| n != "host") {
            request = request.header(name, value);
        }
        request = request.header("Authorization", signed.authorization);
        let response = self.http.send(&request).map_err(|e| e.to_string())?;
        if response.status != 200 {
            let detail = serde_json::from_slice::<Value>(&response.body)
                .ok()
                .and_then(|v| v.get("__type").and_then(Value::as_str).map(str::to_string))
                .map(|t| brief(t.as_bytes()))
                .unwrap_or_default();
            return Err(
                format!("Secrets Manager answered {} {detail}", response.status)
                    .trim()
                    .to_string(),
            );
        }
        serde_json::from_slice(&response.body)
            .map_err(|_| "Secrets Manager answered with something that is not JSON".to_string())
    }
}

impl SecretStore for AwsSecretsManager {
    fn put(&self, id: &str, value: &str) -> Result<(), StoreError> {
        debug_assert_eq!(id, self.reference.arn);
        let token = random_hex(16).map_err(|e| StoreError::Write(e.to_string()))?;
        let body = json!({
            "SecretId": self.reference.arn,
            "SecretString": value,
            "ClientRequestToken": token,
        });
        self.call("secretsmanager.PutSecretValue", &body)
            .map(|_| ())
            .map_err(StoreError::Write)
    }

    fn get(&self, id: &str) -> Result<String, StoreError> {
        debug_assert_eq!(id, self.reference.arn);
        let answer = self
            .call(
                "secretsmanager.GetSecretValue",
                &json!({ "SecretId": self.reference.arn }),
            )
            .map_err(StoreError::Read)?;
        answer
            .get("SecretString")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| StoreError::Read("the secret has no text value".to_string()))
    }
}
