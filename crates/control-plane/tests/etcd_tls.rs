#![cfg(feature = "etcd")]
//! K-15: the control plane reaches an etcd that requires TLS 1.3 and client certificates, as the
//! etcd module's does. The test starts its own etcd with `--client-cert-auth`, so it needs the
//! `etcd` binary: put it on the path or name it in `LEDGER_ETCD_BIN`.
//! Run with `cargo test -p control-plane --test etcd_tls -- --ignored`.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use control_plane::{ControlPlane, EtcdConfig, EtcdControlPlane, EtcdTls};
use rcgen::{
    BasicConstraints, Certificate, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa,
    KeyPair, SanType,
};

struct Authority {
    cert: Certificate,
    key: KeyPair,
}

fn authority(name: &str) -> Authority {
    let mut params = CertificateParams::new(Vec::new()).unwrap();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.distinguished_name.push(DnType::CommonName, name);
    let key = KeyPair::generate().unwrap();
    let cert = params.self_signed(&key).unwrap();
    Authority { cert, key }
}

/// A leaf certificate, as a pair of PEM files in `dir`. Returns (cert path, key path).
fn leaf(
    ca: &Authority,
    dir: &Path,
    name: &str,
    usage: ExtendedKeyUsagePurpose,
) -> (PathBuf, PathBuf) {
    let mut params = CertificateParams::new(Vec::new()).unwrap();
    params.distinguished_name.push(DnType::CommonName, name);
    params.subject_alt_names = vec![
        SanType::DnsName("localhost".try_into().unwrap()),
        SanType::IpAddress("127.0.0.1".parse().unwrap()),
    ];
    params.extended_key_usages = vec![usage];
    let key = KeyPair::generate().unwrap();
    let cert = params.signed_by(&key, &ca.cert, &ca.key).unwrap();
    let (c, k) = (
        dir.join(format!("{name}.crt")),
        dir.join(format!("{name}.key")),
    );
    std::fs::write(&c, cert.pem()).unwrap();
    std::fs::write(&k, key.serialize_pem()).unwrap();
    (c, k)
}

