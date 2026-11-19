//! The control plane in etcd.
//!
//! Layout under a prefix `P`:
//!
//! | key                          | value                  | notes                                   |
//! |------------------------------|------------------------|-----------------------------------------|
//! | `P/accounts/{account}`       | `{"home":..,"epoch":..}` | changes only by promotion, by CAS       |
//! | `P/leases/{account}`         | the same record        | attached to an etcd lease; gone = lapsed |
//! | `P/forced/{account}`         | `1`                    | an expiry on purpose; blocks heartbeat  |
//! | `P/fences/{account}/{epoch}` | a [`Fence`] as JSON    | written once, never changed             |
//! | `P/draining/{cell}`          | `1`                    | present while a cell is draining        |
//!
//! Lease expiry is decided by etcd's own clock and its own TTL, so a home that cannot reach
//! etcd cannot renew, and its lease lapses without anyone else having to notice. A promotion
//! replaces the account record with a transaction that compares the record's current value,
//! so two promotions never both win the same epoch, and writes the fence, the new lease and
//! the removal of any forced expiry in the same transaction.

use std::collections::BTreeSet;
use std::time::Duration;

use etcd_client::{
    Client, Compare, CompareOp, ConnectOptions, EventType, GetOptions, GetResponse, PutOptions,
    Txn, TxnOp, TxnOpResponse, WatchOptions,
};
use ledger_core::AccountId;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::{ControlError, ControlPlane, Epoch, Fence, FenceWatch, NodeId};

/// How to reach etcd and how long a lease lasts.
#[derive(Debug, Clone)]
pub struct EtcdConfig {
    /// Client URLs of the members, such as `http://127.0.0.1:2379`. Give several, one per
    /// region, so the client can use whichever members it can reach.
    pub endpoints: Vec<String>,
    /// Every key lives under this, so several control planes (or tests) share a cluster.
    pub prefix: String,
    /// How long a lease lasts without renewal. etcd works in whole seconds and has a floor
    /// of about two.
    pub lease_ttl: Duration,
    /// How long one request may take before it counts as unavailable.
    pub request_timeout: Duration,
}

impl EtcdConfig {
    pub fn new(endpoints: Vec<String>, prefix: impl Into<String>, lease_ttl: Duration) -> Self {
        EtcdConfig {
            endpoints,
            prefix: prefix.into().trim_end_matches('/').to_owned(),
            lease_ttl,
            request_timeout: Duration::from_secs(3),
        }
    }
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
struct Record {
    home: NodeId,
    epoch: Epoch,
}

pub struct EtcdControlPlane {
    client: Client,
    prefix: String,
    ttl_secs: i64,
}

/// etcd's answer when a lease has already expired or been revoked.
fn lease_is_gone(e: &etcd_client::Error) -> bool {
    match e {
        etcd_client::Error::LeaseKeepAliveError(m) => m.contains("not found"),
        etcd_client::Error::GRpcStatus(status) => status.message().contains("lease not found"),
        _ => false,
    }
}

fn unavailable(e: impl std::fmt::Display) -> ControlError {
    ControlError::Unavailable(e.to_string())
}

fn to_json<T: Serialize>(value: &T) -> Vec<u8> {
    // Records and fences are plain structs of strings and numbers, so this cannot fail.
    serde_json::to_vec(value).unwrap_or_default()
}

fn from_json<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, ControlError> {
    serde_json::from_slice(bytes).map_err(|e| unavailable(format!("unreadable record: {e}")))
}

impl EtcdControlPlane {
    pub async fn connect(config: EtcdConfig) -> Result<Self, ControlError> {
        let options = ConnectOptions::new()
            .with_timeout(config.request_timeout)
            .with_connect_timeout(config.request_timeout)
            .with_keep_alive(Duration::from_secs(5), config.request_timeout);
        let client = Client::connect(config.endpoints, Some(options))
            .await
            .map_err(unavailable)?;
        let ttl_secs = config.lease_ttl.as_secs_f64().ceil().max(1.0) as i64;
        Ok(EtcdControlPlane {
            client,
            prefix: config.prefix,
            ttl_secs,
        })
    }

