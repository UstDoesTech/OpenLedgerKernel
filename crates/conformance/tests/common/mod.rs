//! The three-cell lab shared by the conformance tests that run on the local nodes.
#![allow(dead_code)]

use std::time::Duration;

use ledger_check::{Options, check_cell};
use ledger_core::{AccountId, Currency, EntryId, JournalId, Money, TenantId};
use ledger_repl::pull_all;
use ledger_store::cross_cell::{self, Transfer};
use ledger_store::{Leg, LegError, LegWriter, Side, migrate};
use tokio_postgres::{Client, NoTls};

pub const TENANT: &str = "ten_acme";
pub const PORTS: [u16; 3] = [5431, 5432, 5433];
pub const NAMES: [&str; 3] = ["aws-lon-01", "azure-lon-01", "gcp-lon-01"];

pub async fn try_connect(port: u16) -> Result<Client, tokio_postgres::Error> {
    let (client, conn) = tokio_postgres::Config::new()
        .host("127.0.0.1")
        .port(port)
        .user("ledger")
        .dbname("postgres")
        .connect_timeout(Duration::from_secs(5))
        .connect(NoTls)
        .await?;
    tokio::spawn(conn);
    Ok(client)
}

pub async fn connect(port: u16) -> Client {
    try_connect(port)
        .await
        .unwrap_or_else(|e| panic!("port {port}: {e}"))
}

pub fn gbp(minor: i64) -> Money {
    Money::new(minor, "GBP".parse::<Currency>().unwrap())
}

pub fn transit(from: usize, to: usize) -> (String, String) {
    (format!("acc_tr{from}x{to}s"), format!("acc_tr{from}x{to}r"))
}

/// Three cells with the same schema and accounts. Each account has a home cell, and every
/// ordered pair of cells has its in-transit accounts.
pub struct Lab {
    pub schema: String,
    pub homes: Vec<(String, usize)>,
    /// The ports `cells()` connects to: direct by default, or through Toxiproxy.
    pub ports: [u16; 3],
}

impl Lab {
    /// `accounts` are `(id, home cell index, may overdraw)`.
    pub async fn new(schema: &str, accounts: &[(&str, usize, bool)]) -> Lab {
        for port in PORTS {
            let mut c = connect(port).await;
            c.batch_execute(&format!("drop schema if exists {schema} cascade"))
                .await
                .unwrap();
            migrate(&mut c, schema).await.unwrap();
            for (id, home, overdraw) in accounts {
                c.batch_execute(&format!(
                    "insert into {schema}.account (tenant_id, id, kind, currency, normal_side, allow_negative)
                     values ('{TENANT}', '{id}', 'customer', 'GBP', 'credit', {overdraw});
                     insert into {schema}.epoch_fence (tenant_id, account_id, epoch, home)
                     values ('{TENANT}', '{id}', 1, '{}')",
                    NAMES[*home]
                ))
                .await
                .unwrap();
            }
            for (from, to) in pairs() {
                let (s, r) = transit(from, to);
                c.execute(
                    &format!("select {schema}.register_in_transit($1,$2,$3,'GBP',$4,$5)"),
                    &[&TENANT, &NAMES[from], &NAMES[to], &s, &r],
                )
                .await
                .unwrap();
                for a in [&s, &r] {
                    c.batch_execute(&format!(
                        "insert into {schema}.epoch_fence (tenant_id, account_id, epoch, home)
                         values ('{TENANT}', '{a}', 1, '{}') on conflict do nothing",
                        NAMES[from]
                    ))
                    .await
                    .unwrap();
                }
            }
        }
        Lab {
            schema: schema.to_owned(),
            ports: PORTS,
            homes: accounts
                .iter()
                .map(|(a, h, _)| ((*a).to_owned(), *h))
                .collect(),
        }
    }

    /// The same lab as seen through other ports, for example the Toxiproxy ones.
    pub fn via(&self, ports: [u16; 3]) -> Lab {
        Lab {
            schema: self.schema.clone(),
            homes: self.homes.clone(),
            ports,
        }
    }

    pub fn home(&self, account: &str) -> usize {
        self.homes.iter().find(|(a, _)| a == account).unwrap().1
    }

    pub async fn try_cells(&self) -> Result<Vec<Client>, tokio_postgres::Error> {
        let mut v = Vec::new();
        for p in self.ports {
            v.push(try_connect(p).await?);
        }
        Ok(v)
    }

