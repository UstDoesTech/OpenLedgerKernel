//! K-24a: the service signs in to its database with a token for each connection, keeps working
//! when a token has expired, and makes a lost connection again with a new token. This starts a real
//! Postgres node. The node cannot check a cloud's tokens, so the role's password stands in for the
//! token: the simulated source gives `<prefix>1`, `<prefix>2` and so on, and the test changes the
//! role's password to the next one, which is what an expired token looks like to a connection that
//! is already open.
//!
//! Needs the Postgres server binaries (`apt install postgresql-16`, or `PG_BIN`):
//! `cargo test -p api --test db_identity -- --ignored --test-threads=1`.

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use ledger_api::dbauth::{SimulatedTokens, TokenSource};
use ledger_api::dbconn::{Db, Identity, connect, keep_connected};

/// A value made for this run only, so no password is written in the source.
fn random_secret() -> String {
    use std::io::Read;
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .expect("cannot read /dev/urandom");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn bin(name: &str) -> PathBuf {
    let dir = std::env::var("PG_BIN").ok().or_else(|| {
        let mut dirs: Vec<_> = std::fs::read_dir("/usr/lib/postgresql")
            .ok()?
            .filter_map(|e| e.ok().map(|e| e.path().join("bin")))
            .collect();
        dirs.sort();
        dirs.pop().map(|p| p.display().to_string())
    });
    PathBuf::from(dir.expect("no Postgres binaries: set PG_BIN")).join(name)
}

fn is_root() -> bool {
    String::from_utf8(Command::new("id").arg("-u").output().unwrap().stdout)
        .unwrap()
        .trim()
        == "0"
}

/// Postgres refuses to run as root, so as root the commands run as the `postgres` user.
fn pg(args: &[String]) -> std::process::Output {
    let line = args.join(" ");
    let out = if is_root() {
        Command::new("su").args(["postgres", "-c", &line]).output()
    } else {
        Command::new("bash").args(["-c", &line]).output()
    };
    out.unwrap()
}

struct Node {
    dir: PathBuf,
    port: u16,
    admin: String,
}

impl Drop for Node {
    fn drop(&mut self) {
        let _ = pg(&[
            bin("pg_ctl").display().to_string(),
            "-D".into(),
            self.dir.join("data").display().to_string(),
            "-m".into(),
            "immediate".into(),
            "stop".into(),
        ]);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Node {
    fn admin_conn(&self) -> String {
        format!(
            "host=127.0.0.1 port={} user=ledger_admin password={} dbname=postgres",
            self.port, self.admin
        )
    }

    /// The connection string a managed cell has: a host, a user and a database, and no password.
    fn app_conn(&self) -> String {
        format!(
            "host=127.0.0.1 port={} user=ledger_app dbname=postgres",
            self.port
        )
    }

    /// Makes the role's password this value, as a token that has just been given or has expired.
    async fn set_token(&self, token: &str) {
        let admin = connect(&self.admin_conn(), false, None).await.unwrap();
        admin
            .batch_execute(&format!("alter role ledger_app login password '{token}'"))
            .await
            .unwrap();
    }

    /// Ends every connection the role has, as a failover or a restart would.
    async fn cut(&self) {
        let admin = connect(&self.admin_conn(), false, None).await.unwrap();
        admin
            .batch_execute(
                "select pg_terminate_backend(pid) from pg_stat_activity where usename = 'ledger_app'",
            )
            .await
            .unwrap();
    }
}

async fn start() -> Node {
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let dir = std::env::temp_dir().join(format!("ledger-dbid-{}-{port}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let data = dir.join("data");
    let admin = random_secret();
    std::fs::write(dir.join("pw"), &admin).unwrap();
    std::fs::write(
        dir.join("hba.conf"),
        "host all all 127.0.0.1/32 scram-sha-256\n",
    )
    .unwrap();
    if is_root() {
        let _ = Command::new("chown")
            .args(["-R", "postgres"])
            .arg(&dir)
            .status();
    }
    let init = pg(&[
        bin("initdb").display().to_string(),
        "-D".into(),
        data.display().to_string(),
        "-A".into(),
        "scram-sha-256".into(),
        "-U".into(),
        "ledger_admin".into(),
        format!("--pwfile={}", dir.join("pw").display()),
    ]);
    assert!(
        init.status.success(),
        "initdb failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    std::fs::copy(dir.join("hba.conf"), data.join("pg_hba.conf")).unwrap();
    if is_root() {
        let _ = Command::new("chown")
            .arg("postgres")
            .arg(data.join("pg_hba.conf"))
            .status();
    }
    let options = format!(
        "-p {port} -k {} -c listen_addresses=127.0.0.1 -c fsync=off",
        dir.display()
    );
    let started = pg(&[
        bin("pg_ctl").display().to_string(),
        "-D".into(),
        data.display().to_string(),
        "-o".into(),
        format!("'{options}'"),
        "-l".into(),
        dir.join("log").display().to_string(),
        "-w".into(),
        "start".into(),
    ]);
    assert!(
        started.status.success(),
        "the test's Postgres did not start"
    );
    let node = Node { dir, port, admin };
    let admin = connect(&node.admin_conn(), false, None).await.unwrap();
    admin
        .batch_execute("create role ledger_app login")
        .await
        .unwrap();
    node
}

async fn user_now(db: &Db) -> String {
    db.get()
        .query_one("select current_user::text", &[])
        .await
        .unwrap()
        .get(0)
}

async fn wait_until_open(db: &Db) {
    for _ in 0..80 {
        let c = db.get();
        if !c.is_closed() && c.query_one("select 1", &[]).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the service did not sign in again");
}

#[tokio::test]
#[ignore = "needs the Postgres server binaries"]
async fn the_service_signs_in_with_a_token_and_keeps_its_open_connection_when_the_token_expires() {
    let node = start().await;
    let source = Arc::new(SimulatedTokens::new(&random_secret()));
    let identity = Identity::with_source(&node.app_conn(), false, None, source.clone()).unwrap();

    // The database accepts the first token, and only that one.
    node.set_token(&source.nth(1)).await;
    let db = Db::new(
        identity
            .connect()
            .await
            .expect("the first token must sign in"),
    );
    assert_eq!(source.issued(), 1);
    assert_eq!(user_now(&db).await, "ledger_app");

    // The first token expires. The connection that is open does not notice.
    node.set_token(&source.nth(2)).await;
    assert_eq!(user_now(&db).await, "ledger_app");
    assert_eq!(
        source.issued(),
        1,
        "no token is asked for while a connection is open"
    );
}

#[tokio::test]
#[ignore = "needs the Postgres server binaries"]
async fn a_lost_connection_is_made_again_with_a_new_token() {
    let node = start().await;
    let source = Arc::new(SimulatedTokens::new(&random_secret()));
    let identity =
        Arc::new(Identity::with_source(&node.app_conn(), false, None, source.clone()).unwrap());
    node.set_token(&source.nth(1)).await;
    let db = Db::new(identity.connect().await.unwrap());
    let stopped = Arc::new(std::sync::Mutex::new(None::<String>));
    let flag = stopped.clone();
    let _keeper = keep_connected(db.clone(), identity, Duration::from_secs(20), move |why| {
        *flag.lock().unwrap() = Some(why);
    });

    // The token expires, then the connection is cut. The next one needs the second token.
    node.set_token(&source.nth(2)).await;
    node.cut().await;
    wait_until_open(&db).await;
    assert_eq!(user_now(&db).await, "ledger_app");
    assert_eq!(source.issued(), 2);

    // And again, with the third.
    node.set_token(&source.nth(3)).await;
    node.cut().await;
    wait_until_open(&db).await;
    assert_eq!(source.issued(), 3);
    assert!(
        stopped.lock().unwrap().is_none(),
        "the service must not stop"
    );
}

#[tokio::test]
#[ignore = "needs the Postgres server binaries"]
async fn when_a_new_token_cannot_be_had_within_the_lease_the_service_says_so_and_stops() {
    let node = start().await;
    let prefix = random_secret();
    let source = Arc::new(SimulatedTokens::new(&prefix));
    let identity =
        Arc::new(Identity::with_source(&node.app_conn(), false, None, source.clone()).unwrap());
    node.set_token(&source.nth(1)).await;
    let db = Db::new(identity.connect().await.unwrap());
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _keeper = keep_connected(db.clone(), identity, Duration::from_secs(2), move |why| {
        let _ = tx.send(why);
    });

    source.set_failing(true);
    node.cut().await;
    let why = tokio::time::timeout(Duration::from_secs(15), rx)
        .await
        .expect("the service must stop within the lease")
        .unwrap();
    assert!(
        why.contains("could not sign in again within the lease"),
        "{why}"
    );
    assert!(why.contains("the identity service did not answer"), "{why}");
    // Nothing that names the database, the role or a token.
    for secret in [
        prefix.as_str(),
        "127.0.0.1",
        "ledger_app",
        &node.port.to_string(),
        &node.admin,
    ] {
        assert!(
            !why.contains(secret),
            "the reason must not contain {secret}"
        );
    }
}

#[tokio::test]
#[ignore = "needs the Postgres server binaries"]
async fn a_token_the_database_refuses_is_said_to_be_a_refused_sign_in() {
    let node = start().await;
    let source = Arc::new(SimulatedTokens::new(&random_secret()));
    let identity = Identity::with_source(&node.app_conn(), false, None, source.clone()).unwrap();
    // The role takes a different token from the one the source gives.
    node.set_token(&random_secret()).await;
    let err = identity.connect().await.unwrap_err();
    assert!(err.contains("the database refused the sign-in"), "{err}");
    assert!(!err.contains(&source.nth(1)));
    // Both the source and the identity can be told apart from a source that fails.
    source.set_failing(true);
    let err = identity.connect().await.unwrap_err();
    assert!(err.contains("cannot get a token"), "{err}");
}

#[tokio::test]
async fn a_source_is_asked_once_for_each_connection() {
    // No database: the count is what is checked.
    let source = SimulatedTokens::new("x");
    for _ in 0..3 {
        source.token().await.unwrap();
    }
    assert_eq!(source.issued(), 3);
}
