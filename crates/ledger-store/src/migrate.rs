use tokio_postgres::Client;

/// One migration: a version, a name and its SQL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Migration {
    pub version: i32,
    pub name: &'static str,
    pub sql: &'static str,
}

/// Every migration, oldest first. Versions start at 1 and have no gaps. Never edit one that
/// has shipped: add a new one.
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "schema_v1",
        sql: include_str!("../migrations/0001_schema_v1.sql"),
    },
    Migration {
        version: 2,
        name: "post_leg",
        sql: include_str!("../migrations/0002_post_leg.sql"),
    },
    Migration {
        version: 3,
        name: "cross_cell",
        sql: include_str!("../migrations/0003_cross_cell.sql"),
    },
    Migration {
        version: 4,
        name: "holds",
        sql: include_str!("../migrations/0004_holds.sql"),
    },
    Migration {
        version: 5,
        name: "replication",
        sql: include_str!("../migrations/0005_replication.sql"),
    },
    Migration {
        version: 6,
        name: "fencing",
        sql: include_str!("../migrations/0006_fencing.sql"),
    },
];

#[derive(Debug, thiserror::Error)]
pub enum MigrationError {
    #[error(
        "a schema name is 1 to 63 lower-case letters, digits or underscores, not starting with a digit: {0:?}"
    )]
    BadSchemaName(String),
    #[error(
        "migration {version} ({name}) was applied with different SQL: expected checksum {recorded}, found {current}"
    )]
    Changed {
        version: i32,
        name: String,
        recorded: String,
        current: String,
    },
    #[error(
        "the database has migration {0}, which this build does not know: it is newer than the code"
    )]
    DatabaseIsNewer(i32),
    #[error("migration {version} ({name}) failed: {source}")]
    Failed {
        version: i32,
        name: &'static str,
        source: tokio_postgres::Error,
    },
    #[error(transparent)]
    Database(#[from] tokio_postgres::Error),
}

/// What a call to [`migrate`] did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MigrationReport {
    /// Versions applied by this call, in order.
    pub applied: Vec<i32>,
    /// Versions that were already applied and unchanged.
    pub already_applied: Vec<i32>,
}

/// Schema names are interpolated into SQL, so they are checked first.
pub fn validate_schema_name(name: &str) -> Result<(), MigrationError> {
    let ok = (1..=63).contains(&name.len())
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if ok {
        Ok(())
    } else {
        Err(MigrationError::BadSchemaName(name.to_owned()))
    }
}

