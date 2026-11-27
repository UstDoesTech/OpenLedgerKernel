//! K-24ca: a managed cell's application role. The statements run against a real Postgres node
//! (port 5431, as the other database tests do), because what they grant and refuse is the point.
//! `rds_iam` is a role RDS ships; the tests make a stand-in with the same name, which is all the
//! AWS tie needs, so the rest of the statements are proven as they are on every cloud.

use std::sync::atomic::{AtomicUsize, Ordering};

use ledger_pki::application_role::{Plan, Tie, apply};
use tokio_postgres::{Client, NoTls};

static NEXT: AtomicUsize = AtomicUsize::new(0);
const ADMIN: &str = "host=127.0.0.1 port=5431 user=ledger dbname=postgres";

struct Lab {
    /// The administrator, connected to the cell's own database, as the real step is.
    admin: Client,
    root: Client,
    database: String,
    role: String,
}

async fn open(conn: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(conn, NoTls).await.unwrap();
    tokio::spawn(connection);
    client
}

async fn lab() -> Lab {
    let n = NEXT.fetch_add(1, Ordering::SeqCst);
    let id = format!("{}_{n}", std::process::id());
    let root = open(ADMIN).await;
    root.batch_execute(
        "do $$ begin if not exists (select 1 from pg_roles where rolname = 'rds_iam') then \
             create role rds_iam; end if; end $$",
    )
    .await
    .unwrap();
    let database = format!("pki_role_{id}");
    root.batch_execute(&format!("create database {database}"))
        .await
        .unwrap();
    let admin = open(&format!(
        "host=127.0.0.1 port=5431 user=ledger dbname={database}"
    ))
    .await;
    Lab {
        admin,
        root,
        database,
        role: format!("app_{id}"),
    }
}

impl Lab {
    fn plan(&self) -> Plan {
        Plan::new(Tie::AwsIam, &self.role, &self.database, "ledger").unwrap()
    }

    /// A connection to the lab's database as a given role, with no password: the node trusts
    /// local connections, which is what a token-signed-in role looks like to the database.
    async fn as_role(&self, role: &str) -> Client {
        open(&format!(
            "host=127.0.0.1 port=5431 user={role} dbname={}",
            self.database
        ))
        .await
    }

    /// Everything the role and its rights are made of, as text, so two runs can be compared.
    async fn snapshot(&self) -> String {
        let db = open(&format!(
            "host=127.0.0.1 port=5431 user=ledger dbname={}",
            self.database
        ))
        .await;
        let role = &self.role;
        let rows = db
            .query(
                &format!(
                    "select concat_ws('|', r.rolname, r.rolsuper, r.rolcreaterole, r.rolcreatedb, r.rolcanlogin,
                        r.rolinherit, r.rolbypassrls, r.rolreplication, r.rolconnlimit, a.rolpassword is null,
                        coalesce(r.rolconfig::text, ''),
                        (select string_agg(m.rolname, ',' order by m.rolname) from pg_auth_members am
                           join pg_roles m on m.oid = am.roleid where am.member = r.oid),
                        (select nspowner::regrole::text from pg_namespace where nspname = 'ledger'),
                        (select coalesce(datacl::text, '') from pg_database where datname = current_database()))
                     from pg_roles r join pg_authid a on a.oid = r.oid where r.rolname = '{role}'"
                ),
                &[],
            )
            .await
            .unwrap();
        rows[0].get::<_, String>(0)
    }

    async fn finish(self) {
        drop(self.admin);
        self.root
            .batch_execute(&format!("drop database {} with (force)", self.database))
            .await
            .unwrap();
        self.root
            .batch_execute(&format!("drop role if exists {}", self.role))
            .await
            .unwrap();
    }
}

#[tokio::test]
#[ignore = "needs Postgres on port 5431"]
async fn the_role_has_no_password_no_powers_and_owns_only_the_ledger_schema() {
    let lab = lab().await;
    // The doc and the printed line say eight; this holds them to it.
    assert_eq!(lab.plan().statements().len(), 8);
    let report = apply(&lab.admin, &lab.plan()).await.unwrap();
    assert_eq!(
        report.to_string(),
        format!(
            "managed role: statements run: {}, failed: 0",
            lab.plan().statements().len()
        )
    );

    let s = lab.snapshot().await;
    let parts: Vec<&str> = s.split('|').collect();
    // name, super, createrole, createdb, login, inherit, bypassrls, replication, limit, no password
    assert_eq!(
        &parts[1..10],
        ["f", "f", "f", "t", "f", "f", "f", "-1", "t"]
    );
    assert!(parts.contains(&"rds_iam"), "tied to the identity: {s}");
    assert!(parts.contains(&lab.role.as_str()), "owns the ledger schema");
    lab.finish().await;
}

#[tokio::test]
#[ignore = "needs Postgres on port 5431"]
async fn running_it_again_changes_nothing() {
    let lab = lab().await;
    apply(&lab.admin, &lab.plan()).await.unwrap();
    let first = lab.snapshot().await;
    apply(&lab.admin, &lab.plan()).await.unwrap();
    assert_eq!(lab.snapshot().await, first);
    lab.finish().await;
}

