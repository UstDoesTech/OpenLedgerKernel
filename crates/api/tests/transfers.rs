//! K-12: transfers over HTTP, on one cell and across two. Everything runs against real Postgres
//! nodes on ports 5431 and 5432, never a mock. Start them with `make up` (or
//! `scripts/local_nodes.sh start`), then `cargo test -p api --test transfers -- --ignored`.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use control_plane::{
    ControlPlane, GuardConfig, InMemoryControlPlane, LeaseGuard, NodeId, SystemClock,
};
use http_body_util::BodyExt;
use ledger_api::auth::{PeerTokens, StaticTokens};
use ledger_api::peer::PeerClient;
use ledger_api::{AppState, ROUTES, internal_router, router};
use ledger_check::{Options, check_cell};
use ledger_core::{AccountId, TenantId};
use ledger_store::cross_cell;
use ledger_store::migrate;
use serde_json::{Value, json};
use tokio_postgres::{Client, NoTls};
use tower::ServiceExt;

const ACME: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const ZED: &str = "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz";
const CELL_A: &str = "cccccccccccccccccccccccccccccccccccccccc";
const CELL_B: &str = "dddddddddddddddddddddddddddddddddddddddd";
const A: u16 = 5431;
const B: u16 = 5432;

async fn connect(port: u16) -> Client {
    let (client, conn) = tokio_postgres::Config::new()
        .host("127.0.0.1")
        .port(port)
        .user("ledger")
        .dbname("postgres")
        .connect_timeout(Duration::from_secs(5))
        .connect(NoTls)
        .await
        .unwrap_or_else(|e| panic!("port {port}: {e}"));
    tokio::spawn(conn);
    client
}

/// Two cells sharing one control plane. The service at `a` can reach `b` as a peer.
struct Pair {
    a: Router,
    b: Router,
    /// Cell b's state and internal port, so a test can stop its internal listener and start it again.
    state_b: AppState<InMemoryControlPlane>,
    port_b: u16,
    server_a: tokio::task::JoinHandle<()>,
    server_b: std::sync::Mutex<tokio::task::JoinHandle<()>>,
    cp: Arc<InMemoryControlPlane>,
    db_a: Client,
    db_b: Client,
    schema: String,
}

