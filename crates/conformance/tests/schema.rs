//! Schema v1 against real Postgres: the migrations apply cleanly and again without change,
//! and every rule the schema promises is enforced by the database itself.
//!
//! Run with `make up` first, then `cargo test -p conformance -- --ignored`.

use std::time::Duration;

use ledger_store::{MIGRATIONS, MigrationError, migrate};
use tokio_postgres::{Client, NoTls};

const NODES: [u16; 3] = [5431, 5432, 5433];
const T1: &str = "ten_acme";
const T2: &str = "ten_zed";

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

/// A migrated schema of its own, so tests do not disturb each other.
async fn fresh(name: &str) -> Client {
    let mut c = connect(NODES[0]).await;
    c.batch_execute(&format!("drop schema if exists {name} cascade"))
        .await
        .unwrap();
    migrate(&mut c, name).await.unwrap();
    c.batch_execute(&format!("set search_path to {name}"))
        .await
        .unwrap();
    c
}

async fn drop_schema(c: &Client, name: &str) {
    c.batch_execute(&format!("drop schema if exists {name} cascade"))
        .await
        .unwrap();
}

/// The SQLSTATE of a failure, so tests assert why, not just that.
async fn fails(c: &Client, sql: &str) -> String {
    match c.batch_execute(sql).await {
        Ok(()) => panic!("expected an error, but this succeeded: {sql}"),
        Err(e) => e
            .code()
            .map(|c| c.code().to_owned())
            .unwrap_or_else(|| format!("{e}")),
    }
}

async fn ok(c: &Client, sql: &str) {
    c.batch_execute(sql)
        .await
        .unwrap_or_else(|e| panic!("{e}: {sql}"));
}

const UNIQUE: &str = "23505";
const CHECK: &str = "23514";
const FOREIGN_KEY: &str = "23503";
const GUARD: &str = "23000";

fn account(tenant: &str, id: &str) -> String {
    format!(
        "insert into account (tenant_id, id, kind, currency, normal_side) values ('{tenant}', '{id}', 'customer', 'GBP', 'credit')"
    )
}

#[allow(clippy::too_many_arguments)]
fn entry(
    tenant: &str,
    id: &str,
    journal: &str,
    acc: &str,
    epoch: i64,
    seq: i64,
    leg: i16,
    kind: &str,
    amount: i64,
) -> String {
    format!(
        "insert into entry (tenant_id, id, journal_id, account_id, epoch, seq, leg, kind, amount, origin) \
         values ('{tenant}', '{id}', '{journal}', '{acc}', {epoch}, {seq}, {leg}, '{kind}', {amount}, 'aws-lon-01')"
    )
}

/// Columns, constraints, triggers and functions, as text, to compare schemas.
async fn snapshot(c: &Client, schema: &str) -> Vec<String> {
    let mut out = Vec::new();
    for r in c
        .query(
            &format!(
                "select table_name::text || '.' || column_name::text || ' ' || data_type::text || ' ' ||
                        coalesce(domain_name::text, '-') || ' ' || is_nullable::text || ' ' || coalesce(column_default::text, '-')
                 from information_schema.columns where table_schema = '{schema}' order by table_name, ordinal_position"
            ),
            &[],
        )
        .await
        .unwrap()
    {
        out.push(r.get::<_, String>(0));
    }
    for r in c
        .query(
            &format!(
                "select conrelid::regclass::text || ' ' || conname::text || ' ' || pg_get_constraintdef(oid)
                 from pg_constraint where connamespace = '{schema}'::regnamespace
                 union all select tgrelid::regclass::text || ' trigger ' || tgname::text from pg_trigger
                   where not tgisinternal and tgrelid in (select oid from pg_class where relnamespace = '{schema}'::regnamespace)
                 union all select 'function ' || proname::text from pg_proc where pronamespace = '{schema}'::regnamespace
                 union all select 'index ' || indexname::text || ' ' || indexdef from pg_indexes where schemaname = '{schema}'
                 order by 1"
            ),
            &[],
        )
        .await
        .unwrap()
    {
        out.push(r.get::<_, String>(0).replace(&format!("{schema}."), ""));
    }
    out
}

