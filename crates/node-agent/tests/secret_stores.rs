//! The cloud secret stores, tried without a cloud: reference formats, request signing against
//! AWS's published example, and each store's calls against a fake metadata and secret service
//! on the loopback, through the real HTTP client.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use node_agent::aws::{AwsEndpoints, AwsSecretsManager};
use node_agent::azure::{AzureEndpoints, AzureKeyVault};
use node_agent::config::{ConfigError, NodeConfig, StoreConfig};
use node_agent::gcp::{GcpEndpoints, GcpSecretManager};
use node_agent::http::{Http, HttpError, Request, Response, UreqHttp};
use node_agent::node::store_credential;
use node_agent::reference::{parse_aws, parse_azure, parse_gcp};
use node_agent::sigv4::{self, Credentials};
use node_agent::store::{SecretStore, StoreError};
use node_agent::stores::Retrying;

const ARN: &str =
    "arn:aws:secretsmanager:eu-west-2:111111111111:secret:cell-a/administrator-AbCdEf";
const AZURE: &str = "https://cell-a-vault.vault.azure.net/secrets/cell-a-administrator";
const GCP: &str = "projects/ledger-uk-prod/secrets/cell-a-administrator";
const PASSWORD: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

// ---- reference formats -----------------------------------------------------------------------

#[test]
fn aws_references_are_arns_for_secrets_manager() {
    assert_eq!(parse_aws(ARN).unwrap().region, "eu-west-2");
    assert!(parse_aws("arn:aws:secretsmanager:eu-west-2:111111111111:secret:a").is_ok());
    for bad in [
        "cell-a/administrator",
        "arn:aws:ssm:eu-west-2:111111111111:parameter/a",
        "arn:aws:secretsmanager:eu-west-2:11111:secret:a",
        "arn:aws:secretsmanager:europe:111111111111:secret:a",
        "arn:aws-cn:secretsmanager:cn-north-1:111111111111:secret:a",
        "arn:aws:secretsmanager:eu-west-2:111111111111:secret:",
        "arn:aws:secretsmanager:eu-west-2:111111111111:secret:a b",
        "arn:aws:secretsmanager:eu-west-2:111111111111:key:a",
    ] {
        assert!(parse_aws(bad).is_err(), "{bad}");
    }
}

#[test]
fn azure_references_are_key_vault_secret_addresses_without_a_version() {
    assert!(parse_azure(AZURE).is_ok());
    for bad in [
        "/subscriptions/1/resourceGroups/rg/providers/Microsoft.KeyVault/vaults/v/secrets/s",
        "http://v.vault.azure.net/secrets/s",
        "https://v.vault.azure.net/secrets/s",
        "https://cell-a-vault.vault.azure.net/keys/s",
        "https://cell-a-vault.vault.azure.net/secrets/s/0123456789abcdef",
        "https://cell-a-vault.vault.azure.net/secrets/s?x=1",
        "https://cell-a-vault.example.com/secrets/s",
        "https://evil.example/.vault.azure.net/secrets/s",
        "https://cell-a-vault.vault.azure.net/secrets/",
        "https://-bad.vault.azure.net/secrets/s",
    ] {
        assert!(parse_azure(bad).is_err(), "{bad}");
    }
}

#[test]
fn gcp_references_are_secret_names_without_a_version() {
    assert!(parse_gcp(GCP).is_ok());
    for bad in [
        "cell-a-administrator",
        "projects/ledger-uk-prod/secrets/cell-a/versions/1",
        "projects/ledger-uk-prod/secrets/",
        "projects/Ledger/secrets/a",
        "projects/ledger-uk-prod/keys/a",
        "projects/ledger-uk-prod/secrets/a b",
        "projects/ab/secrets/a",
    ] {
        assert!(parse_gcp(bad).is_err(), "{bad}");
    }
}

fn config(store: &str, secret_id: &str) -> String {
    format!(
        r#"{{"tenant":"ten_acme","node":"cell-a-n1","role":"primary","postgres_version":16,"primary_address":"10.10.4.10",
        "synchronous_standby_names":"ANY 1 (cell-a-n2, cell-a-n3)","secret_id":"{secret_id}","secret_store":{{"kind":"{store}"}}}}"#
    )
}

