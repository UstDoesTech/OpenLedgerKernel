//! The service: its state, the routes and what each does.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{FromRequestParts, Path, State};
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use control_plane::{Admission, ControlError, ControlPlane, LeaseGuard, NodeId};
use ledger_core::{AccountId, Currency, JournalId, Money, TenantId};
use ledger_store::accounts::{self, NewAccount};
use ledger_store::cross_cell::{self, Pair};
use ledger_store::holds;
use ledger_store::requests::{self, Claim, Kind, Request};
use ledger_store::same_cell::{self, SameCell};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio_postgres::Client;

use crate::auth::{Authenticator, PeerAuthenticator, Principal};
use crate::error::ApiError;
use crate::login::{Login, Refusal};
use crate::peer::{PeerClient, PeerError};
use crate::reload::Reloadable;

/// Everything a handler needs. Cheap to clone: it is one shared pointer.
pub struct AppState<C> {
    pub(crate) inner: Arc<Inner<C>>,
}

impl<C> Clone for AppState<C> {
    fn clone(&self) -> Self {
        AppState {
            inner: self.inner.clone(),
        }
    }
}

/// The clients for the other cells, by cell name.
pub type Peers = HashMap<String, Arc<PeerClient>>;

pub(crate) struct Inner<C> {
    /// The cell this service runs next to.
    pub(crate) node: NodeId,
    pub(crate) schema: String,
    pub(crate) db: Client,
    pub(crate) cp: Arc<C>,
    pub(crate) guard: LeaseGuard,
    pub(crate) auth: Arc<dyn Authenticator>,
    pub(crate) admission: Admission,
    /// How to call the other cells' services, for transfers that end there.
    pub(crate) peers: Reloadable<Peers>,
    /// Who may call this cell's internal endpoints. None means nobody may.
    pub(crate) peer_auth: Option<Arc<dyn PeerAuthenticator>>,
    /// Sign-in with a username and password. None means this cell only takes bearer tokens.
    pub(crate) login: Option<Arc<Login>>,
    /// The pairs of cells already made ready for in-transit value, so each is done once.
    pub(crate) prepared: Mutex<HashSet<(String, String, String, String)>>,
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
                peers: Reloadable::new(HashMap::new()),
                peer_auth: None,
                login: None,
                prepared: Mutex::new(HashSet::new()),
            }),
        }
    }

    /// Adds how to call another cell's service, for transfers to accounts homed there. Call it
    /// before the state is shared.
    #[must_use]
    pub fn with_peer(self, cell: &str, client: PeerClient) -> Self {
        let mut peers = (*self.inner.peers.get()).clone();
        peers.insert(cell.to_owned(), Arc::new(client));
        self.inner.peers.set(Arc::new(peers));
        self
    }

    /// The handle that swaps the clients for the other cells, for whatever re-reads the list of
    /// cells while the service runs.
    pub fn peers_handle(&self) -> Reloadable<Peers> {
        self.inner.peers.clone()
    }

    /// Says who may call this cell's internal endpoints. Without it, none may be called.
    #[must_use]
    pub fn with_peer_auth(mut self, auth: Arc<dyn PeerAuthenticator>) -> Self {
        Arc::get_mut(&mut self.inner)
            .expect("peer credentials are set before the state is shared")
            .peer_auth = Some(auth);
        self
    }

    /// Lets a person sign in with a username and password, and accepts the tokens that gives.
    /// Call it before the state is shared.
    #[must_use]
    pub fn with_login(mut self, login: Arc<Login>) -> Self {
        Arc::get_mut(&mut self.inner)
            .expect("sign-in is set before the state is shared")
            .login = Some(login);
        self
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

/// Every operation the service answers, as a method and a path in OpenAPI's form. A test holds
/// this list and `api/openapi.yaml` to each other.
pub const ROUTES: &[(&str, &str)] = &[
    ("get", "/healthz"),
    ("post", "/v1/accounts"),
    ("get", "/v1/accounts/{accountId}"),
    ("get", "/v1/accounts/{accountId}/balances"),
    ("post", "/v1/transfers"),
    ("post", "/v1/sessions"),
    ("post", "/v1/sessions:end"),
];

/// The routes.
pub fn router<C: ControlPlane + 'static>(state: AppState<C>) -> Router {
    Router::new()
        .route("/healthz", get(healthz::<C>))
        .route("/v1/accounts", post(open_account::<C>))
        .route("/v1/accounts/{account_id}", get(get_account::<C>))
        .route("/v1/accounts/{account_id}/balances", get(get_balances::<C>))
        .route("/v1/transfers", post(post_transfer::<C>))
        .route("/v1/sessions", post(sign_in::<C>))
        .route("/v1/sessions:end", post(sign_out::<C>))
        .with_state(state)
        // Nothing the API says is for a cache to keep: a balance, an account or a token.
        .layer(axum::middleware::map_response(no_store))
        .merge(crate::ui::routes())
}

