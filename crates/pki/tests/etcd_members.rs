//! K-18a: a rotation issues each etcd member its files from the same authority as the cells', and
//! writes them where the member reads them. The handshake tests run real TLS 1.3 in memory, with
//! client certificates required, as the etcd module starts etcd.

use std::sync::Arc;
use std::time::Duration;

use ledger_pki::manifest::{Authority, CellEntry, EtcdMember};
use ledger_pki::{
    Cloud, Kind, Manifest, MemorySigner, MemoryStore, PkiError, Rotation, SecretStore, Signer,
};
use rustls::client::WebPkiServerVerifier;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::server::WebPkiClientVerifier;
use rustls::{ClientConfig, ClientConnection, RootCertStore, ServerConfig, ServerConnection};
use rustls_pki_types::pem::PemObject;
use serde_json::Value;
use time::OffsetDateTime;

const AUTHORITY: &str = "authority-secret";
const FILES: [&str; 5] = ["ca.crt", "server.crt", "server.key", "peer.crt", "peer.key"];

fn member(i: usize, cloud: Cloud) -> EtcdMember {
    EtcdMember {
        name: format!("etcd-{i}"),
        cloud,
        address: format!("10.11.64.{}", 10 + i),
        secret: format!("etcd-{i}-tls"),
    }
}

fn manifest(members: Vec<EtcdMember>) -> Manifest {
    Manifest {
        authority: Authority {
            key_id: "key".into(),
            certificate_secret: AUTHORITY.into(),
        },
        tenants: vec!["ten_acme".into()],
        cells: vec![CellEntry {
            name: "alpha".into(),
            cloud: Cloud::Aws,
            secret: "api-of-alpha".into(),
            address: "10.1.0.1".into(),
            internal_port: 8444,
            db_ca: None,
            database: None,
        }],
        etcd_members: members,
        proving: false,
    }
}

fn three() -> Vec<EtcdMember> {
    vec![
        member(1, Cloud::Aws),
        member(2, Cloud::Aws),
        member(3, Cloud::Azure),
    ]
}

fn run(
    m: &Manifest,
    store: &dyn SecretStore,
    signer: Arc<dyn Signer>,
    kind: Kind,
) -> Result<ledger_pki::Report, PkiError> {
    Rotation {
        manifest: m,
        signer,
        store,
        now: OffsetDateTime::now_utc(),
        settle: Duration::ZERO,
        sleep: &|_| {},
        database: None,
    }
    .run(kind)
}

fn object(store: &MemoryStore, cloud: Cloud, secret: &str) -> serde_json::Map<String, Value> {
    match serde_json::from_str(&store.latest(cloud, secret).expect("not written")).unwrap() {
        Value::Object(o) => o,
        _ => panic!("not an object"),
    }
}

fn text(o: &serde_json::Map<String, Value>, k: &str) -> String {
    o[k].as_str().unwrap().to_owned()
}

fn roots(pem: &str) -> Arc<RootCertStore> {
    let mut r = RootCertStore::empty();
    for c in CertificateDer::pem_slice_iter(pem.as_bytes()) {
        r.add(c.unwrap()).unwrap();
    }
    Arc::new(r)
}

fn chain(pem: &str) -> Vec<CertificateDer<'static>> {
    CertificateDer::pem_slice_iter(pem.as_bytes())
        .map(|c| c.unwrap())
        .collect()
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// A member as etcd starts it: TLS 1.3, its own certificate, clients must show one from the authority.
fn serving(crt: &str, key: &str, ca: &str) -> Arc<ServerConfig> {
    let verifier = WebPkiClientVerifier::builder_with_provider(roots(ca), provider())
        .build()
        .unwrap();
    Arc::new(
        ServerConfig::builder_with_provider(provider())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .with_client_cert_verifier(verifier)
            .with_single_cert(
                chain(crt),
                PrivateKeyDer::from_pem_slice(key.as_bytes()).unwrap(),
            )
            .unwrap(),
    )
}

fn calling(crt: &str, key: &str, ca: &str) -> Arc<ClientConfig> {
    let verifier = WebPkiServerVerifier::builder_with_provider(roots(ca), provider())
        .build()
        .unwrap();
    Arc::new(
        ClientConfig::builder_with_provider(provider())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .with_webpki_verifier(verifier)
            .with_client_auth_cert(
                chain(crt),
                PrivateKeyDer::from_pem_slice(key.as_bytes()).unwrap(),
            )
            .unwrap(),
    )
}

/// Runs the handshake between the two, in memory, and says whether both sides finished it.
fn handshake(
    server: Arc<ServerConfig>,
    client: Arc<ClientConfig>,
    name: &str,
) -> Result<(), String> {
    let mut s = rustls::Connection::from(ServerConnection::new(server).unwrap());
    let mut c = rustls::Connection::from(
        ClientConnection::new(client, ServerName::try_from(name.to_owned()).unwrap()).unwrap(),
    );
    for _ in 0..20 {
        move_bytes(&mut c, &mut s)?;
        move_bytes(&mut s, &mut c)?;
        if !s.is_handshaking() && !c.is_handshaking() {
            return Ok(());
        }
    }
    Err("the handshake did not finish".into())
}

