//! K-11 against the real control plane: etcd decides who is home and when a lease has run out.
//! Needs Postgres on port 5431 and etcd on `LEDGER_ETCD` (default `http://127.0.0.1:2379`),
//! which `make up` provides: `cargo test -p api --test etcd -- --ignored`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use control_plane::{
    ControlPlane, Epoch, EtcdConfig, EtcdControlPlane, GuardConfig, LeaseGuard, NodeId, SystemClock,
};
use http_body_util::BodyExt;
use ledger_api::auth::StaticTokens;
use ledger_api::{AppState, router};
use ledger_core::AccountId;
use ledger_store::migrate;
use serde_json::{Value, json};
use tokio_postgres::NoTls;
use tower::ServiceExt;

const TOKEN: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

fn endpoint() -> String {
    std::env::var("LEDGER_ETCD").unwrap_or_else(|_| "http://127.0.0.1:2379".into())
}

fn unique_prefix() -> String {
    static N: AtomicU32 = AtomicU32::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!(
        "/api-test/{nanos}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    )
}

async fn connect() -> tokio_postgres::Client {
    let (client, conn) = tokio_postgres::Config::new()
        .host("127.0.0.1")
        .port(5431)
        .user("ledger")
        .dbname("postgres")
        .connect(NoTls)
        .await
        .expect("Postgres is on port 5431");
    tokio::spawn(conn);
    client
}

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    key: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {TOKEN}"));
    if let Some(k) = key {
        req = req.header("idempotency-key", k);
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

#[tokio::test]
#[ignore = "needs Postgres on port 5431 and etcd on 2379"]
async fn the_same_behaviour_holds_with_etcd_as_the_control_plane() {
    let schema = "api_etcd";
    let mut db = connect().await;
    db.batch_execute(&format!("drop schema if exists {schema} cascade"))
        .await
        .unwrap();
    migrate(&mut db, schema).await.unwrap();

    let ttl = Duration::from_secs(30);
    let cp = Arc::new(
        EtcdControlPlane::connect(EtcdConfig::new(vec![endpoint()], unique_prefix(), ttl))
            .await
            .expect("etcd is reachable"),
    );
    let node: NodeId = "cell-a".parse().unwrap();
    let guard = LeaseGuard::new(
        node.clone(),
        GuardConfig::for_ttl(ttl),
        Arc::new(SystemClock::new()),
    )
    .unwrap();
    let tokens = StaticTokens::new()
        .with(TOKEN, "ten_acme".parse().unwrap(), "usr_ada")
        .unwrap();
    let state = AppState::new(
        node,
        schema,
        connect().await,
        cp.clone(),
        guard,
        Arc::new(tokens),
    );
    let app = router(state);

    // Open an account: etcd records the home and epoch, the cell records the account.
    let (status, body) = call(
        &app,
        "POST",
        "/v1/accounts",
        Some("etcd-0001"),
        Some(json!({"currency": "GBP"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["home"], json!({"cell": "cell-a", "epoch": 1}));
    let id = body["account_id"].as_str().unwrap().to_owned();
    let account: AccountId = id.parse().unwrap();
    let (home, epoch) = cp.home(&account).await.unwrap();
    assert_eq!((home.as_str(), epoch), ("cell-a", Epoch(1)));

    // A repeat is the same account.
    let (status, again) = call(
        &app,
        "POST",
        "/v1/accounts",
        Some("etcd-0001"),
        Some(json!({"currency": "GBP"})),
    )
    .await;
    assert_eq!(
        (status, &again["account_id"]),
        (StatusCode::CREATED, &body["account_id"])
    );

    // The health check counts the lease etcd holds.
    let (status, h) = call(&app, "GET", "/healthz", None, None).await;
    assert_eq!(
        (status, h["leases_held"].as_u64()),
        (StatusCode::OK, Some(1))
    );

    // A lease expired on purpose stops service, and renewing it brings it back.
    cp.expire(&account).await.unwrap();
    let (status, b) = call(&app, "GET", &format!("/v1/accounts/{id}"), None, None).await;
    assert_eq!(
        (status, b["code"].as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, Some("home_unavailable"))
    );
    cp.renew(&account).await.unwrap();
    let (status, _) = call(&app, "GET", &format!("/v1/accounts/{id}"), None, None).await;
    assert_eq!(status, StatusCode::OK);

    // A promotion moves the home, and this cell says where it went.
    cp.promote(&account, &"cell-b".parse().unwrap(), Epoch(1), 0)
        .await
        .unwrap();
    let (status, b) = call(&app, "GET", &format!("/v1/accounts/{id}"), None, None).await;
    assert_eq!(
        (status, b["code"].as_str(), b["home"].as_str()),
        (
            StatusCode::MISDIRECTED_REQUEST,
            Some("wrong_home"),
            Some("cell-b")
        )
    );
    db.batch_execute(&format!("drop schema if exists {schema} cascade"))
        .await
        .unwrap();
}
