//! K-21: sign in with a username and password, through the real routes, against a real Postgres
//! node. Start the nodes with `make up` (or `scripts/local_nodes.sh start`), then
//! `cargo test -p api --test sign_in -- --ignored`.
//!
//! The tests hold the rules: the token a sign-in makes works on the other routes and names the
//! user's tenant, another tenant's account is still a `404`, ending it stops it, an unknown user and a
//! wrong password look the same, and nothing that was sent comes back.

use std::sync::Arc;
use std::time::Duration;

use argon2::{Argon2, PasswordHasher};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use control_plane::{GuardConfig, InMemoryControlPlane, LeaseGuard, NodeId, SystemClock};
use http_body_util::BodyExt;
use ledger_api::auth::StaticTokens;
use ledger_api::client_addr::TrustedProxies;
use ledger_api::login::{Login, MonotonicClock, Users};
use ledger_api::reload::Reloadable;
use ledger_api::{AppState, router};
use ledger_store::migrate;
use serde_json::{Value, json};
use tokio_postgres::{Client, NoTls};
use tower::ServiceExt;

const PORT: u16 = 5431;
const NAME: &str = "user@bankofmumanddad.test";
/// Made when it is used, never written down, so that no password is a literal in the code.
fn password() -> String {
    format!("test-{}-password", std::process::id())
}
fn wrong() -> String {
    format!("wrong-{}-password", std::process::id())
}
const OTHER_TOKEN: &str = "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz";

async fn connect() -> Client {
    let (client, conn) = tokio_postgres::Config::new()
        .host("127.0.0.1")
        .port(PORT)
        .user("ledger")
        .dbname("postgres")
        .connect_timeout(Duration::from_secs(5))
        .connect(NoTls)
        .await
        .unwrap_or_else(|e| panic!("port {PORT}: {e}"));
    tokio::spawn(conn);
    client
}

struct Cell {
    app: Router,
    db: Client,
    schema: String,
}

impl Cell {
    async fn start(name: &str, with_sign_in: bool) -> Cell {
        Cell::start_behind(name, with_sign_in, "").await
    }

    /// As `start`, with the ranges whose forwarded client address is believed.
    async fn start_behind(name: &str, with_sign_in: bool, trusted: &str) -> Cell {
        let schema = format!("api_{name}");
        let mut db = connect().await;
        db.batch_execute(&format!("drop schema if exists {schema} cascade"))
            .await
            .unwrap();
        migrate(&mut db, &schema).await.unwrap();
        let node: NodeId = "cell-a".parse().unwrap();
        let clock = Arc::new(SystemClock::new());
        let cp = Arc::new(InMemoryControlPlane::new(
            Duration::from_secs(30),
            clock.clone(),
        ));
        let guard = LeaseGuard::new(
            node.clone(),
            GuardConfig::for_ttl(Duration::from_secs(30)),
            clock,
        )
        .unwrap();
        let tokens = StaticTokens::new()
            .with(OTHER_TOKEN, "ten_zed".parse().unwrap(), "usr_zed")
            .unwrap();
        let mut state = AppState::new(node, &schema, connect().await, cp, guard, Arc::new(tokens));
        if with_sign_in {
            let hash = Argon2::default()
                .hash_password_with_salt(password().as_bytes(), b"a test salt.....")
                .unwrap()
                .to_string();
            let users = Users::from_json(
                &json!([{"username": NAME, "password_hash": hash,
                         "tenant": "ten_acme", "principal": "usr_ada"}])
                .to_string(),
            )
            .unwrap();
            let login = Login::new(Reloadable::new(users), Arc::new(MonotonicClock::new()));
            state = state
                .with_login(login)
                .with_trusted_proxies(TrustedProxies::parse(trusted).unwrap());
        }
        // Take the lease for the node, as the other tests do, by opening nothing: the guard
        // renews on the first write.
        Cell {
            app: router(state),
            db,
            schema,
        }
    }

    async fn send(
        &self,
        uri: &str,
        token: Option<&str>,
        key: Option<&str>,
        body: Option<Value>,
    ) -> (StatusCode, axum::http::HeaderMap, Value, String) {
        let mut req = Request::builder().method("POST").uri(uri);
        if let Some(t) = token {
            req = req.header("authorization", format!("Bearer {t}"));
        }
        if let Some(k) = key {
            req = req.header("idempotency-key", k);
        }
        let body = body
            .map(|b| Body::from(b.to_string()))
            .unwrap_or_else(Body::empty);
        let response = self
            .app
            .clone()
            .oneshot(req.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, headers, json, text)
    }