#[tokio::test]
#[ignore = "needs `make up`"]
async fn applies_to_an_empty_database_and_reruns_without_change() {
    let s = "k02_idempotent";
    let mut c = connect(NODES[0]).await;
    drop_schema(&c, s).await;

    let first = migrate(&mut c, s).await.unwrap();
    let all: Vec<i32> = MIGRATIONS.iter().map(|m| m.version).collect();
    assert_eq!(first.applied, all);
    let before = snapshot(&c, s).await;
    assert!(
        before.len() > 100,
        "schema looks empty: {} lines",
        before.len()
    );

    let again = migrate(&mut c, s).await.unwrap();
    assert!(again.applied.is_empty(), "{again:?}");
    assert_eq!(again.already_applied, all);
    assert_eq!(snapshot(&c, s).await, before);

    let n: i64 = c
        .query_one(&format!("select count(*) from {s}.schema_migrations"), &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, all.len() as i64);
    drop_schema(&c, s).await;
}

#[tokio::test]
#[ignore = "needs `make up`"]
async fn concurrent_migrators_take_turns() {
    let s = "k02_concurrent";
    let mut a = connect(NODES[0]).await;
    drop_schema(&a, s).await;
    let mut b = connect(NODES[0]).await;
    let (ra, rb) = tokio::join!(migrate(&mut a, s), migrate(&mut b, s));
    let (ra, rb) = (ra.unwrap(), rb.unwrap());
    assert_eq!(
        ra.applied.len() + rb.applied.len(),
        MIGRATIONS.len(),
        "applied twice: {ra:?} {rb:?}"
    );
    drop_schema(&a, s).await;
}

#[tokio::test]
#[ignore = "needs `make up`"]
async fn an_edited_or_unknown_migration_is_refused() {
    let s = "k02_tamper";
    let mut c = connect(NODES[0]).await;
    drop_schema(&c, s).await;
    migrate(&mut c, s).await.unwrap();

    ok(
        &c,
        &format!("update {s}.schema_migrations set checksum = repeat('0', 32) where version = 1"),
    )
    .await;
    assert!(matches!(
        migrate(&mut c, s).await,
        Err(MigrationError::Changed { version: 1, .. })
    ));

    ok(&c, &format!("delete from {s}.schema_migrations")).await;
    ok(&c, &format!("insert into {s}.schema_migrations (version, slug, checksum) values (99, 'from_the_future', repeat('a', 32))")).await;
    assert!(matches!(
        migrate(&mut c, s).await,
        Err(MigrationError::DatabaseIsNewer(99))
    ));
    drop_schema(&c, s).await;
}

#[tokio::test]
#[ignore = "needs `make up`"]
async fn every_node_ends_up_with_the_same_schema() {
    let s = "k02_nodes";
    let mut snaps = Vec::new();
    for port in NODES {
        let mut c = connect(port).await;
        drop_schema(&c, s).await;
        migrate(&mut c, s).await.unwrap();
        snaps.push(snapshot(&c, s).await);
        drop_schema(&c, s).await;
    }
    assert_eq!(snaps[0], snaps[1]);
    assert_eq!(snaps[0], snaps[2]);
}

