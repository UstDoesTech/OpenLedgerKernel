//! The service: its state, the routes and what each does.

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{FromRequestParts, Path, State};
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use control_plane::{Admission, ControlError, ControlPlane, LeaseGuard, NodeId};
use ledger_core::{AccountId, Currency, Money, TenantId};
use ledger_store::accounts::{self, NewAccount};
use ledger_store::holds;
use ledger_store::requests::{self, Claim, Kind, Request};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio_postgres::Client;

use crate::auth::{Authenticator, Principal};
use crate::error::ApiError;

/// Everything a handler needs. Cheap to clone: it is one shared pointer.
pub struct AppState<C> {
    inner: Arc<Inner<C>>,
}

impl<C> Clone for AppState<C> {
    fn clone(&self) -> Self {
        AppState {
            inner: self.inner.clone(),
        }
    }
}

struct Inner<C> {
    /// The cell this service runs next to.
    node: NodeId,
    schema: String,
    db: Client,
    cp: Arc<C>,
    guard: LeaseGuard,
    auth: Arc<dyn Authenticator>,
    admission: Admission,
}

impl<C: ControlPlane> AppState<C> {
    /// `db` is a connection to this cell's database, whose schema has been migrated. `cp` is shared
    /// so that whatever runs the service can keep a handle to it.
    pub fn new(
        node: NodeId,
        schema: &str,
        db: Client,
        cp: Arc<C>,
        guard: LeaseGuard,
        auth: Arc<dyn Authenticator>,
    ) -> Self {
        AppState {
            inner: Arc::new(Inner {
                node,
                schema: schema.to_owned(),
                db,
                cp,
                guard,
                auth,
                admission: Admission::new(),
            }),
        }
    }

    /// What a healthy cell does on a timer: confirm the lease of every account homed here, which
    /// renews it. Returns how many were confirmed.
    pub async fn renew(&self) -> Result<usize, ControlError> {
        self.inner.guard.renew(&*self.inner.cp).await
    }

    /// Lets a drain stop new requests and wait for those in flight.
    pub fn admission(&self) -> &Admission {
        &self.inner.admission
    }
}

/// The routes.
pub fn router<C: ControlPlane + 'static>(state: AppState<C>) -> Router {
    Router::new()
        .route("/healthz", get(healthz::<C>))
        .route("/v1/accounts", post(open_account::<C>))
        .route("/v1/accounts/{account_id}", get(get_account::<C>))
        .route("/v1/accounts/{account_id}/balances", get(get_balances::<C>))
        .with_state(state)
}

/// The caller, from the bearer token. A request without one never reaches a handler.
struct Caller(Principal);

impl<C: ControlPlane + 'static> FromRequestParts<AppState<C>> for Caller {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState<C>) -> Result<Self, ApiError> {
        let token = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or_else(ApiError::unauthenticated)?;
        state
            .inner
            .auth
            .authenticate(token)
            .map(Caller)
            .ok_or_else(ApiError::unauthenticated)
    }
}

fn idempotency_key(headers: &HeaderMap) -> Result<String, ApiError> {
    let key = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| ApiError::invalid("an Idempotency-Key header is needed"))?;
    let ok = !key.is_empty()
        && key.len() <= 64
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if ok {
        Ok(key.to_owned())
    } else {
        Err(ApiError::invalid(
            "an Idempotency-Key is 1 to 64 letters, digits, hyphens or underscores",
        ))
    }
}

/// An id that depends only on what asked for it, so a retry after a crash finds the same one.
fn derived_id(prefix: &str, parts: &[&str]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p.as_bytes());
        h.update([0]);
    }
    let digest = h.finalize();
    let mut id = String::from(prefix);
    for b in &digest[..16] {
        id.push_str(&format!("{b:02x}"));
    }
    id
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn money(minor: i64, currency: &str) -> Result<Money, ApiError> {
    let c: Currency = currency.parse().map_err(|_| ApiError::internal())?;
    Ok(Money::new(minor, c))
}

#[derive(Serialize)]
struct Home {
    cell: String,
    epoch: u64,
}

#[derive(Serialize)]
struct AccountView {
    account_id: String,
    product: &'static str,
    currency: String,
    status: &'static str,
    home: Home,
}