    async fn sign_in(&self, username: &str, password: &str) -> (StatusCode, Value, String) {
        let (status, _, json, text) = self
            .send(
                "/v1/sessions",
                None,
                None,
                Some(json!({"username": username, "password": password})),
            )
            .await;
        (status, json, text)
    }

    /// A sign-in that arrives from `peer`, with `forwarded` as its `X-Forwarded-For` header.
    async fn sign_in_from(
        &self,
        peer: &str,
        forwarded: Option<&str>,
        username: &str,
        password: &str,
    ) -> (StatusCode, Value, String) {
        let mut req = Request::builder()
            .method("POST")
            .uri("/v1/sessions")
            .header("content-type", "application/json");
        if let Some(f) = forwarded {
            req = req.header("x-forwarded-for", f);
        }
        let mut req = req
            .body(Body::from(
                json!({"username": username, "password": password}).to_string(),
            ))
            .unwrap();
        let peer: std::net::SocketAddr = format!("{peer}:44321").parse().unwrap();
        req.extensions_mut()
            .insert(axum::extract::ConnectInfo(ledger_api::client_addr::Peer(
                peer,
            )));
        let response = self.app.clone().oneshot(req).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            text,
        )
    }

    async fn drop_schema(&self) {
        self.db
            .batch_execute(&format!("drop schema if exists {} cascade", self.schema))
            .await
            .unwrap();
    }
}

