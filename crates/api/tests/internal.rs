//! K-14: who may call a cell's internal endpoints. Real Postgres on port 5431, never a mock.
//! `cargo test -p api --test internal -- --ignored`.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use control_plane::{GuardConfig, InMemoryControlPlane, LeaseGuard, NodeId, SystemClock};
use http_body_util::BodyExt;
use ledger_api::auth::{PeerTokens, StaticTokens};
use ledger_api::{AppState, INTERNAL_ROUTES, internal_router, router};
use ledger_store::migrate;
use serde_json::{Value, json};
use tokio_postgres::NoTls;
use tower::ServiceExt;

const USER: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const ACME_CELL: &str = "cccccccccccccccccccccccccccccccccccccccc";
const ZED_CELL: &str = "dddddddddddddddddddddddddddddddddddddddd";

struct Cell {
    public: Router,
    internal: Router,
    /// The same service with no internal credentials configured at all.
    closed: Router,
    schema: String,
}

async fn start(name: &str) -> Cell {
    let schema = format!("int_{name}");
    let connect = || async {
        let (c, conn) = tokio_postgres::Config::new()
            .host("127.0.0.1")
            .port(5431)
            .user("ledger")
            .dbname("postgres")
            .connect_timeout(Duration::from_secs(5))
            .connect(NoTls)
            .await
            .expect("Postgres on port 5431");
        tokio::spawn(conn);
        c
    };
    let mut db = connect().await;
    db.batch_execute(&format!("drop schema if exists {schema} cascade"))
        .await
        .unwrap();
    migrate(&mut db, &schema).await.unwrap();
    let clock = Arc::new(SystemClock::new());
    let cp = Arc::new(InMemoryControlPlane::new(
        Duration::from_secs(30),
        clock.clone(),
    ));
    let build = |peers: bool, conn: tokio_postgres::Client| {
        let node: NodeId = "cell-a".parse().unwrap();
        let guard = LeaseGuard::new(
            node.clone(),
            GuardConfig::for_ttl(Duration::from_secs(30)),
            clock.clone(),
        )
        .unwrap();
        let tokens = StaticTokens::new()
            .with(USER, "ten_acme".parse().unwrap(), "usr_ada")
            .unwrap();
        let state = AppState::new(node, &schema, conn, cp.clone(), guard, Arc::new(tokens));
        if peers {
            state.with_peer_auth(Arc::new(
                PeerTokens::new()
                    .with(ACME_CELL, "cell-b", vec!["ten_acme".parse().unwrap()])
                    .unwrap()
                    .with(ZED_CELL, "cell-b", vec!["ten_zed".parse().unwrap()])
                    .unwrap(),
            ))
        } else {
            state
        }
    };
    let open = build(true, connect().await);
    let closed = build(false, connect().await);
    Cell {
        public: router(open.clone()),
        internal: internal_router(open),
        closed: internal_router(closed),
        schema,
    }
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    tenant: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(t) = token {
        req = req.header("authorization", format!("Bearer {t}"));
    }
    if let Some(t) = tenant {
        req = req.header("x-ledger-tenant", t);
    }
    let body = body
        .map(|b| Body::from(b.to_string()))
        .unwrap_or_else(Body::empty);
    let r = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = r.status();
    let bytes = r.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn prepare_body() -> Value {
    json!({"journal_id": "jr_abc", "payee_account_id": "acc_x",
           "amount": {"minor": 1, "currency": "GBP"}})
}

#[tokio::test]
#[ignore = "needs Postgres on port 5431"]
async fn only_a_cell_credential_for_the_tenant_is_accepted_on_the_internal_endpoints() {
    let c = start("auth").await;
    let uri = "/internal/v1/transfers/prepare";
    let refused = |status: StatusCode, body: &Value| {
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
        assert_eq!(body["code"], "unauthenticated");
    };

    // No credential, a wrong one, and a tenant's own token: none is a cell.
    for token in [
        None,
        Some("zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz"),
        Some(USER),
    ] {
        let (s, b) = call(
            &c.internal,
            "POST",
            uri,
            token,
            Some("ten_acme"),
            Some(prepare_body()),
        )
        .await;
        refused(s, &b);
    }
    // A credential, but no tenant, a garbled tenant, or a tenant it does not cover.
    for tenant in [None, Some("not a tenant"), Some("ten_zed")] {
        let (s, b) = call(
            &c.internal,
            "POST",
            uri,
            Some(ACME_CELL),
            tenant,
            Some(prepare_body()),
        )
        .await;
        refused(s, &b);
    }
    // The other cell credential covers ten_zed only.
    let (s, b) = call(
        &c.internal,
        "POST",
        uri,
        Some(ZED_CELL),
        Some("ten_acme"),
        Some(prepare_body()),
    )
    .await;
    refused(s, &b);
    // With the right credential and tenant it gets as far as the account, which does not exist.
    let (s, b) = call(
        &c.internal,
        "POST",
        uri,
        Some(ACME_CELL),
        Some("ten_acme"),
        Some(prepare_body()),
    )
    .await;
    assert_eq!(
        (s, b["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("not_found"))
    );

    // A service with no internal credentials configured answers nobody.
    let (s, b) = call(
        &c.closed,
        "POST",
        uri,
        Some(ACME_CELL),
        Some("ten_acme"),
        Some(prepare_body()),
    )
    .await;
    refused(s, &b);

    // And a cell credential is not a tenant token on the public listener.
    let (s, b) = call(
        &c.public,
        "POST",
        "/v1/accounts",
        Some(ACME_CELL),
        None,
        Some(json!({"currency": "GBP"})),
    )
    .await;
    refused(s, &b);

    let db = tokio_postgres::Config::new()
        .host("127.0.0.1")
        .port(5431)
        .user("ledger")
        .dbname("postgres")
        .connect(NoTls)
        .await
        .unwrap();
    tokio::spawn(db.1);
    db.0.batch_execute(&format!("drop schema if exists {} cascade", c.schema))
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "needs Postgres on port 5431"]
async fn another_tenants_account_is_a_missing_one_even_to_a_cell() {
    let c = start("tenant").await;
    let (s, opened) = call(
        &c.public,
        "POST",
        "/v1/accounts",
        Some(USER),
        None,
        Some(json!({"currency": "GBP"})),
    )
    .await;
    // Header check: the public call needs the idempotency key.
    assert_eq!(s, StatusCode::BAD_REQUEST, "{opened}");
    let req = Request::builder()
        .method("POST")
        .uri("/v1/accounts")
        .header("authorization", format!("Bearer {USER}"))
        .header("idempotency-key", "k-1")
        .body(Body::from(json!({"currency": "GBP"}).to_string()))
        .unwrap();
    let r = c.public.clone().oneshot(req).await.unwrap();
    let bytes = r.into_body().collect().await.unwrap().to_bytes();
    let id = serde_json::from_slice::<Value>(&bytes).unwrap()["account_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let uri = format!("/internal/v1/accounts/{id}");
    let (s, b) = call(
        &c.internal,
        "GET",
        &uri,
        Some(ACME_CELL),
        Some("ten_acme"),
        None,
    )
    .await;
    assert_eq!((s, b["currency"].as_str()), (StatusCode::OK, Some("GBP")));
    // The credential for ten_zed asks about the same id: the answer is the one for a missing account.
    let (s, b) = call(
        &c.internal,
        "GET",
        &uri,
        Some(ZED_CELL),
        Some("ten_zed"),
        None,
    )
    .await;
    assert_eq!(
        (s, b["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("not_found"))
    );
    let (s, _) = call(
        &c.internal,
        "GET",
        "/internal/v1/accounts/acc_nothere",
        Some(ZED_CELL),
        Some("ten_zed"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    let db = tokio_postgres::Config::new()
        .host("127.0.0.1")
        .port(5431)
        .user("ledger")
        .dbname("postgres")
        .connect(NoTls)
        .await
        .unwrap();
    tokio::spawn(db.1);
    db.0.batch_execute(&format!("drop schema if exists {} cascade", c.schema))
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "needs Postgres on port 5431"]
async fn every_internal_route_in_the_list_is_served_and_wants_a_credential() {
    let c = start("routes").await;
    for (method, path) in INTERNAL_ROUTES {
        let uri = path.replace("{accountId}", "acc_x");
        let (s, b) = call(&c.internal, &method.to_uppercase(), &uri, None, None, None).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{method} {path}: {b}");
    }
    let db = tokio_postgres::Config::new()
        .host("127.0.0.1")
        .port(5431)
        .user("ledger")
        .dbname("postgres")
        .connect(NoTls)
        .await
        .unwrap();
    tokio::spawn(db.1);
    db.0.batch_execute(&format!("drop schema if exists {} cascade", c.schema))
        .await
        .unwrap();
}