    fn account_key(&self, a: &AccountId) -> String {
        format!("{}/accounts/{}", self.prefix, a)
    }
    fn lease_key(&self, a: &AccountId) -> String {
        format!("{}/leases/{}", self.prefix, a)
    }
    fn forced_key(&self, a: &AccountId) -> String {
        format!("{}/forced/{}", self.prefix, a)
    }
    fn fence_key(&self, a: &AccountId, e: Epoch) -> String {
        format!("{}/fences/{}/{:020}", self.prefix, a, e.0)
    }
    fn draining_key(&self, cell: &NodeId) -> String {
        format!("{}/draining/{}", self.prefix, cell)
    }
    fn fence_prefix(&self) -> String {
        format!("{}/fences/", self.prefix)
    }
    fn account_prefix(&self) -> String {
        format!("{}/accounts/", self.prefix)
    }

    /// A new etcd lease for a lease key. The caller attaches it in the same transaction that
    /// makes the lease current. If that transaction fails the lease is left to run out.
    async fn grant(&self) -> Result<i64, ControlError> {
        let granted = self
            .client
            .clone()
            .lease_grant(self.ttl_secs, None)
            .await
            .map_err(unavailable)?;
        Ok(granted.id())
    }

    fn attach(&self, id: i64) -> PutOptions {
        PutOptions::new().with_lease(id)
    }

    /// Extends an etcd lease. False when etcd no longer has it, which is a lapse: the lease
    /// can run out between reading the lease key and asking to extend it, and that is the
    /// lease expiring, not etcd being unavailable.
    async fn keep_alive(&self, id: i64) -> Result<bool, ControlError> {
        let outcome = async {
            let (mut keeper, mut stream) = self.client.clone().lease_keep_alive(id).await?;
            keeper.keep_alive().await?;
            stream.message().await
        }
        .await;
        match outcome {
            Ok(Some(r)) => Ok(r.ttl() > 0),
            Ok(None) => Err(unavailable("keep-alive stream closed")),
            Err(e) if lease_is_gone(&e) => Ok(false),
            Err(e) => Err(unavailable(e)),
        }
    }

    /// Reads an account's record and its raw bytes, for a later compare-and-swap.
    async fn record(&self, a: &AccountId) -> Result<(Record, Vec<u8>), ControlError> {
        let resp = self
            .client
            .clone()
            .get(self.account_key(a), None)
            .await
            .map_err(unavailable)?;
        match resp.kvs().first() {
            Some(kv) => Ok((from_json(kv.value())?, kv.value().to_vec())),
            None => Err(ControlError::UnknownAccount(a.clone())),
        }
    }

    async fn run(&self, txn: Txn) -> Result<etcd_client::TxnResponse, ControlError> {
        self.client.clone().txn(txn).await.map_err(unavailable)
    }

    /// A consistent snapshot of the account records, the lease keys and the forced markers.
    async fn snapshot(&self) -> Result<Snapshot, ControlError> {
        let range = |p: String| TxnOp::get(p, Some(GetOptions::new().with_prefix()));
        let resp = self
            .run(Txn::new().and_then([
                range(self.account_prefix()),
                range(format!("{}/leases/", self.prefix)),
                range(format!("{}/forced/", self.prefix)),
            ]))
            .await?;
        let mut gets = resp.op_responses().into_iter().map(|r| match r {
            TxnOpResponse::Get(g) => Ok(g),
            _ => Err(unavailable("unexpected transaction response")),
        });
        let mut next = || {
            gets.next()
                .unwrap_or_else(|| Err(unavailable("short response")))
        };
        let (accounts, leases, forced) = (next()?, next()?, next()?);
        Ok(Snapshot {
            accounts: self.parse_accounts(&accounts)?,
            leases: leases
                .kvs()
                .iter()
                .map(|kv| (self.tail(kv.key(), "leases"), kv.lease()))
                .collect(),
            forced: forced
                .kvs()
                .iter()
                .map(|kv| self.tail(kv.key(), "forced"))
                .collect(),
        })
    }

