//! K-24a: each token source asks the right service the right way and reads the answer. The
//! services are replaced by a local server that gives recorded answers, so nothing here reaches a
//! cloud or needs a credential. The answers use values made for this test only.

use std::sync::{Arc, Mutex};

use ledger_api::dbauth::{
    AwsIam, AzureEntra, GcpIam, LakebaseOauth, TokenError, TokenSource, region_of_host,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// What the server was asked: the request line, the headers (names in lower case) and the body.
#[derive(Clone, Debug)]
struct Seen {
    line: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

type Log = Arc<Mutex<Vec<Seen>>>;

/// Serves recorded answers. A route is the start of "METHOD /path", its status and its body.
async fn serve(routes: Vec<(&'static str, u16, String)>) -> (String, Log) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let seen = log.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let routes = routes.clone();
            let seen = seen.clone();
            tokio::spawn(async move {
                let mut raw = Vec::new();
                let mut buf = [0u8; 4096];
                let (head, mut body) = loop {
                    let n = stream.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    raw.extend_from_slice(&buf[..n]);
                    if let Some(i) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                        break (
                            String::from_utf8_lossy(&raw[..i]).into_owned(),
                            raw[i + 4..].to_vec(),
                        );
                    }
                };
                let mut lines = head.lines();
                let line = lines.next().unwrap_or("").to_owned();
                let headers: Vec<(String, String)> = lines
                    .filter_map(|l| l.split_once(':'))
                    .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
                    .collect();
                let want: usize = headers
                    .iter()
                    .find(|(k, _)| k == "content-length")
                    .and_then(|(_, v)| v.parse().ok())
                    .unwrap_or(0);
                while body.len() < want {
                    let n = stream.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    body.extend_from_slice(&buf[..n]);
                }
                seen.lock().unwrap().push(Seen {
                    line: line.clone(),
                    headers,
                    body: String::from_utf8_lossy(&body).into_owned(),
                });
                let (status, answer) = routes
                    .iter()
                    .find(|(prefix, _, _)| line.starts_with(prefix))
                    .map(|(_, s, b)| (*s, b.clone()))
                    .unwrap_or((404, String::new()));
                let reply = format!(
                    "HTTP/1.1 {status} X\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{answer}",
                    answer.len()
                );
                let _ = stream.write_all(reply.as_bytes()).await;
            });
        }
    });
    (base, log)
}