/// Adds `Cache-Control: no-store` to a response that has no cache rule of its own.
async fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .entry(header::CACHE_CONTROL)
        .or_insert(HeaderValue::from_static("no-store"));
    response
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
            .or_else(|| {
                state
                    .inner
                    .login
                    .as_ref()
                    .and_then(|login| login.authenticate(token))
            })
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewTransfer {
    payer_account_id: String,
    payee_account_id: String,
    amount: Money,
}

#[derive(Serialize)]
struct Watermark {
    cell: String,
    watermark: i64,
}

#[derive(Serialize)]
struct TransferView {
    journal_id: String,
    status: &'static str,
    payer_account_id: String,
    payee_account_id: String,
    amount: Money,
    watermarks: Vec<Watermark>,
    /// True when the same request had been made before and nothing new was written.
    replayed: bool,
}

/// `POST /v1/transfers`: moves an amount between two of the caller's accounts. The payer's
/// account must be homed at this cell; the payee's may be homed anywhere.
async fn post_transfer<C: ControlPlane + 'static>(
    State(s): State<AppState<C>>,
    Caller(caller): Caller,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let key = idempotency_key(&headers)?;
    let request: NewTransfer =
        serde_json::from_slice(&body).map_err(|e| ApiError::invalid(e.to_string()))?;
    let payer: AccountId = request
        .payer_account_id
        .parse()
        .map_err(|_| ApiError::not_found())?;
    let payee: AccountId = request
        .payee_account_id
        .parse()
        .map_err(|_| ApiError::not_found())?;
    if request.amount.minor() <= 0 {
        return Err(ApiError::invalid("the amount must be positive"));
    }
    if payer == payee {
        return Err(ApiError::invalid("the payer and the payee must differ"));
    }
    let _ticket = s
        .inner
        .admission
        .enter()
        .map_err(|_| ApiError::home_unavailable("this cell is draining"))?;

    let journal_id = derived_id(
        "jr_",
        &["transfer", caller.tenant.as_str(), &caller.name, &key],
    );
    let journal: JournalId = journal_id.parse().map_err(|_| ApiError::internal())?;
    let hash = hex(&Sha256::digest(
        format!(
            "transfer|{payer}|{payee}|{}|{}",
            request.amount.minor(),
            request.amount.currency()
        )
        .as_bytes(),
    ));
    let claim_request = Request {
        tenant: &caller.tenant,
        principal: &caller.name,
        key: &key,
        kind: Kind::Transfer,
        hash: &hash,
        resource: &journal_id,
    };
    let claim = requests::claim(&s.inner.db, &s.inner.schema, &claim_request).await?;
    if claim == Claim::Conflict {
        return Err(ApiError::idempotency_conflict());
    }

    let mut wrote = false;
    let result = run_transfer(
        &s,
        &caller,
        &journal,
        &payer,
        &payee,
        request.amount,
        &mut wrote,
    )
    .await;
    let watermarks = match result {
        Ok(w) => w,
        Err(e) => {
            // A decline before anything was written gives the key back. A failure after the first
            // half was written keeps it, so a retry finishes the transfer exactly once.
            if !wrote {
                let _ = requests::release(&s.inner.db, &s.inner.schema, &claim_request).await;
            }
            return Err(e);
        }
    };
    let repeat = matches!(claim, Claim::Repeat { .. }) || watermarks.1;
    let view = TransferView {
        journal_id,
        status: "posted",
        payer_account_id: payer.to_string(),
        payee_account_id: payee.to_string(),
        amount: request.amount,
        watermarks: watermarks.0,
        replayed: repeat,
    };
    Ok(replayed(
        (StatusCode::CREATED, Json(view)).into_response(),
        repeat,
    ))
}

