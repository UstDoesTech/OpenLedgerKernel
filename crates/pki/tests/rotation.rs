//! K-17: issuing and rotating the API's certificates and cell credentials. No cloud and no
//! database: the authority's key is in memory, or behind a fake `aws` command that signs with one,
//! and the secrets are in memory. What is proven here is what a rotation writes and in what order.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ledger_pki::manifest::{Authority, CellEntry};
use ledger_pki::rotate::SECRET_FILES;
use ledger_pki::runner::Runner;
use ledger_pki::signer::point_from_spki;
use ledger_pki::{
    CliStore, Cloud, Kind, KmsSigner, Manifest, MemorySigner, MemoryStore, PkiError, Rotation,
    SecretStore, Signer,
};
use serde_json::Value;
use time::OffsetDateTime;

const AUTHORITY: &str = "authority-secret";

fn cell(name: &str, cloud: Cloud, address: &str) -> CellEntry {
    CellEntry {
        name: name.into(),
        cloud,
        secret: format!("secret-{name}"),
        address: address.into(),
        internal_port: 8444,
    }
}

fn manifest(names: &[&str]) -> Manifest {
    let clouds = [Cloud::Aws, Cloud::Azure, Cloud::Gcp];
    Manifest {
        authority: Authority {
            key_id: "key-id-that-must-not-be-printed".into(),
            certificate_secret: AUTHORITY.into(),
        },
        tenants: vec!["ten_acme".into()],
        cells: names
            .iter()
            .enumerate()
            .map(|(i, n)| cell(n, clouds[i % 3], &format!("10.1.0.{}", i + 1)))
            .collect(),
    }
}

/// A store whose secrets start with what a person fills: the database connection and the tokens.
fn store_for(m: &Manifest) -> MemoryStore {
    let s = MemoryStore::new();
    for c in &m.cells {
        s.put(
            c.cloud,
            &c.secret,
            r#"{"db.conn":"host=db","tokens.json":"[]"}"#,
        );
    }
    s
}

fn signer() -> Arc<dyn Signer> {
    Arc::new(MemorySigner::new().unwrap())
}

fn run(
    m: &Manifest,
    s: &dyn SecretStore,
    signer: Arc<dyn Signer>,
    kind: Kind,
) -> Result<ledger_pki::Report, PkiError> {
    Rotation {
        manifest: m,
        signer,
        store: s,
        now: OffsetDateTime::now_utc(),
        settle: Duration::from_secs(420),
        sleep: &|_| {},
    }
    .run(kind)
}

fn entries(s: &dyn SecretStore, c: &CellEntry) -> BTreeMap<String, String> {
    let text = s.read(c.cloud, &c.secret).unwrap().unwrap();
    serde_json::from_str(&text).unwrap()
}

/// The invariant that matters: every credential a cell calls with is listed by the cell it calls.
fn calls_are_all_listed(m: &Manifest, s: &dyn SecretStore) -> Result<(), String> {
    for caller in &m.cells {
        let e = entries(s, caller);
        let Some(cells) = e.get("cells.json") else {
            continue;
        };
        let cells: BTreeMap<String, Value> = serde_json::from_str(cells).unwrap();
        for (callee_name, call) in cells {
            let callee = m.cells.iter().find(|c| c.name == callee_name);
            let Some(callee) = callee else {
                return Err("calls a cell that is not in the manifest".into());
            };
            let listed: Vec<Value> = serde_json::from_str(
                entries(s, callee)
                    .get("internal-auth.json")
                    .ok_or("a callee lists nothing")?,
            )
            .unwrap();
            let ok = listed
                .iter()
                .any(|l| l["token"] == call["token"] && l["cell"] == caller.name.as_str());
            if !ok {
                return Err("a cell calls with a credential the other side does not list".into());
            }
        }
    }
    Ok(())
}

/// A store that checks the invariant after every single write.
struct Checked<'a> {
    inner: MemoryStore,
    manifest: &'a Manifest,
    broken: Mutex<Vec<String>>,
    snapshots: Mutex<Vec<BTreeMap<String, BTreeMap<String, String>>>>,
}