#[tokio::test]
#[ignore = "needs `make up`"]
async fn every_table_has_tenant_id_and_no_column_can_hold_free_text() {
    let s = "k02_columns";
    let c = fresh(s).await;

    let rows = c
        .query(
            &format!(
                "select c.table_name::text, c.column_name::text, c.data_type::text, c.is_nullable::text,
                        c.domain_name::text,
                        exists (select 1 from pg_constraint k
                                  join pg_attribute a on a.attrelid = k.conrelid and a.attnum = any (k.conkey)
                                 where k.contype = 'c' and k.conrelid = ('{s}.' || c.table_name)::regclass
                                   and a.attname = c.column_name)
                 from information_schema.columns c where c.table_schema = '{s}'"
            ),
            &[],
        )
        .await
        .unwrap();

    let mut tables = std::collections::BTreeMap::<String, bool>::new();
    for r in &rows {
        let (table, column, ty, nullable, domain, has_check): (
            String,
            String,
            String,
            String,
            Option<String>,
            bool,
        ) = (r.get(0), r.get(1), r.get(2), r.get(3), r.get(4), r.get(5));
        let t = tables.entry(table.clone()).or_insert(false);
        if column == "tenant_id" {
            assert_eq!(nullable, "NO", "{table}.tenant_id must be not null");
            assert_eq!(
                domain.as_deref(),
                Some("tenant_ref"),
                "{table}.tenant_id must be a tenant_ref"
            );
            *t = true;
        }
        let allowed = [
            "text",
            "bigint",
            "integer",
            "smallint",
            "boolean",
            "date",
            "timestamp with time zone",
        ];
        assert!(
            allowed.contains(&ty.as_str()),
            "{table}.{column} has type {ty}"
        );
        if ty == "text" && table != "schema_migrations" {
            assert!(
                domain.is_some() || has_check,
                "{table}.{column} is unconstrained text"
            );
        }
        let banned = [
            "name",
            "first_name",
            "last_name",
            "address",
            "email",
            "phone",
            "card",
            "pan",
            "card_number",
            "reference",
            "narrative",
            "description",
            "memo",
            "note",
            "notes",
            "comment",
            "message",
        ];
        assert!(
            !banned.contains(&column.as_str()),
            "{table}.{column} could hold personal data"
        );
    }
    for (table, has_tenant) in &tables {
        // The migrations table records which SQL ran. It holds no tenant data.
        assert!(
            *has_tenant || table == "schema_migrations",
            "table {table} has no tenant_id"
        );
    }
    for expected in [
        "account",
        "epoch_fence",
        "journal",
        "entry",
        "quarantine",
        "settlement",
        "hold",
        "in_transit_account",
        "outbox",
        "outbox_cursor",
        "cell_watermark",
    ] {
        assert!(tables.contains_key(expected), "missing table {expected}");
    }
    drop_schema(&c, s).await;
}

