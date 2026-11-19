//! Idempotency keys for the API: one row per key, holding a hash of the request and the id of
//! what it made. Never the request itself, because this schema holds no free text.
//!
//! A key is kept per tenant and principal. The same key with the same request is a repeat, and the
//! caller rebuilds the answer from the account or journal the row names. The same key with another
//! request is a conflict.

use ledger_core::TenantId;
use tokio_postgres::Client;

use crate::LegError;
use crate::leg::classify;
use crate::validate_schema_name;

/// What a request is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    OpenAccount,
    Transfer,
}

impl Kind {
    fn as_sql(self) -> &'static str {
        match self {
            Kind::OpenAccount => "account.open",
            Kind::Transfer => "transfer",
        }
    }
}

/// What claiming a key found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Claim {
    /// The key was new and now belongs to this request.
    New,
    /// The key was used before with this same request. The id is what that request made.
    Repeat { resource: String },
    /// The key was used before with a different request.
    Conflict,
}

/// A request that holds an idempotency key.
#[derive(Debug, Clone, Copy)]
pub struct Request<'a> {
    pub tenant: &'a TenantId,
    /// Who made it, such as `usr_ada`. Keys are kept per principal.
    pub principal: &'a str,
    pub key: &'a str,
    pub kind: Kind,
    /// The lower-case hex SHA-256 of the request's canonical form.
    pub hash: &'a str,
    /// The id of the account or journal the request makes.
    pub resource: &'a str,
}

/// Claims the request's key.
pub async fn claim(client: &Client, schema: &str, r: &Request<'_>) -> Result<Claim, LegError> {
    validate_schema_name(schema)?;
    let made = client
        .execute(
            &format!(
                "insert into {schema}.api_request (tenant_id, principal, idempotency_key, kind, request_hash, resource_id) \
                 values ($1::text, $2::text, $3::text, $4, $5::text, $6::text) on conflict do nothing"
            ),
            &[&r.tenant.as_str(), &r.principal, &r.key, &r.kind.as_sql(), &r.hash, &r.resource],
        )
        .await
        .map_err(classify)?;
    if made == 1 {
        return Ok(Claim::New);
    }
    let row = client
        .query_one(
            &format!(
                "select kind, request_hash::text, resource_id::text from {schema}.api_request \
                 where tenant_id = $1::text and principal = $2::text and idempotency_key = $3::text"
            ),
            &[&r.tenant.as_str(), &r.principal, &r.key],
        )
        .await
        .map_err(classify)?;
    let (found_kind, found_hash, found_resource): (String, String, String) =
        (row.get(0), row.get(1), row.get(2));
    if found_kind == r.kind.as_sql() && found_hash == r.hash {
        Ok(Claim::Repeat {
            resource: found_resource,
        })
    } else {
        Ok(Claim::Conflict)
    }
}