#[test]
fn a_malformed_reference_stops_the_node_at_configuration_time() {
    for (store, good, bad) in [
        ("aws_secrets_manager", ARN, "cell-a/administrator"),
        ("azure_key_vault", AZURE, "cell-a/administrator"),
        ("gcp_secret_manager", GCP, "cell-a/administrator"),
    ] {
        assert!(
            NodeConfig::from_json(&config(store, good)).is_ok(),
            "{store}"
        );
        match NodeConfig::from_json(&config(store, bad)) {
            Err(ConfigError::Invalid {
                field: "secret_id", ..
            }) => {}
            other => panic!("{store}: {other:?}"),
        }
    }
    let kinds: Vec<StoreConfig> = [
        "aws_secrets_manager",
        "azure_key_vault",
        "gcp_secret_manager",
    ]
    .iter()
    .zip([ARN, AZURE, GCP])
    .map(|(k, id)| NodeConfig::from_json(&config(k, id)).unwrap().secret_store)
    .collect();
    assert_eq!(
        kinds,
        [
            StoreConfig::AwsSecretsManager {},
            StoreConfig::AzureKeyVault {},
            StoreConfig::GcpSecretManager {}
        ]
    );
}

#[test]
fn the_cloud_store_settings_carry_no_key_token_or_address() {
    // A store kind is all there is to configure: unknown fields are refused, so there is nowhere
    // to put a key, a token or a different endpoint.
    let text = config("aws_secrets_manager", ARN).replace(
        r#"{"kind":"aws_secrets_manager"}"#,
        r#"{"kind":"aws_secrets_manager","access_key":"x"}"#,
    );
    assert!(NodeConfig::from_json(&text).is_err());
    let text = config("azure_key_vault", AZURE).replace(
        r#"{"kind":"azure_key_vault"}"#,
        r#"{"kind":"azure_key_vault","endpoint":"http://evil"}"#,
    );
    assert!(NodeConfig::from_json(&text).is_err());
}

// ---- signing ---------------------------------------------------------------------------------

#[test]
fn signing_matches_the_example_in_aws_documentation() {
    let credentials = Credentials {
        access_key: "AKIDEXAMPLE".into(),
        secret_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
        session_token: None,
    };
    let headers = vec![
        (
            "Content-Type".to_string(),
            "application/x-www-form-urlencoded; charset=utf-8".to_string(),
        ),
        ("Host".to_string(), "iam.amazonaws.com".to_string()),
        ("X-Amz-Date".to_string(), "20150830T123600Z".to_string()),
    ];
    let signed = sigv4::sign(
        &credentials,
        "us-east-1",
        "iam",
        "GET",
        "/",
        "Action=ListUsers&Version=2010-05-08",
        &headers,
        b"",
        "20150830T123600Z",
    );
    assert_eq!(
        signed.canonical_request_hash,
        "f536975d06c0309214f805bb90ccff089219ecd68b2577efef23edd43b7e1a59"
    );
    assert_eq!(
        signed.signature,
        "5d672d79c15b13162d9279b0855cfba6789a8edb4c82c400e06b5924a6f2b5d7"
    );
    assert_eq!(
        signed.authorization,
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/iam/aws4_request, SignedHeaders=content-type;host;x-amz-date, Signature=5d672d79c15b13162d9279b0855cfba6789a8edb4c82c400e06b5924a6f2b5d7"
    );
}

#[test]
fn timestamps_are_utc_in_the_signing_format() {
    assert_eq!(sigv4::amz_date(0), "19700101T000000Z");
    assert_eq!(sigv4::amz_date(1_440_938_160), "20150830T123600Z");
    assert_eq!(sigv4::amz_date(1_709_164_799), "20240228T235959Z");
    assert_eq!(
        sigv4::amz_date(1_709_164_800),
        "20240229T000000Z",
        "a leap day"
    );
    assert_eq!(sigv4::amz_date(1_794_700_800), "20261115T000000Z");
}

#[test]
fn credentials_never_print() {
    let c = Credentials {
        access_key: "AKIA".into(),
        secret_key: "shh".into(),
        session_token: Some("tok".into()),
    };
    let shown = format!("{c:?}");
    assert!(!shown.contains("shh") && !shown.contains("tok") && !shown.contains("AKIA"));
}

// ---- a fake cloud on the loopback ------------------------------------------------------------