    pub async fn cells(&self) -> Vec<Client> {
        let mut v = Vec::new();
        for p in self.ports {
            v.push(connect(p).await);
        }
        v
    }

    /// A journal between two accounts, at the home of the payer and, across cells, at the home
    /// of the payee too. `epochs` gives the writer's epoch for an account, 1 unless it moved.
    pub async fn transfer(
        &self,
        journal: &str,
        payer: &str,
        payee: &str,
        minor: i64,
        epochs: &(dyn Fn(&str) -> i64 + Sync),
    ) -> Result<bool, LegError> {
        let (p, q) = (self.home(payer), self.home(payee));
        // Connect only to the cells this journal touches, so a cut link to a third cell does
        // not get in the way.
        let payer_cell = try_connect(self.ports[p]).await?;
        if p == q {
            let w = LegWriter::prepare(&payer_cell, &self.schema).await?;
            let mut first = true;
            for (i, (acc, side)) in [(payer, Side::Debit), (payee, Side::Credit)]
                .into_iter()
                .enumerate()
            {
                let r = w
                    .post(&self.leg(
                        journal,
                        acc,
                        i as i16 + 1,
                        side,
                        minor,
                        (NAMES[p], epochs(acc)),
                    ))
                    .await?;
                first &= !r.replayed;
            }
            return Ok(first);
        }
        let payee_cell = try_connect(self.ports[q]).await?;
        cross_cell::transfer(
            &payer_cell,
            &payee_cell,
            &self.schema,
            &self.cross(journal, payer, payee, minor, epochs),
        )
        .await
    }

    pub fn cross(
        &self,
        journal: &str,
        payer: &str,
        payee: &str,
        minor: i64,
        epochs: &(dyn Fn(&str) -> i64 + Sync),
    ) -> Transfer {
        let (p, q) = (self.home(payer), self.home(payee));
        let (sending, _) = transit(p, q);
        let (_, receiving) = transit(p, q);
        Transfer {
            tenant: TENANT.parse::<TenantId>().unwrap(),
            journal: journal.parse::<JournalId>().unwrap(),
            actor: "usr_ada".to_owned(),
            payer: payer.parse().unwrap(),
            payee: payee.parse().unwrap(),
            sending: sending.parse().unwrap(),
            receiving: receiving.parse().unwrap(),
            amount: gbp(minor),
            payer_cell: NAMES[p].to_owned(),
            payee_cell: NAMES[q].to_owned(),
            payer_epoch: epochs(payer),
            sending_epoch: 1,
            receiving_epoch: 1,
            payee_epoch: epochs(payee),
        }
    }

    pub fn leg(
        &self,
        journal: &str,
        account: &str,
        leg: i16,
        side: Side,
        minor: i64,
        writer: (&str, i64),
    ) -> Leg {
        let (origin, epoch) = writer;
        Leg {
            tenant: TENANT.parse::<TenantId>().unwrap(),
            journal: journal.parse::<JournalId>().unwrap(),
            account: account.parse::<AccountId>().unwrap(),
            leg,
            side,
            amount: gbp(minor),
            counterparty: None,
            origin: origin.to_owned(),
            epoch,
            entry: format!("ent_{}x{leg}", journal.trim_start_matches("jr_"))
                .parse::<EntryId>()
                .unwrap(),
        }
    }

    pub async fn fund(&self, account: &str, minor: i64) {
        self.transfer(
            &format!("jr_fund{}", account.trim_start_matches("acc_")),
            "acc_treasury",
            account,
            minor,
            &|_| 1,
        )
        .await
        .unwrap();
    }

    /// Every cell ships to every other until they hold the same log.
    pub async fn settle(&self) {
        let cells = self.cells().await;
        for _ in 0..2 {
            for (i, src) in cells.iter().enumerate() {
                for (j, dst) in cells.iter().enumerate() {
                    if i != j {
                        pull_all(src, dst, &self.schema, TENANT, NAMES[i], 100)
                            .await
                            .unwrap_or_else(|e| {
                                panic!("shipping {} to {}: {e:?}", NAMES[i], NAMES[j])
                            });
                    }
                }
            }
        }
    }

    pub async fn balance(&self, cell: &Client, account: &str) -> i64 {
        cell.query_one(
            &format!(
                "select balance from {}.account where tenant_id = $1 and id = $2",
                self.schema
            ),
            &[&TENANT, &account],
        )
        .await
        .unwrap()
        .get(0)
    }

