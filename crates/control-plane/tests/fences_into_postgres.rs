#![cfg(feature = "etcd")]
//! C-02: a node follows the etcd control plane's fences and installs them in SQL. The control
//! plane is real etcd and the cell is a real Postgres node, so a promotion made through etcd
//! ends with the old home's later entries in quarantine and out of the balance.
//!
//! Run with `make up` (or `scripts/local_nodes.sh start` and an etcd on 2379), then
//! `cargo test -p control-plane --test fences_into_postgres -- --ignored`.

use std::sync::Arc;
use std::time::Duration;

use control_plane::{ControlPlane, Epoch, EtcdConfig, EtcdControlPlane, NodeId, follow_fences};
use ledger_core::{AccountId, Currency, EntryId, JournalId, Money, TenantId};
use ledger_repl::{Fence, high_water_of, install_fence};
use ledger_store::{Leg, LegWriter, Side, migrate};
use tokio_postgres::{Client, NoTls};

const TENANT: &str = "ten_acme";
const SCHEMA: &str = "c02_follow";

async fn connect(port: u16) -> Client {
    let (client, conn) = tokio_postgres::Config::new()
        .host("127.0.0.1")
        .port(port)
        .user("ledger")
        .dbname("postgres")
        .connect_timeout(Duration::from_secs(5))
        .connect(NoTls)
        .await
        .unwrap();
    tokio::spawn(conn);
    client
}

async fn journal(c: &Client, n: i64) {
    let w = LegWriter::prepare(c, SCHEMA).await.unwrap();
    for (i, (acc, side)) in [("acc_a", Side::Debit), ("acc_b", Side::Credit)]
        .into_iter()
        .enumerate()
    {
        w.post(&Leg {
            tenant: TENANT.parse::<TenantId>().unwrap(),
            journal: format!("jr_n{n}").parse::<JournalId>().unwrap(),
            account: acc.parse::<AccountId>().unwrap(),
            leg: i as i16 + 1,
            side,
            amount: Money::new(n, "GBP".parse::<Currency>().unwrap()),
            counterparty: None,
            origin: "aws-lon-01".to_owned(),
            epoch: 1,
            entry: format!("ent_n{n}x{i}").parse::<EntryId>().unwrap(),
        })
        .await
        .unwrap();
    }
}

fn unique_prefix() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("/control-plane-test/follow-{nanos}-{}", std::process::id())
}

#[tokio::test]
#[ignore = "needs Postgres on port 5431 and etcd on 2379"]
async fn a_promotion_made_in_etcd_is_installed_on_the_node_and_voids_later_entries() {
    let etcd = std::env::var("LEDGER_ETCD").unwrap_or_else(|_| "http://127.0.0.1:2379".into());
    let cp = Arc::new(
        EtcdControlPlane::connect(EtcdConfig::new(
            vec![etcd],
            unique_prefix(),
            Duration::from_secs(2),
        ))
        .await
        .unwrap(),
    );

    // A cell with two accounts and five journals written by the old home at epoch 1.
    let mut cell = connect(5431).await;
    cell.batch_execute(&format!("drop schema if exists {SCHEMA} cascade"))
        .await
        .unwrap();
    migrate(&mut cell, SCHEMA).await.unwrap();
    cell.batch_execute(&format!(
        "insert into {SCHEMA}.account (tenant_id, id, kind, currency, normal_side, allow_negative) values
           ('{TENANT}', 'acc_a', 'customer', 'GBP', 'credit', true),
           ('{TENANT}', 'acc_b', 'customer', 'GBP', 'credit', true)"
    ))
    .await
    .unwrap();

    let account: AccountId = "acc_a".parse().unwrap();
    let old_home: NodeId = "aws-lon-01".parse().unwrap();
    let new_home: NodeId = "azure-lon-01".parse().unwrap();
    cp.register(&account, &old_home).await.unwrap();
    cp.register(&"acc_b".parse().unwrap(), &old_home)
        .await
        .unwrap();

    // The node follows the control plane. Its first fences are the registrations, which it
    // installs from history; later ones arrive live.
    let installer = Arc::new(connect(5431).await);
    let follower = {
        let (cp, installer) = (cp.clone(), installer.clone());
        tokio::spawn(async move {
            follow_fences(&*cp, |f| {
                let installer = installer.clone();
                async move {
                    install_fence(
                        &installer,
                        SCHEMA,
                        &Fence {
                            tenant: TENANT.to_owned(),
                            account: f.account.to_string(),
                            epoch: f.epoch.0 as i64,
                            home: f.home.to_string(),
                            prev_epoch: f.prev_epoch.0 as i64,
                            prev_cutoff_seq: f.prev_cutoff_seq as i64,
                        },
                    )
                    .await
                    .map(|_| ())
                }
            })
            .await
        })
    };

    // Wait for both registration fences to land, then let the old home write.
    wait_for(&cell, "select count(*) from {S}.epoch_fence", 2).await;
    for n in 1..=5 {
        journal(&cell, n).await;
    }
    assert_eq!(
        high_water_of(&cell, SCHEMA, TENANT, "acc_a").await.unwrap(),
        (1, 5)
    );

    // The new home holds entries up to sequence 3, so the cutoff is 3.
    let fence = cp.promote(&account, &new_home, Epoch(1), 3).await.unwrap();
    assert_eq!(fence.epoch, Epoch(2));

    // The node installs it without being asked, and entries 4 and 5 leave the books.
    wait_for(
        &cell,
        "select count(*) from {S}.quarantine where account_id = 'acc_a'",
        2,
    )
    .await;
    let kept: Vec<i64> = cell
        .query(
            &format!("select seq from {SCHEMA}.entry where account_id = 'acc_a' order by seq"),
            &[],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| r.get(0))
        .collect();
    assert_eq!(kept, vec![1, 2, 3]);
    let balance: i64 = cell
        .query_one(
            &format!("select balance from {SCHEMA}.account where id = 'acc_a'"),
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(balance, -(1 + 2 + 3));
    assert!(!follower.is_finished(), "the follower keeps running");
    follower.abort();
    cell.batch_execute(&format!("drop schema if exists {SCHEMA} cascade"))
        .await
        .unwrap();
}

async fn wait_for(c: &Client, sql: &str, want: i64) {
    let sql = sql.replace("{S}", SCHEMA);
    for _ in 0..100 {
        let n: i64 = c.query_one(&sql, &[]).await.unwrap().get(0);
        if n == want {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("never saw {sql} = {want}");
}