fn write_ca(ca: &Authority, dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(format!("{name}.crt"));
    std::fs::write(&p, ca.cert.pem()).unwrap();
    p
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

struct Etcd {
    child: Child,
    url: String,
}

impl Drop for Etcd {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start_etcd(dir: &Path, ca: &Path, cert: &Path, key: &Path) -> Etcd {
    let bin = std::env::var("LEDGER_ETCD_BIN").unwrap_or_else(|_| "etcd".into());
    let (client, peer) = (free_port(), free_port());
    let url = format!("https://127.0.0.1:{client}");
    let peer_url = format!("http://127.0.0.1:{peer}");
    let child = Command::new(bin)
        .arg("--data-dir")
        .arg(dir.join("data"))
        .args([
            "--listen-client-urls",
            &url,
            "--advertise-client-urls",
            &url,
        ])
        .args(["--listen-peer-urls", &peer_url])
        .args(["--initial-advertise-peer-urls", &peer_url])
        .args(["--initial-cluster", &format!("default={peer_url}")])
        // As the etcd module does: client certificates required, TLS 1.3 only.
        .arg("--client-cert-auth")
        .arg("--trusted-ca-file")
        .arg(ca)
        .arg("--cert-file")
        .arg(cert)
        .arg("--key-file")
        .arg(key)
        .args(["--tls-min-version", "TLS1.3"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("the etcd binary: put it on the path or set LEDGER_ETCD_BIN");
    Etcd { child, url }
}

fn config(url: &str, tls: Option<EtcdTls>) -> EtcdConfig {
    let mut c = EtcdConfig::new(vec![url.to_owned()], "/k15", Duration::from_secs(2));
    c.request_timeout = Duration::from_secs(2);
    c.tls = tls;
    c
}

/// Connects and reads, trying for a while because etcd takes a moment to start. Returns the
/// last error if it never works.
async fn reach(config: EtcdConfig, tries: usize) -> Result<(), String> {
    let mut last = String::new();
    for _ in 0..tries {
        match EtcdControlPlane::connect(config.clone()).await {
            Ok(cp) => match cp.accounts().await {
                Ok(_) => return Ok(()),
                Err(e) => last = e.to_string(),
            },
            Err(e) => last = e.to_string(),
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Err(last)
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("etcd-tls-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[tokio::test]
#[ignore = "needs the etcd binary"]
async fn a_client_with_a_valid_certificate_connects_and_one_without_is_refused() {
    let dir = temp_dir("main");
    let ca = authority("etcd authority");
    let ca_file = write_ca(&ca, &dir, "ca");
    let (server_cert, server_key) = leaf(&ca, &dir, "server", ExtendedKeyUsagePurpose::ServerAuth);
    let (client_cert, client_key) = leaf(&ca, &dir, "client", ExtendedKeyUsagePurpose::ClientAuth);
    let etcd = start_etcd(&dir, &ca_file, &server_cert, &server_key);

    // With the certificate: connects, writes and reads.
    let good = EtcdTls {
        ca: ca_file.clone(),
        cert: client_cert,
        key: client_key,
    };
    reach(config(&etcd.url, Some(good.clone())), 40)
        .await
        .expect("a valid client certificate is accepted");
    let cp = EtcdControlPlane::connect(config(&etcd.url, Some(good)))
        .await
        .unwrap();
    let account = "acc_k15".parse().unwrap();
    cp.register(&account, &"aws-test-01".parse().unwrap())
        .await
        .unwrap();
    assert_eq!(cp.accounts().await.unwrap().len(), 1);

    // Without one: the TLS handshake is refused, so nothing is read.
    let refused = reach(config(&etcd.url, None), 2).await;
    assert!(
        refused.is_err(),
        "a client with no certificate must be refused"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
#[ignore = "needs the etcd binary"]
async fn a_certificate_from_another_authority_is_refused() {
    let dir = temp_dir("other");
    let ca = authority("etcd authority");
    let other = authority("some other authority");
    let ca_file = write_ca(&ca, &dir, "ca");
    let (server_cert, server_key) = leaf(&ca, &dir, "server", ExtendedKeyUsagePurpose::ServerAuth);
    let (good_cert, good_key) = leaf(&ca, &dir, "good", ExtendedKeyUsagePurpose::ClientAuth);
    let (bad_cert, bad_key) = leaf(&other, &dir, "bad", ExtendedKeyUsagePurpose::ClientAuth);
    let etcd = start_etcd(&dir, &ca_file, &server_cert, &server_key);

    // Wait until etcd is up, with a good certificate.
    let good = EtcdTls {
        ca: ca_file.clone(),
        cert: good_cert,
        key: good_key,
    };
    reach(config(&etcd.url, Some(good)), 40)
        .await
        .expect("etcd starts");

    let bad = EtcdTls {
        ca: ca_file.clone(),
        cert: bad_cert,
        key: bad_key,
    };
    assert!(reach(config(&etcd.url, Some(bad)), 2).await.is_err());

    // A client that trusts the wrong authority will not talk to etcd either.
    let wrong_ca = write_ca(&other, &dir, "other-ca");
    let (cert, key) = leaf(&ca, &dir, "again", ExtendedKeyUsagePurpose::ClientAuth);
    let distrusting = EtcdTls {
        ca: wrong_ca,
        cert,
        key,
    };
    assert!(
        reach(config(&etcd.url, Some(distrusting)), 2)
            .await
            .is_err()
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_missing_file_is_reported_and_nothing_is_read() {
    let tls = EtcdTls {
        ca: "/nonexistent/ca.crt".into(),
        cert: "/nonexistent/client.crt".into(),
        key: "/nonexistent/client.key".into(),
    };
    let err = match EtcdControlPlane::connect(config("https://127.0.0.1:1", Some(tls))).await {
        Ok(_) => panic!("a missing file must not connect"),
        Err(e) => e.to_string(),
    };
    assert!(err.contains("etcd TLS file"), "{err}");
}
