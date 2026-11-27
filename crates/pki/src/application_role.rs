//! The database role a managed cell's API signs in as (K-24ca, the application half of D-24).
//!
//! A person runs this once for each managed cell, and approves it. It uses the managed service's
//! administrator for those few statements and for nothing else: this module has no secret store
//! and writes no secret, so the administrator's credential cannot reach anything the API reads.
//! The role it makes has no password. It is tied to the cloud's identity, owns the ledger's own
//! schema, and cannot make roles, make databases or touch any other schema. Run again, it changes
//! nothing, and puts right a password or a power that has crept in. (PostgreSQL lets a role change
//! its own password, so the database cannot forbid that; on RDS the `rds_iam` membership makes the
//! service refuse password sign-in for the role.) It prints counts and the fixed name of a step that failed, never a host, a role or a
//! token.

use tokio_postgres::Client;

use crate::error::PkiError;

/// How the role is tied to an identity. The same statements make the rest of the role on every
/// cloud, so only this part differs. The other clouds come with K-24cb.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tie {
    /// RDS: membership of `rds_iam` lets IAM tokens, not passwords, sign in as the role.
    AwsIam,
}

#[derive(Clone, Debug)]
pub struct Plan {
    pub tie: Tie,
    pub role: String,
    pub database: String,
    pub schema: String,
}

/// What a run did, as counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Report {
    pub statements_run: usize,
}

impl std::fmt::Display for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "managed role: statements run: {}, failed: 0",
            self.statements_run
        )
    }
}

/// A name that is safe to put in a statement as it is: lower case, digits and underscores.
fn name_ok(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('a'..='z' | '_'))
        && name.len() <= 63
        && chars.all(|c| matches!(c, 'a'..='z' | '0'..='9' | '_'))
}

impl Plan {
    pub fn new(tie: Tie, role: &str, database: &str, schema: &str) -> Result<Plan, PkiError> {
        if ![role, database, schema].iter().all(|n| name_ok(n)) {
            return Err(PkiError::Manifest(
                "a role, database or schema name is not lower case letters, digits and underscores",
            ));
        }
        if matches!(role, "ledger_admin" | "postgres") || role.starts_with("pg_") {
            return Err(PkiError::Manifest(
                "the application role cannot be the administrator or a system role",
            ));
        }
        if schema == "public" || schema.starts_with("pg_") || schema == "information_schema" {
            return Err(PkiError::Manifest(
                "the ledger's schema cannot be a shared or system schema",
            ));
        }
        Ok(Plan {
            tie,
            role: role.to_owned(),
            database: database.to_owned(),
            schema: schema.to_owned(),
        })
    }

    /// The statements, each with the fixed name of the step it is. They are the same on every run.
    pub fn statements(&self) -> Vec<(&'static str, String)> {
        let (r, d, s) = (&self.role, &self.database, &self.schema);
        let tie = match self.tie {
            Tie::AwsIam => format!(r#"grant rds_iam to "{r}""#),
        };
        vec![
            (
                "making the role",
                format!(
                    "do $$ begin if not exists (select 1 from pg_roles where rolname = '{r}') then \
                     create role \"{r}\" login; end if; end $$"
                ),
            ),
            (
                "limiting the role",
                format!(
                    "alter role \"{r}\" login nosuperuser nocreatedb nocreaterole noinherit \
                     nobypassrls noreplication connection limit -1 password null"
                ),
            ),
            ("tying the role to the identity", tie),
            (
                "letting the role connect",
                format!("grant connect on database \"{d}\" to \"{r}\""),
            ),
            (
                "taking back database rights",
                format!("revoke create, temporary on database \"{d}\" from \"{r}\""),
            ),
            (
                "making the ledger's schema",
                format!("create schema if not exists \"{s}\" authorization \"{r}\""),
            ),
            (
                "giving the role the schema",
                format!("alter schema \"{s}\" owner to \"{r}\""),
            ),
            (
                "setting the role's search path",
                format!("alter role \"{r}\" set search_path = \"{s}\""),
            ),
        ]
    }
}

/// Runs the plan as the administrator. The error names the step, never the database's own words.
pub async fn apply(admin: &Client, plan: &Plan) -> Result<Report, PkiError> {
    let steps = plan.statements();
    for (doing, sql) in &steps {
        admin
            .batch_execute(sql)
            .await
            .map_err(|_| PkiError::Command {
                code: 1,
                doing,
                tool: "the database",
                cause: "the statement was refused",
            })?;
    }
    Ok(Report {
        statements_run: steps.len(),
    })
}