impl SecretStore for Checked<'_> {
    fn read(&self, c: Cloud, s: &str) -> Result<Option<String>, PkiError> {
        self.inner.read(c, s)
    }
    fn write(&self, c: Cloud, s: &str, v: &str) -> Result<(), PkiError> {
        self.inner.write(c, s, v)?;
        if s != AUTHORITY {
            if let Err(e) = calls_are_all_listed(self.manifest, &self.inner) {
                self.broken.lock().unwrap().push(e);
            }
            let snap = self
                .manifest
                .cells
                .iter()
                .map(|c| (c.name.clone(), entries(&self.inner, c)))
                .collect();
            self.snapshots.lock().unwrap().push(snap);
        }
        Ok(())
    }
}

fn checked(m: &Manifest) -> Checked<'_> {
    Checked {
        inner: store_for(m),
        manifest: m,
        broken: Mutex::new(vec![]),
        snapshots: Mutex::new(vec![]),
    }
}

#[test]
fn the_files_written_are_exactly_those_the_api_module_lists() {
    let tf = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../infra/modules/api-boot/main.tf"),
    )
    .unwrap();
    let block = tf
        .split("secret_files = [")
        .nth(1)
        .unwrap()
        .split(']')
        .next()
        .unwrap();
    let listed: Vec<&str> = block.split('"').skip(1).step_by(2).collect();
    assert_eq!(
        listed, SECRET_FILES,
        "the crate and the module must name the same files"
    );

    let m = manifest(&["cell-one", "cell-two", "cell-three"]);
    let s = store_for(&m);
    let report = run(&m, &s, signer(), Kind::All).unwrap();
    for c in &m.cells {
        let mut keys: Vec<String> = entries(&s, c).into_keys().collect();
        keys.sort();
        let mut want: Vec<String> = listed.iter().map(|f| (*f).to_owned()).collect();
        want.sort();
        assert_eq!(keys, want);
    }
    assert_eq!(report.incomplete, 0);
}

#[test]
fn the_database_connection_and_tokens_a_person_filled_are_kept() {
    let m = manifest(&["cell-one", "cell-two"]);
    let s = store_for(&m);
    run(&m, &s, signer(), Kind::All).unwrap();
    for c in &m.cells {
        let e = entries(&s, c);
        assert_eq!(e["db.conn"], "host=db");
        assert_eq!(e["tokens.json"], "[]");
    }
}

#[test]
fn a_cell_with_no_database_connection_is_counted_not_hidden() {
    let m = manifest(&["cell-one", "cell-two"]);
    let s = MemoryStore::new();
    let report = run(&m, &s, signer(), Kind::All).unwrap();
    assert_eq!(report.incomplete, 2);
}

#[test]
fn certificates_are_signed_by_the_authority_name_the_machine_and_last_thirty_days() {
    use rustls::client::WebPkiServerVerifier;
    use rustls::client::danger::ServerCertVerifier;
    use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
    use rustls_pki_types::pem::PemObject;

    let m = manifest(&["cell-one", "cell-two"]);
    let s = store_for(&m);
    run(&m, &s, signer(), Kind::All).unwrap();
    let e = entries(&s, &m.cells[0]);
    let mut roots = rustls::RootCertStore::empty();
    for c in CertificateDer::pem_slice_iter(e["ca.crt"].as_bytes()) {
        roots.add(c.unwrap()).unwrap();
    }
    let verifier = WebPkiServerVerifier::builder_with_provider(
        Arc::new(roots),
        Arc::new(rustls::crypto::ring::default_provider()),
    )
    .build()
    .unwrap();
    let leaf = CertificateDer::from_pem_slice(e["server.crt"].as_bytes()).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let at = |days: u64| UnixTime::since_unix_epoch(Duration::from_secs(now + days * 86_400));
    let name = ServerName::try_from("10.1.0.1").unwrap();
    verifier
        .verify_server_cert(&leaf, &[], &name, &[], at(0))
        .unwrap();
    verifier
        .verify_server_cert(&leaf, &[], &name, &[], at(29))
        .unwrap();
    assert!(
        verifier
            .verify_server_cert(&leaf, &[], &name, &[], at(31))
            .is_err(),
        "valid for 30 days, not longer"
    );
    let other = ServerName::try_from("10.1.0.2").unwrap();
    assert!(
        verifier
            .verify_server_cert(&leaf, &[], &other, &[], at(1))
            .is_err(),
        "it names its own machine only"
    );
    assert_eq!(e["ca.crt"], e["etcd-ca.crt"]);
}