impl Pair {
    async fn start(name: &str) -> Pair {
        let schema = format!("xfer_{name}");
        let mut dbs = Vec::new();
        for port in [A, B] {
            let mut db = connect(port).await;
            db.batch_execute(&format!("drop schema if exists {schema} cascade"))
                .await
                .unwrap();
            migrate(&mut db, &schema).await.unwrap();
            dbs.push(db);
        }
        let db_b = dbs.pop().unwrap();
        let db_a = dbs.pop().unwrap();
        let clock = Arc::new(SystemClock::new());
        let cp = Arc::new(InMemoryControlPlane::new(
            Duration::from_secs(30),
            clock.clone(),
        ));
        let tokens = || {
            Arc::new(
                StaticTokens::new()
                    .with(ACME, "ten_acme".parse().unwrap(), "usr_ada")
                    .unwrap()
                    .with(ZED, "ten_zed".parse().unwrap(), "usr_zed")
                    .unwrap(),
            )
        };
        let peer_tokens = || {
            Arc::new(
                PeerTokens::new()
                    .with(
                        CELL_A,
                        "cell-a",
                        vec!["ten_acme".parse().unwrap(), "ten_zed".parse().unwrap()],
                    )
                    .unwrap()
                    .with(
                        CELL_B,
                        "cell-b",
                        vec!["ten_acme".parse().unwrap(), "ten_zed".parse().unwrap()],
                    )
                    .unwrap(),
            )
        };
        let listener_a = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let listener_b = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (port_a, port_b) = (
            listener_a.local_addr().unwrap().port(),
            listener_b.local_addr().unwrap().port(),
        );
        // Each cell's service knows how to call the other one, and who may call it.
        let service = |node: &str, port: u16, other: &str, other_port: u16, token: &'static str| {
            let clock = clock.clone();
            let cp = cp.clone();
            let schema = schema.clone();
            let tokens = tokens();
            let node: NodeId = node.parse().unwrap();
            let other = other.to_owned();
            let peer_tokens = peer_tokens();
            async move {
                let guard = LeaseGuard::new(
                    node.clone(),
                    GuardConfig::for_ttl(Duration::from_secs(30)),
                    clock,
                )
                .unwrap();
                AppState::new(node, &schema, connect(port).await, cp, guard, tokens)
                    .with_peer(
                        &other,
                        PeerClient::new(&format!("http://127.0.0.1:{other_port}"), token, None)
                            .unwrap(),
                    )
                    .with_peer_auth(peer_tokens)
            }
        };
        let state_a = service("cell-a", A, "cell-b", port_b, CELL_A).await;
        let state_b = service("cell-b", B, "cell-a", port_a, CELL_B).await;
        let server_a = tokio::spawn({
            let app = internal_router(state_a.clone());
            async move {
                let _ = axum::serve(listener_a, app).await;
            }
        });
        let server_b = tokio::spawn({
            let app = internal_router(state_b.clone());
            async move {
                let _ = axum::serve(listener_b, app).await;
            }
        });
        Pair {
            a: router(state_a),
            b: router(state_b.clone()),
            state_b,
            port_b,
            server_a,
            server_b: std::sync::Mutex::new(server_b),
            cp,
            db_a,
            db_b,
            schema,
        }
    }

    /// Stops cell b's internal listener, as if its service were down or unreachable.
    async fn stop_b(&self) {
        let old = {
            let mut handle = self.server_b.lock().unwrap();
            std::mem::replace(&mut *handle, tokio::spawn(async {}))
        };
        old.abort();
        let _ = old.await;
    }

    /// Starts it again on the same port.
    async fn start_b(&self) {
        let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{}", self.port_b))
            .await
            .unwrap();
        let app = internal_router(self.state_b.clone());
        *self.server_b.lock().unwrap() = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
    }

    async fn call(
        app: &Router,
        method: &str,
        uri: &str,
        token: Option<&str>,
        key: Option<&str>,
        body: Option<Value>,
    ) -> (StatusCode, HeaderMap, Value) {
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
        let r = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
        let (status, headers) = (r.status(), r.headers().clone());
        let bytes = r.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            headers,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    /// Opens an account with an overdraft allowance, which is how a test gives it spending power.
    async fn open(&self, at_b: bool, key: &str, overdraft: i64) -> String {
        let app = if at_b { &self.b } else { &self.a };
        let (status, _, body) = Self::call(
            app,
            "POST",
            "/v1/accounts",
            Some(ACME),
            Some(key),
            Some(json!({"currency": "GBP", "overdraft_limit": {"minor": overdraft, "currency": "GBP"}})),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        body["account_id"].as_str().unwrap().to_owned()
    }

    async fn transfer(
        &self,
        key: &str,
        payer: &str,
        payee: &str,
        minor: i64,
    ) -> (StatusCode, HeaderMap, Value) {
        Self::call(
            &self.a,
            "POST",
            "/v1/transfers",
            Some(ACME),
            Some(key),
            Some(json!({"payer_account_id": payer, "payee_account_id": payee,
                        "amount": {"minor": minor, "currency": "GBP"}})),
        )
        .await
    }

    async fn balance(&self, at_b: bool, account: &str) -> i64 {
        let db = if at_b { &self.db_b } else { &self.db_a };
        db.query_one(
            &format!(
                "select balance from {}.account where tenant_id = 'ten_acme' and id = $1::text",
                self.schema
            ),
            &[&account],
        )
        .await
        .unwrap()
        .get(0)
    }

    async fn count(&self, db: &Client, table: &str) -> i64 {
        db.query_one(
            &format!("select count(*) from {}.{table}", self.schema),
            &[],
        )
        .await
        .unwrap()
        .get(0)
    }

    async fn invariants_hold(&self) {
        for (db, cell) in [(&self.db_a, "cell-a"), (&self.db_b, "cell-b")] {
            let v = check_cell(db, &self.schema, cell, &Options::default())
                .await
                .unwrap();
            assert!(
                v.is_empty(),
                "{cell}: {} violations: {:?}",
                v.len(),
                v.first()
            );
        }
    }

    async fn drop(&self) {
        self.server_a.abort();
        self.server_b.lock().unwrap().abort();
        for db in [&self.db_a, &self.db_b] {
            db.batch_execute(&format!("drop schema if exists {} cascade", self.schema))
                .await
                .unwrap();
        }
    }
}

fn code(body: &Value) -> &str {
    body["code"].as_str().unwrap_or("")
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431 and 5432"]
async fn a_transfer_on_one_cell_posts_and_a_repeat_writes_nothing() {
    let p = Pair::start("same").await;
    let payer = p.open(false, "open-1", 1_000).await;
    let payee = p.open(false, "open-2", 0).await;

    let (status, headers, body) = p.transfer("pay-1", &payer, &payee, 250).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["status"], "posted");
    assert_eq!(body["amount"], json!({"minor": 250, "currency": "GBP"}));
    assert_eq!(body["replayed"], false);
    assert!(headers.get("idempotent-replayed").is_none());
    assert!(body["journal_id"].as_str().unwrap().starts_with("jr_"));
    assert_eq!(body["watermarks"].as_array().unwrap().len(), 1);
    assert_eq!(body["watermarks"][0]["cell"], "cell-a");
    assert_eq!(p.balance(false, &payer).await, -250);
    assert_eq!(p.balance(false, &payee).await, 250);
    let entries = p.count(&p.db_a, "entry").await;
    assert_eq!(entries, 2);

    // The same request again: the same journal, nothing new written.
    let (status, headers, again) = p.transfer("pay-1", &payer, &payee, 250).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(again["journal_id"], body["journal_id"]);
    assert_eq!(again["replayed"], true);
    assert_eq!(headers.get("idempotent-replayed").unwrap(), "true");
    assert_eq!(p.count(&p.db_a, "entry").await, entries);
    assert_eq!(p.balance(false, &payer).await, -250);

    // The same key with another body is refused, and writes nothing.
    let (status, _, conflict) = p.transfer("pay-1", &payer, &payee, 251).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(code(&conflict), "idempotency_conflict");
    assert_eq!(p.count(&p.db_a, "entry").await, entries);

    p.invariants_hold().await;
    p.drop().await;
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431 and 5432"]
async fn a_transfer_between_two_cells_moves_value_through_the_in_transit_accounts() {
    let p = Pair::start("cross").await;
    let payer = p.open(false, "open-1", 1_000).await;
    let payee = p.open(true, "open-2", 0).await;

    let (status, _, body) = p.transfer("pay-1", &payer, &payee, 400).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let cells: Vec<&str> = body["watermarks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["cell"].as_str().unwrap())
        .collect();
    assert_eq!(cells, ["cell-a", "cell-b"]);
    assert_eq!(p.balance(false, &payer).await, -400);
    assert_eq!(p.balance(true, &payee).await, 400);
    // Four entries in all: two at each cell, and the journal is settled.
    assert_eq!(p.count(&p.db_a, "entry").await, 2);
    assert_eq!(p.count(&p.db_b, "entry").await, 2);
    assert_eq!(p.count(&p.db_a, "settlement").await, 1);

    // A repeat writes nothing at either cell.
    let (status, headers, again) = p.transfer("pay-1", &payer, &payee, 400).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(again["replayed"], true);
    assert_eq!(headers.get("idempotent-replayed").unwrap(), "true");
    assert_eq!(p.count(&p.db_a, "entry").await, 2);
    assert_eq!(p.count(&p.db_b, "entry").await, 2);

    // And a second transfer reuses the same in-transit accounts.
    let (status, _, _) = p.transfer("pay-2", &payer, &payee, 100).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(p.balance(true, &payee).await, 500);

    p.invariants_hold().await;
    p.drop().await;
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431 and 5432"]
async fn a_crash_between_the_legs_is_finished_exactly_once() {
    let p = Pair::start("crash").await;
    let payer = p.open(false, "open-1", 1_000).await;
    let payee = p.open(true, "open-2", 0).await;

    // The payee's cell refuses the second half, as if it had failed after the first was written.
    p.db_b
        .batch_execute(&format!(
            "create function {s}.refuse() returns trigger language plpgsql as $$ begin
               raise exception 'refused for the test'; end $$;
             create trigger refuse before insert on {s}.entry
               for each row when (new.account_id = '{payee}') execute function {s}.refuse();",
            s = p.schema
        ))
        .await
        .unwrap();
    let (status, _, body) = p.transfer("pay-1", &payer, &payee, 300).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(
        p.balance(false, &payer).await,
        -300,
        "the first half is written"
    );
    assert_eq!(p.balance(true, &payee).await, 0);
    assert_eq!(p.count(&p.db_a, "settlement").await, 0);

    // The cell comes back. Recovery finishes the journal from what the two cells hold.
    p.db_b
        .batch_execute(&format!("drop trigger refuse on {}.entry", p.schema))
        .await
        .unwrap();
    let tenant: TenantId = "ten_acme".parse().unwrap();
    let done = cross_cell::recover(&p.db_a, &p.db_b, &p.schema, &tenant)
        .await
        .unwrap();
    assert_eq!((done.found, done.settled.len()), (1, 1));
    assert_eq!(p.balance(true, &payee).await, 300);
    assert_eq!(p.count(&p.db_a, "settlement").await, 1);

    // The caller's retry, with the same key, then writes nothing more.
    let before = (
        p.count(&p.db_a, "entry").await,
        p.count(&p.db_b, "entry").await,
    );
    let (status, headers, again) = p.transfer("pay-1", &payer, &payee, 300).await;
    assert_eq!(status, StatusCode::CREATED, "{again}");
    assert_eq!(again["replayed"], true);
    assert_eq!(headers.get("idempotent-replayed").unwrap(), "true");
    assert_eq!(
        (
            p.count(&p.db_a, "entry").await,
            p.count(&p.db_b, "entry").await
        ),
        before
    );
    assert_eq!(p.balance(false, &payer).await, -300);
    assert_eq!(p.balance(true, &payee).await, 300);
    p.invariants_hold().await;
    p.drop().await;
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431 and 5432"]
async fn a_retry_after_a_crash_finishes_the_transfer_without_recovery() {
    let p = Pair::start("retry").await;
    let payer = p.open(false, "open-1", 1_000).await;
    let payee = p.open(true, "open-2", 0).await;
    p.db_b
        .batch_execute(&format!(
            "create function {s}.refuse() returns trigger language plpgsql as $$ begin
               raise exception 'refused for the test'; end $$;
             create trigger refuse before insert on {s}.entry
               for each row when (new.account_id = '{payee}') execute function {s}.refuse();",
            s = p.schema
        ))
        .await
        .unwrap();
    let (status, _, _) = p.transfer("pay-1", &payer, &payee, 300).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    p.db_b
        .batch_execute(&format!("drop trigger refuse on {}.entry", p.schema))
        .await
        .unwrap();

    let (status, headers, body) = p.transfer("pay-1", &payer, &payee, 300).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(headers.get("idempotent-replayed").unwrap(), "true");
    assert_eq!(p.balance(false, &payer).await, -300, "debited once");
    assert_eq!(p.balance(true, &payee).await, 300, "credited once");
    assert_eq!(p.count(&p.db_a, "settlement").await, 1);
    p.invariants_hold().await;
    p.drop().await;
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431 and 5432"]
async fn a_decline_writes_nothing_and_says_why() {
    let p = Pair::start("decline").await;
    let payer = p.open(false, "open-1", 100).await;
    let payee = p.open(false, "open-2", 0).await;
    let far = p.open(true, "open-3", 0).await;

    // Not enough funds: on one cell and across two.
    for (key, to) in [("big-1", &payee), ("big-2", &far)] {
        let (status, _, body) = p.transfer(key, &payer, to, 101).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(code(&body), "insufficient_funds");
        assert_eq!(body["type"], "urn:ledger:problem:insufficient_funds");
    }
    assert_eq!(
        (
            p.count(&p.db_a, "entry").await,
            p.count(&p.db_b, "entry").await
        ),
        (0, 0)
    );
    assert_eq!(
        p.count(&p.db_a, "journal").await,
        0,
        "no journal was left behind"
    );

    // The key was given back: the same key can be used once the payer can afford it.
    let (status, _, _) = p.transfer("big-1", &payer, &payee, 100).await;
    assert_eq!(status, StatusCode::CREATED);

    // An account that does not exist, one that is another tenant's, and a bad amount.
    let (status, _, body) = p.transfer("ghost", &payer, "acc_doesnotexist", 1).await;
    assert_eq!((status, code(&body)), (StatusCode::NOT_FOUND, "not_found"));
    let (status, _, body) = Pair::call(
        &p.a,
        "POST",
        "/v1/transfers",
        Some(ZED),
        Some("theirs"),
        Some(json!({"payer_account_id": payer, "payee_account_id": payee,
                    "amount": {"minor": 1, "currency": "GBP"}})),
    )
    .await;
    assert_eq!((status, code(&body)), (StatusCode::NOT_FOUND, "not_found"));
    for minor in [0, -5] {
        let (status, _, body) = p.transfer("bad", &payer, &payee, minor).await;
        assert_eq!(
            (status, code(&body)),
            (StatusCode::BAD_REQUEST, "invalid_payload")
        );
    }
    let (status, _, body) = p.transfer("same", &payer, &payer, 1).await;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::BAD_REQUEST, "invalid_payload")
    );

    // A currency the payer's account is not in.
    let (status, _, body) = Pair::call(
        &p.a,
        "POST",
        "/v1/transfers",
        Some(ACME),
        Some("usd"),
        Some(json!({"payer_account_id": payer, "payee_account_id": payee,
                    "amount": {"minor": 1, "currency": "USD"}})),
    )
    .await;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::BAD_REQUEST, "invalid_payload")
    );

    // A payer homed at another cell: this cell says where to go.
    let theirs = p.open(true, "open-4", 100).await;
    let (status, _, body) = p.transfer("elsewhere", &theirs, &payee, 1).await;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::MISDIRECTED_REQUEST, "wrong_home")
    );
    assert_eq!(body["home"], "cell-b");

    // Another tenant's account at another cell is never confirmed: it answers as a missing one.
    let (status, _, body) = Pair::call(
        &p.a,
        "POST",
        "/v1/transfers",
        Some(ZED),
        Some("probe"),
        Some(
            json!({"payer_account_id": theirs, "payee_account_id": payee,
                    "amount": {"minor": 1, "currency": "GBP"}}),
        ),
    )
    .await;
    assert_eq!((status, code(&body)), (StatusCode::NOT_FOUND, "not_found"));

    // A payer whose home has been promoted away since: the same answer, and nothing written.
    let before = p.count(&p.db_a, "entry").await;
    let moved: AccountId = payer.parse().unwrap();
    p.cp.promote(
        &moved,
        &"cell-b".parse().unwrap(),
        control_plane::Epoch(1),
        0,
    )
    .await
    .unwrap();
    let (status, _, body) = p.transfer("stale", &payer, &payee, 1).await;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::MISDIRECTED_REQUEST, "wrong_home")
    );
    assert_eq!(p.count(&p.db_a, "entry").await, before);

    // Nothing above shows the database.
    p.invariants_hold().await;
    p.drop().await;
}