    pub async fn drop_all(&self) {
        for c in self.cells().await {
            c.batch_execute(&format!("drop schema if exists {} cascade", self.schema))
                .await
                .unwrap();
        }
    }

    /// Nothing is out of balance, no cell disagrees about a balance, and the books are clean
    /// apart from entries that were quarantined and are waiting to be re-posted.
    pub async fn assert_healthy(&self, tolerate_quarantine: bool) {
        let cells = self.cells().await;
        let options = Options {
            quarantine_max_age_secs: if tolerate_quarantine { 3600 } else { 0 },
            ..Options::default()
        };
        for (i, c) in cells.iter().enumerate() {
            let v = check_cell(c, &self.schema, NAMES[i], &options)
                .await
                .unwrap();
            let v: Vec<_> = v
                .into_iter()
                .filter(|x| !(tolerate_quarantine && x.invariant == "journal_unbalanced"))
                .collect();
            assert!(v.is_empty(), "{}: {v:?}", NAMES[i]);
        }
    }
}

pub fn pairs() -> Vec<(usize, usize)> {
    (0..3)
        .flat_map(|f| (0..3).filter(move |t| *t != f).map(move |t| (f, t)))
        .collect()
}

pub fn one(_: &str) -> i64 {
    1
}

pub fn accounts() -> Vec<(&'static str, usize, bool)> {
    vec![
        ("acc_treasury", 0, true),
        ("acc_p", 0, false),
        ("acc_q", 1, false),
    ]
}

pub async fn log_of(c: &Client, schema: &str, account: &str) -> Vec<(i64, i64, String)> {
    c.query(
        &format!(
            "select epoch, seq, id from {schema}.entry where account_id = $1 order by epoch, seq"
        ),
        &[&account],
    )
    .await
    .unwrap()
    .iter()
    .map(|r| (r.get(0), r.get(1), r.get(2)))
    .collect()
}

/// The same accounts with X now homed on cell `to`, so transfers are written where X is homed.
pub fn lab_with_home(lab: &Lab, account: &str, to: usize) -> Lab {
    Lab {
        schema: lab.schema.clone(),
        ports: lab.ports,
        homes: lab
            .homes
            .iter()
            .map(|(a, h)| (a.clone(), if a == account { to } else { *h }))
            .collect(),
    }
}

/// Reconciliation as `design.md` describes it: re-post each quarantined entry of `account`
/// under the current `epoch` at its new home, keyed by its original journal id but with a new
/// entry id, because a quarantined entry is never merged back. Marks them resolved.
pub async fn repost_quarantined(
    lab: &Lab,
    cells: &[Client],
    account: &str,
    new_home: usize,
    epoch: i64,
) -> usize {
    let schema = &lab.schema;
    let mut reposts: Vec<(String, i64, i64)> = Vec::new();
    for c in cells {
        for r in c
            .query(
                &format!("select journal_id, leg::int8, -amount from {schema}.quarantine where account_id = $1 and not resolved"),
                &[&account],
            )
            .await
            .unwrap()
        {
            reposts.push((r.get(0), r.get(1), r.get(2)));
        }
    }
    reposts.sort();
    reposts.dedup();
    let w = LegWriter::prepare(&cells[new_home], schema).await.unwrap();
    for (j, leg, amount) in &reposts {
        let mut re = lab.leg(
            j,
            account,
            *leg as i16,
            Side::Debit,
            *amount,
            (NAMES[new_home], epoch),
        );
        re.entry = format!("ent_rp{}x{leg}", j.trim_start_matches("jr_"))
            .parse()
            .unwrap();
        w.post(&re).await.unwrap();
    }
    for c in cells {
        c.batch_execute(&format!("update {schema}.quarantine set resolved = true"))
            .await
            .unwrap();
    }
    reposts.len()
}

// Toxiproxy: faults on the links to each node and to etcd.
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

pub const PROXIED: [u16; 3] = [15431, 15432, 15433];
pub const PROXY_NAMES: [&str; 3] = ["pg1", "pg2", "pg3"];
pub const TOXIPROXY: &str = "127.0.0.1:8474";
pub const ETCD_PROXIED_ADDR: &str = "127.0.0.1:12379";

/// One HTTP request over a plain socket, so the tests need no HTTP client.
pub async fn http(method: &str, path: &str, body: &str) -> (u16, String) {
    let mut s = TcpStream::connect(TOXIPROXY).await.expect("toxiproxy");
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {TOXIPROXY}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).await.expect("write");
    let mut raw = String::new();
    s.read_to_string(&mut raw).await.expect("read");
    let status = raw
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .expect("status line");
    (
        status,
        raw.split_once("\r\n\r\n").map_or("", |x| x.1).to_owned(),
    )
}

