//! UI-03: the basic page in a real browser, against a running API and a real Postgres node. Start the
//! nodes with `make up` (or `scripts/local_nodes.sh start`), run `npm ci` in `scripts/ui_a11y`, then
//! `cargo test -p api --test ui_browser -- --ignored`. It needs Node and Chrome (set `CHROME_PATH`
//! if Chrome is not on the path).
//!
//! The browser signs in, opens two accounts, moves money, presses the button again and reads the
//! balances. The rule it holds: a second press is a repeat, never a second transfer. The test then
//! counts the journals in the database itself, so it does not rely on what the page says.

use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use argon2::{Argon2, PasswordHasher};
use control_plane::{GuardConfig, InMemoryControlPlane, LeaseGuard, NodeId, SystemClock};
use ledger_api::auth::StaticTokens;
use ledger_api::client_addr::{Peer, TrustedProxies};
use ledger_api::login::{Login, MonotonicClock, Users};
use ledger_api::reload::Reloadable;
use ledger_api::{AppState, router};
use ledger_store::migrate;
use serde_json::json;
use tokio_postgres::{Client, NoTls};

const PORT: u16 = 5431;
const NAME: &str = "user@bankofmumanddad.test";

fn password() -> String {
    format!("browser-{}-password", std::process::id())
}

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

#[tokio::test]
#[ignore = "needs the local Postgres nodes, Node with `npm ci` done in scripts/ui_a11y, and Chrome"]
async fn the_page_makes_a_transfer_and_its_repeat_writes_nothing_new() {
    let schema = "api_ui_browser";
    let mut db = connect().await;
    db.batch_execute(&format!("drop schema if exists {schema} cascade"))
        .await
        .unwrap();
    migrate(&mut db, schema).await.unwrap();

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
    let state = AppState::new(
        node,
        schema,
        connect().await,
        cp,
        guard,
        Arc::new(StaticTokens::new()),
    )
    .with_login(login)
    .with_trusted_proxies(TrustedProxies::none());
    let app = router(state);

    // Serve it for real, on a port the operating system picks.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app.into_make_service_with_connect_info::<Peer>())
            .await
            .unwrap();
    });

    let script = format!(
        "{}/../../scripts/ui_a11y/e2e.mjs",
        env!("CARGO_MANIFEST_DIR")
    );
    assert!(
        std::path::Path::new(&format!(
            "{}/../../scripts/ui_a11y/node_modules/puppeteer-core",
            env!("CARGO_MANIFEST_DIR")
        ))
        .exists(),
        "run `npm ci` in scripts/ui_a11y first"
    );
    let pass = password();
    let output = tokio::task::spawn_blocking(move || {
        Command::new("node")
            .arg(script)
            .env("E2E_URL", url)
            .env("E2E_USERNAME", NAME)
            .env("E2E_PASSWORD", pass)
            .output()
            .expect("node is needed")
    })
    .await
    .unwrap();
    let said = String::from_utf8_lossy(&output.stdout).into_owned();
    // The script prints counts and the names of what failed, never a secret.
    assert!(output.status.success(), "the browser run failed:\n{said}");
    assert!(said.contains("problems: 0"), "{said}");

    // Two transfers were made by the page (one pressed twice), and one was declined. The database
    // holds two transfer journals, not three.
    let journals: i64 = db
        .query_one(
            &format!("select count(*) from {schema}.journal where type like 'transfer%'"),
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(journals, 2, "the second press must not have made a journal");

    server.abort();
    db.batch_execute(&format!("drop schema if exists {schema} cascade"))
        .await
        .unwrap();
}