fn move_bytes(from: &mut rustls::Connection, to: &mut rustls::Connection) -> Result<(), String> {
    let mut wire = Vec::new();
    while from.wants_write() {
        from.write_tls(&mut wire).map_err(|e| e.to_string())?;
    }
    let mut rest: &[u8] = &wire;
    while !rest.is_empty() {
        to.read_tls(&mut rest).map_err(|e| e.to_string())?;
        to.process_new_packets().map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[test]
fn each_member_gets_the_five_files_the_etcd_module_lists_in_its_own_secret() {
    let m = manifest(three());
    let store = MemoryStore::new();
    run(
        &m,
        &store,
        Arc::new(MemorySigner::new().unwrap()),
        Kind::All,
    )
    .unwrap();
    let cell = object(&store, Cloud::Aws, "api-of-alpha");
    for member in &m.etcd_members {
        let files = object(&store, member.cloud, &member.secret);
        assert_eq!(
            files
                .keys()
                .map(String::as_str)
                .collect::<std::collections::BTreeSet<_>>(),
            FILES.iter().copied().collect()
        );
        assert_eq!(text(&files, "ca.crt"), text(&cell, "etcd-ca.crt"));
        assert_ne!(text(&files, "server.crt"), text(&files, "peer.crt"));
    }
    // One member's files are in no other member's secret.
    assert_ne!(
        text(&object(&store, Cloud::Aws, "etcd-1-tls"), "server.key"),
        text(&object(&store, Cloud::Aws, "etcd-2-tls"), "server.key")
    );
}

#[test]
fn a_cells_etcd_client_certificate_is_accepted_by_a_member_that_requires_client_certificates() {
    let m = manifest(three());
    let store = MemoryStore::new();
    run(
        &m,
        &store,
        Arc::new(MemorySigner::new().unwrap()),
        Kind::All,
    )
    .unwrap();
    let cell = object(&store, Cloud::Aws, "api-of-alpha");
    let files = object(&store, Cloud::Aws, "etcd-1-tls");
    let server = serving(
        &text(&files, "server.crt"),
        &text(&files, "server.key"),
        &text(&files, "ca.crt"),
    );
    let client = calling(
        &text(&cell, "etcd-client.crt"),
        &text(&cell, "etcd-client.key"),
        &text(&cell, "etcd-ca.crt"),
    );
    handshake(server, client, "10.11.64.11").expect("the cell is accepted by the member");
}

#[test]
fn a_member_refuses_a_certificate_from_another_authority_and_a_name_that_is_not_its_own() {
    let m = manifest(three());
    let store = MemoryStore::new();
    run(
        &m,
        &store,
        Arc::new(MemorySigner::new().unwrap()),
        Kind::All,
    )
    .unwrap();
    let files = object(&store, Cloud::Aws, "etcd-1-tls");
    let server = || {
        serving(
            &text(&files, "server.crt"),
            &text(&files, "server.key"),
            &text(&files, "ca.crt"),
        )
    };

    // A cell of another authority.
    let other = MemoryStore::new();
    run(
        &m,
        &other,
        Arc::new(MemorySigner::new().unwrap()),
        Kind::All,
    )
    .unwrap();
    let stranger = object(&other, Cloud::Aws, "api-of-alpha");
    let ours = object(&store, Cloud::Aws, "api-of-alpha");
    let client = calling(
        &text(&stranger, "etcd-client.crt"),
        &text(&stranger, "etcd-client.key"),
        &text(&ours, "etcd-ca.crt"),
    );
    assert!(handshake(server(), client, "10.11.64.11").is_err());

    // Our cell, asking for a different member's address.
    let client = calling(
        &text(&ours, "etcd-client.crt"),
        &text(&ours, "etcd-client.key"),
        &text(&ours, "etcd-ca.crt"),
    );
    assert!(handshake(server(), client, "10.11.64.12").is_err());
}

#[test]
fn members_trust_each_other_on_the_peer_port_in_both_directions() {
    let m = manifest(three());
    let store = MemoryStore::new();
    run(
        &m,
        &store,
        Arc::new(MemorySigner::new().unwrap()),
        Kind::All,
    )
    .unwrap();
    let a = object(&store, Cloud::Aws, "etcd-1-tls");
    let b = object(&store, Cloud::Azure, "etcd-3-tls");
    for (from, to, name) in [(&a, &b, "10.11.64.13"), (&b, &a, "10.11.64.11")] {
        let server = serving(
            &text(to, "peer.crt"),
            &text(to, "peer.key"),
            &text(to, "ca.crt"),
        );
        let client = calling(
            &text(from, "peer.crt"),
            &text(from, "peer.key"),
            &text(from, "ca.crt"),
        );
        handshake(server, client, name).expect("a peer is accepted by a peer");
    }
}

#[test]
fn a_run_makes_new_files_from_the_same_authority_so_a_member_holding_old_ones_still_works() {
    let m = manifest(three());
    let store = MemoryStore::new();
    let signer: Arc<dyn Signer> = Arc::new(MemorySigner::new().unwrap());
    run(&m, &store, signer.clone(), Kind::All).unwrap();
    let before = object(&store, Cloud::Aws, "etcd-1-tls");
    run(&m, &store, signer, Kind::All).unwrap();
    let after = object(&store, Cloud::Aws, "etcd-1-tls");
    assert_ne!(text(&before, "server.crt"), text(&after, "server.crt"));
    assert_eq!(text(&before, "ca.crt"), text(&after, "ca.crt"));
}

#[test]
fn a_run_that_makes_no_certificates_leaves_the_members_alone() {
    let m = manifest(three());
    let store = MemoryStore::new();
    run(
        &m,
        &store,
        Arc::new(MemorySigner::new().unwrap()),
        Kind::Credentials,
    )
    .unwrap();
    assert!(store.latest(Cloud::Aws, "etcd-1-tls").is_none());
}

struct RefusesMembers(MemoryStore);

impl SecretStore for RefusesMembers {
    fn read(&self, cloud: Cloud, secret: &str) -> Result<Option<String>, PkiError> {
        self.0.read(cloud, secret)
    }
    fn write(&self, cloud: Cloud, secret: &str, value: &str) -> Result<(), PkiError> {
        if secret.starts_with("etcd-") {
            return Err(PkiError::Store("written: refused"));
        }
        self.0.write(cloud, secret, value)
    }
}

#[test]
fn a_member_secret_that_cannot_be_written_stops_the_run_before_the_cell_changes_and_names_only_the_step()
 {
    let m = manifest(three());
    let store = RefusesMembers(MemoryStore::new());
    store
        .0
        .put(Cloud::Aws, "api-of-alpha", r#"{"db.conn":"kept"}"#);
    let said = run(
        &m,
        &store,
        Arc::new(MemorySigner::new().unwrap()),
        Kind::All,
    )
    .unwrap_err()
    .to_string();
    assert!(
        said.contains("step 1")
            && said.contains("an etcd member's certificates could not be written"),
        "{said}"
    );
    for forbidden in ["etcd-", "10.11", "alpha", "-tls"] {
        assert!(!said.contains(forbidden), "{said}");
    }
    assert_eq!(
        store.0.latest(Cloud::Aws, "api-of-alpha").unwrap(),
        r#"{"db.conn":"kept"}"#
    );
}

#[test]
fn the_report_counts_the_members_files_and_names_nothing() {
    let m = manifest(three());
    let line = run(
        &m,
        &MemoryStore::new(),
        Arc::new(MemorySigner::new().unwrap()),
        Kind::All,
    )
    .unwrap()
    .to_string();
    assert!(
        line.contains("etcd member certificates issued: 6"),
        "{line}"
    );
    for forbidden in ["etcd-", "10.11", "BEGIN", "-tls"] {
        assert!(!line.contains(forbidden), "{line}");
    }
    let none = run(
        &manifest(vec![]),
        &MemoryStore::new(),
        Arc::new(MemorySigner::new().unwrap()),
        Kind::All,
    )
    .unwrap()
    .to_string();
    assert!(
        none.contains("etcd member certificates issued: 0"),
        "{none}"
    );
}

#[test]
fn a_manifest_with_bad_members_is_refused_without_their_text() {
    let good = r#"{"name":"a","cloud":"aws","address":"10.0.0.1","secret":"s"}"#;
    let with = |members: &str| {
        format!(
            r#"{{"authority":{{"key_id":"k","certificate_secret":"c"}},"tenants":["ten_a"],"cells":[{{"name":"x","cloud":"aws","secret":"s","address":"10.1.0.1","internal_port":1}}],"etcd_members":[{members}]}}"#
        )
    };
    assert!(Manifest::parse(&with(good)).is_ok());
    for bad in [
        format!("{good},{good}"),
        r#"{"name":"a","cloud":"aws","address":"not-an-ip","secret":"s"}"#.to_owned(),
        r#"{"name":"a","cloud":"aws","address":"10.0.0.1","secret":""}"#.to_owned(),
        r#"{"name":"a","cloud":"aws","address":"10.0.0.1","secret":"s","extra":1}"#.to_owned(),
    ] {
        let said = Manifest::parse(&with(&bad)).unwrap_err().to_string();
        assert!(
            !said.contains("not-an-ip") && !said.contains("10.0.0.1"),
            "{said}"
        );
    }
}