async fn twenty_debits(cross: bool, name: &str) {
    let p = Pair::start(name).await;
    let payer = p.open(false, "open-1", 100).await;
    let payee = p.open(cross, "open-2", 0).await;
    let mut tasks = Vec::new();
    for i in 0..20 {
        let app = p.a.clone();
        let (payer, payee) = (payer.clone(), payee.clone());
        tasks.push(tokio::spawn(async move {
            Pair::call(
                &app,
                "POST",
                "/v1/transfers",
                Some(ACME),
                Some(&format!("debit-{i}")),
                Some(json!({"payer_account_id": payer, "payee_account_id": payee,
                            "amount": {"minor": 10, "currency": "GBP"}})),
            )
            .await
        }));
    }
    let (mut ok, mut declined) = (0, 0);
    for t in tasks {
        let (status, _, body) = t.await.unwrap();
        match status {
            StatusCode::CREATED => ok += 1,
            StatusCode::UNPROCESSABLE_ENTITY => {
                assert_eq!(code(&body), "insufficient_funds");
                declined += 1;
            }
            other => panic!("unexpected {other}: {body}"),
        }
    }
    assert_eq!((ok, declined), (10, 10));
    assert_eq!(p.balance(false, &payer).await, -100);
    assert_eq!(p.balance(cross, &payee).await, 100);
    p.invariants_hold().await;
    p.drop().await;
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431 and 5432"]
async fn twenty_concurrent_debits_of_10_against_100_yield_exactly_10_on_one_cell() {
    twenty_debits(false, "twenty_one").await;
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431 and 5432"]
async fn twenty_concurrent_debits_of_10_against_100_yield_exactly_10_across_two_cells() {
    twenty_debits(true, "twenty_two").await;
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431 and 5432"]
async fn nothing_can_update_or_delete_an_entry_over_http() {
    let p = Pair::start("nowrite").await;
    let payer = p.open(false, "open-1", 100).await;
    let payee = p.open(false, "open-2", 0).await;
    let (status, _, body) = p.transfer("pay-1", &payer, &payee, 10).await;
    assert_eq!(status, StatusCode::CREATED);
    let journal = body["journal_id"].as_str().unwrap();
    for method in ["PUT", "PATCH", "DELETE"] {
        for uri in [
            "/v1/transfers".to_owned(),
            format!("/v1/transfers/{journal}"),
            format!("/v1/journals/{journal}"),
            format!("/v1/accounts/{payer}/entries"),
        ] {
            let (status, _, _) = Pair::call(&p.a, method, &uri, Some(ACME), Some("k"), None).await;
            assert!(
                status == StatusCode::NOT_FOUND || status == StatusCode::METHOD_NOT_ALLOWED,
                "{method} {uri} gave {status}"
            );
        }
    }
    assert_eq!(p.count(&p.db_a, "entry").await, 2);
    p.drop().await;
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431 and 5432"]
async fn every_route_in_the_list_is_served_and_wants_a_token() {
    let p = Pair::start("routes").await;
    // Signing in and out are the only routes that take no token before they
    // are called: signing in is how a person gets one. They are named here so
    // the exemption cannot grow, and `sign_in.rs` tests what they do.
    const OPEN_BY_DESIGN: [&str; 3] = ["/healthz", "/v1/sessions", "/v1/sessions:end"];
    for (method, path) in ROUTES {
        if OPEN_BY_DESIGN.contains(path) {
            continue;
        }
        let uri = path.replace("{accountId}", "acc_x");
        let (status, _, body) =
            Pair::call(&p.a, &method.to_uppercase(), &uri, None, Some("k"), None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {path}: {body}");
    }
    p.drop().await;
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431 and 5432"]
async fn a_payee_cell_that_is_down_stops_the_transfer_before_anything_is_written() {
    let p = Pair::start("down").await;
    let payer = p.open(false, "open-1", 1_000).await;
    let payee = p.open(true, "open-2", 0).await;
    p.stop_b().await;

    let (status, _, body) = p.transfer("pay-1", &payer, &payee, 100).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(code(&body), "home_unavailable");
    assert_eq!(p.count(&p.db_a, "entry").await, 0);
    assert_eq!(p.count(&p.db_a, "journal").await, 0);
    assert_eq!(p.balance(false, &payer).await, 0);

    // It comes back. The same key now works, because the decline gave it back.
    p.start_b().await;
    let (status, _, body) = p.transfer("pay-1", &payer, &payee, 100).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(p.balance(true, &payee).await, 100);
    p.invariants_hold().await;
    p.drop().await;
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431 and 5432"]
async fn a_payee_cell_that_goes_down_between_the_halves_is_finished_when_it_returns() {
    let p = Pair::start("between").await;
    let payer = p.open(false, "open-1", 1_000).await;
    let payee = p.open(true, "open-2", 0).await;
    // Refuse the second half at cell b's database, so the first half is written and then it stops.
    p.db_b
        .batch_execute(&format!(
            "create function {s}.refuse() returns trigger language plpgsql as $$ begin
               raise exception 'refused for the test'; end $$;
             create trigger refuse before insert on {s}.entry
               for each row when (new.account_id = '{payee}') execute function {s}.refuse();",
            s = p.schema
        ))
        .await
        .unwrap();
    let (status, _, _) = p.transfer("pay-1", &payer, &payee, 100).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(p.balance(false, &payer).await, -100);
    p.stop_b().await;
    p.db_b
        .batch_execute(&format!("drop trigger refuse on {}.entry", p.schema))
        .await
        .unwrap();
    // While it is down the retry cannot finish, and writes nothing more.
    let (status, _, _) = p.transfer("pay-1", &payer, &payee, 100).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(p.balance(false, &payer).await, -100);
    p.start_b().await;
    let (status, _, body) = p.transfer("pay-1", &payer, &payee, 100).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(
        (
            p.balance(false, &payer).await,
            p.balance(true, &payee).await
        ),
        (-100, 100)
    );
    p.invariants_hold().await;
    p.drop().await;
}