/// A value made from a label, so no credential-looking value is written in this file.
fn made(label: &str) -> String {
    use ring::digest;
    digest::digest(&digest::SHA256, label.as_bytes())
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[tokio::test]
async fn azure_asks_the_metadata_service_for_the_database_resource() {
    let token = made("azure-token");
    let answer = format!(
        r#"{{"access_token":"{token}","expires_in":"86399","resource":"https://ossrdbms-aad.database.windows.net","token_type":"Bearer"}}"#
    );
    let (base, log) = serve(vec![("GET /metadata/identity/oauth2/token", 200, answer)]).await;

    let got = AzureEntra::at(&base, None).token().await.unwrap();
    assert_eq!(got.secret(), token);
    assert_eq!(got.valid_for().as_secs(), 86_399);
    let first = log.lock().unwrap()[0].clone();
    assert!(
        first
            .line
            .contains("resource=https%3A%2F%2Fossrdbms-aad.database.windows.net")
    );
    assert!(first.line.contains("api-version=2018-02-01"));
    assert!(!first.line.contains("client_id"));
    assert_eq!(first.header("metadata"), Some("true"));

    // A user-assigned identity is named by its client id.
    AzureEntra::at(
        &base,
        Some("11111111-2222-3333-4444-555555555555".to_owned()),
    )
    .token()
    .await
    .unwrap();
    let second = log.lock().unwrap()[1].clone();
    assert!(
        second
            .line
            .contains("client_id=11111111-2222-3333-4444-555555555555")
    );
}

#[tokio::test]
async fn google_asks_the_metadata_server_for_the_sql_login_scope() {
    let token = made("gcp-token");
    let answer = format!(r#"{{"access_token":"{token}","expires_in":3599,"token_type":"Bearer"}}"#);
    let (base, log) = serve(vec![(
        "GET /computeMetadata/v1/instance/service-accounts/default/token",
        200,
        answer,
    )])
    .await;

    let got = GcpIam::at(&base).token().await.unwrap();
    assert_eq!(got.secret(), token);
    assert_eq!(got.valid_for().as_secs(), 3_599);
    let first = log.lock().unwrap()[0].clone();
    assert!(
        first
            .line
            .contains("scopes=https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fsqlservice.login")
    );
    assert_eq!(first.header("metadata-flavor"), Some("Google"));
}

#[tokio::test]
async fn aws_gets_the_roles_credentials_with_a_session_token_and_signs_a_database_token() {
    let session = made("imds-session");
    let (akid, secret, security) = (
        format!("AKIA{}", made("a")[..16].to_uppercase()),
        made("b")[..40].to_owned(),
        made("c"),
    );
    let creds = format!(
        r#"{{"Code":"Success","AccessKeyId":"{akid}","SecretAccessKey":"{secret}","Token":"{security}","Expiration":"2027-01-15T14:00:00Z"}}"#
    );
    let (base, log) = serve(vec![
        ("PUT /latest/api/token", 200, session.clone()),
        (
            "GET /latest/meta-data/iam/security-credentials/ ",
            200,
            "ledger-api-role\n".to_owned(),
        ),
        (
            "GET /latest/meta-data/iam/security-credentials/ledger-api-role",
            200,
            creds,
        ),
    ])
    .await;

    let host = "db1.example.eu-west-2.rds.amazonaws.com";
    let got = AwsIam::at(&base, host, 5432, "ledger_api", "eu-west-2")
        .token()
        .await
        .unwrap();
    let t = got.secret();
    assert!(t.starts_with(&format!("{host}:5432/?Action=connect&DBUser=ledger_api&")));
    assert!(t.contains("X-Amz-Algorithm=AWS4-HMAC-SHA256"));
    assert!(t.contains(&format!("X-Amz-Security-Token={security}")));
    assert!(t.contains("%2Feu-west-2%2Frds-db%2Faws4_request"));
    assert!(t.contains("&X-Amz-Signature="));
    assert!(
        !t.contains(&secret),
        "the secret key must not be in the token"
    );

    let seen = log.lock().unwrap().clone();
    assert_eq!(seen.len(), 3);
    assert_eq!(
        seen[0].header("x-aws-ec2-metadata-token-ttl-seconds"),
        Some("60")
    );
    assert_eq!(
        seen[1].header("x-aws-ec2-metadata-token"),
        Some(session.as_str())
    );
    assert_eq!(
        seen[2].header("x-aws-ec2-metadata-token"),
        Some(session.as_str())
    );
}

#[tokio::test]
async fn aws_with_no_role_on_the_machine_is_not_a_token() {
    let (base, _) = serve(vec![
        ("PUT /latest/api/token", 200, made("s")),
        (
            "GET /latest/meta-data/iam/security-credentials/ ",
            200,
            String::new(),
        ),
    ])
    .await;
    let err = AwsIam::at(&base, "h", 5432, "u", "eu-west-2")
        .token()
        .await
        .unwrap_err();
    assert_eq!(err, TokenError::Malformed);
}

#[tokio::test]
async fn lakebase_trades_the_principals_secret_for_a_workspace_token_and_then_a_database_credential()
 {
    let (workspace_token, credential) = (made("workspace"), made("database-credential"));
    let (base, log) = serve(vec![
        (
            "POST /oidc/v1/token",
            200,
            format!(
                r#"{{"access_token":"{workspace_token}","token_type":"Bearer","expires_in":3600}}"#
            ),
        ),
        (
            "POST /api/2.0/postgres/credentials",
            200,
            format!(r#"{{"token":"{credential}","expire_time":"2027-01-15T09:00:00Z"}}"#),
        ),
    ])
    .await;
    let dir = std::env::temp_dir().join(format!("ledger-lakebase-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("oauth.json");
    // Client id "a" and secret "b" make the Basic header "YTpi".
    std::fs::write(
        &file,
        format!(
            r#"{{"workspace":"{base}/","client_id":"a","client_secret":"b","endpoint":"projects/p/branches/b/endpoints/e"}}"#
        ),
    )
    .unwrap();

    let got = LakebaseOauth::new(file.clone()).token().await.unwrap();
    assert_eq!(got.secret(), credential);
    let seen = log.lock().unwrap().clone();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].header("authorization"), Some("Basic YTpi"));
    assert_eq!(seen[0].body, "grant_type=client_credentials&scope=all-apis");
    assert_eq!(
        seen[1].header("authorization"),
        Some(format!("Bearer {workspace_token}").as_str())
    );
    assert_eq!(
        seen[1].body,
        r#"{"endpoint":"projects/p/branches/b/endpoints/e"}"#
    );

    // The file is read again each time, so a missing one is a failure to get a token, not a crash.
    std::fs::remove_file(&file).unwrap();
    assert_eq!(
        LakebaseOauth::new(file).token().await.unwrap_err(),
        TokenError::Unavailable
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn an_answer_that_is_a_refusal_a_fault_or_not_a_token_is_named_and_not_quoted() {
    let quoted = made("must-not-appear");
    for (status, body, want) in [
        (
            403,
            format!(r#"{{"error":"{quoted}"}}"#),
            TokenError::Refused,
        ),
        (401, quoted.clone(), TokenError::Refused),
        (500, quoted.clone(), TokenError::Unavailable),
        (200, quoted.clone(), TokenError::Malformed),
        (
            200,
            r#"{"expires_in":3600}"#.to_owned(),
            TokenError::Malformed,
        ),
        (
            200,
            r#"{"access_token":""}"#.to_owned(),
            TokenError::Malformed,
        ),
    ] {
        let (base, _) = serve(vec![("GET /", status, body)]).await;
        let err = GcpIam::at(&base).token().await.unwrap_err();
        assert_eq!(err, want, "status {status}");
        assert!(!format!("{err:?} {}", err.text()).contains(&quoted));
    }
}

#[tokio::test]
async fn a_service_that_does_not_answer_is_unavailable() {
    // A port that was open and is now closed.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let err = GcpIam::at(&format!("http://127.0.0.1:{port}"))
        .token()
        .await
        .unwrap_err();
    assert_eq!(err, TokenError::Unavailable);
}

#[tokio::test]
async fn plain_http_is_refused_for_any_address_but_this_machine_and_the_metadata_services() {
    for base in [
        "http://example.com",
        "http://10.1.2.3",
        "http://203.0.113.9:8080",
    ] {
        let err = AzureEntra::at(base, None).token().await.unwrap_err();
        assert_eq!(err, TokenError::Refused, "{base}");
    }
}

#[test]
fn the_region_comes_from_the_address_or_from_a_setting() {
    assert_eq!(
        region_of_host("x.y.us-east-1.rds.amazonaws.com").as_deref(),
        Some("us-east-1")
    );
    assert_eq!(region_of_host("localhost"), None);
}