    fn tail(&self, key: &[u8], kind: &str) -> String {
        let p = format!("{}/{}/", self.prefix, kind);
        String::from_utf8_lossy(key)
            .strip_prefix(&p)
            .unwrap_or_default()
            .to_owned()
    }

    fn parse_accounts(&self, g: &GetResponse) -> Result<Vec<(AccountId, Record)>, ControlError> {
        let mut out = Vec::new();
        for kv in g.kvs() {
            let id = self.tail(kv.key(), "accounts");
            let id: AccountId = id.parse().map_err(|e| unavailable(format!("{e}")))?;
            out.push((id, from_json(kv.value())?));
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }

    fn parse_fence(kv: &etcd_client::KeyValue) -> Result<Fence, ControlError> {
        from_json(kv.value())
    }

    async fn all_fences(&self) -> Result<(Vec<Fence>, i64), ControlError> {
        let resp = self
            .client
            .clone()
            .get(self.fence_prefix(), Some(GetOptions::new().with_prefix()))
            .await
            .map_err(unavailable)?;
        let revision = resp.header().map_or(0, |h| h.revision());
        let mut fences = resp
            .kvs()
            .iter()
            .map(Self::parse_fence)
            .collect::<Result<Vec<_>, _>>()?;
        fences.sort_by(|a, b| (&a.account, a.epoch).cmp(&(&b.account, b.epoch)));
        Ok((fences, revision))
    }
}

struct Snapshot {
    accounts: Vec<(AccountId, Record)>,
    /// Account id and the etcd lease its lease key is attached to.
    leases: Vec<(String, i64)>,
    forced: BTreeSet<String>,
}

impl ControlPlane for EtcdControlPlane {
    async fn register(&self, account: &AccountId, home: &NodeId) -> Result<(), ControlError> {
        let record = to_json(&Record {
            home: home.clone(),
            epoch: Epoch::FIRST,
        });
        let first = to_json(&Fence {
            account: account.clone(),
            epoch: Epoch::FIRST,
            home: home.clone(),
            prev_epoch: Epoch::NONE,
            prev_cutoff_seq: 0,
        });
        let lease = self.grant().await?;
        let key = self.account_key(account);
        let resp = self
            .run(
                Txn::new()
                    .when([Compare::create_revision(key.clone(), CompareOp::Equal, 0)])
                    .and_then([
                        TxnOp::put(key, record.clone(), None),
                        TxnOp::put(self.fence_key(account, Epoch::FIRST), first, None),
                        TxnOp::put(self.lease_key(account), record, Some(self.attach(lease))),
                        TxnOp::delete(self.forced_key(account), None),
                    ]),
            )
            .await?;
        if resp.succeeded() {
            Ok(())
        } else {
            let _ = self.client.clone().lease_revoke(lease).await;
            Err(ControlError::AlreadyRegistered(account.clone()))
        }
    }

    async fn home(&self, account: &AccountId) -> Result<(NodeId, Epoch), ControlError> {
        let (r, _) = self.record(account).await?;
        Ok((r.home, r.epoch))
    }

    async fn accounts(&self) -> Result<Vec<AccountId>, ControlError> {
        let resp = self
            .client
            .clone()
            .get(self.account_prefix(), Some(GetOptions::new().with_prefix()))
            .await
            .map_err(unavailable)?;
        Ok(self
            .parse_accounts(&resp)?
            .into_iter()
            .map(|(a, _)| a)
            .collect())
    }

    async fn homed_at(&self, node: &NodeId) -> Result<Vec<AccountId>, ControlError> {
        let resp = self
            .client
            .clone()
            .get(self.account_prefix(), Some(GetOptions::new().with_prefix()))
            .await
            .map_err(unavailable)?;
        Ok(self
            .parse_accounts(&resp)?
            .into_iter()
            .filter(|(_, r)| &r.home == node)
            .map(|(a, _)| a)
            .collect())
    }

