//! K-24d: for a managed cell, a run writes `db.conn` with a host, port, role and database and no
//! password, says how the service signs in, and never reads the managed service's administrator
//! secret. A self-hosted cell is unchanged. Simulated stores stand in for each cloud's.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ledger_pki::manifest::{Authority, CellEntry, Database, DatabaseKind, SignIn};
use ledger_pki::{
    AdministratorCopy, Cloud, Kind, Manifest, MemorySigner, MemoryStore, PkiError, Rotation,
    SecretStore, Signer,
};
use serde_json::Value;
use time::OffsetDateTime;

const CA: &str = "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n";

/// Remembers which secrets a run asked to read.
struct Watching {
    inner: MemoryStore,
    reads: Mutex<Vec<String>>,
}

impl SecretStore for Watching {
    fn read(&self, cloud: Cloud, secret: &str) -> Result<Option<String>, PkiError> {
        self.reads.lock().unwrap().push(secret.to_owned());
        self.inner.read(cloud, secret)
    }
    fn write(&self, cloud: Cloud, secret: &str, value: &str) -> Result<(), PkiError> {
        self.inner.write(cloud, secret, value)
    }
}

fn cell(name: &str, cloud: Cloud, sign_in: Option<SignIn>) -> CellEntry {
    let managed = sign_in.is_some();
    CellEntry {
        name: name.into(),
        cloud,
        secret: format!("api-of-{name}"),
        address: "10.1.0.1".into(),
        internal_port: 8444,
        db_ca: managed.then(|| CA.to_owned()),
        database: Some(Database {
            host: format!("db-of-{name}.example.internal"),
            port: 5432,
            kind: if managed {
                DatabaseKind::Managed
            } else {
                DatabaseKind::SelfHosted
            },
            credential_secret: format!("administrator-of-{name}"),
            nodes: vec![],
            sign_in,
            role: None,
            name: None,
        }),
    }
}

fn manifest(cells: Vec<CellEntry>, proving: bool) -> Manifest {
    Manifest {
        authority: Authority {
            key_id: "key".into(),
            certificate_secret: "authority".into(),
        },
        tenants: vec!["ten_acme".into()],
        cells,
        etcd_members: vec![],
        proving,
    }
}

fn run(m: &Manifest, store: &Watching, kind: Kind) -> String {
    run_with(m, store, kind, Arc::new(MemorySigner::new().unwrap()))
}

fn run_with(m: &Manifest, store: &Watching, kind: Kind, signer: Arc<dyn Signer>) -> String {
    let copy = AdministratorCopy::new(store);
    Rotation {
        manifest: m,
        signer,
        store,
        now: OffsetDateTime::now_utc(),
        settle: Duration::ZERO,
        sleep: &|_| {},
        database: Some(&copy),
    }
    .run(kind)
    .unwrap()
    .to_string()
}

fn store_for(m: &Manifest, held: &str) -> Watching {
    let inner = MemoryStore::new();
    inner.put(Cloud::Aws, "authority", "");
    for c in &m.cells {
        inner.put(c.cloud, &c.secret, held);
        // The administrator's secret holds something that must never be copied or read.
        inner.put(
            c.cloud,
            &c.database.as_ref().unwrap().credential_secret,
            "ADMIN-PASSWORD-MARKER",
        );
    }
    Watching {
        inner,
        reads: Mutex::new(vec![]),
    }
}

fn entries(store: &Watching, c: &CellEntry) -> serde_json::Map<String, Value> {
    match serde_json::from_str::<Value>(&store.inner.latest(c.cloud, &c.secret).unwrap()).unwrap() {
        Value::Object(o) => o,
        _ => panic!("not an object"),
    }
}

fn three() -> Vec<CellEntry> {
    vec![
        cell("alpha", Cloud::Aws, Some(SignIn::AwsIam)),
        cell("beta", Cloud::Azure, Some(SignIn::AzureEntra)),
        cell("gamma", Cloud::Gcp, Some(SignIn::GcpIam)),
    ]
}

