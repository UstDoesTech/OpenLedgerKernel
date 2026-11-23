//! K-20b: a rotation issues each self-hosted cell's database machines their certificates, from the
//! same authority as everything else, and writes them where the machines read them.
//!
//! The last test starts a real Postgres with the primary's files and connects the way a standby
//! does. It needs the server binaries (`apt install postgresql-16`, or `PG_BIN`):
//! `cargo test -p pki --test node_certificates -- --ignored`.

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use ledger_pki::manifest::{Authority, CellEntry, Database, DatabaseKind, Node, NodeRole};
use ledger_pki::{
    Cloud, Kind, Manifest, MemorySigner, MemoryStore, PkiError, Rotation, SecretStore, Signer,
};
use serde_json::Value;
use time::OffsetDateTime;

const AUTHORITY: &str = "authority-certificate-secret";

fn node(cell: &str, i: usize, role: NodeRole) -> Node {
    Node {
        name: format!("{cell}-n{}", i + 1),
        role,
        address: format!("10.9.{}.{}", cell.len(), 10 + i),
        secret: format!("{cell}-n{}-tls", i + 1),
    }
}

fn cell(name: &str, cloud: Cloud, with_nodes: bool) -> CellEntry {
    CellEntry {
        name: name.into(),
        cloud,
        secret: format!("api-of-{name}"),
        address: "10.1.0.1".into(),
        internal_port: 8444,
        db_ca: None,
        database: Some(Database {
            host: "10.9.0.10".into(),
            port: 5432,
            kind: DatabaseKind::SelfHosted,
            credential_secret: format!("administrator-of-{name}"),
            nodes: if with_nodes {
                vec![
                    node(name, 0, NodeRole::Primary),
                    node(name, 1, NodeRole::Standby),
                    node(name, 2, NodeRole::Standby),
                ]
            } else {
                vec![]
            },
        }),
    }
}

fn manifest(cells: Vec<CellEntry>) -> Manifest {
    Manifest {
        authority: Authority {
            key_id: "key".into(),
            certificate_secret: AUTHORITY.into(),
        },
        tenants: vec!["ten_acme".into()],
        cells,
        proving: false,
    }
}

fn run(
    m: &Manifest,
    store: &dyn SecretStore,
    signer: Arc<dyn Signer>,
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
    .run(Kind::All)
}

fn files(store: &MemoryStore, cloud: Cloud, secret: &str) -> serde_json::Map<String, Value> {
    let text = store
        .latest(cloud, secret)
        .unwrap_or_else(|| panic!("{secret} was not written"));
    match serde_json::from_str(&text).unwrap() {
        Value::Object(o) => o,
        _ => panic!("not an object"),
    }
}

/// Whether a PEM certificate carries this IPv4 address in its names.
fn names_address(pem: &str, address: &str) -> bool {
    use base64::Engine;
    let body: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
    let der = base64::engine::general_purpose::STANDARD
        .decode(body.trim())
        .unwrap();
    let ip: std::net::Ipv4Addr = address.parse().unwrap();
    der.windows(4).any(|w| w == ip.octets())
}

#[test]
fn each_node_gets_the_files_the_node_agent_lists_for_its_role_in_its_own_secret() {
    let m = manifest(vec![cell("alpha", Cloud::Aws, true)]);
    let store = MemoryStore::new();
    let report = run(&m, &store, Arc::new(MemorySigner::new().unwrap())).unwrap();
    // One server certificate each, and a replication certificate for each of the two standbys.
    assert_eq!(report.node_certificates, 5);
    let db = m.cells[0].database.as_ref().unwrap();
    let authority = store.latest(Cloud::Aws, AUTHORITY).unwrap();
    for n in &db.nodes {
        let f = files(&store, Cloud::Aws, &n.secret);
        let mut got: Vec<&str> = f.keys().map(String::as_str).collect();
        got.sort_unstable();
        let want: &[&str] = match n.role {
            NodeRole::Primary => &["ca.crt", "server.crt", "server.key"],
            NodeRole::Standby => &[
                "ca.crt",
                "replication.crt",
                "replication.key",
                "server.crt",
                "server.key",
            ],
        };
        assert_eq!(got, want, "{:?}", n.role);
        assert_eq!(f["ca.crt"].as_str().unwrap(), authority);
        // The server certificate names the primary's address and the node's own, and no other node's.
        let crt = f["server.crt"].as_str().unwrap();
        assert!(names_address(crt, &db.nodes[0].address));
        assert!(names_address(crt, &n.address));
        for other in db
            .nodes
            .iter()
            .filter(|o| o.name != n.name && o.role != NodeRole::Primary)
        {
            assert!(!names_address(crt, &other.address));
        }
    }
}

