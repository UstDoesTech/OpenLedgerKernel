//! K-11: the API service next to one cell. Everything runs against a real Postgres node, never a
//! mock. Start the nodes with `make up` (or `scripts/local_nodes.sh start`), then
//! `cargo test -p api --test accounts -- --ignored`.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use control_plane::{
    ControlPlane, Epoch, GuardConfig, InMemoryControlPlane, LeaseGuard, NodeId, SystemClock,
};
use http_body_util::BodyExt;
use ledger_api::auth::StaticTokens;
use ledger_api::{AppState, router};
use ledger_core::AccountId;
use ledger_store::migrate;
use serde_json::{Value, json};
use tokio_postgres::{Client, NoTls};
use tower::ServiceExt;

const PORT: u16 = 5431;
const ACME: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const ZED: &str = "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz";

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
    state: AppState<InMemoryControlPlane>,
    cp: Arc<InMemoryControlPlane>,
    db: Client,
    schema: String,
}

impl Cell {
    async fn start(name: &str) -> Cell {
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
            .with(ACME, "ten_acme".parse().unwrap(), "usr_ada")
            .unwrap()
            .with(ZED, "ten_zed".parse().unwrap(), "usr_zed")
            .unwrap();
        let state = AppState::new(
            node,
            &schema,
            connect().await,
            cp.clone(),
            guard,
            Arc::new(tokens),
        );
        Cell {
            app: router(state.clone()),
            state,
            cp,
            db,
            schema,
        }
    }

    async fn send(
        &self,
        method: &str,
        uri: &str,
        token: Option<&str>,
        key: Option<&str>,
        body: Option<Value>,
    ) -> (StatusCode, axum::http::HeaderMap, Value) {
        let mut req = Request::builder().method(method).uri(uri);
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
        let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, headers, json)
    }

    async fn open(&self, token: &str, key: &str) -> String {
        let (status, _, body) = self
            .send(
                "POST",
                "/v1/accounts",
                Some(token),
                Some(key),
                Some(json!({"currency": "GBP"})),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        body["account_id"].as_str().unwrap().to_owned()
    }

    async fn drop_schema(&self) {
        self.db
            .batch_execute(&format!("drop schema if exists {} cascade", self.schema))
            .await
            .unwrap();
    }
}