    async fn check_lease(&self, account: &AccountId, node: &NodeId) -> Result<Epoch, ControlError> {
        // One transaction, so the record, the lease and the forced marker agree.
        let resp = self
            .run(Txn::new().and_then([
                TxnOp::get(self.account_key(account), None),
                TxnOp::get(self.lease_key(account), None),
                TxnOp::get(self.forced_key(account), None),
            ]))
            .await?;
        let mut gets = resp.op_responses().into_iter().filter_map(|r| match r {
            TxnOpResponse::Get(g) => Some(g),
            _ => None,
        });
        let (Some(acct), Some(lease), Some(forced)) = (gets.next(), gets.next(), gets.next())
        else {
            return Err(unavailable("short response"));
        };
        let Some(kv) = acct.kvs().first() else {
            return Err(ControlError::UnknownAccount(account.clone()));
        };
        let record: Record = from_json(kv.value())?;
        if &record.home != node {
            return Err(ControlError::NotHome {
                account: account.clone(),
                node: node.clone(),
                home: record.home,
                epoch: record.epoch,
            });
        }
        let expired = || ControlError::LeaseExpired {
            account: account.clone(),
            node: node.clone(),
        };
        if !forced.kvs().is_empty() {
            return Err(expired());
        }
        let Some(lease_kv) = lease.kvs().first() else {
            return Err(expired());
        };
        if !self.keep_alive(lease_kv.lease()).await? {
            return Err(expired());
        }
        Ok(record.epoch)
    }

    async fn expire(&self, account: &AccountId) -> Result<(), ControlError> {
        let resp = self
            .run(
                Txn::new()
                    .when([Compare::create_revision(
                        self.account_key(account),
                        CompareOp::Greater,
                        0,
                    )])
                    .and_then([
                        TxnOp::put(self.forced_key(account), "1", None),
                        TxnOp::delete(self.lease_key(account), None),
                    ]),
            )
            .await?;
        if resp.succeeded() {
            Ok(())
        } else {
            Err(ControlError::UnknownAccount(account.clone()))
        }
    }

    async fn renew(&self, account: &AccountId) -> Result<(), ControlError> {
        let (record, current) = self.record(account).await?;
        let lease = self.grant().await?;
        let key = self.account_key(account);
        // The compare makes sure the lease is not written for a home that has just changed.
        let resp = self
            .run(
                Txn::new()
                    .when([Compare::value(key, CompareOp::Equal, current)])
                    .and_then([
                        TxnOp::put(
                            self.lease_key(account),
                            to_json(&record),
                            Some(self.attach(lease)),
                        ),
                        TxnOp::delete(self.forced_key(account), None),
                    ]),
            )
            .await?;
        if resp.succeeded() {
            Ok(())
        } else {
            // A promotion got in first and has issued a fresh lease of its own.
            let _ = self.client.clone().lease_revoke(lease).await;
            Ok(())
        }
    }

    async fn heartbeat(&self, unreachable: &BTreeSet<NodeId>) -> Result<usize, ControlError> {
        let snap = self.snapshot().await?;
        let mut renewed = 0;
        for (account, record) in &snap.accounts {
            let id = account.to_string();
            if unreachable.contains(&record.home) || snap.forced.contains(&id) {
                continue;
            }
            let existing = snap.leases.iter().find(|(a, _)| a == &id).map(|(_, l)| *l);
            if let Some(lease) = existing
                && self.keep_alive(lease).await?
            {
                renewed += 1;
                continue;
            }
            // Lapsed on its own, so the home is reachable again: start a new lease, but only
            // if nothing has changed since the snapshot (no forced expiry, no promotion).
            let lease = self.grant().await?;
            let lease_key = self.lease_key(account);
            let resp = self
                .run(
                    Txn::new()
                        .when([
                            Compare::create_revision(lease_key.clone(), CompareOp::Equal, 0),
                            Compare::create_revision(self.forced_key(account), CompareOp::Equal, 0),
                            Compare::value(
                                self.account_key(account),
                                CompareOp::Equal,
                                to_json(record),
                            ),
                        ])
                        .and_then([TxnOp::put(
                            lease_key,
                            to_json(record),
                            Some(self.attach(lease)),
                        )]),
                )
                .await?;
            if resp.succeeded() {
                renewed += 1;
            } else {
                let _ = self.client.clone().lease_revoke(lease).await;
            }
        }
        Ok(renewed)
    }