#[tokio::test]
#[ignore = "needs `make up`"]
async fn entries_keep_every_constraint_from_the_reference_schema() {
    let s = "k02_entries";
    let c = fresh(s).await;
    ok(&c, &account(T1, "acc_a")).await;
    ok(&c, &account(T1, "acc_b")).await;

    // Defaults of the account projection.
    let r = c
        .query_one(
            "select balance, next_seq, allow_negative from account where id = 'acc_a'",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(
        (r.get::<_, i64>(0), r.get::<_, i64>(1), r.get::<_, bool>(2)),
        (0, 1, false)
    );

    ok(
        &c,
        &entry(T1, "ent_1", "jr_1", "acc_a", 1, 1, 1, "credit", 100),
    )
    .await;

    // unique (account, epoch, seq)
    assert_eq!(
        fails(
            &c,
            &entry(T1, "ent_2", "jr_2", "acc_a", 1, 1, 1, "credit", 5)
        )
        .await,
        UNIQUE
    );
    // unique (journal, account, leg)
    assert_eq!(
        fails(
            &c,
            &entry(T1, "ent_3", "jr_1", "acc_a", 1, 2, 1, "credit", 5)
        )
        .await,
        UNIQUE
    );
    // another leg of the same journal on the same account is a different entry
    ok(
        &c,
        &entry(T1, "ent_4", "jr_1", "acc_a", 1, 2, 2, "debit", -5),
    )
    .await;
    // the same id twice
    assert_eq!(
        fails(
            &c,
            &entry(T1, "ent_1", "jr_9", "acc_b", 1, 1, 1, "credit", 5)
        )
        .await,
        UNIQUE
    );

    // sign must match kind, and zero is not an entry
    assert_eq!(
        fails(
            &c,
            &entry(T1, "ent_5", "jr_5", "acc_b", 1, 1, 1, "debit", 5)
        )
        .await,
        CHECK
    );
    assert_eq!(
        fails(
            &c,
            &entry(T1, "ent_5", "jr_5", "acc_b", 1, 1, 1, "credit", -5)
        )
        .await,
        CHECK
    );
    assert_eq!(
        fails(
            &c,
            &entry(T1, "ent_5", "jr_5", "acc_b", 1, 1, 1, "credit", 0)
        )
        .await,
        CHECK
    );
    assert_eq!(
        fails(
            &c,
            &entry(T1, "ent_5", "jr_5", "acc_b", 1, 1, 1, "refund", 5)
        )
        .await,
        CHECK
    );
    // epoch and seq start at 1
    assert_eq!(
        fails(
            &c,
            &entry(T1, "ent_5", "jr_5", "acc_b", 0, 1, 1, "credit", 5)
        )
        .await,
        CHECK
    );
    assert_eq!(
        fails(
            &c,
            &entry(T1, "ent_5", "jr_5", "acc_b", 1, 0, 1, "credit", 5)
        )
        .await,
        CHECK
    );
    // the account must exist, in the same tenant
    assert_eq!(
        fails(
            &c,
            &entry(T1, "ent_5", "jr_5", "acc_nobody", 1, 1, 1, "credit", 5)
        )
        .await,
        FOREIGN_KEY
    );

    // tenants never mix: the same ids and positions are free in another tenant, but an
    // entry cannot point at another tenant's account
    ok(&c, &account(T2, "acc_a")).await;
    ok(
        &c,
        &entry(T2, "ent_1", "jr_1", "acc_a", 1, 1, 1, "credit", 100),
    )
    .await;
    assert_eq!(
        fails(
            &c,
            &entry(T2, "ent_7", "jr_7", "acc_b", 1, 1, 1, "credit", 5)
        )
        .await,
        FOREIGN_KEY
    );

    // the account projection's own checks
    assert_eq!(
        fails(&c, "update account set next_seq = 0 where id = 'acc_a'").await,
        CHECK
    );
    drop_schema(&c, s).await;
}

#[tokio::test]
#[ignore = "needs `make up`"]
async fn identifiers_and_codes_are_opaque_and_nothing_else_fits() {
    let s = "k02_opaque";
    let c = fresh(s).await;
    ok(&c, &account(T1, "acc_a")).await;

    // malformed identifiers
    for bad in ["acc_ABC", "acc_", "ACC_1", "acc_a-b", "acc_a b", "a"] {
        let sql = format!(
            "insert into account (tenant_id, id, kind, currency, normal_side) values ('{T1}', '{bad}', 'customer', 'GBP', 'credit')"
        );
        assert_eq!(fails(&c, &sql).await, CHECK, "{bad}");
    }
    // text where a code belongs
    let free_text = [
        ("counterparty", "'Mr J Smith, 1 High Street'"),
        ("origin", "'London Office'"),
        ("journal_id", "'invoice 4471 for J Smith'"),
    ];
    for (col, value) in free_text {
        let mut sql = entry(T1, "ent_1", "jr_1", "acc_a", 1, 1, 1, "credit", 5);
        sql = match col {
            "counterparty" => sql
                .replace("origin)", "counterparty, origin)")
                .replace("5, 'aws-lon-01'", &format!("5, {value}, 'aws-lon-01'")),
            "origin" => sql.replace("'aws-lon-01'", value),
            _ => sql.replace("'jr_1'", value),
        };
        assert_eq!(fails(&c, &sql).await, CHECK, "{col}");
    }
    // a currency is a code, not a word
    let sql = format!(
        "insert into account (tenant_id, id, kind, currency, normal_side) values ('{T1}', 'acc_z', 'customer', 'pounds', 'credit')"
    );
    assert_eq!(fails(&c, &sql).await, CHECK);
    // idempotency keys cannot carry a sentence
    let journal = |key: &str, ty: &str| {
        format!(
            "insert into journal (tenant_id, id, idempotency_key, type, effective_date, actor) values ('{T1}', 'jr_j', '{key}', '{ty}', '2026-11-13', 'agt_refunds')"
        )
    };
    assert_eq!(
        fails(&c, &journal("pay J Smith 20 quid", "transfer.internal")).await,
        CHECK
    );
    assert_eq!(fails(&c, &journal("key1", "Pay Mr Smith")).await, CHECK);
    ok(&c, &journal("key-1_A", "transfer.internal")).await;
    // retry key is unique within a tenant
    let again = journal("key-1_A", "transfer.internal").replace("jr_j", "jr_k");
    assert_eq!(fails(&c, &again).await, UNIQUE);
    // quarantine reasons are a fixed list
    let q = |reason: &str| {
        format!(
            "insert into quarantine (tenant_id, id, journal_id, account_id, epoch, seq, kind, amount, origin, created_at, reason) values ('{T1}', 'ent_q', 'jr_1', 'acc_a', 1, 1, 'credit', 5, 'aws-lon-01', now(), '{reason}')"
        )
    };
    assert_eq!(fails(&c, &q("the customer rang us")).await, CHECK);
    // an entry's kind is not free text in quarantine either
    assert_eq!(
        fails(
            &c,
            &q("void_after_fence").replace("'credit'", "'refund for Mr Smith'")
        )
        .await,
        CHECK
    );
    ok(&c, &q("void_after_fence")).await;
    let r = c
        .query_one("select resolved from quarantine", &[])
        .await
        .unwrap();
    assert!(!r.get::<_, bool>(0), "quarantine starts unresolved");
    drop_schema(&c, s).await;
}

#[tokio::test]
#[ignore = "needs `make up`"]
async fn entries_are_append_only_and_leave_only_when_a_fence_voids_them() {
    let s = "k02_append_only";
    let c = fresh(s).await;
    ok(&c, &account(T1, "acc_a")).await;
    ok(
        &c,
        &entry(T1, "ent_1", "jr_1", "acc_a", 1, 1, 1, "credit", 10),
    )
    .await;
    ok(
        &c,
        &entry(T1, "ent_2", "jr_2", "acc_a", 1, 2, 1, "credit", 20),
    )
    .await;

    // never updated
    assert_eq!(
        fails(&c, "update entry set amount = 99 where id = 'ent_1'").await,
        GUARD
    );
    // not deleted while nothing has voided them
    assert_eq!(
        fails(&c, "delete from entry where id = 'ent_1'").await,
        GUARD
    );

    // The new home started from seq 1: epoch 1 entries past it are void, seq 1 is not.
    ok(&c, &format!("insert into epoch_fence (tenant_id, account_id, epoch, home, prev_epoch, prev_cutoff_seq) values ('{T1}', 'acc_a', 2, 'azure-uk-01', 1, 1)")).await;
    assert_eq!(
        fails(&c, "delete from entry where id = 'ent_1'").await,
        GUARD
    );
    ok(&c, "delete from entry where id = 'ent_2'").await;
    assert_eq!(
        fails(&c, "update entry set amount = 99 where id = 'ent_1'").await,
        GUARD
    );

    // An epoch skipped entirely is void too. The entry is written while its epoch is still the
    // latest, then the fence that skips its epoch arrives and sweeps it into quarantine (R-02).
    // Before that change this test inserted the void entry directly and deleted it by hand.
    ok(
        &c,
        &entry(T1, "ent_3", "jr_3", "acc_a", 3, 1, 1, "credit", 5),
    )
    .await;
    ok(&c, &format!("insert into epoch_fence (tenant_id, account_id, epoch, home, prev_epoch, prev_cutoff_seq) values ('{T1}', 'acc_a', 4, 'gcp-lon-01', 2, 0)")).await;
    let left: i64 = c
        .query_one("select count(*) from entry where id = 'ent_3'", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(left, 0, "the fence swept the void entry out of the log");
    let reason: String = c
        .query_one("select reason from quarantine where id = 'ent_3'", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(reason, "void_after_fence");
    // And a void entry can no longer be inserted at all.
    assert_eq!(
        fails(
            &c,
            &entry(T1, "ent_4", "jr_4", "acc_a", 3, 2, 1, "credit", 5)
        )
        .await,
        "LE011"
    );

    // A fence is sane: its previous epoch is older than itself, and it has a real account.
    assert_eq!(fails(&c, &format!("insert into epoch_fence (tenant_id, account_id, epoch, home, prev_epoch) values ('{T1}', 'acc_a', 5, 'x1', 5)")).await, CHECK);
    assert_eq!(fails(&c, &format!("insert into epoch_fence (tenant_id, account_id, epoch, home) values ('{T1}', 'acc_nobody', 2, 'x1')")).await, FOREIGN_KEY);
    drop_schema(&c, s).await;
}

#[tokio::test]
#[ignore = "needs `make up`"]
async fn holds_in_transit_accounts_and_the_outbox_enforce_their_rules() {
    let s = "k02_others";
    let c = fresh(s).await;
    ok(&c, &account(T1, "acc_a")).await;
    ok(&c, &account(T1, "acc_transit")).await;

    // holds
    let hold = |id: &str, amount: i64, state: &str, journal: &str, command: &str| {
        format!(
            "insert into hold (tenant_id, id, account_id, amount, state, journal_id, command_id) values ('{T1}', '{id}', 'acc_a', {amount}, '{state}', {journal}, {command})"
        )
    };
    ok(&c, &hold("hld_1", 50, "active", "null", "'cmd_1'")).await;
    assert_eq!(
        fails(&c, &hold("hld_2", 0, "active", "null", "null")).await,
        CHECK
    );
    assert_eq!(
        fails(&c, &hold("hld_3", 5, "pending", "null", "null")).await,
        CHECK
    );
    assert_eq!(
        fails(&c, &hold("hld_4", 5, "converted", "null", "null")).await,
        CHECK,
        "converted needs a journal"
    );
    assert_eq!(
        fails(&c, &hold("hld_5", 5, "active", "'jr_1'", "null")).await,
        CHECK,
        "only a converted hold has a journal"
    );
    ok(&c, &hold("hld_6", 5, "converted", "'jr_1'", "null")).await;
    assert_eq!(
        fails(&c, &hold("hld_7", 5, "active", "null", "'cmd_1'")).await,
        UNIQUE,
        "one hold per command"
    );

    // in-transit accounts: one per ordered pair of cells, and a cell is not in transit to itself
    let transit = |from: &str, to: &str, acc: &str| {
        format!(
            "insert into in_transit_account (tenant_id, from_cell, to_cell, account_id) values ('{T1}', '{from}', '{to}', '{acc}')"
        )
    };
    ok(&c, &transit("aws-lon-01", "azure-uk-01", "acc_transit")).await;
    assert_eq!(
        fails(&c, &transit("aws-lon-01", "aws-lon-01", "acc_a")).await,
        CHECK
    );
    assert_eq!(
        fails(&c, &transit("aws-lon-01", "azure-uk-01", "acc_a")).await,
        UNIQUE
    );
    assert_eq!(
        fails(&c, &transit("azure-uk-01", "aws-lon-01", "acc_transit")).await,
        UNIQUE,
        "one account per pair"
    );

    // outbox: positions count up and an entry is published once
    let out = |entry: &str| {
        format!(
            "insert into outbox (tenant_id, entry_id, account_id, epoch, seq) values ('{T1}', '{entry}', 'acc_a', 1, 1)"
        )
    };
    ok(&c, &out("ent_1")).await;
    ok(&c, &out("ent_2")).await;
    assert_eq!(fails(&c, &out("ent_1")).await, UNIQUE);
    let positions: Vec<i64> = c
        .query("select position from outbox order by position", &[])
        .await
        .unwrap()
        .iter()
        .map(|r| r.get(0))
        .collect();
    assert!(positions.len() == 2 && positions[0] < positions[1]);
    assert_eq!(fails(&c, &format!("insert into outbox (tenant_id, position, entry_id, account_id, epoch, seq) values ('{T1}', 1, 'ent_9', 'acc_a', 1, 1)")).await, "428C9");

    // cursors and watermarks
    ok(
        &c,
        &format!(
            "insert into outbox_cursor (tenant_id, source_cell) values ('{T1}', 'aws-lon-01')"
        ),
    )
    .await;
    assert_eq!(fails(&c, &format!("insert into outbox_cursor (tenant_id, source_cell, last_position) values ('{T1}', 'azure-uk-01', -1)")).await, CHECK);
    assert_eq!(fails(&c, &format!("insert into cell_watermark (tenant_id, cell, position) values ('{T1}', 'aws-lon-01', -1)")).await, CHECK);
    ok(&c, &format!("insert into cell_watermark (tenant_id, cell, position) values ('{T1}', 'aws-lon-01', 0)")).await;
    ok(
        &c,
        &format!("insert into settlement (tenant_id, journal_id) values ('{T1}', 'jr_1')"),
    )
    .await;
    assert_eq!(
        fails(
            &c,
            &format!("insert into settlement (tenant_id, journal_id) values ('{T1}', 'jr_1')")
        )
        .await,
        UNIQUE
    );
    drop_schema(&c, s).await;
}