#[derive(Clone, Debug)]
struct Seen {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

type Handler = Box<dyn Fn(&Seen) -> (u16, String) + Send + Sync>;

/// A small HTTP server. Returns its address and the requests it has seen.
fn serve(handler: Handler) -> (String, Arc<Mutex<Vec<Seen>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                continue;
            }
            let mut parts = line.split_whitespace();
            let (method, path) = (
                parts.next().unwrap().to_string(),
                parts.next().unwrap().to_string(),
            );
            let mut headers = HashMap::new();
            loop {
                let mut h = String::new();
                reader.read_line(&mut h).unwrap();
                if h.trim().is_empty() {
                    break;
                }
                if let Some((n, v)) = h.split_once(':') {
                    headers.insert(n.trim().to_ascii_lowercase(), v.trim().to_string());
                }
            }
            let length: usize = headers
                .get("content-length")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            let mut body = vec![0u8; length];
            reader.read_exact(&mut body).unwrap();
            let seen_request = Seen {
                method,
                path,
                headers,
                body,
            };
            log.lock().unwrap().push(seen_request.clone());
            let (status, text) = handler(&seen_request);
            let _ = write!(
                stream,
                "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{text}",
                text.len()
            );
        }
    });
    (address, seen)
}

fn real_http() -> Box<dyn Http> {
    Box::new(UreqHttp::new())
}

/// The secret the fake service holds, shared by the handlers.
type Held = Arc<Mutex<Option<String>>>;