/// Brings `schema` up to the latest version. Safe to call repeatedly and from several
/// processes at once: callers take turns on an advisory lock.
///
/// Each migration runs in its own transaction, so a failure leaves the schema at the last
/// good version.
pub async fn migrate(client: &mut Client, schema: &str) -> Result<MigrationReport, MigrationError> {
    validate_schema_name(schema)?;
    let mut report = MigrationReport::default();

    let tx = client.transaction().await?;
    tx.execute(
        "select pg_advisory_xact_lock(hashtext($1))",
        &[&format!("ledger-migrate:{schema}")],
    )
    .await?;
    // `create schema if not exists` can read a stale catalog inside a transaction and collide
    // with a migrator that has just committed, even under the lock. An ordinary query under
    // the lock always sees the committed state.
    let exists: bool = tx
        .query_one(
            "select exists (select 1 from pg_namespace where nspname = $1)",
            &[&schema],
        )
        .await?
        .get(0);
    if !exists {
        tx.batch_execute(&format!("create schema {schema}")).await?;
    }
    tx.batch_execute(&format!(
        "create table if not exists {schema}.schema_migrations (
           version    integer     primary key,
           slug       text        not null check (slug ~ '^[a-z0-9_]{{1,64}}$'),
           checksum   text        not null check (checksum ~ '^[0-9a-f]{{32}}$'),
           applied_at timestamptz not null default now()
         );"
    ))
    .await?;
    tx.commit().await?;

    let known = |v: i32| MIGRATIONS.iter().any(|m| m.version == v);
    let rows = client
        .query(
            &format!("select version, checksum from {schema}.schema_migrations order by version"),
            &[],
        )
        .await?;
    if let Some(v) = rows.iter().map(|r| r.get::<_, i32>(0)).find(|v| !known(*v)) {
        return Err(MigrationError::DatabaseIsNewer(v));
    }

    for m in MIGRATIONS {
        let tx = client.transaction().await?;
        tx.execute(
            "select pg_advisory_xact_lock(hashtext($1))",
            &[&format!("ledger-migrate:{schema}")],
        )
        .await?;
        let current: String = tx
            .query_one("select md5($1::text)", &[&m.sql])
            .await?
            .get(0);
        let recorded: Option<String> = tx
            .query_opt(
                &format!("select checksum from {schema}.schema_migrations where version = $1"),
                &[&m.version],
            )
            .await?
            .map(|r| r.get(0));
        match recorded {
            Some(recorded) if recorded == current => {
                report.already_applied.push(m.version);
                tx.commit().await?;
            }
            Some(recorded) => {
                return Err(MigrationError::Changed {
                    version: m.version,
                    name: m.name.to_owned(),
                    recorded,
                    current,
                });
            }
            None => {
                tx.batch_execute(&format!("set local search_path to {schema}"))
                    .await?;
                tx.batch_execute(m.sql)
                    .await
                    .map_err(|source| MigrationError::Failed {
                        version: m.version,
                        name: m.name,
                        source,
                    })?;
                tx.execute(
                    &format!("insert into {schema}.schema_migrations (version, slug, checksum) values ($1, $2, $3)"),
                    &[&m.version, &m.name, &current],
                )
                .await?;
                tx.commit().await?;
                report.applied.push(m.version);
            }
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_versions_start_at_one_and_have_no_gaps() {
        for (i, m) in MIGRATIONS.iter().enumerate() {
            assert_eq!(m.version, i as i32 + 1, "{}", m.name);
            assert!(!m.sql.trim().is_empty());
        }
    }

    #[test]
    fn schema_names_are_checked_before_they_reach_sql() {
        for ok in ["ledger", "ledger_lab", "k02_test_1", "_x"] {
            assert!(validate_schema_name(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "1ledger",
            "Ledger",
            "led-ger",
            "led ger",
            "ledger;drop table x",
            "\"ledger\"",
            &"a".repeat(64),
        ] {
            assert!(validate_schema_name(bad).is_err(), "{bad:?}");
        }
    }

    /// Offline rules about the SQL itself, so they hold even without a database.
    #[test]
    fn every_table_carries_tenant_id_and_no_column_can_hold_personal_data() {
        let sql = MIGRATIONS
            .iter()
            .map(|m| m.sql)
            .collect::<Vec<_>>()
            .join("\n");
        // Strip comments, which may mention the forbidden words.
        let code: String = sql
            .lines()
            .map(|l| l.split("--").next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n");

        let tables: Vec<&str> = code.split("create table ").skip(1).collect();
        assert!(tables.len() >= 10, "expected the full v1 table set");
        for t in &tables {
            let name = t.split_whitespace().next().unwrap();
            let body = t.split(");").next().unwrap();
            assert!(body.contains("tenant_id"), "table {name} has no tenant_id");
        }

        // Types that could carry free text or arbitrary payloads. `text` is allowed only
        // through the checked domains and check constraints, which a live test verifies.
        let banned_types = [
            "json",
            "jsonb",
            "bytea",
            "xml",
            "varchar",
            "char",
            "character",
            "varying",
            "citext",
        ];
        for line in code.lines() {
            let lower = line.to_lowercase();
            for word in lower.split(|c: char| !c.is_alphanumeric() && c != '_') {
                assert!(!banned_types.contains(&word), "type {word} in {line:?}");
            }
        }
        let banned_names = [
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
            "text",
        ];
        for t in &tables {
            let body = t.split(");").next().unwrap();
            for line in body.lines().skip(1) {
                let col = line
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .trim_matches(',');
                assert!(
                    !banned_names.contains(&col),
                    "column {col} could hold personal data: {line:?}"
                );
            }
        }
    }
}