#[test]
fn the_service_trusts_the_authority_for_the_database_and_a_cell_without_nodes_gets_none() {
    let m = manifest(vec![
        cell("alpha", Cloud::Aws, true),
        cell("beta", Cloud::Gcp, false),
    ]);
    let store = MemoryStore::new();
    run(&m, &store, Arc::new(MemorySigner::new().unwrap())).unwrap();
    let authority = store.latest(Cloud::Aws, AUTHORITY).unwrap();
    let alpha = files(&store, Cloud::Aws, "api-of-alpha");
    assert_eq!(alpha["db-ca.crt"].as_str().unwrap(), authority);
    let beta = files(&store, Cloud::Gcp, "api-of-beta");
    assert!(!beta.contains_key("db-ca.crt"));
    assert!(store.latest(Cloud::Gcp, "beta-n1-tls").is_none());
}

/// Refuses to write a node's secret, as a cloud does when the run's identity has no right to it.
struct RefusesNodes(MemoryStore);

impl SecretStore for RefusesNodes {
    fn read(&self, cloud: Cloud, secret: &str) -> Result<Option<String>, PkiError> {
        self.0.read(cloud, secret)
    }
    fn write(&self, cloud: Cloud, secret: &str, value: &str) -> Result<(), PkiError> {
        if secret.ends_with("-tls") {
            return Err(PkiError::Store("written: refused"));
        }
        self.0.write(cloud, secret, value)
    }
}