pub async fn heal() {
    let (status, _) = http("POST", "/reset", "").await;
    assert_eq!(status, 204);
    wait_for_links().await;
}

/// After a reset the listeners come back a moment later.
pub async fn wait_for_links() {
    for port in PROXIED {
        let mut ok = false;
        for _ in 0..40 {
            if try_connect(port).await.is_ok() {
                ok = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        assert!(ok, "proxy on {port} did not come back");
    }
    for _ in 0..40 {
        if TcpStream::connect(ETCD_PROXIED_ADDR).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    panic!("the etcd proxy did not come back");
}

pub async fn cut(proxy: &str) {
    let (status, body) = http("POST", &format!("/proxies/{proxy}"), r#"{"enabled":false}"#).await;
    assert_eq!(status, 200, "{body}");
}

pub async fn toxic(proxy: &str, json: &str) {
    let (status, body) = http("POST", &format!("/proxies/{proxy}/toxics"), json).await;
    assert_eq!(status, 200, "{body}");
}

pub fn latency(ms: u32) -> String {
    format!(
        r#"{{"name":"slow-{ms}","type":"latency","stream":"downstream","attributes":{{"latency":{ms}}}}}"#
    )
}

impl Lab {
    /// After reconciliation every journal balances again and nothing waits in quarantine
    /// unresolved. Quarantined entries stay in quarantine, as the design says, so their age
    /// is not held against the check.
    pub async fn assert_clean_after_reconcile(&self) {
        let options = Options {
            quarantine_max_age_secs: 3600,
            ..Options::default()
        };
        for (i, c) in self.cells().await.iter().enumerate() {
            let v = check_cell(c, &self.schema, NAMES[i], &options)
                .await
                .unwrap();
            assert!(v.is_empty(), "{}: {v:?}", NAMES[i]);
        }
    }
}

/// Finishes every unfinished journal at cell `payer`, each one at the payee cell it names.
/// `cross_cell::recover` takes one payee cell for the whole pass, which cannot serve a payer
/// cell with journals owed to two different cells (and credits the wrong cell if given the
/// wrong one, where every cell holds every in-transit pair as the lab's do). That is K-09 in
/// the plan, and until it lands the random runs recover through this. Returns how many it settled.
pub async fn recover_each(cells: &[Client], schema: &str, payer: usize) -> usize {
    let tenant: TenantId = TENANT.parse().unwrap();
    let mut settled = 0;
    for u in cross_cell::unfinished(&cells[payer], schema, &tenant)
        .await
        .unwrap()
    {
        let payee = NAMES
            .iter()
            .position(|n| *n == u.payee_cell)
            .expect("a known cell");
        let epoch = |account: String| async move {
            cells[payee]
                .query_one(
                    &format!(
                        "select coalesce(max(epoch), 0)::bigint from {schema}.epoch_fence
                          where tenant_id = $1 and account_id = $2"
                    ),
                    &[&TENANT, &account],
                )
                .await
                .map(|r| r.get::<_, i64>(0))
                .unwrap()
        };
        let receiving: String = cells[payee]
            .query_one(
                &format!(
                    "select account_id from {schema}.in_transit_receipt
                      where tenant_id = $1 and from_cell = $2 and to_cell = $3"
                ),
                &[&TENANT, &u.payer_cell, &u.payee_cell],
            )
            .await
            .unwrap()
            .get(0);
        let t = Transfer {
            tenant: tenant.clone(),
            journal: u.journal.parse().unwrap(),
            actor: String::new(),
            payer: u.payer.parse().unwrap(),
            payee: u.payee.parse().unwrap(),
            sending: u.sending.parse().unwrap(),
            receiving: receiving.parse().unwrap(),
            amount: gbp(u.amount),
            payer_cell: u.payer_cell.clone(),
            payee_cell: u.payee_cell.clone(),
            payer_epoch: 0,
            sending_epoch: 0,
            receiving_epoch: epoch(receiving.clone()).await,
            payee_epoch: epoch(u.payee.clone()).await,
        };
        cross_cell::receive(&cells[payee], schema, &t)
            .await
            .unwrap();
        if cross_cell::settle(&cells[payer], schema, &tenant, &t.journal)
            .await
            .unwrap()
        {
            settled += 1;
        }
    }
    settled
}