#[tokio::test]
#[ignore = "needs the local Postgres nodes"]
async fn a_sign_in_makes_a_token_that_works_on_the_other_routes_until_it_is_ended() {
    let cell = Cell::start("signin_works", true).await;
    let (status, headers, body, _) = cell
        .send(
            "/v1/sessions",
            None,
            None,
            Some(json!({"username": NAME, "password": &password()})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(headers["cache-control"], "no-store");
    assert_eq!(body["principal"], "usr_ada");
    assert_eq!(body["expires_in"], 1800);
    let token = body["token"].as_str().unwrap().to_owned();

    // The token opens an account for the user's tenant.
    let (status, _, account, _) = cell
        .send(
            "/v1/accounts",
            Some(&token),
            Some("signin-open-1"),
            Some(json!({"currency": "GBP"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{account}");

    // Ending it ends it, and ending it again is the same.
    let (status, ..) = cell
        .send("/v1/sessions:end", Some(&token), None, None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _, body, _) = cell
        .send(
            "/v1/accounts",
            Some(&token),
            Some("signin-open-2"),
            Some(json!({"currency": "GBP"})),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["code"], "unauthenticated");
    let (status, ..) = cell
        .send("/v1/sessions:end", Some(&token), None, None)
        .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the token no longer counts as one"
    );
    cell.drop_schema().await;
}

#[tokio::test]
#[ignore = "needs the local Postgres nodes"]
async fn another_tenants_account_is_still_not_found_with_a_signed_in_token() {
    let cell = Cell::start("signin_tenant", true).await;
    let (_, body, _) = cell.sign_in(NAME, &password()).await;
    let token = body["token"].as_str().unwrap().to_owned();
    let (status, _, mine, _) = cell
        .send(
            "/v1/accounts",
            Some(&token),
            Some("tenant-open-1"),
            Some(json!({"currency": "GBP"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{mine}");
    let id = mine["account_id"].as_str().unwrap();
    // Another tenant's token reads it as not found, not as forbidden.
    let response = cell
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/v1/accounts/{id}"))
                .header("authorization", format!("Bearer {OTHER_TOKEN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    cell.drop_schema().await;
}

#[tokio::test]
#[ignore = "needs the local Postgres nodes"]
async fn an_unknown_user_and_a_wrong_password_get_the_same_answer_and_nothing_sent_comes_back() {
    let cell = Cell::start("signin_same", true).await;
    let (s1, b1, t1) = cell.sign_in(NAME, &wrong()).await;
    let (s2, b2, t2) = cell
        .sign_in("nobody@bankofmumanddad.test", &password())
        .await;
    assert_eq!((s1, &b1), (s2, &b2));
    assert_eq!(s1, StatusCode::UNAUTHORIZED);
    assert_eq!(b1["code"], "invalid_credentials");
    for text in [t1, t2] {
        assert!(!text.contains("bankofmumanddad") && !text.contains(&wrong()));
        assert!(!text.contains(&password()));
    }
    cell.drop_schema().await;
}

#[tokio::test]
#[ignore = "needs the local Postgres nodes"]
async fn five_failures_in_a_row_are_a_429_and_a_bad_body_is_refused() {
    let cell = Cell::start("signin_lock", true).await;
    for _ in 0..5 {
        let (status, ..) = cell.sign_in(NAME, &wrong()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    let (status, body, _) = cell.sign_in(NAME, &password()).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert_eq!(body["code"], "too_many_attempts");
    // A field that is not listed, such as a tenant, is refused.
    let (status, _, body, _) = cell
        .send(
            "/v1/sessions",
            None,
            None,
            Some(json!({"username": "other@bankofmumanddad.test", "password": &password(), "tenant": "ten_zed"})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    cell.drop_schema().await;
}

#[tokio::test]
#[ignore = "needs the local Postgres nodes"]
async fn a_cell_with_no_sign_in_says_so_and_still_takes_bearer_tokens() {
    let cell = Cell::start("signin_off", false).await;
    let (status, body, _) = cell.sign_in(NAME, &password()).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["code"], "sign_in_not_available");
    let (status, ..) = cell
        .send(
            "/v1/accounts",
            Some(OTHER_TOKEN),
            Some("off-open-1"),
            Some(json!({"currency": "GBP"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    cell.drop_schema().await;
}

#[tokio::test]
#[ignore = "needs the local Postgres nodes"]
async fn the_page_is_served_by_the_real_router_with_no_token_and_the_policy_headers() {
    let cell = Cell::start("page", true).await;
    for path in ["/ui", "/ui/app.js", "/ui/style.css"] {
        let response = cell
            .app
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        let headers = response.headers();
        assert_eq!(
            headers["content-security-policy"],
            ledger_api::ui::CONTENT_SECURITY_POLICY
        );
        assert_eq!(headers["cache-control"], "no-store");
        assert_eq!(headers["x-frame-options"], "DENY");
    }
    // The page does not open the API: a route that needs a token still does.
    let response = cell
        .app
        .clone()
        .oneshot(
            Request::get("/v1/accounts/acc_x")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    cell.drop_schema().await;
}

#[tokio::test]
#[ignore = "needs the local Postgres nodes"]
async fn the_page_can_open_an_account_and_read_it_and_nothing_the_api_says_is_cached() {
    let cell = Cell::start("page_flow", true).await;
    let (_, _, body, _) = cell
        .send(
            "/v1/sessions",
            None,
            None,
            Some(json!({"username": NAME, "password": &password()})),
        )
        .await;
    let token = body["token"].as_str().unwrap().to_owned();

    // Exactly what the page sends to open an account: an overdraft limit as minor units and a currency.
    let (status, headers, account, _) = cell
        .send(
            "/v1/accounts",
            Some(&token),
            Some("page-open-1"),
            Some(
                json!({"currency": "GBP", "overdraft_limit": {"minor": 50000, "currency": "GBP"}}),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "opening the account failed");
    assert_eq!(headers["cache-control"], "no-store");
    let id = account["account_id"].as_str().unwrap().to_owned();
    assert!(id.starts_with("acc_"));

    // Sending the same form again with the same key opens the same account, not another.
    let (status, _, again, _) = cell
        .send(
            "/v1/accounts",
            Some(&token),
            Some("page-open-1"),
            Some(
                json!({"currency": "GBP", "overdraft_limit": {"minor": 50000, "currency": "GBP"}}),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(again["account_id"], id);

    let get = |uri: String, token: Option<String>| {
        let app = cell.app.clone();
        async move {
            let mut req = Request::get(uri);
            if let Some(t) = token {
                req = req.header("authorization", format!("Bearer {t}"));
            }
            let response = app.oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
            let status = response.status();
            let headers = response.headers().clone();
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            (
                status,
                headers,
                serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null),
            )
        }
    };

    // What the page reads: the account, and its balances as whole minor units with a currency.
    let (status, headers, view) = get(format!("/v1/accounts/{id}"), Some(token.clone())).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["cache-control"], "no-store");
    assert_eq!(view["currency"], "GBP");
    assert_eq!(view["status"], "open");
    assert!(view["home"]["cell"].is_string());

    let (status, headers, balances) =
        get(format!("/v1/accounts/{id}/balances"), Some(token.clone())).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["cache-control"], "no-store");
    for name in ["posted", "held", "available", "arranged_overdraft"] {
        assert!(
            balances[name]["minor"].is_i64(),
            "{name} is not whole minor units"
        );
        assert_eq!(balances[name]["currency"], "GBP", "{name}");
    }
    assert_eq!(balances["posted"]["minor"], 0);
    assert_eq!(balances["arranged_overdraft"]["minor"], 50000);
    assert_eq!(balances["available"]["minor"], 50000);

    // Failures are not cached either, and a missing account and a wrong token say so plainly.
    let (status, headers, _) = get(
        "/v1/accounts/acc_nosuch/balances".into(),
        Some(token.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(headers["cache-control"], "no-store");
    let (status, headers, _) = get(format!("/v1/accounts/{id}/balances"), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(headers["cache-control"], "no-store");
    let (_, headers, _) = get("/healthz".into(), None).await;
    assert_eq!(headers["cache-control"], "no-store");
    cell.drop_schema().await;
}

const BALANCER: &str = "10.11.192.5";
const TRUSTED: &str = "10.11.192.0/20";

#[tokio::test]
#[ignore = "needs the local Postgres nodes"]
async fn behind_the_balancer_a_lock_is_for_the_real_address_and_a_made_up_one_is_not_believed() {
    let cell = Cell::start_behind("signin_forwarded", true, TRUSTED).await;
    // Someone at 198.51.100.7 gets the name wrong five times, and each time claims to be someone
    // else on the left of the header. The balancer adds the address it saw to the right.
    for i in 0..5 {
        let claim = format!("192.0.2.{i}, 198.51.100.7");
        let (status, ..) = cell
            .sign_in_from(BALANCER, Some(&claim), NAME, &wrong())
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    let (status, body, text) = cell
        .sign_in_from(BALANCER, Some("198.51.100.7"), NAME, &password())
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert_eq!(body["code"], "too_many_attempts");
    assert!(!text.contains("198.51.100") && !text.contains(NAME));
    // The person, from another address behind the same balancer, signs in.
    let (status, ..) = cell
        .sign_in_from(BALANCER, Some("203.0.113.9"), NAME, &password())
        .await;
    assert_eq!(status, StatusCode::CREATED);
    cell.drop_schema().await;
}

#[tokio::test]
#[ignore = "needs the local Postgres nodes"]
async fn a_caller_that_is_not_the_balancer_cannot_choose_its_address() {
    let cell = Cell::start_behind("signin_spoof", true, TRUSTED).await;
    // The same stranger, connecting directly, sends a different forwarded address each time.
    for i in 0..5 {
        let claim = format!("192.0.2.{i}");
        let (status, ..) = cell
            .sign_in_from("198.51.100.7", Some(&claim), NAME, &wrong())
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    let (status, ..) = cell
        .sign_in_from("198.51.100.7", Some("192.0.2.200"), NAME, &password())
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    // With no trusted range at all, the header is never read, even from the balancer's address.
    let cell2 = Cell::start("signin_spoof_none", true).await;
    for i in 0..5 {
        let claim = format!("192.0.2.{i}");
        let _ = cell2
            .sign_in_from(BALANCER, Some(&claim), NAME, &wrong())
            .await;
    }
    let (status, ..) = cell2
        .sign_in_from(BALANCER, Some("192.0.2.99"), NAME, &password())
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    cell.drop_schema().await;
    cell2.drop_schema().await;
}

#[tokio::test]
#[ignore = "needs the local Postgres nodes"]
async fn one_address_trying_many_names_gets_the_same_429_whether_or_not_the_name_exists() {
    let cell = Cell::start("signin_sweep", true).await;
    for i in 0..20 {
        let (status, ..) = cell
            .sign_in_from(
                "198.51.100.7",
                None,
                &format!("nobody{i}@bankofmumanddad.test"),
                &wrong(),
            )
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    let (s1, b1, t1) = cell
        .sign_in_from("198.51.100.7", None, NAME, &password())
        .await;
    let (s2, b2, t2) = cell
        .sign_in_from(
            "198.51.100.7",
            None,
            "nobody@bankofmumanddad.test",
            &password(),
        )
        .await;
    assert_eq!((s1, &b1), (s2, &b2));
    assert_eq!(s1, StatusCode::TOO_MANY_REQUESTS);
    for text in [t1, t2] {
        assert!(!text.contains("198.51.100") && !text.contains("bankofmumanddad"));
    }
    cell.drop_schema().await;
}