    async fn promote(
        &self,
        account: &AccountId,
        new_home: &NodeId,
        prev_epoch: Epoch,
        prev_cutoff_seq: u64,
    ) -> Result<Fence, ControlError> {
        // Compare-and-swap on the account record, retried if another change lands first.
        for _ in 0..16 {
            let (current, raw) = self.record(account).await?;
            if prev_epoch > current.epoch {
                return Err(ControlError::CutoffInTheFuture {
                    account: account.clone(),
                    current: current.epoch,
                    claimed: prev_epoch,
                });
            }
            let next = Record {
                home: new_home.clone(),
                epoch: current.epoch.next(),
            };
            let fence = Fence {
                account: account.clone(),
                epoch: next.epoch,
                home: new_home.clone(),
                prev_epoch,
                prev_cutoff_seq,
            };
            let lease = self.grant().await?;
            let resp = self
                .run(
                    Txn::new()
                        .when([Compare::value(
                            self.account_key(account),
                            CompareOp::Equal,
                            raw,
                        )])
                        .and_then([
                            TxnOp::put(self.account_key(account), to_json(&next), None),
                            TxnOp::put(self.fence_key(account, next.epoch), to_json(&fence), None),
                            TxnOp::put(
                                self.lease_key(account),
                                to_json(&next),
                                Some(self.attach(lease)),
                            ),
                            TxnOp::delete(self.forced_key(account), None),
                        ]),
                )
                .await?;
            if resp.succeeded() {
                return Ok(fence);
            }
            let _ = self.client.clone().lease_revoke(lease).await;
        }
        Err(unavailable("too many concurrent changes to the account"))
    }

    async fn fences(&self) -> Result<Vec<Fence>, ControlError> {
        Ok(self.all_fences().await?.0)
    }

    async fn watch_fences(&self) -> Result<FenceWatch, ControlError> {
        // History first, then everything after the revision the history was read at, so
        // nothing is missed and nothing arrives twice.
        let (history, revision) = self.all_fences().await?;
        let (tx, rx) = mpsc::unbounded_channel();
        for fence in history {
            let _ = tx.send(Ok(fence));
        }
        let (watcher, mut stream) = self
            .client
            .clone()
            .watch(
                self.fence_prefix(),
                Some(
                    WatchOptions::new()
                        .with_prefix()
                        .with_start_revision(revision + 1),
                ),
            )
            .await
            .map_err(unavailable)?;
        let task = tokio::spawn(async move {
            let _watcher = watcher; // dropping it would cancel the watch
            loop {
                match stream.message().await {
                    Ok(Some(resp)) => {
                        if resp.canceled() {
                            let _ = tx.send(Err(unavailable("watch cancelled by etcd")));
                            return;
                        }
                        for event in resp.events() {
                            if event.event_type() != EventType::Put {
                                continue;
                            }
                            if let Some(kv) = event.kv()
                                && tx.send(Self::parse_fence(kv)).is_err()
                            {
                                return;
                            }
                        }
                    }
                    Ok(None) => {
                        let _ = tx.send(Err(unavailable("watch closed")));
                        return;
                    }
                    Err(e) => {
                        let _ = tx.send(Err(unavailable(e)));
                        return;
                    }
                }
            }
        });
        Ok(FenceWatch::new(rx, Some(task)))
    }

    async fn drain(&self, cell: &NodeId) -> Result<(), ControlError> {
        self.client
            .clone()
            .put(self.draining_key(cell), "1", None)
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    async fn undrain(&self, cell: &NodeId) -> Result<(), ControlError> {
        self.client
            .clone()
            .delete(self.draining_key(cell), None)
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    async fn draining(&self) -> Result<BTreeSet<NodeId>, ControlError> {
        let resp = self
            .client
            .clone()
            .get(
                format!("{}/draining/", self.prefix),
                Some(GetOptions::new().with_prefix().with_keys_only()),
            )
            .await
            .map_err(unavailable)?;
        resp.kvs()
            .iter()
            .map(|kv| {
                self.tail(kv.key(), "draining")
                    .parse()
                    .map_err(|e| unavailable(format!("unreadable cell name: {e}")))
            })
            .collect()
    }
}