/// Does the posting. Returns the watermark at each cell touched, and whether every step had
/// already been done. `wrote` says whether anything reached a ledger before a failure.
async fn run_transfer<C: ControlPlane + 'static>(
    s: &AppState<C>,
    caller: &Principal,
    journal: &JournalId,
    payer: &AccountId,
    payee: &AccountId,
    amount: Money,
    wrote: &mut bool,
) -> Result<(Vec<Watermark>, bool), ApiError> {
    let i = &s.inner;
    let currency = amount.currency().to_string();
    let Some(payer_record) = accounts::get(&i.db, &i.schema, &caller.tenant, payer).await? else {
        return Err(payer_elsewhere(s, &caller.tenant, payer).await);
    };
    if payer_record.currency != currency {
        return Err(ApiError::invalid(
            "the amount is not in the payer's currency",
        ));
    }
    let payer_epoch = i.guard.check(&*i.cp, payer).await?;

    let (payee_home, _) = match i.cp.home(payee).await {
        Ok(h) => h,
        Err(ControlError::UnknownAccount(_)) => return Err(ApiError::not_found()),
        Err(e) => return Err(e.into()),
    };

    if payee_home == i.node {
        let payee_record = accounts::get(&i.db, &i.schema, &caller.tenant, payee)
            .await?
            .ok_or_else(ApiError::not_found)?;
        if payee_record.currency != currency {
            return Err(ApiError::invalid(
                "the payee's account is in another currency",
            ));
        }
        let payee_epoch = i.guard.check(&*i.cp, payee).await?;
        let step = same_cell::post(
            &i.db,
            &i.schema,
            &SameCell {
                tenant: caller.tenant.clone(),
                journal: journal.clone(),
                actor: caller.name.clone(),
                payer: payer.clone(),
                payee: payee.clone(),
                amount,
                cell: i.node.to_string(),
                payer_epoch: payer_epoch.0 as i64,
                payee_epoch: payee_epoch.0 as i64,
            },
        )
        .await?;
        return Ok((
            vec![Watermark {
                cell: i.node.to_string(),
                watermark: step.watermark,
            }],
            step.replayed,
        ));
    }

    // The payee is homed at another cell. Ask that cell's service to check the payee first, so
    // nothing is written here unless the second half can be done.
    let peers = i.peers.get();
    let peer = peers
        .get(payee_home.as_str())
        .ok_or_else(|| ApiError::home_unavailable("this cell has no route to the payee's cell"))?;
    let pair = in_transit_pair(
        &caller.tenant,
        i.node.as_str(),
        payee_home.as_str(),
        &currency,
    )?;
    ensure_prepared(i, &pair, i.node.as_str()).await?;
    peer_call(
        peer,
        "POST",
        "/internal/v1/transfers/prepare",
        &caller.tenant,
        Some(json!({
            "journal_id": journal.to_string(),
            "payee_account_id": payee.to_string(),
            "amount": amount,
        })),
    )
    .await?;

    let t = cross_cell::Transfer {
        tenant: caller.tenant.clone(),
        journal: journal.clone(),
        actor: caller.name.clone(),
        payer: payer.clone(),
        payee: payee.clone(),
        sending: pair.sending.clone(),
        receiving: pair.receiving.clone(),
        amount,
        payer_cell: pair.from_cell.clone(),
        payee_cell: pair.to_cell.clone(),
        payer_epoch: payer_epoch.0 as i64,
        sending_epoch: 1,
        receiving_epoch: 1,
        payee_epoch: 0,
    };
    let sent = cross_cell::send(&i.db, &i.schema, &t).await?;
    *wrote = true;
    let received = peer_call(
        peer,
        "POST",
        "/internal/v1/transfers/receive",
        &caller.tenant,
        Some(json!({
            "journal_id": journal.to_string(),
            "payer_account_id": payer.to_string(),
            "payee_account_id": payee.to_string(),
            "amount": amount,
        })),
    )
    .await?;
    cross_cell::settle(&i.db, &i.schema, &caller.tenant, journal).await?;
    let their_watermark = received["watermark"]
        .as_i64()
        .ok_or_else(ApiError::internal)?;
    let their_replay = received["replayed"].as_bool().unwrap_or(false);
    Ok((
        vec![
            Watermark {
                cell: pair.from_cell,
                watermark: sent.watermark,
            },
            Watermark {
                cell: pair.to_cell,
                watermark: their_watermark,
            },
        ],
        sent.replayed && their_replay,
    ))
}

/// The in-transit accounts for an ordered pair of cells and a currency. Ids are derived, so both
/// cells name the same accounts without asking each other.
pub(crate) fn in_transit_pair(
    tenant: &TenantId,
    from: &str,
    to: &str,
    currency: &str,
) -> Result<Pair, ApiError> {
    let id = |side: &str| -> Result<AccountId, ApiError> {
        derived_id(
            "acc_",
            &["in-transit", side, tenant.as_str(), from, to, currency],
        )
        .parse()
        .map_err(|_| ApiError::internal())
    };
    Ok(Pair {
        tenant: tenant.clone(),
        from_cell: from.to_owned(),
        to_cell: to.to_owned(),
        currency: currency.to_owned(),
        sending: id("send")?,
        receiving: id("recv")?,
    })
}