fn replayed(mut response: Response, repeat: bool) -> Response {
    if repeat {
        response
            .headers_mut()
            .insert("idempotent-replayed", HeaderValue::from_static("true"));
    }
    response
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenAccount {
    currency: Currency,
    /// How far below zero the account may go. Defaults to none.
    #[serde(default)]
    overdraft_limit: Option<Money>,
}

/// `POST /v1/accounts`: opens an account for the caller's tenant, homed at this cell.
async fn open_account<C: ControlPlane + 'static>(
    State(s): State<AppState<C>>,
    Caller(caller): Caller,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let key = idempotency_key(&headers)?;
    let request: OpenAccount =
        serde_json::from_slice(&body).map_err(|e| ApiError::invalid(e.to_string()))?;
    let overdraft = match request.overdraft_limit {
        None => 0,
        Some(m) if m.currency() == request.currency && m.minor() >= 0 => m.minor(),
        Some(_) => {
            return Err(ApiError::invalid(
                "overdraft_limit must be zero or more, in the account's currency",
            ));
        }
    };
    let _ticket = s
        .inner
        .admission
        .enter()
        .map_err(|_| ApiError::home_unavailable("this cell is draining"))?;

    let account_id = derived_id(
        "acc_",
        &["account.open", caller.tenant.as_str(), &caller.name, &key],
    );
    let account: AccountId = account_id.parse().map_err(|_| ApiError::internal())?;
    let hash = hex(&Sha256::digest(
        format!("account.open|{}|{overdraft}", request.currency).as_bytes(),
    ));
    let claim = requests::claim(
        &s.inner.db,
        &s.inner.schema,
        &Request {
            tenant: &caller.tenant,
            principal: &caller.name,
            key: &key,
            kind: Kind::OpenAccount,
            hash: &hash,
            resource: &account_id,
        },
    )
    .await?;
    if claim == Claim::Conflict {
        return Err(ApiError::idempotency_conflict());
    }

    // The control plane says which cell is home and at which epoch. A new account is homed here.
    let epoch = match s.inner.cp.home(&account).await {
        Ok((home, epoch)) if home == s.inner.node => epoch,
        Ok((home, _)) => return Err(ApiError::wrong_home(home.as_str())),
        Err(ControlError::UnknownAccount(_)) => {
            match s.inner.cp.register(&account, &s.inner.node).await {
                Ok(()) | Err(ControlError::AlreadyRegistered(_)) => {}
                Err(e) => return Err(e.into()),
            }
            s.inner.cp.home(&account).await?.1
        }
        Err(e) => return Err(e.into()),
    };
    accounts::open(
        &s.inner.db,
        &s.inner.schema,
        &NewAccount {
            tenant: caller.tenant.clone(),
            account: account.clone(),
            currency: request.currency,
            overdraft_limit: overdraft,
            home: s.inner.node.to_string(),
            epoch: epoch.0 as i64,
        },
    )
    .await?;

    let view = AccountView {
        account_id,
        product: "unassigned",
        currency: request.currency.to_string(),
        status: "open",
        home: Home {
            cell: s.inner.node.to_string(),
            epoch: epoch.0,
        },
    };
    Ok(replayed(
        (StatusCode::CREATED, Json(view)).into_response(),
        matches!(claim, Claim::Repeat { .. }),
    ))
}

/// Finds the caller's account at this cell and confirms this cell may serve it. Another tenant's
/// account, and one that does not exist, are the same answer.
async fn serve<C: ControlPlane + 'static>(
    s: &AppState<C>,
    tenant: &TenantId,
    raw: &str,
) -> Result<(AccountId, accounts::AccountRecord, u64), ApiError> {
    let account: AccountId = raw.parse().map_err(|_| ApiError::not_found())?;
    let record = accounts::get(&s.inner.db, &s.inner.schema, tenant, &account)
        .await?
        .ok_or_else(ApiError::not_found)?;
    let epoch = s.inner.guard.check(&*s.inner.cp, &account).await?;
    Ok((account, record, epoch.0))
}

/// `GET /v1/accounts/{id}`.
async fn get_account<C: ControlPlane + 'static>(
    State(s): State<AppState<C>>,
    Caller(caller): Caller,
    Path(raw): Path<String>,
) -> Result<Json<AccountView>, ApiError> {
    let (account, record, epoch) = serve(&s, &caller.tenant, &raw).await?;
    Ok(Json(AccountView {
        account_id: account.to_string(),
        product: "unassigned",
        currency: record.currency,
        status: "open",
        home: Home {
            cell: s.inner.node.to_string(),
            epoch,
        },
    }))
}

#[derive(Serialize)]
struct Balances {
    account_id: String,
    posted: Money,
    held: Money,
    available: Money,
    arranged_overdraft: Money,
}

/// `GET /v1/accounts/{id}/balances`: posted, held and available, all at this cell now.
async fn get_balances<C: ControlPlane + 'static>(
    State(s): State<AppState<C>>,
    Caller(caller): Caller,
    Path(raw): Path<String>,
) -> Result<Json<Balances>, ApiError> {
    let (account, record, _) = serve(&s, &caller.tenant, &raw).await?;
    let available =
        holds::available(&s.inner.db, &s.inner.schema, &caller.tenant, &account).await?;
    // Available is posted, less holds, plus the overdraft, so what is held is what is left over.
    let held = record.balance + record.overdraft_limit - available;
    Ok(Json(Balances {
        account_id: account.to_string(),
        posted: money(record.balance, &record.currency)?,
        held: money(held, &record.currency)?,
        available: money(available, &record.currency)?,
        arranged_overdraft: money(record.overdraft_limit, &record.currency)?,
    }))
}

#[derive(Serialize)]
struct Health {
    cell: String,
    status: &'static str,
    accounts_homed: usize,
    leases_held: usize,
    draining: bool,
}

/// `GET /healthz`: whether this cell holds the leases for the accounts homed here. Counts only.
/// Not an authenticated call, so it says nothing about any tenant.
async fn healthz<C: ControlPlane + 'static>(
    State(s): State<AppState<C>>,
) -> (StatusCode, Json<Health>) {
    let draining = s.inner.admission.is_draining();
    let health = |status, homed, held| Health {
        cell: s.inner.node.to_string(),
        status,
        accounts_homed: homed,
        leases_held: held,
        draining,
    };
    let homed = match s.inner.cp.homed_at(&s.inner.node).await {
        Ok(a) => a,
        Err(_) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(health("control_plane_unavailable", 0, 0)),
            );
        }
    };
    let mut held = 0;
    for account in &homed {
        if s.inner.guard.check(&*s.inner.cp, account).await.is_ok() {
            held += 1;
        }
    }
    if held == homed.len() && !draining {
        (StatusCode::OK, Json(health("ok", homed.len(), held)))
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(health(
                if draining {
                    "draining"
                } else {
                    "leases_missing"
                },
                homed.len(),
                held,
            )),
        )
    }
}
