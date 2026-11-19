//! Three real containers: the proof for I-01i. Needs Docker, openssl and an image built from
//! images/postgres/Dockerfile, so it is ignored by default. CI runs it in the `image` workflow:
//!
//!     docker build -f images/postgres/Dockerfile -t ledger-postgres:ci .
//!     LEDGER_IMAGE=ledger-postgres:ci cargo test -p node-agent --test containers -- --ignored --nocapture

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use node_agent::store::FileStore;

const SECRET_ID: &str = "arn:test:cell/administrator";

fn image() -> String {
    std::env::var("LEDGER_IMAGE").unwrap_or_else(|_| "ledger-postgres:ci".to_string())
}

fn docker(args: &[&str]) -> Output {
    Command::new("docker")
        .args(args)
        .output()
        .expect("docker must be installed")
}

fn docker_ok(args: &[&str]) -> String {
    let out = docker(args);
    assert!(
        out.status.success(),
        "docker {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn wait_for(what: &str, secs: u64, mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(
            start.elapsed() < Duration::from_secs(secs),
            "timed out waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn openssl(dir: &Path, args: &[&str]) {
    let out = Command::new("openssl")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("openssl must be installed");
    assert!(
        out.status.success(),
        "openssl {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A CA, a server certificate for each node and a client certificate named `replicator`.
/// Throwaway test material, made fresh for each run and never committed.
fn make_certificates(dir: &Path, hosts: &[String]) {
    openssl(
        dir,
        &[
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-keyout",
            "ca.key",
            "-out",
            "ca.crt",
            "-subj",
            "/CN=ledger-test-ca",
            "-days",
            "2",
        ],
    );
    let issue = |name: &str, cn: &str, san: &str| {
        openssl(
            dir,
            &[
                "req",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-keyout",
                &format!("{name}.key"),
                "-out",
                &format!("{name}.csr"),
                "-subj",
                &format!("/CN={cn}"),
            ],
        );
        std::fs::write(
            dir.join(format!("{name}.ext")),
            format!("subjectAltName={san}\n"),
        )
        .unwrap();
        openssl(
            dir,
            &[
                "x509",
                "-req",
                "-in",
                &format!("{name}.csr"),
                "-CA",
                "ca.crt",
                "-CAkey",
                "ca.key",
                "-CAcreateserial",
                "-out",
                &format!("{name}.crt"),
                "-days",
                "2",
                "-extfile",
                &format!("{name}.ext"),
            ],
        );
    };
    for host in hosts {
        issue(&format!("server-{host}"), host, &format!("DNS:{host}"));
    }
    issue("replication", "replicator", "DNS:replicator");
}

struct Run {
    id: String,
    dir: PathBuf,
    uid: String,
}

impl Run {
    fn container(&self, node: &str) -> String {
        format!("{}-{node}", self.id)
    }
    fn network(&self) -> String {
        format!("{}-net", self.id)
    }
    fn volume(&self, node: &str, what: &str) -> String {
        format!("{}-{node}-{what}", self.id)
    }

    /// The node's own mount directory: its configuration and its TLS files.
    fn mounts(&self, node: &str) -> PathBuf {
        self.dir.join(node)
    }

    fn write_node(&self, node: &str, role: &str, sync: &str, primary: &str) {
        let dir = self.mounts(node);
        let tls = dir.join("tls");
        std::fs::create_dir_all(&tls).unwrap();
        let host = self.container(node);
        for (from, to) in [
            (format!("server-{host}.crt"), "server.crt"),
            (format!("server-{host}.key"), "server.key"),
            ("ca.crt".to_string(), "ca.crt"),
            ("replication.crt".to_string(), "replication.crt"),
            ("replication.key".to_string(), "replication.key"),
        ] {
            std::fs::copy(self.dir.join(from), tls.join(to)).unwrap();
        }
        let config = format!(
            r#"{{"tenant":"ten_test","node":"{node}","role":"{role}","postgres_version":16,"primary_address":"{primary}","synchronous_standby_names":"{sync}","secret_id":"{SECRET_ID}"}}"#
        );
        std::fs::write(dir.join("node.json"), config).unwrap();
        make_world_readable(&dir);
    }

    /// The flags every node runs with: no privilege, nothing writable but its volumes.
    fn run_args(&self, node: &str, name: &str, detached: bool) -> Vec<String> {
        let dir = self.mounts(node);
        let mut args: Vec<String> = vec!["run".into()];
        args.push(if detached { "-d".into() } else { "--rm".into() });
        let rest = [
            "--name",
            name,
            "--network",
            &self.network(),
            "--read-only",
            "--cap-drop",
            "ALL",
            "--security-opt",
            "no-new-privileges",
            "--tmpfs",
            &format!(
                "/run/ledger:rw,mode=0700,uid={uid},gid={uid}",
                uid = self.uid
            ),
            "--tmpfs",
            "/tmp:rw,mode=1777",
            "-v",
            &format!("{}:/var/lib/postgresql/data", self.volume(node, "data")),
            "-v",
            &format!("{}:/secrets/out", self.volume(node, "secrets")),
            "-v",
            &format!(
                "{}:/etc/ledger/node.json:ro",
                dir.join("node.json").display()
            ),
            "-v",
            &format!("{}:/run/secrets/tls:ro", dir.join("tls").display()),
        ];
        args.extend(rest.iter().map(|s| s.to_string()));
        args
    }

    fn start(&self, node: &str) {
        let name = self.container(node);
        let mut args = self.run_args(node, &name, true);
        args.push(image());
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        docker_ok(&refs);
    }

    fn psql(&self, node: &str, password: &str, sql: &str) -> Output {
        docker(&[
            "exec",
            "-e",
            &format!("PGPASSWORD={password}"),
            &self.container(node),
            "psql",
            "-X",
            "-At",
            "-h",
            "/run/ledger",
            "-U",
            "ledger_admin",
            "-d",
            "postgres",
            "-c",
            sql,
        ])
    }

    fn query(&self, node: &str, password: &str, sql: &str) -> String {
        let out = self.psql(node, password, sql);
        assert!(
            out.status.success(),
            "{sql}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn ready(&self, node: &str) -> bool {
        docker(&[
            "exec",
            &self.container(node),
            "pg_isready",
            "-h",
            "/run/ledger",
            "-q",
        ])
        .status
        .success()
    }

    fn quorum(&self, password: &str, want: &str) -> bool {
        let out = self.psql(
            "n1",
            password,
            "select count(*) from pg_stat_replication where sync_state = 'quorum'",
        );
        out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == want
    }
}

fn make_world_readable(dir: &Path) {
    // The containers run as a different user from the one running the test.
    let status = Command::new("chmod")
        .args(["-R", "a+rX", &dir.display().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
}

impl Drop for Run {
    fn drop(&mut self) {
        let names: Vec<String> = ["n1", "n2", "n3"]
            .iter()
            .map(|n| self.container(n))
            .collect();
        for name in &names {
            docker(&["rm", "-f", name]);
        }
        for n in ["n1", "n2", "n3"] {
            docker(&[
                "volume",
                "rm",
                "-f",
                &self.volume(n, "data"),
                &self.volume(n, "secrets"),
            ]);
        }
        docker(&["network", "rm", &self.network()]);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
#[ignore = "needs Docker and the image; run by the image workflow"]
fn three_containers_commit_with_a_quorum_and_keep_their_data() {
    let id = format!("ledger{:x}", std::process::id());
    let dir = std::env::temp_dir().join(&id);
    std::fs::create_dir_all(&dir).unwrap();
    let uid = docker_ok(&["run", "--rm", "--entrypoint", "id", &image(), "-u"]);
    let run = Run { id, dir, uid };

    // The image runs as a named non-root user.
    let user = docker_ok(&["image", "inspect", "-f", "{{.Config.User}}", &image()]);
    assert!(
        !user.is_empty() && user != "root" && user != "0",
        "image user is {user:?}"
    );
    assert_ne!(run.uid, "0");

    let hosts: Vec<String> = ["n1", "n2", "n3"]
        .iter()
        .map(|n| run.container(n))
        .collect();
    make_certificates(&run.dir, &hosts);
    // A network with no route out: the nodes must work without the internet.
    docker_ok(&["network", "create", "--internal", &run.network()]);

    let primary = run.container("n1");
    run.write_node("n1", "primary", "ANY 1 (n2, n3)", &primary);
    run.write_node("n2", "standby", "", &primary);
    run.write_node("n3", "standby", "", &primary);
    run.start("n1");
    wait_for("the primary", 120, || run.ready("n1"));
    run.start("n2");
    run.start("n3");

    let password = docker_ok(&[
        "exec",
        &primary,
        "cat",
        &format!("/secrets/out/{}", FileStore::file_name(SECRET_ID)),
    ]);
    assert_eq!(password.len(), 64, "a 256-bit password in hexadecimal");
    wait_for("two standbys in the quorum", 180, || {
        run.quorum(&password, "2")
    });

    // ---- hardening ----
    assert!(
        !docker(&["exec", &primary, "touch", "/usr/local/bin/x"])
            .status
            .success(),
        "root file system is read-only"
    );
    assert_eq!(
        docker_ok(&["inspect", "-f", "{{.HostConfig.Privileged}}", &primary]),
        "false"
    );
    let internet = docker(&[
        "exec",
        &primary,
        "bash",
        "-c",
        "timeout 3 bash -c 'echo > /dev/tcp/1.1.1.1/53'",
    ]);
    assert!(
        !internet.status.success(),
        "a node must have no route to the internet"
    );

    // ---- the cloud secret stores can work here: the agent's TLS library and the system's
    // trusted certificates are in the image ----
    let linked = docker_ok(&["exec", &primary, "ldd", "/usr/local/bin/ledger-node"]);
    assert!(
        linked.contains("libssl"),
        "the agent uses the system's OpenSSL"
    );
    assert!(
        !linked.contains("not found"),
        "every library the agent needs is in the image: {linked}"
    );
    assert!(
        docker(&[
            "exec",
            &primary,
            "test",
            "-s",
            "/etc/ssl/certs/ca-certificates.crt"
        ])
        .status
        .success(),
        "the system's trusted certificates are in the image"
    );

    // ---- what runs ----
    assert_eq!(
        run.query("n1", &password, "show server_version_num")
            .get(..2),
        Some("16")
    );
    assert_eq!(
        run.query("n1", &password, "select count(*) from pg_replication_slots"),
        "0",
        "no slots, so no logical slots"
    );
    assert_eq!(run.query("n1", &password, "show wal_level"), "replica");
    assert_eq!(
        run.query(
            "n1",
            &password,
            "select string_agg(extname, ',') from pg_extension"
        ),
        "plpgsql",
        "no extensions"
    );
    assert_eq!(
        run.query(
            "n1",
            &password,
            "select version from pg_stat_ssl where pid = pg_backend_pid()"
        ),
        ""
    );
    assert_eq!(run.query("n1", &password, "select bool_and(ssl) and bool_and(version = 'TLSv1.3') from pg_stat_ssl s join pg_stat_replication r using (pid)"), "t", "replication uses TLS 1.3");

    // ---- commits ----
    run.query("n1", &password, "create table t (i int)");
    run.query("n1", &password, "insert into t values (1)");
    wait_for("the row on a standby", 30, || {
        run.query("n2", &password, "select count(*) from t") == "1"
    });

    // One standby stopped: commits still return.
    docker_ok(&["stop", &run.container("n3")]);
    let committed = docker(&[
        "exec",
        "-e",
        &format!("PGPASSWORD={password}"),
        &primary,
        "timeout",
        "30",
        "psql",
        "-X",
        "-At",
        "-h",
        "/run/ledger",
        "-U",
        "ledger_admin",
        "-d",
        "postgres",
        "-c",
        "insert into t values (2)",
    ]);
    assert!(
        committed.status.success(),
        "a commit must return with one standby left"
    );

    // Both stopped: a commit waits. The row is not acknowledged until a standby returns.
    docker_ok(&["stop", &run.container("n2")]);
    let waited = docker(&[
        "exec",
        "-e",
        &format!("PGPASSWORD={password}"),
        &primary,
        "timeout",
        "5",
        "psql",
        "-X",
        "-At",
        "-h",
        "/run/ledger",
        "-U",
        "ledger_admin",
        "-d",
        "postgres",
        "-c",
        "insert into t values (3)",
    ]);
    assert_eq!(
        waited.status.code(),
        Some(124),
        "a commit must wait with no standby left"
    );

    // ---- a node in the wrong role is refused ----
    let name = format!("{}-wrong-role", run.id);
    std::fs::write(
        run.mounts("n2").join("node.json"),
        format!(r#"{{"tenant":"ten_test","node":"n2","role":"primary","postgres_version":16,"primary_address":"{primary}","synchronous_standby_names":"ANY 1 (n1)","secret_id":"{SECRET_ID}"}}"#),
    )
    .unwrap();
    make_world_readable(&run.mounts("n2"));
    let mut args = run.run_args("n2", &name, false);
    args.push(image());
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let refused = docker(&refs);
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("data is a standby"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    run.write_node("n2", "standby", "", &primary);

    // ---- standbys return: the waiting commit completes and everything agrees ----
    docker_ok(&["start", &run.container("n2")]);
    docker_ok(&["start", &run.container("n3")]);
    wait_for("both standbys to rejoin", 180, || {
        run.quorum(&password, "2")
    });
    wait_for("the standbys to catch up", 60, || {
        ["n2", "n3"]
            .iter()
            .all(|n| run.query(n, &password, "select count(*) from t") == "3")
    });

    // ---- every node restarted: the data survives ----
    for node in ["n1", "n2", "n3"] {
        docker_ok(&["restart", &run.container(node)]);
    }
    wait_for("the primary after a restart", 120, || run.ready("n1"));
    wait_for("the quorum after a restart", 180, || {
        run.quorum(&password, "2")
    });
    for node in ["n1", "n2", "n3"] {
        wait_for("the data after a restart", 60, || {
            run.psql(node, &password, "select count(*) from t").stdout == b"3\n"
        });
    }

    // ---- the password never reached a log ----
    for node in ["n1", "n2", "n3"] {
        let logs = docker(&["logs", &run.container(node)]);
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&logs.stdout),
            String::from_utf8_lossy(&logs.stderr)
        );
        assert!(
            !text.contains(&password),
            "{node} logged the administrator's password"
        );
    }
}