/// Makes this cell ready to carry value for the pair, once per pair for the life of the process.
/// `here` is this cell. It only ever touches this cell's own database.
pub(crate) async fn ensure_prepared<C>(
    i: &Inner<C>,
    pair: &Pair,
    here: &str,
) -> Result<(), ApiError> {
    let marker = (
        pair.tenant.to_string(),
        pair.from_cell.clone(),
        pair.to_cell.clone(),
        pair.currency.clone(),
    );
    let known = i
        .prepared
        .lock()
        .map(|p| p.contains(&marker))
        .unwrap_or(false);
    if !known {
        cross_cell::prepare(&i.db, &i.schema, pair, here).await?;
        if let Ok(mut p) = i.prepared.lock() {
            p.insert(marker);
        }
    }
    Ok(())
}

/// Calls a peer and turns what comes back into this cell's own errors. A peer that does not answer
/// is `home_unavailable`, which the caller may retry with the same key.
pub(crate) async fn peer_call(
    peer: &PeerClient,
    method: &str,
    path: &str,
    tenant: &TenantId,
    body: Option<Value>,
) -> Result<Value, ApiError> {
    let (status, answer) = match peer.call(method, path, tenant, body.as_ref()).await {
        Ok(r) => r,
        Err(PeerError::Unreachable(_)) => {
            return Err(ApiError::home_unavailable(
                "the payee's cell did not answer",
            ));
        }
    };
    if status.is_success() {
        return Ok(answer);
    }
    Err(match status {
        StatusCode::NOT_FOUND => ApiError::not_found(),
        StatusCode::BAD_REQUEST => ApiError::invalid(
            answer["detail"]
                .as_str()
                .unwrap_or("the payee's cell refused the transfer")
                .to_owned(),
        ),
        StatusCode::MISDIRECTED_REQUEST => match answer["home"].as_str() {
            Some(home) => ApiError::wrong_home(home),
            None => ApiError::home_unavailable("the payee's cell is not home for the payee"),
        },
        _ => ApiError::home_unavailable("the payee's cell could not complete the transfer"),
    })
}

/// The payer is not at this cell. If it is the caller's own account, homed at a cell this one can
/// reach, say where to go; otherwise it is the same answer as an account that does not exist, so
/// another tenant's account is never confirmed.
async fn payer_elsewhere<C: ControlPlane + 'static>(
    s: &AppState<C>,
    tenant: &TenantId,
    payer: &AccountId,
) -> ApiError {
    let i = &s.inner;
    let peers = i.peers.get();
    if let Ok((home, _)) = i.cp.home(payer).await
        && home != i.node
        && let Some(peer) = peers.get(home.as_str())
        && let Ok(_) = peer_call(
            peer,
            "GET",
            &format!("/internal/v1/accounts/{payer}"),
            tenant,
            None,
        )
        .await
    {
        return ApiError::wrong_home(home.as_str());
    }
    ApiError::not_found()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SignIn {
    username: String,
    password: String,
}

/// `POST /v1/sessions`: a username and password for a token that lasts half an hour. It needs no
/// token. It says the same for an unknown user and a wrong password, and nothing it receives is
/// logged or sent back.
async fn sign_in<C: ControlPlane + 'static>(
    State(s): State<AppState<C>>,
    body: Bytes,
) -> Result<Response, ApiError> {
    let login = s
        .inner
        .login
        .as_ref()
        .ok_or_else(ApiError::sign_in_not_available)?;
    let request: SignIn = serde_json::from_slice(&body)
        .map_err(|_| ApiError::invalid("send a username and a password, and nothing else"))?;
    let issued = login
        .sign_in(&request.username, &request.password)
        .await
        .map_err(|refusal| match refusal {
            Refusal::Refused => ApiError::invalid_credentials(),
            Refusal::Locked => ApiError::too_many_attempts(),
            Refusal::Unavailable => ApiError::home_unavailable("sign-in cannot be done just now"),
        })?;
    let mut response = (
        StatusCode::CREATED,
        Json(json!({
            "token": issued.token,
            "expires_in": issued.expires_in,
            "principal": issued.principal,
        })),
    )
        .into_response();
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    Ok(response)
}

/// `POST /v1/sessions:end`: ends the token that was sent. A token that was not made by a sign-in
/// is left as it is. It answers the same either way.
async fn sign_out<C: ControlPlane + 'static>(
    State(s): State<AppState<C>>,
    Caller(_): Caller,
    headers: HeaderMap,
) -> StatusCode {
    if let (Some(login), Some(token)) = (
        s.inner.login.as_ref(),
        headers
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer ")),
    ) {
        login.sign_out(token);
    }
    StatusCode::NO_CONTENT
}