#[test]
fn a_managed_cell_gets_a_connection_with_no_password_and_a_sign_in_word() {
    let m = manifest(three(), false);
    let store = store_for(&m, r#"{"db.conn":"host=old password=old"}"#);
    run(&m, &store, Kind::All);
    for (c, word) in m.cells.iter().zip(["aws_iam", "azure_entra", "gcp_iam"]) {
        let e = entries(&store, c);
        let conn = e["db.conn"].as_str().unwrap();
        assert_eq!(
            conn,
            format!(
                "host=db-of-{}.example.internal port=5432 user=ledger_app dbname=postgres sslmode=require",
                c.name
            )
        );
        assert!(!conn.contains("password"), "no password in db.conn");
        assert_eq!(e["db-sign-in"].as_str().unwrap(), word);
        assert_eq!(e["db-ca.crt"].as_str().unwrap(), CA);
    }
}

#[test]
fn the_administrators_secret_is_never_read_or_copied_even_in_a_proving_environment() {
    for proving in [false, true] {
        let m = manifest(three(), proving);
        let store = store_for(&m, "{}");
        run(&m, &store, Kind::All);
        let reads = store.reads.lock().unwrap().clone();
        assert!(
            !reads.iter().any(|r| r.starts_with("administrator-of-")),
            "a run read an administrator's secret (proving: {proving})"
        );
        for c in &m.cells {
            let held = store.inner.latest(c.cloud, &c.secret).unwrap();
            assert!(!held.contains("ADMIN-PASSWORD-MARKER"));
        }
    }
}

#[test]
fn running_it_again_writes_the_same_connection() {
    let m = manifest(three(), false);
    let store = store_for(&m, "{}");
    // The same authority both times, as a real one is.
    let signer: Arc<dyn Signer> = Arc::new(MemorySigner::new().unwrap());
    run_with(&m, &store, Kind::All, signer.clone());
    let first: Vec<_> = m
        .cells
        .iter()
        .map(|c| entries(&store, c)["db.conn"].clone())
        .collect();
    run_with(&m, &store, Kind::All, signer);
    let second: Vec<_> = m
        .cells
        .iter()
        .map(|c| entries(&store, c)["db.conn"].clone())
        .collect();
    assert_eq!(first, second);
}

#[test]
fn a_self_hosted_cell_is_unchanged() {
    let m = manifest(vec![cell("alpha", Cloud::Aws, None)], false);
    let store = store_for(&m, r#"{"db.conn":"host=db password=kept"}"#);
    run(&m, &store, Kind::All);
    let e = entries(&store, &m.cells[0]);
    assert_eq!(e["db.conn"].as_str().unwrap(), "host=db password=kept");
    assert!(!e.contains_key("db-sign-in"));
}

#[test]
fn a_certificates_only_run_leaves_the_connection_alone() {
    let m = manifest(three(), false);
    let store = store_for(&m, r#"{"db.conn":"host=kept"}"#);
    run(&m, &store, Kind::Certificates);
    assert_eq!(
        entries(&store, &m.cells[0])["db.conn"].as_str().unwrap(),
        "host=kept"
    );
}

#[test]
fn the_report_names_no_host_role_or_word() {
    let m = manifest(three(), false);
    let line = run(&m, &store_for(&m, "{}"), Kind::All);
    for private in ["db-of-", "ledger_app", "aws_iam", "example.internal"] {
        assert!(!line.contains(private), "{line}");
    }
}

fn json(cell_database: &str, ca: &str) -> String {
    format!(
        r#"{{"authority":{{"key_id":"k","certificate_secret":"a"}},"tenants":["ten_acme"],
        "cells":[{{"name":"alpha","cloud":"aws","secret":"s","address":"10.1.0.1","internal_port":8444{ca},
        "database":{cell_database}}}]}}"#
    )
}

#[test]
fn the_manifest_refuses_a_managed_cell_with_no_ca_or_no_sign_in_and_other_mistakes() {
    let ca = format!(r#","db_ca":{}"#, serde_json::to_string(CA).unwrap());
    let db = |extra: &str| {
        format!(
            r#"{{"host":"h.internal","port":5432,"kind":"managed","credential_secret":"x"{extra}}}"#
        )
    };
    // Good.
    assert!(Manifest::parse(&json(&db(r#","sign_in":"aws_iam""#), &ca)).is_ok());
    // No sign-in.
    assert!(Manifest::parse(&json(&db(""), &ca)).is_err());
    // No CA.
    assert!(Manifest::parse(&json(&db(r#","sign_in":"aws_iam""#), "")).is_err());
    // A role that could carry a statement.
    assert!(Manifest::parse(&json(&db(r#","sign_in":"aws_iam","role":"a\";b""#), &ca)).is_err());
    // An unknown sign-in word.
    assert!(Manifest::parse(&json(&db(r#","sign_in":"password""#), &ca)).is_err());
    // A sign-in on a self-hosted cell.
    let own = r#"{"host":"h.internal","port":5432,"kind":"self_hosted","credential_secret":"x","sign_in":"aws_iam"}"#;
    assert!(Manifest::parse(&json(own, &ca)).is_err());
}