#[test]
fn a_node_secret_that_cannot_be_written_stops_the_run_before_any_cell_changes_and_says_only_the_step()
 {
    let m = manifest(vec![cell("alpha", Cloud::Aws, true)]);
    let store = RefusesNodes(MemoryStore::new());
    store
        .0
        .put(Cloud::Aws, "api-of-alpha", r#"{"db.conn":"kept"}"#);
    let err = run(&m, &store, Arc::new(MemorySigner::new().unwrap())).unwrap_err();
    let said = err.to_string();
    assert!(
        said.contains("step 1")
            && said.contains("a database node's certificates could not be written")
    );
    assert!(!said.contains("alpha") && !said.contains("-tls"));
    // The cell's own secret is as it was.
    assert_eq!(
        store.0.latest(Cloud::Aws, "api-of-alpha").unwrap(),
        r#"{"db.conn":"kept"}"#
    );
}

#[test]
fn a_run_that_makes_certificates_again_keeps_every_node_able_to_use_its_files() {
    let m = manifest(vec![cell("alpha", Cloud::Aws, true)]);
    let store = MemoryStore::new();
    let signer: Arc<dyn Signer> = Arc::new(MemorySigner::new().unwrap());
    run(&m, &store, signer.clone()).unwrap();
    let before = files(&store, Cloud::Aws, "alpha-n2-tls");
    run(&m, &store, signer).unwrap();
    let after = files(&store, Cloud::Aws, "alpha-n2-tls");
    // New certificates, from the same authority: a node holding the old ones still trusts and is trusted.
    assert_ne!(before["server.crt"], after["server.crt"]);
    assert_eq!(before["ca.crt"], after["ca.crt"]);
}

#[test]
fn the_report_counts_and_names_nothing() {
    let m = manifest(vec![cell("alpha", Cloud::Aws, true)]);
    let store = MemoryStore::new();
    let line = run(&m, &store, Arc::new(MemorySigner::new().unwrap()))
        .unwrap()
        .to_string();
    assert!(
        line.contains("database node certificates issued: 5"),
        "{line}"
    );
    for forbidden in ["alpha", "10.9", "BEGIN", "-tls"] {
        assert!(!line.contains(forbidden));
    }
}

fn parse(json: &str) -> Result<Manifest, PkiError> {
    Manifest::parse(json)
}

fn manifest_json(database: &str) -> String {
    format!(
        r#"{{"authority":{{"key_id":"k","certificate_secret":"c"}},"tenants":["ten_a"],"cells":[{{"name":"a","cloud":"aws","secret":"s","address":"10.0.0.1","internal_port":1,"database":{database}}}]}}"#
    )
}

#[test]
fn a_manifest_with_bad_nodes_is_refused() {
    let node = |name: &str, role: &str, address: &str| {
        format!(r#"{{"name":"{name}","role":"{role}","address":"{address}","secret":"s-{name}"}}"#)
    };
    let db = |kind: &str, nodes: &[String]| {
        format!(
            r#"{{"host":"10.0.0.2","port":5432,"kind":"{kind}","credential_secret":"x","nodes":[{}]}}"#,
            nodes.join(",")
        )
    };
    let ok = [
        node("n1", "primary", "10.0.0.2"),
        node("n2", "standby", "10.0.0.3"),
    ];
    assert!(parse(&manifest_json(&db("self_hosted", &ok))).is_ok());
    // Two primaries, none, a repeated name, an address that is not an IP, a managed cell with nodes.
    let two = [
        node("n1", "primary", "10.0.0.2"),
        node("n2", "primary", "10.0.0.3"),
    ];
    let none = [node("n1", "standby", "10.0.0.2")];
    let same = [
        node("n1", "primary", "10.0.0.2"),
        node("n1", "standby", "10.0.0.3"),
    ];
    let name = [node("n1", "primary", "db.example")];
    for bad in [&two[..], &none[..], &same[..], &name[..]] {
        assert!(parse(&manifest_json(&db("self_hosted", bad))).is_err());
    }
    assert!(parse(&manifest_json(&db("managed", &ok))).is_err());
}

// ---- a real Postgres -------------------------------------------------------------------------

fn bin(name: &str) -> PathBuf {
    let dir = std::env::var("PG_BIN").ok().or_else(|| {
        let mut dirs: Vec<_> = std::fs::read_dir("/usr/lib/postgresql")
            .ok()?
            .filter_map(|e| e.ok().map(|e| e.path().join("bin")))
            .collect();
        dirs.sort();
        dirs.pop().map(|p| p.display().to_string())
    });
    PathBuf::from(dir.expect("no Postgres binaries: set PG_BIN")).join(name)
}

fn is_root() -> bool {
    String::from_utf8(Command::new("id").arg("-u").output().unwrap().stdout)
        .unwrap()
        .trim()
        == "0"
}

/// Postgres refuses to run as root, so as root the commands run as the `postgres` user.
fn pg(args: &[String]) -> std::process::Output {
    let line = args.join(" ");
    let out = if is_root() {
        Command::new("su").args(["postgres", "-c", &line]).output()
    } else {
        Command::new("bash").args(["-c", &line]).output()
    };
    out.unwrap()
}

struct Server {
    dir: PathBuf,
    port: u16,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = pg(&[
            bin("pg_ctl").display().to_string(),
            "-D".into(),
            self.dir.join("data").display().to_string(),
            "-m".into(),
            "immediate".into(),
            "stop".into(),
        ]);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn write_private(dir: &std::path::Path, name: &str, text: &str) {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, text).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

fn start(files: &serde_json::Map<String, Value>) -> Server {
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let dir = std::env::temp_dir().join(format!("ledger-nodecert-{}-{port}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for name in ["server.crt", "server.key", "ca.crt"] {
        write_private(&dir, name, files[name].as_str().unwrap());
    }
    // Only the replication role may sign in, with a certificate, as a standby does.
    write_private(
        &dir,
        "hba.conf",
        "hostssl all replicator 127.0.0.1/32 cert\n",
    );
    if is_root() {
        let _ = Command::new("chown")
            .args(["-R", "postgres"])
            .arg(&dir)
            .status();
    }
    let data = dir.join("data");
    let init = pg(&[
        bin("initdb").display().to_string(),
        "-D".into(),
        data.display().to_string(),
        "-U".into(),
        "replicator".into(),
        "--auth-local=trust".into(),
    ]);
    assert!(init.status.success(), "initdb failed");
    std::fs::copy(dir.join("hba.conf"), data.join("pg_hba.conf")).unwrap();
    if is_root() {
        let _ = Command::new("chown")
            .arg("postgres")
            .arg(data.join("pg_hba.conf"))
            .status();
    }
    let options = format!(
        "-p {port} -k {} -c listen_addresses=127.0.0.1 -c fsync=off -c ssl=on -c ssl_cert_file={} -c ssl_key_file={} -c ssl_ca_file={}",
        dir.display(),
        dir.join("server.crt").display(),
        dir.join("server.key").display(),
        dir.join("ca.crt").display()
    );
    let started = pg(&[
        bin("pg_ctl").display().to_string(),
        "-D".into(),
        data.display().to_string(),
        "-o".into(),
        format!("'{options}'"),
        "-l".into(),
        dir.join("log").display().to_string(),
        "-w".into(),
        "start".into(),
    ]);
    assert!(
        started.status.success(),
        "Postgres did not start with the issued files: {}",
        std::fs::read_to_string(dir.join("log")).unwrap_or_default()
    );
    Server { dir, port }
}

/// Connects as a standby does: verifying the primary's name against the authority, and showing its
/// own replication certificate.
fn connect_as_standby(
    server: &Server,
    root: &str,
    standby: &serde_json::Map<String, Value>,
) -> bool {
    let dir = &server.dir;
    write_private(dir, "root.crt", root);
    write_private(
        dir,
        "replication.crt",
        standby["replication.crt"].as_str().unwrap(),
    );
    write_private(
        dir,
        "replication.key",
        standby["replication.key"].as_str().unwrap(),
    );
    if is_root() {
        let _ = Command::new("chown")
            .args(["-R", "postgres"])
            .arg(dir)
            .status();
    }
    let conninfo = format!(
        "host=127.0.0.1 port={} user=replicator dbname=postgres sslmode=verify-full sslrootcert={} sslcert={} sslkey={}",
        server.port,
        dir.join("root.crt").display(),
        dir.join("replication.crt").display(),
        dir.join("replication.key").display()
    );
    pg(&[
        bin("psql").display().to_string(),
        format!("'{conninfo}'"),
        "-Atc".into(),
        "'select 1'".into(),
    ])
    .status
    .success()
}

#[test]
#[ignore = "needs the Postgres server binaries"]
fn a_primary_starts_with_the_issued_files_and_accepts_a_standby_that_trusts_the_same_authority() {
    // The addresses in the other tests are made up. Here the primary is this machine, because the
    // real server listens on 127.0.0.1 and the standby checks that name.
    let mut local = manifest(vec![cell("alpha", Cloud::Aws, true)]);
    local.cells[0].database.as_mut().unwrap().nodes[0].address = "127.0.0.1".into();
    let store = MemoryStore::new();
    run(&local, &store, Arc::new(MemorySigner::new().unwrap())).unwrap();
    let primary_local = files(&store, Cloud::Aws, "alpha-n1-tls");
    let standby_local = files(&store, Cloud::Aws, "alpha-n2-tls");

    let server = start(&primary_local);
    let authority = primary_local["ca.crt"].as_str().unwrap();
    assert!(
        connect_as_standby(&server, authority, &standby_local),
        "a standby must be accepted by a primary started with the issued files"
    );

    // Another authority is not trusted by the standby, so it does not connect.
    let other = {
        let s = MemoryStore::new();
        run(&local, &s, Arc::new(MemorySigner::new().unwrap())).unwrap();
        files(&s, Cloud::Aws, "alpha-n1-tls")["ca.crt"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    assert!(!connect_as_standby(&server, &other, &standby_local));
}