#[tokio::test]
#[ignore = "needs Postgres on port 5431"]
async fn it_puts_a_password_and_extra_powers_right_if_they_drifted() {
    let lab = lab().await;
    apply(&lab.admin, &lab.plan()).await.unwrap();
    let good = lab.snapshot().await;
    lab.admin
        .batch_execute(&format!(
            "alter role {} createrole createdb password 'x' connection limit 3",
            lab.role
        ))
        .await
        .unwrap();
    apply(&lab.admin, &lab.plan()).await.unwrap();
    assert_eq!(lab.snapshot().await, good);
    lab.finish().await;
}

#[tokio::test]
#[ignore = "needs Postgres on port 5431"]
async fn the_role_can_use_the_ledger_schema_and_nothing_else() {
    let lab = lab().await;
    // Something of another owner in another schema, which the role must not reach.
    let other = open(&format!(
        "host=127.0.0.1 port=5431 user=ledger dbname={}",
        lab.database
    ))
    .await;
    other
        .batch_execute(
            "create schema other; create table other.t (a int); create table public.p (a int)",
        )
        .await
        .unwrap();
    apply(&lab.admin, &lab.plan()).await.unwrap();

    let mut app = lab.as_role(&lab.role).await;
    // The whole ledger schema comes from the migrations, run as the role, as `ledger-api migrate` does.
    ledger_store::migrate(&mut app, "ledger").await.unwrap();
    app.batch_execute("create table ledger.mine (a int); insert into ledger.mine values (1)")
        .await
        .unwrap();

    for (what, sql) in [
        ("make a role", "create role evil"),
        ("make a database", "create database evil"),
        ("make a schema", "create schema evil"),
        ("make a table in public", "create table public.evil (a int)"),
        (
            "change another schema's table",
            "alter table other.t add column b int",
        ),
        ("read another schema's table", "select * from other.t"),
        (
            "change a public table",
            "alter table public.p add column b int",
        ),
        (
            "make itself a superuser",
            &format!("alter role {} superuser", lab.role),
        ),
    ] {
        assert!(app.batch_execute(sql).await.is_err(), "it could {what}");
    }
    // PostgreSQL lets any role change its own password, so that is not refused here. On RDS the
    // `rds_iam` membership makes the service refuse passwords for the role, and each run of the
    // step clears one (see `it_puts_a_password_and_extra_powers_right_if_they_drifted`).
    drop(app);
    drop(other);
    lab.finish().await;
}

#[test]
fn names_that_could_carry_a_statement_or_a_wider_right_are_refused() {
    for (role, database, schema) in [
        ("ledger_admin", "postgres", "ledger"),
        ("postgres", "postgres", "ledger"),
        ("pg_monitor", "postgres", "ledger"),
        ("app", "postgres", "public"),
        ("app", "postgres", "pg_catalog"),
        ("app", "postgres", "information_schema"),
        ("App", "postgres", "ledger"),
        ("app\"; drop", "postgres", "ledger"),
        ("app", "post gres", "ledger"),
        ("app", "postgres", ""),
        ("1app", "postgres", "ledger"),
    ] {
        assert!(
            Plan::new(Tie::AwsIam, role, database, schema).is_err(),
            "{role} {database} {schema}"
        );
    }
    assert!(Plan::new(Tie::AwsIam, "ledger_app", "postgres", "ledger").is_ok());
}

#[test]
fn the_step_has_no_secret_store_so_the_administrators_credential_cannot_reach_one() {
    let src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/application_role.rs"
    ))
    .unwrap();
    let code: String = src
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    for banned in [
        "SecretStore",
        "MemoryStore",
        "CliStore",
        "store::",
        "put(",
        "write(",
        "fs::",
    ] {
        assert!(!code.contains(banned), "application_role.rs uses {banned}");
    }
}

#[test]
fn a_failed_run_prints_no_host_role_password_or_token() {
    let dir = std::env::temp_dir().join(format!("pki-role-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("admin");
    let marker = "marker-secret-1f9c";
    std::fs::write(
        &file,
        format!(
            "host=db-marker.internal port=1 user=marker_admin password={marker} dbname=postgres"
        ),
    )
    .unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_ledger-pki"))
        .args(["managed-role", "--cloud", "aws", "--admin-file"])
        .arg(&file)
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success());
    for private in [marker, "db-marker", "marker_admin", "ledger_app"] {
        assert!(!text.contains(private), "the output has {private}");
    }
    assert!(text.contains("signing in as the administrator"), "{text}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn only_aws_is_supported_so_far_and_a_cloud_is_needed() {
    for args in [
        vec!["managed-role"],
        vec!["managed-role", "--cloud", "azure", "--admin-file", "x"],
    ] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_ledger-pki"))
            .args(&args)
            .output()
            .unwrap();
        assert!(!out.status.success());
    }
}
