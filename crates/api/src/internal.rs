//! The endpoints one cell's service offers another, on a listener of their own. They are not the
//! public API: a tenant token is never accepted here, and a cell's credential is never accepted
//! there. Each call names the calling cell by its credential and the tenant in `x-ledger-tenant`,
//! which the credential must cover. See `api/internal.md`.

use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{FromRequestParts, Path, State};
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use control_plane::ControlPlane;
use ledger_core::{AccountId, JournalId, Money, TenantId};
use ledger_store::accounts;
use ledger_store::cross_cell::{self, Transfer};
use serde::Deserialize;
use serde_json::json;

use crate::app::{AppState, ensure_prepared, in_transit_pair};
use crate::error::ApiError;

/// Every operation of the internal listener, as a method and a path. A test holds this list and
/// `api/internal.md` to each other.
pub const INTERNAL_ROUTES: &[(&str, &str)] = &[
    ("get", "/internal/v1/accounts/{accountId}"),
    ("post", "/internal/v1/transfers/prepare"),
    ("post", "/internal/v1/transfers/receive"),
];

/// The internal routes.
pub fn internal_router<C: ControlPlane + 'static>(state: AppState<C>) -> Router {
    Router::new()
        .route("/internal/v1/accounts/{account_id}", get(account::<C>))
        .route("/internal/v1/transfers/prepare", post(prepare::<C>))
        .route("/internal/v1/transfers/receive", post(receive::<C>))
        .with_state(state)
}

/// The calling cell and the tenant it acts for, once the credential has been checked.
struct PeerCaller {
    cell: String,
    tenant: TenantId,
}

impl<C: ControlPlane + 'static> FromRequestParts<AppState<C>> for PeerCaller {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState<C>) -> Result<Self, ApiError> {
        let token = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or_else(ApiError::unauthenticated)?;
        let auth = state
            .inner
            .peer_auth
            .as_ref()
            .ok_or_else(ApiError::unauthenticated)?;
        let peer = auth
            .authenticate(token)
            .ok_or_else(ApiError::unauthenticated)?;
        let tenant: TenantId = parts
            .headers
            .get("x-ledger-tenant")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
            .ok_or_else(ApiError::unauthenticated)?;
        // A credential covers only the tenants it names. Anything else is the same refusal.
        if !peer.tenants.contains(&tenant) {
            return Err(ApiError::unauthenticated());
        }
        Ok(PeerCaller {
            cell: peer.cell,
            tenant,
        })
    }
}

fn parse<T: for<'de> Deserialize<'de>>(body: &Bytes) -> Result<T, ApiError> {
    serde_json::from_slice(body).map_err(|e| ApiError::invalid(e.to_string()))
}

/// `GET /internal/v1/accounts/{id}`: whether the tenant has this account here, and its currency.
async fn account<C: ControlPlane + 'static>(
    State(s): State<AppState<C>>,
    caller: PeerCaller,
    Path(raw): Path<String>,
) -> Result<Response, ApiError> {
    let id: AccountId = raw.parse().map_err(|_| ApiError::not_found())?;
    let record = accounts::get(&s.inner.db.get(), &s.inner.schema, &caller.tenant, &id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    Ok(Json(json!({"account_id": id.to_string(), "currency": record.currency})).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Prepare {
    journal_id: String,
    payee_account_id: String,
    amount: Money,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Receive {
    journal_id: String,
    payer_account_id: String,
    payee_account_id: String,
    amount: Money,
}

/// Checks the payee and makes this cell ready for the pair. Writes no entry.
async fn check_payee<C: ControlPlane + 'static>(
    s: &AppState<C>,
    caller: &PeerCaller,
    payee: &str,
    amount: Money,
) -> Result<(AccountId, u64), ApiError> {
    let i = &s.inner;
    if amount.minor() <= 0 {
        return Err(ApiError::invalid("the amount must be positive"));
    }
    let payee: AccountId = payee.parse().map_err(|_| ApiError::not_found())?;
    let record = accounts::get(&i.db.get(), &i.schema, &caller.tenant, &payee)
        .await?
        .ok_or_else(ApiError::not_found)?;
    let currency = amount.currency().to_string();
    if record.currency != currency {
        return Err(ApiError::invalid(
            "the payee's account is in another currency",
        ));
    }
    // This cell must be home for the payee, with its lease in hand.
    let epoch = i.guard.check(&*i.cp, &payee).await?;
    let pair = in_transit_pair(&caller.tenant, &caller.cell, i.node.as_str(), &currency)?;
    ensure_prepared(i, &pair, i.node.as_str()).await?;
    Ok((payee, epoch.0))
}

/// `POST /internal/v1/transfers/prepare`: the first question of a cross-cell transfer, asked
/// before anything is written anywhere. Answers 200 only if `receive` can then succeed.
async fn prepare<C: ControlPlane + 'static>(
    State(s): State<AppState<C>>,
    caller: PeerCaller,
    body: Bytes,
) -> Result<Response, ApiError> {
    let request: Prepare = parse(&body)?;
    let _journal: JournalId = request
        .journal_id
        .parse()
        .map_err(|_| ApiError::invalid("journal_id is not a journal id"))?;
    let _ticket = s
        .inner
        .admission
        .enter()
        .map_err(|_| ApiError::home_unavailable("this cell is draining"))?;
    check_payee(&s, &caller, &request.payee_account_id, request.amount).await?;
    Ok(Json(json!({"ready": true})).into_response())
}

/// `POST /internal/v1/transfers/receive`: the second half. Credits the payee against this cell's
/// in-transit account. A repeat writes nothing.
async fn receive<C: ControlPlane + 'static>(
    State(s): State<AppState<C>>,
    caller: PeerCaller,
    body: Bytes,
) -> Result<Response, ApiError> {
    let request: Receive = parse(&body)?;
    let journal: JournalId = request
        .journal_id
        .parse()
        .map_err(|_| ApiError::invalid("journal_id is not a journal id"))?;
    let payer: AccountId = request
        .payer_account_id
        .parse()
        .map_err(|_| ApiError::invalid("payer_account_id is not an account id"))?;
    let _ticket = s
        .inner
        .admission
        .enter()
        .map_err(|_| ApiError::home_unavailable("this cell is draining"))?;
    let (payee, epoch) =
        check_payee(&s, &caller, &request.payee_account_id, request.amount).await?;
    let i = &s.inner;
    let pair = in_transit_pair(
        &caller.tenant,
        &caller.cell,
        i.node.as_str(),
        &request.amount.currency().to_string(),
    )?;
    let step = cross_cell::receive(
        &i.db.get(),
        &i.schema,
        &Transfer {
            tenant: caller.tenant.clone(),
            journal,
            actor: format!("sys_{}", caller.cell.replace('-', "")),
            payer,
            payee,
            sending: pair.sending,
            receiving: pair.receiving,
            amount: request.amount,
            payer_cell: caller.cell.clone(),
            payee_cell: i.node.to_string(),
            payer_epoch: 0,
            sending_epoch: 1,
            receiving_epoch: 1,
            payee_epoch: epoch as i64,
        },
    )
    .await?;
    Ok(Json(json!({"watermark": step.watermark, "replayed": step.replayed})).into_response())
}