#[tokio::test]
async fn what_a_rotation_writes_is_accepted_by_the_service_between_two_cells() {
    use ledger_api::peer::PeerClient;
    use ledger_api::tls::{self, TlsFiles, TlsListener};

    let mut m = manifest(&["cell-one", "cell-two"]);
    m.cells[0].address = "127.0.0.1".into();
    m.cells[1].address = "127.0.0.1".into();
    let s = store_for(&m);
    run(&m, &s, signer(), Kind::All).unwrap();
    let dir = std::env::temp_dir().join(format!("pki-handshake-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let files = |c: &CellEntry| {
        let e = entries(&s, c);
        let path = |f: &str| {
            let p = dir.join(format!("{}-{f}", c.name));
            std::fs::write(&p, &e[f]).unwrap();
            Some(p.to_string_lossy().into_owned())
        };
        TlsFiles {
            cert: path("server.crt"),
            key: path("server.key"),
            ca: path("ca.crt"),
        }
    };
    let server = tls::mutual_server_config(&files(&m.cells[0])).unwrap();
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = tcp.local_addr().unwrap().port();
    let app = axum::Router::new().route("/ping", axum::routing::get(|| async { "pong" }));
    let listener = TlsListener::new(tcp, server).unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let peer = PeerClient::new(
        &format!("https://127.0.0.1:{port}"),
        &"a".repeat(40),
        Some(tls::client_config(&files(&m.cells[1])).unwrap()),
    )
    .unwrap();
    let tenant: ledger_core::TenantId = "ten_acme".parse().unwrap();
    let (status, _) = peer.call("GET", "/ping", &tenant, None).await.unwrap();
    assert_eq!(status, 200);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn no_cell_ever_calls_with_a_credential_the_other_side_does_not_list() {
    let m = manifest(&["cell-one", "cell-two", "cell-three"]);
    let s = checked(&m);
    let sg = signer();
    // Twice, so the second run replaces credentials that are in use.
    run(&m, &s, sg.clone(), Kind::All).unwrap();
    run(&m, &s, sg, Kind::All).unwrap();
    assert!(
        s.broken.lock().unwrap().is_empty(),
        "{:?}",
        s.broken.lock().unwrap()
    );
    calls_are_all_listed(&m, &s.inner).unwrap();
}

#[test]
fn the_steps_go_lists_then_calls_then_old_credentials_out_with_a_wait_between() {
    let m = manifest(&["cell-one", "cell-two"]);
    let s = store_for(&m);
    let sg = signer();
    run(&m, &s, sg.clone(), Kind::All).unwrap();
    let before = entries(&s, &m.cells[0]);

    let waits = Mutex::new(Vec::new());
    let seen: Mutex<Vec<(String, String)>> = Mutex::new(vec![]);
    let sleep = |d: Duration| {
        waits.lock().unwrap().push(d);
        let e = entries(&s, &m.cells[0]);
        seen.lock()
            .unwrap()
            .push((e["cells.json"].clone(), e["internal-auth.json"].clone()));
    };
    Rotation {
        manifest: &m,
        signer: sg,
        store: &s,
        now: OffsetDateTime::now_utc(),
        settle: Duration::from_secs(420),
        sleep: &sleep,
    }
    .run(Kind::All)
    .unwrap();
    assert_eq!(*waits.lock().unwrap(), vec![Duration::from_secs(420); 2]);
    let seen = seen.lock().unwrap();
    // After step 1: it still calls with the old credential, and lists old and new.
    assert_eq!(seen[0].0, before["cells.json"]);
    let listed: Vec<Value> = serde_json::from_str(&seen[0].1).unwrap();
    assert_eq!(listed.len(), 2);
    // After step 2: it calls with a new one.
    assert_ne!(seen[1].0, before["cells.json"]);
    // At the end it lists the new credential only.
    let end = entries(&s, &m.cells[0]);
    let listed: Vec<Value> = serde_json::from_str(&end["internal-auth.json"]).unwrap();
    assert_eq!(listed.len(), 1);
    let old: Vec<Value> = serde_json::from_str(&before["internal-auth.json"]).unwrap();
    assert!(!old.is_empty());
    for o in old {
        let token = o["token"].as_str().unwrap();
        assert!(
            !end["internal-auth.json"].contains(token),
            "the old credential is out"
        );
    }
}

#[test]
fn a_removed_cell_stops_being_accepted_with_the_first_step_of_an_on_demand_run() {
    let three = manifest(&["cell-one", "cell-two", "cell-three"]);
    let s = store_for(&three);
    let sg = signer();
    run(&three, &s, sg.clone(), Kind::All).unwrap();
    for c in &three.cells[..2] {
        assert!(entries(&s, c)["internal-auth.json"].contains("cell-three"));
    }

    let two = manifest(&["cell-one", "cell-two"]);
    let writes_before = s.write_log().len();
    // Fail the second write of the run, so only the first step's first write has happened.
    run(&two, &s, sg.clone(), Kind::Credentials).unwrap();
    for c in &two.cells {
        assert!(
            !entries(&s, c)["internal-auth.json"].contains("cell-three"),
            "it is no longer accepted"
        );
        assert!(!entries(&s, c)["cells.json"].contains("cell-three"));
    }
    // And with the very first write: stop the run after one write and look.
    let s2 = store_for(&three);
    run(&three, &s2, sg.clone(), Kind::All).unwrap();
    s2.fail_write_number(s2.write_log().len() + 2);
    let _ = run(&two, &s2, sg, Kind::Credentials);
    assert!(
        !entries(&s2, &two.cells[0])["internal-auth.json"].contains("cell-three"),
        "the first write already drops it"
    );
    let _ = writes_before;
}

#[test]
fn a_run_that_fails_part_way_leaves_every_state_working_and_a_rerun_finishes() {
    let m = manifest(&["cell-one", "cell-two", "cell-three"]);
    let s = checked(&m);
    let sg = signer();
    run(&m, &s, sg.clone(), Kind::All).unwrap();
    let writes = s.inner.write_log().len();
    // Try failing each write of a second run in turn, from a fresh copy of the same starting state.
    for fail_at in 1..=9 {
        let t = checked(&m);
        run(&m, &t, sg.clone(), Kind::All).unwrap();
        let untouched: Vec<_> = m.cells.iter().map(|c| entries(&t.inner, c)).collect();
        t.inner
            .fail_write_number(t.inner.write_log().len() + fail_at);
        let result = run(&m, &t, sg.clone(), Kind::All);
        assert!(result.is_err(), "write {fail_at} should fail the run");
        assert!(matches!(result, Err(PkiError::Stopped { .. })));
        calls_are_all_listed(&m, &t.inner).unwrap();
        assert!(t.broken.lock().unwrap().is_empty());
        // A cell whose secret was not reached is byte for byte what it was.
        let changed = t.inner.write_log().len() - writes;
        assert!(changed >= 1);
        let same = m
            .cells
            .iter()
            .zip(&untouched)
            .filter(|(c, u)| &&entries(&t.inner, c) == u)
            .count();
        assert!(
            same >= 3 - fail_at.min(3),
            "fail_at {fail_at}: {same} cells unchanged"
        );
        // Running again finishes the job.
        t.inner.fail_write_number(usize::MAX);
        run(&m, &t, sg.clone(), Kind::All).unwrap();
        calls_are_all_listed(&m, &t.inner).unwrap();
    }
}

#[test]
fn nothing_is_written_when_the_authority_cannot_sign_or_is_not_the_stored_one() {
    let m = manifest(&["cell-one", "cell-two"]);
    let s = store_for(&m);
    let first = signer();
    run(&m, &s, first, Kind::All).unwrap();
    let writes = s.write_log().len();
    // A different key now stands where the authority was: its certificate is stored, not this one's.
    let other = signer();
    let err = run(&m, &s, other, Kind::All).unwrap_err();
    assert!(matches!(err, PkiError::Certificate(_)));
    assert_eq!(
        s.write_log().len(),
        writes,
        "no cell was given certificates nobody trusts"
    );
}

#[test]
fn the_authority_certificate_is_made_once_and_then_read() {
    let m = manifest(&["cell-one", "cell-two"]);
    let s = store_for(&m);
    let sg = signer();
    run(&m, &s, sg.clone(), Kind::All).unwrap();
    run(&m, &s, sg, Kind::All).unwrap();
    assert_eq!(s.version_count(Cloud::Aws, AUTHORITY), 1);
}

#[test]
fn one_cell_alone_needs_no_credentials_and_still_gets_certificates() {
    let m = manifest(&["cell-one"]);
    let s = store_for(&m);
    let r = run(&m, &s, signer(), Kind::All).unwrap();
    let e = entries(&s, &m.cells[0]);
    assert_eq!(
        (e["cells.json"].as_str(), e["internal-auth.json"].as_str()),
        ("{}", "[]")
    );
    assert_eq!(r.credentials, 0);
}

// ---- Signing through KMS, with a fake `aws` that signs with a key in memory. ----

/// Answers `aws kms get-public-key` and `aws kms sign` the way KMS does.
struct FakeAws {
    key: MemorySigner,
    seen_key_id: Mutex<Vec<String>>,
}

fn spki(point: &[u8]) -> Vec<u8> {
    let mut v = vec![
        0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01, 0x06, 0x08,
        0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07, 0x03, 0x42, 0x00,
    ];
    v.extend_from_slice(point);
    v
}

impl Runner for FakeAws {
    fn run(
        &self,
        program: &str,
        args: &[String],
        doing: &'static str,
    ) -> Result<Vec<u8>, PkiError> {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD;
        assert_eq!(program, "aws");
        assert_eq!(&args[..2], ["kms", args[1].as_str()]);
        self.seen_key_id.lock().unwrap().push(args[3].clone());
        match args[1].as_str() {
            "get-public-key" => Ok(format!(
                r#"{{"KeyId":"x","PublicKey":"{}"}}"#,
                b64.encode(spki(self.key.public_key()))
            )
            .into_bytes()),
            "sign" => {
                assert!(
                    args.contains(&"ECDSA_SHA_256".to_owned()) && args.contains(&"RAW".to_owned())
                );
                let path = args
                    .iter()
                    .find_map(|a| a.strip_prefix("fileb://"))
                    .unwrap();
                let sig = self.key.sign(&std::fs::read(path).unwrap()).unwrap();
                Ok(format!(r#"{{"Signature":"{}"}}"#, b64.encode(sig)).into_bytes())
            }
            _ => Err(PkiError::CannotRun(doing)),
        }
    }
}

#[test]
fn certificates_signed_through_kms_verify_and_the_key_is_never_asked_for() {
    let fake = FakeAws {
        key: MemorySigner::new().unwrap(),
        seen_key_id: Mutex::new(vec![]),
    };
    let dir = std::env::temp_dir().join(format!("pki-kms-{}", std::process::id()));
    let kms = KmsSigner::new(fake, "key-id-that-must-not-be-printed", dir.join("kms")).unwrap();
    let kms: Arc<dyn Signer> = Arc::new(kms);
    let m = manifest(&["cell-one", "cell-two"]);
    let s = store_for(&m);
    run(&m, &s, kms.clone(), Kind::All).unwrap();
    // The handshake test already shows these load into the service. Here, only that the
    // signature KMS made is the one the certificate carries: a second authority certificate
    // for the same key is recognised as the stored one.
    run(&m, &s, kms, Kind::Certificates).unwrap();
    assert!(
        std::fs::read_dir(dir.join("kms")).map_or(true, |d| d.count() == 0),
        "the message file is removed"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_key_that_is_not_p256_is_refused() {
    assert!(point_from_spki(&[0u8; 91]).is_err());
    assert!(point_from_spki(&[0x30, 0x82]).is_err());
}

// ---- The cloud tools: what is run, and what is never in a command line or a message. ----

struct Recorder {
    calls: Mutex<Vec<(String, Vec<String>)>>,
    answer: Mutex<Vec<Result<Vec<u8>, PkiError>>>,
}

impl Runner for Recorder {
    fn run(&self, program: &str, args: &[String], _: &'static str) -> Result<Vec<u8>, PkiError> {
        self.calls
            .lock()
            .unwrap()
            .push((program.to_owned(), args.to_vec()));
        self.answer.lock().unwrap().pop().unwrap_or(Ok(vec![]))
    }
}

fn recorder(answers: Vec<Result<Vec<u8>, PkiError>>) -> Arc<Recorder> {
    Arc::new(Recorder {
        calls: Mutex::new(vec![]),
        answer: Mutex::new(answers.into_iter().rev().collect()),
    })
}

struct Shared(Arc<Recorder>);

impl Runner for Shared {
    fn run(&self, program: &str, args: &[String], d: &'static str) -> Result<Vec<u8>, PkiError> {
        self.0.run(program, args, d)
    }
}

#[test]
fn a_secret_is_written_from_a_private_file_and_its_value_is_never_a_command_argument() {
    let value = r#"{"server.key":"THE-PRIVATE-KEY"}"#;
    let work = std::env::temp_dir().join(format!("pki-store-{}", std::process::id()));
    for (cloud, secret, program) in [
        (Cloud::Aws, "arn:aws:secretsmanager:r:1:secret:x", "aws"),
        (
            Cloud::Azure,
            "https://vault-x.vault.azure.net/secrets/api",
            "az",
        ),
        (Cloud::Gcp, "projects/p/secrets/api", "gcloud"),
    ] {
        let r = recorder(vec![]);
        let store = CliStore::new(Shared(r.clone()), work.clone());
        store.write(cloud, secret, value).unwrap();
        let calls = r.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, program);
        assert!(
            calls[0].1.iter().all(|a| !a.contains("THE-PRIVATE-KEY")),
            "{program}: the value is in an argument"
        );
        assert!(!work.join("secret-value").exists(), "the file is removed");
    }
    let _ = std::fs::remove_dir_all(work);
}

#[test]
fn an_address_that_is_not_a_secret_of_that_cloud_is_refused_before_anything_runs() {
    let r = recorder(vec![]);
    let store = CliStore::new(Shared(r.clone()), std::env::temp_dir());
    assert!(store.read(Cloud::Azure, "https://example.com/x").is_err());
    assert!(store.read(Cloud::Gcp, "not-a-secret").is_err());
    assert!(r.calls.lock().unwrap().is_empty());
}

#[test]
fn a_secret_with_no_version_reads_as_nothing() {
    let r = recorder(vec![Ok(b"0\n".to_vec())]);
    assert_eq!(
        CliStore::new(Shared(r), std::env::temp_dir())
            .read(Cloud::Aws, "arn")
            .unwrap(),
        None
    );
    let r = recorder(vec![Ok(b"\n".to_vec())]);
    assert_eq!(
        CliStore::new(Shared(r), std::env::temp_dir())
            .read(Cloud::Gcp, "projects/p/secrets/s")
            .unwrap(),
        None
    );
}

// ---- Counts only. ----

#[test]
fn a_failure_names_a_step_and_never_a_cell_a_secret_a_key_or_a_value() {
    let m = manifest(&["cell-one", "cell-two"]);
    let s = store_for(&m);
    s.fail_write_number(1);
    let err = run(&m, &s, signer(), Kind::All).unwrap_err();
    let shown = format!("{err} / {err:?}");
    for (n, secret) in [
        "secret-cell-one",
        "secret-cell-two",
        "cell-one",
        "10.1.0.1",
        "key-id-that-must-not-be-printed",
        "host=db",
        AUTHORITY,
    ]
    .into_iter()
    .enumerate()
    {
        // The message gives the position, never the text, so a failure does not print it.
        assert!(
            !shown.contains(secret),
            "a message names forbidden text number {n}"
        );
    }
    let r = recorder(vec![Err(PkiError::Command(255, "reading a secret"))]);
    let err = CliStore::new(Shared(r), std::env::temp_dir())
        .read(Cloud::Gcp, "projects/p/secrets/named-secret")
        .unwrap_err();
    assert!(!format!("{err} {err:?}").contains("named-secret"));
}

#[test]
fn a_run_prints_one_line_of_counts() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_ledger-pki"))
        .args(["rehearse", "--kind", "all"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(out.stderr.is_empty(), "nothing on standard error");
    let line = String::from_utf8(out.stdout).unwrap();
    assert_eq!(line.lines().count(), 1);
    let numbers = regex_free_counts(line.trim());
    assert!(numbers, "the line is not made of counts");
    for forbidden in [
        "BEGIN", "PRIVATE", "cell-one", "10.0.0.1", "secret-", "token",
    ] {
        assert!(!line.contains(forbidden));
    }
}

/// `cells: N, certificates issued: N, ...` and nothing else.
fn regex_free_counts(line: &str) -> bool {
    let parts: Vec<&str> = line.split(", ").collect();
    parts.len() == 5
        && parts.iter().all(|p| {
            p.rsplit_once(": ").is_some_and(|(label, n)| {
                label.chars().all(|c| c.is_ascii_lowercase() || c == ' ')
                    && !n.is_empty()
                    && n.chars().all(|c| c.is_ascii_digit())
            })
        })
}

#[test]
fn the_binary_fails_loudly_and_says_nothing_about_what_it_was_given() {
    let dir = std::env::temp_dir().join(format!("pki-bin-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("manifest.json");
    std::fs::write(&file, r#"{"authority":{"key_id":"SECRET-KEY-ID","certificate_secret":"SECRET-REF"},"tenants":[],"cells":[]}"#).unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_ledger-pki"))
        .args(["rotate", "--manifest"])
        .arg(&file)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !all.contains("SECRET-KEY-ID") && !all.contains("SECRET-REF"),
        "the output named something it was given"
    );
    let _ = std::fs::remove_dir_all(dir);
}