fn aws_fake(held: Held) -> Handler {
    Box::new(move |r| {
        match (r.method.as_str(), r.path.as_str()) {
            ("PUT", "/latest/api/token") => {
                assert_eq!(r.headers.get("x-aws-ec2-metadata-token-ttl-seconds").map(String::as_str), Some("60"));
                (200, "session-token".into())
            }
            ("GET", "/latest/meta-data/iam/security-credentials/") => {
                assert_eq!(r.headers["x-aws-ec2-metadata-token"], "session-token", "version 2 of the metadata service");
                (200, "cell-a-node\n".into())
            }
            ("GET", "/latest/meta-data/iam/security-credentials/cell-a-node") => (
                200,
                r#"{"Code":"Success","AccessKeyId":"AKIDEXAMPLE","SecretAccessKey":"wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY","Token":"role-session-token"}"#.into(),
            ),
            ("POST", "/") => {
                // Check the signature the way AWS would: recompute it from what arrived.
                let auth = r.headers["authorization"].clone();
                let signed_names = auth.split("SignedHeaders=").nth(1).unwrap().split(',').next().unwrap().to_string();
                let headers: Vec<(String, String)> = signed_names.split(';').map(|n| (n.to_string(), r.headers[n].clone())).collect();
                assert!(signed_names.contains("host") && signed_names.contains("x-amz-target") && signed_names.contains("x-amz-security-token"));
                let credentials = Credentials {
                    access_key: "AKIDEXAMPLE".into(),
                    secret_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
                    session_token: None,
                };
                let expected = sigv4::sign(&credentials, "eu-west-2", "secretsmanager", "POST", "/", "", &headers, &r.body, &r.headers["x-amz-date"]);
                assert_eq!(auth, expected.authorization, "the signature must match what was sent");
                assert_eq!(r.headers["x-amz-security-token"], "role-session-token");
                let body: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
                assert_eq!(body["SecretId"], ARN);
                match r.headers["x-amz-target"].as_str() {
                    "secretsmanager.PutSecretValue" => {
                        let token = body["ClientRequestToken"].as_str().unwrap();
                        assert!((32..=64).contains(&token.len()));
                        *held.lock().unwrap() = Some(body["SecretString"].as_str().unwrap().to_string());
                        (200, r#"{"ARN":"x","VersionId":"v"}"#.into())
                    }
                    "secretsmanager.GetSecretValue" => match held.lock().unwrap().clone() {
                        Some(v) => (200, serde_json::json!({ "SecretString": v }).to_string()),
                        None => (400, r#"{"__type":"ResourceNotFoundException","message":"no"}"#.into()),
                    },
                    other => panic!("unexpected target {other}"),
                }
            }
            other => panic!("unexpected request {other:?}"),
        }
    })
}

#[test]
fn aws_writes_and_reads_the_credential_with_a_correct_signature_and_the_machines_role() {
    let held: Held = Arc::default();
    let (address, seen) = serve(aws_fake(held.clone()));
    let store = AwsSecretsManager::with_endpoints(
        real_http(),
        parse_aws(ARN).unwrap(),
        AwsEndpoints {
            metadata: address.clone(),
            secrets: Some(address),
        },
        || 1_440_938_160,
    );
    store_credential(&store, ARN, PASSWORD).unwrap();
    assert_eq!(held.lock().unwrap().as_deref(), Some(PASSWORD));
    assert_eq!(store.get(ARN).unwrap(), PASSWORD);
    // The credential travels in the body, never in a path, and no request carries a configured key.
    for r in seen.lock().unwrap().iter() {
        assert!(!r.path.contains(PASSWORD));
    }
}

fn azure_fake(held: Held) -> Handler {
    Box::new(move |r| {
        if r.path.starts_with("/metadata/identity/oauth2/token") {
            assert_eq!(r.headers["metadata"], "true");
            assert!(r.path.contains("resource=https%3A%2F%2Fvault.azure.net"));
            return (
                200,
                r#"{"access_token":"vault-token","token_type":"Bearer"}"#.into(),
            );
        }
        assert_eq!(r.headers["authorization"], "Bearer vault-token");
        assert_eq!(r.path, "/secrets/cell-a-administrator?api-version=7.4");
        match r.method.as_str() {
            "PUT" => {
                let body: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
                *held.lock().unwrap() = Some(body["value"].as_str().unwrap().to_string());
                (200, r#"{"id":"x"}"#.into())
            }
            "GET" => match held.lock().unwrap().clone() {
                Some(v) => (200, serde_json::json!({ "value": v }).to_string()),
                None => (404, r#"{"error":{"code":"SecretNotFound"}}"#.into()),
            },
            other => panic!("unexpected method {other}"),
        }
    })
}

#[test]
fn azure_writes_and_reads_the_credential_with_the_machines_managed_identity() {
    let held: Held = Arc::default();
    let (address, seen) = serve(azure_fake(held.clone()));
    let store = AzureKeyVault::with_endpoints(
        real_http(),
        parse_azure(AZURE).unwrap(),
        AzureEndpoints {
            metadata: address.clone(),
            vault: Some(address),
        },
    );
    store_credential(&store, AZURE, PASSWORD).unwrap();
    assert_eq!(held.lock().unwrap().as_deref(), Some(PASSWORD));
    for r in seen.lock().unwrap().iter() {
        assert!(!r.path.contains(PASSWORD));
    }
}

fn gcp_fake(held: Held) -> Handler {
    Box::new(move |r| {
        if r.path == "/computeMetadata/v1/instance/service-accounts/default/token" {
            assert_eq!(r.headers["metadata-flavor"], "Google");
            return (
                200,
                r#"{"access_token":"sa-token","expires_in":3000,"token_type":"Bearer"}"#.into(),
            );
        }
        assert_eq!(r.headers["authorization"], "Bearer sa-token");
        match (r.method.as_str(), r.path.as_str()) {
            ("POST", "/v1/projects/ledger-uk-prod/secrets/cell-a-administrator:addVersion") => {
                use base64::Engine;
                let body: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
                let data = body["payload"]["data"].as_str().unwrap();
                let text = String::from_utf8(
                    base64::engine::general_purpose::STANDARD
                        .decode(data)
                        .unwrap(),
                )
                .unwrap();
                *held.lock().unwrap() = Some(text);
                (200, r#"{"name":"x"}"#.into())
            }
            (
                "GET",
                "/v1/projects/ledger-uk-prod/secrets/cell-a-administrator/versions/latest:access",
            ) => match held.lock().unwrap().clone() {
                Some(v) => {
                    use base64::Engine;
                    (200, serde_json::json!({ "payload": { "data": base64::engine::general_purpose::STANDARD.encode(v) } }).to_string())
                }
                None => (404, r#"{"error":{"status":"NOT_FOUND"}}"#.into()),
            },
            other => panic!("unexpected request {other:?}"),
        }
    })
}

#[test]
fn gcp_writes_and_reads_the_credential_with_the_machines_service_account() {
    let held: Held = Arc::default();
    let (address, seen) = serve(gcp_fake(held.clone()));
    let store = GcpSecretManager::with_endpoints(
        real_http(),
        parse_gcp(GCP).unwrap(),
        GcpEndpoints {
            metadata: address.clone(),
            secrets: address,
        },
    );
    store_credential(&store, GCP, PASSWORD).unwrap();
    assert_eq!(held.lock().unwrap().as_deref(), Some(PASSWORD));
    for r in seen.lock().unwrap().iter() {
        assert!(!r.path.contains(PASSWORD));
    }
}

// ---- failures --------------------------------------------------------------------------------

#[test]
fn a_refusal_names_the_service_status_but_never_a_token_or_the_secret() {
    let (address, _) = serve(Box::new(|r| {
        if r.path.starts_with("/metadata/identity") {
            (200, r#"{"access_token":"super-secret-token"}"#.into())
        } else {
            (403, r#"{"error":{"code":"Forbidden","message":"caller super-secret-token lacks access"}}"#.into())
        }
    }));
    let store = AzureKeyVault::with_endpoints(
        real_http(),
        parse_azure(AZURE).unwrap(),
        AzureEndpoints {
            metadata: address.clone(),
            vault: Some(address),
        },
    );
    let error = store.put(AZURE, PASSWORD).unwrap_err().to_string();
    assert!(
        error.contains("403") && error.contains("Forbidden"),
        "{error}"
    );
    assert!(
        !error.contains("super-secret-token") && !error.contains(PASSWORD),
        "{error}"
    );
}

#[test]
fn a_machine_with_no_role_or_identity_is_a_clear_error() {
    let (address, _) = serve(Box::new(|r| match r.path.as_str() {
        "/latest/api/token" => (200, "t".into()),
        _ => (404, String::new()),
    }));
    let store = AwsSecretsManager::with_endpoints(
        real_http(),
        parse_aws(ARN).unwrap(),
        AwsEndpoints {
            metadata: address.clone(),
            secrets: Some(address),
        },
        || 0,
    );
    let error = store.put(ARN, PASSWORD).unwrap_err().to_string();
    assert!(error.contains("instance metadata answered 404"), "{error}");
}

#[test]
fn an_unreachable_service_is_an_error_that_does_not_name_the_secret() {
    let store = GcpSecretManager::with_endpoints(
        real_http(),
        parse_gcp(GCP).unwrap(),
        GcpEndpoints {
            metadata: "http://127.0.0.1:1".into(),
            secrets: "http://127.0.0.1:1".into(),
        },
    );
    let error = store.put(GCP, PASSWORD).unwrap_err().to_string();
    assert!(error.contains("metadata server"), "{error}");
    assert!(!error.contains(PASSWORD));
}

struct Scripted {
    answers: Mutex<Vec<Result<Response, HttpError>>>,
}
impl Http for Scripted {
    fn send(&self, _: &Request) -> Result<Response, HttpError> {
        self.answers.lock().unwrap().remove(0)
    }
}

#[test]
fn a_store_that_returns_something_else_than_was_written_is_refused() {
    struct Lossy;
    impl SecretStore for Lossy {
        fn put(&self, _: &str, _: &str) -> Result<(), StoreError> {
            Ok(())
        }
        fn get(&self, _: &str) -> Result<String, StoreError> {
            Ok("an older value".into())
        }
    }
    let error = store_credential(&Lossy, "x", PASSWORD)
        .unwrap_err()
        .to_string();
    assert!(error.contains("did not return"), "{error}");
    assert!(!error.contains(PASSWORD) && !error.contains("an older value"));
}

#[test]
fn a_new_machines_identity_may_take_a_few_tries_to_be_recognised() {
    struct Flaky(Mutex<u32>);
    impl SecretStore for Flaky {
        fn put(&self, _: &str, _: &str) -> Result<(), StoreError> {
            let mut n = self.0.lock().unwrap();
            *n += 1;
            if *n < 3 {
                Err(StoreError::Write("403".into()))
            } else {
                Ok(())
            }
        }
        fn get(&self, _: &str) -> Result<String, StoreError> {
            Ok(String::new())
        }
    }
    let store = Retrying::new(Flaky(Mutex::new(0)), 5, Duration::ZERO, Duration::ZERO);
    store.put("x", "v").unwrap();
    assert_eq!(*store.inner_for_test().0.lock().unwrap(), 3);

    let never = Retrying::new(Flaky(Mutex::new(0)), 2, Duration::ZERO, Duration::ZERO);
    let _ = Scripted {
        answers: Mutex::new(vec![]),
    };
    assert!(
        never.put("x", "v").is_err(),
        "it gives up after the attempts"
    );
}
