//! K-14: calls between cells use mutual TLS, and a listener off loopback refuses to run without
//! TLS. No database needed: the certificates are made here.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::routing::get;
use ledger_api::peer::{PeerClient, PeerError};
use ledger_api::tls::{self, TlsFiles, TlsListener};
use rcgen::{
    BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose,
};
use rustls::ClientConfig;
use rustls::crypto::ring::default_provider;

const TOKEN: &str = "pppppppppppppppppppppppppppppppppppppppp";

struct Authority {
    cert: rcgen::Certificate,
    key: KeyPair,
}

fn authority() -> Authority {
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(vec![]).unwrap();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let cert = params.self_signed(&key).unwrap();
    Authority { cert, key }
}

/// A certificate for `localhost` signed by the authority, usable as a server or a client.
fn leaf(ca: &Authority) -> (String, String) {
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(vec!["localhost".to_owned()]).unwrap();
    params.extended_key_usages = vec![
        ExtendedKeyUsagePurpose::ServerAuth,
        ExtendedKeyUsagePurpose::ClientAuth,
    ];
    let cert = params.signed_by(&key, &ca.cert, &ca.key).unwrap();
    (cert.pem(), key.serialize_pem())
}

fn files(dir: &std::path::Path, name: &str, ca: &Authority, signer: &Authority) -> TlsFiles {
    let (cert, key) = leaf(signer);
    let path = |suffix: &str, text: &str| {
        let p = dir.join(format!("{name}.{suffix}"));
        std::fs::write(&p, text).unwrap();
        p.to_string_lossy().into_owned()
    };
    TlsFiles {
        cert: Some(path("crt", &cert)),
        key: Some(path("key", &key)),
        ca: Some(path("ca", &ca.cert.pem())),
    }
}

/// Serves `/ping` with mutual TLS and returns the port.
async fn internal_server(config: Arc<rustls::ServerConfig>) -> u16 {
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = tcp.local_addr().unwrap().port();
    let listener = TlsListener::new(tcp, config).unwrap();
    let app = Router::new().route("/ping", get(|| async { "pong" }));
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    port
}

fn dir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("ledger-api-tls-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn tenant() -> ledger_core::TenantId {
    "ten_acme".parse().unwrap()
}

#[tokio::test]
async fn a_peer_with_a_certificate_from_our_authority_gets_through() {
    let d = dir("good");
    let ca = authority();
    let server = files(&d, "server", &ca, &ca);
    let client = files(&d, "client", &ca, &ca);
    let port = internal_server(tls::mutual_server_config(&server).unwrap()).await;
    let peer = PeerClient::new(
        &format!("https://localhost:{port}"),
        TOKEN,
        Some(tls::client_config(&client).unwrap()),
    )
    .unwrap();
    let (status, _) = peer.call("GET", "/ping", &tenant(), None).await.unwrap();
    assert_eq!(status, 200);
    let _ = std::fs::remove_dir_all(d);
}

#[tokio::test]
async fn a_peer_with_no_client_certificate_is_refused_at_the_handshake() {
    let d = dir("nocert");
    let ca = authority();
    let server = files(&d, "server", &ca, &ca);
    let port = internal_server(tls::mutual_server_config(&server).unwrap()).await;
    // A client that trusts the server but shows no certificate of its own.
    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca.cert.der().clone()).unwrap();
    let bare = ClientConfig::builder_with_provider(Arc::new(default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let peer = PeerClient::new(
        &format!("https://localhost:{port}"),
        TOKEN,
        Some(Arc::new(bare)),
    )
    .unwrap();
    let got = peer.call("GET", "/ping", &tenant(), None).await;
    assert!(matches!(got, Err(PeerError::Unreachable(_))), "{got:?}");
    let _ = std::fs::remove_dir_all(d);
}

#[tokio::test]
async fn a_peer_with_a_certificate_from_another_authority_is_refused() {
    let d = dir("other");
    let ours = authority();
    let theirs = authority();
    let server = files(&d, "server", &ours, &ours);
    // Its certificate is signed by an authority we do not trust, though it trusts ours.
    let stranger = files(&d, "stranger", &ours, &theirs);
    let port = internal_server(tls::mutual_server_config(&server).unwrap()).await;
    let peer = PeerClient::new(
        &format!("https://localhost:{port}"),
        TOKEN,
        Some(tls::client_config(&stranger).unwrap()),
    )
    .unwrap();
    let got = peer.call("GET", "/ping", &tenant(), None).await;
    assert!(matches!(got, Err(PeerError::Unreachable(_))), "{got:?}");
    let _ = std::fs::remove_dir_all(d);
}

#[tokio::test]
async fn a_server_that_is_not_ours_is_not_called() {
    let d = dir("impostor");
    let ours = authority();
    let theirs = authority();
    let client = files(&d, "client", &ours, &ours);
    let impostor = files(&d, "impostor", &theirs, &theirs);
    let port = internal_server(tls::server_config(&impostor).unwrap()).await;
    let peer = PeerClient::new(
        &format!("https://localhost:{port}"),
        TOKEN,
        Some(tls::client_config(&client).unwrap()),
    )
    .unwrap();
    let got = peer.call("GET", "/ping", &tenant(), None).await;
    assert!(matches!(got, Err(PeerError::Unreachable(_))), "{got:?}");
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn a_listener_off_loopback_needs_tls() {
    let anywhere: SocketAddr = "0.0.0.0:8080".parse().unwrap();
    let private: SocketAddr = "10.1.2.3:8080".parse().unwrap();
    let local: SocketAddr = "127.0.0.1:8080".parse().unwrap();
    let v6: SocketAddr = "[::1]:8080".parse().unwrap();
    assert!(tls::check_bind(&anywhere, false, "LEDGER_LISTEN").is_err());
    assert!(tls::check_bind(&private, false, "LEDGER_LISTEN").is_err());
    assert!(tls::check_bind(&anywhere, true, "LEDGER_LISTEN").is_ok());
    assert!(tls::check_bind(&local, false, "LEDGER_LISTEN").is_ok());
    assert!(tls::check_bind(&v6, false, "LEDGER_LISTEN").is_ok());
}

#[test]
fn a_peer_url_must_be_https_unless_it_is_loopback() {
    assert!(PeerClient::new("http://10.0.0.5:9000", TOKEN, None).is_err());
    assert!(PeerClient::new("https://cell-b.example:9000", TOKEN, None).is_err());
    assert!(PeerClient::new("http://127.0.0.1:9000", TOKEN, None).is_ok());
    assert!(PeerClient::new("http://127.0.0.1:9000", "short", None).is_err());
    // The credential is never shown.
    let shown = format!(
        "{:?}",
        PeerClient::new("http://127.0.0.1:9000", TOKEN, None).unwrap()
    );
    assert!(!shown.contains(TOKEN));
}