fn problem(headers: &axum::http::HeaderMap, body: &Value, status: StatusCode, code: &str) {
    assert_eq!(headers["content-type"], "application/problem+json");
    assert_eq!(body["status"], status.as_u16());
    assert_eq!(body["code"], code, "{body}");
    assert!(body["title"].is_string() && body["type"].is_string());
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn opening_an_account_homes_it_here_at_the_control_planes_epoch() {
    let c = Cell::start("open").await;
    let (status, headers, body) = c
        .send(
            "POST",
            "/v1/accounts",
            Some(ACME),
            Some("open-0001"),
            Some(json!({"currency": "GBP"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(headers.get("idempotent-replayed").is_none());
    let id = body["account_id"].as_str().unwrap();
    assert!(id.starts_with("acc_") && id.parse::<AccountId>().is_ok());
    assert_eq!(body["currency"], "GBP");
    assert_eq!(body["home"], json!({"cell": "cell-a", "epoch": 1}));

    // The control plane and the cell's database agree.
    let account: AccountId = id.parse().unwrap();
    let (home, epoch) = c.cp.home(&account).await.unwrap();
    assert_eq!((home.as_str(), epoch), ("cell-a", Epoch(1)));
    let row = c.db.query_one(&format!("select tenant_id::text, kind, currency::text, overdraft_limit from {}.account where id = $1", c.schema), &[&id]).await.unwrap();
    assert_eq!(
        (
            row.get::<_, String>(0),
            row.get::<_, String>(1),
            row.get::<_, String>(2),
            row.get::<_, i64>(3)
        ),
        (
            "ten_acme".to_owned(),
            "customer".to_owned(),
            "GBP".to_owned(),
            0
        )
    );
    let fences: i64 = c.db.query_one(&format!("select count(*) from {}.epoch_fence where account_id = $1 and epoch = 1 and home = 'cell-a'", c.schema), &[&id]).await.unwrap().get(0);
    assert_eq!(fences, 1);

    let (status, _, got) = c
        .send("GET", &format!("/v1/accounts/{id}"), Some(ACME), None, None)
        .await;
    assert_eq!(
        (status, &got["home"]),
        (StatusCode::OK, &json!({"cell": "cell-a", "epoch": 1}))
    );
    c.drop_schema().await;
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn an_overdraft_limit_is_kept_and_shows_in_the_balances() {
    let c = Cell::start("overdraft").await;
    let (status, _, body) = c
        .send(
            "POST",
            "/v1/accounts",
            Some(ACME),
            Some("od-0001"),
            Some(json!({"currency": "GBP", "overdraft_limit": {"minor": 5000, "currency": "GBP"}})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = body["account_id"].as_str().unwrap();
    let (status, _, b) = c
        .send(
            "GET",
            &format!("/v1/accounts/{id}/balances"),
            Some(ACME),
            None,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(b["posted"], json!({"minor": 0, "currency": "GBP"}));
    assert_eq!(b["held"], json!({"minor": 0, "currency": "GBP"}));
    assert_eq!(b["available"], json!({"minor": 5000, "currency": "GBP"}));
    assert_eq!(
        b["arranged_overdraft"],
        json!({"minor": 5000, "currency": "GBP"})
    );
    // A negative limit, or one in another currency, is refused.
    for bad in [
        json!({"minor": -1, "currency": "GBP"}),
        json!({"minor": 1, "currency": "EUR"}),
    ] {
        let (status, h, b) = c
            .send(
                "POST",
                "/v1/accounts",
                Some(ACME),
                Some("od-0002"),
                Some(json!({"currency": "GBP", "overdraft_limit": bad})),
            )
            .await;
        problem(&h, &b, status, "invalid_payload");
    }
    c.drop_schema().await;
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn repeating_a_request_returns_the_same_account_and_another_body_is_a_conflict() {
    let c = Cell::start("idem").await;
    let first = c.open(ACME, "idem-0001").await;
    let (status, headers, body) = c
        .send(
            "POST",
            "/v1/accounts",
            Some(ACME),
            Some("idem-0001"),
            Some(json!({"currency": "GBP"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["account_id"], first.as_str());
    assert_eq!(headers["idempotent-replayed"], "true");
    let n: i64 =
        c.db.query_one(
            &format!(
                "select count(*) from {}.account where kind = 'customer'",
                c.schema
            ),
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 1, "a repeat opens nothing");

    let (status, h, b) = c
        .send(
            "POST",
            "/v1/accounts",
            Some(ACME),
            Some("idem-0001"),
            Some(json!({"currency": "EUR"})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    problem(&h, &b, status, "idempotency_conflict");

    // A key belongs to a tenant and principal: another tenant's same key makes its own account.
    let theirs = c.open(ZED, "idem-0001").await;
    assert_ne!(theirs, first);

    // Many concurrent repeats still make one account.
    let mut tasks = Vec::new();
    for _ in 0..16 {
        let app = c.app.clone();
        tasks.push(tokio::spawn(async move {
            let req = Request::builder()
                .method("POST")
                .uri("/v1/accounts")
                .header("authorization", format!("Bearer {ACME}"))
                .header("idempotency-key", "idem-race")
                .body(Body::from(json!({"currency": "GBP"}).to_string()))
                .unwrap();
            let r = app.oneshot(req).await.unwrap();
            let status = r.status();
            let bytes = r.into_body().collect().await.unwrap().to_bytes();
            (
                status,
                serde_json::from_slice::<Value>(&bytes).unwrap()["account_id"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
            )
        }));
    }
    let mut ids = std::collections::BTreeSet::new();
    for t in tasks {
        let (status, id) = t.await.unwrap();
        assert_eq!(status, StatusCode::CREATED);
        ids.insert(id);
    }
    assert_eq!(ids.len(), 1);
    c.drop_schema().await;
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn the_key_and_the_body_are_checked() {
    let c = Cell::start("invalid").await;
    for (key, body) in [
        (None, json!({"currency": "GBP"})),
        (Some("bad key!"), json!({"currency": "GBP"})),
        (Some("ok-0001"), json!({"currency": "ZZZ"})),
        (Some("ok-0002"), json!({})),
        (Some("ok-0003"), json!({"currency": "GBP", "unknown": 1})),
    ] {
        let (status, h, b) = c
            .send("POST", "/v1/accounts", Some(ACME), key, Some(body))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{b}");
        problem(&h, &b, status, "invalid_payload");
    }
    let n: i64 =
        c.db.query_one(&format!("select count(*) from {}.account", c.schema), &[])
            .await
            .unwrap()
            .get(0);
    assert_eq!(n, 0, "nothing was opened");
    c.drop_schema().await;
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn the_tenant_comes_only_from_the_token() {
    let c = Cell::start("tenant").await;
    // No token, a wrong one, and one that is not a bearer token.
    for token in [None, Some("nope")] {
        let (status, h, b) = c.send("GET", "/v1/accounts/acc_x", token, None, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        problem(&h, &b, status, "unauthenticated");
    }
    // A tenant named in the body is refused, whichever tenant it names.
    for tenant in ["ten_acme", "ten_zed"] {
        let (status, h, b) = c
            .send(
                "POST",
                "/v1/accounts",
                Some(ACME),
                Some("t-0001"),
                Some(json!({"currency": "GBP", "tenant": tenant})),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        problem(&h, &b, status, "invalid_payload");
    }
    // One tenant's account is a 404 to the other, whichever way it is asked for.
    let mine = c.open(ACME, "t-0002").await;
    for path in [
        format!("/v1/accounts/{mine}"),
        format!("/v1/accounts/{mine}/balances"),
    ] {
        let (status, h, b) = c.send("GET", &path, Some(ZED), None, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        problem(&h, &b, status, "not_found");
        let (status, _, _) = c.send("GET", &path, Some(ACME), None, None).await;
        assert_eq!(status, StatusCode::OK);
    }
    // The same answer for an account that does not exist, so existence does not leak.
    let (_, _, missing) = c
        .send(
            "GET",
            "/v1/accounts/acc_doesnotexist",
            Some(ZED),
            None,
            None,
        )
        .await;
    let (_, _, other) = c
        .send(
            "GET",
            &format!("/v1/accounts/{mine}"),
            Some(ZED),
            None,
            None,
        )
        .await;
    assert_eq!(missing, other);
    c.drop_schema().await;
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn a_cell_that_is_not_home_says_where_home_is() {
    let c = Cell::start("home").await;
    let id = c.open(ACME, "h-0001").await;
    let account: AccountId = id.parse().unwrap();
    // The account moves to another cell (a promotion), and this cell still holds a copy.
    c.cp.promote(&account, &"cell-b".parse().unwrap(), Epoch(1), 0)
        .await
        .unwrap();
    let (status, h, b) = c
        .send("GET", &format!("/v1/accounts/{id}"), Some(ACME), None, None)
        .await;
    assert_eq!(status, StatusCode::MISDIRECTED_REQUEST);
    problem(&h, &b, status, "wrong_home");
    assert_eq!(b["home"], "cell-b");
    // The other tenant still cannot learn that the account exists or where it is.
    let (status, _, b) = c
        .send("GET", &format!("/v1/accounts/{id}"), Some(ZED), None, None)
        .await;
    assert_eq!(
        (status, b["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("not_found"))
    );
    c.drop_schema().await;
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn a_cell_that_loses_its_lease_stops_serving_and_says_so() {
    let c = Cell::start("lease").await;
    let id = c.open(ACME, "l-0001").await;
    let account: AccountId = id.parse().unwrap();
    let (status, _, h) = c.send("GET", "/healthz", None, None, None).await;
    assert_eq!(
        (
            status,
            h["status"].as_str(),
            h["accounts_homed"].as_u64(),
            h["leases_held"].as_u64()
        ),
        (StatusCode::OK, Some("ok"), Some(1), Some(1))
    );

    c.cp.expire(&account).await.unwrap();
    let (status, headers, b) = c
        .send("GET", &format!("/v1/accounts/{id}"), Some(ACME), None, None)
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    problem(&headers, &b, status, "home_unavailable");
    assert_eq!(b["retryable"], true);
    assert_eq!(headers["retry-after"], "1");
    let (status, _, h) = c.send("GET", "/healthz", None, None, None).await;
    assert_eq!(
        (status, h["status"].as_str(), h["leases_held"].as_u64()),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Some("leases_missing"),
            Some(0)
        )
    );

    c.cp.renew(&account).await.unwrap();
    let (status, _, _) = c
        .send("GET", &format!("/v1/accounts/{id}"), Some(ACME), None, None)
        .await;
    assert_eq!(status, StatusCode::OK);
    c.drop_schema().await;
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn a_draining_cell_takes_no_new_requests_and_says_so() {
    let c = Cell::start("drain").await;
    c.open(ACME, "d-0001").await;
    c.state.admission().set_draining(true);
    let (status, h, b) = c
        .send(
            "POST",
            "/v1/accounts",
            Some(ACME),
            Some("d-0002"),
            Some(json!({"currency": "GBP"})),
        )
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    problem(&h, &b, status, "home_unavailable");
    let (status, _, h) = c.send("GET", "/healthz", None, None, None).await;
    assert_eq!(
        (status, h["status"].as_str(), h["draining"].as_bool()),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Some("draining"),
            Some(true)
        )
    );
    c.state.admission().set_draining(false);
    c.open(ACME, "d-0002").await;
    c.drop_schema().await;
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn nothing_in_an_error_shows_the_database() {
    let c = Cell::start("quiet").await;
    // Break the schema under the service, then ask for something.
    c.db.batch_execute(&format!("drop table {}.api_request cascade", c.schema))
        .await
        .unwrap();
    let (status, h, b) = c
        .send(
            "POST",
            "/v1/accounts",
            Some(ACME),
            Some("q-0001"),
            Some(json!({"currency": "GBP"})),
        )
        .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    problem(&h, &b, status, "internal");
    let text = b.to_string();
    for word in ["api_request", "relation", "postgres", "schema", "sql"] {
        assert!(!text.to_lowercase().contains(word), "{text}");
    }
    c.drop_schema().await;
}
