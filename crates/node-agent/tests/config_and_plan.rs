use std::path::PathBuf;

use node_agent::config::{ConfigError, NodeConfig, Role, parse_sync_names};
use node_agent::plan::{self, DataState, Paths};

const PRIMARY: &str = r#"{
  "tenant": "ten_acme", "node": "cell-a-n1", "role": "primary", "postgres_version": 16,
  "primary_address": "10.10.4.10", "synchronous_standby_names": "ANY 1 (cell-a-n2, cell-a-n3)",
  "secret_id": "arn:aws:secretsmanager:eu-west-2:111111111111:secret:cell-a/administrator"
}"#;
const STANDBY: &str = r#"{
  "tenant": "ten_acme", "node": "cell-a-n2", "role": "standby", "postgres_version": 16,
  "primary_address": "10.10.4.10", "synchronous_standby_names": "",
  "secret_id": "arn:aws:secretsmanager:eu-west-2:111111111111:secret:cell-a/administrator"
}"#;

fn paths() -> Paths {
    Paths {
        bin: PathBuf::from("/usr/lib/postgresql/16/bin"),
        data: PathBuf::from("/var/lib/postgresql/data/pgdata"),
        run: PathBuf::from("/run/ledger"),
        tls_in: PathBuf::from("/run/secrets/tls"),
    }
}

fn arg<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.windows(2)
        .find(|w| w[0] == "-c" && w[1].starts_with(&format!("{name}=")))
        .map(|w| &w[1][name.len() + 1..])
}

fn invalid_field(text: &str) -> String {
    match NodeConfig::from_json(text) {
        Err(ConfigError::Invalid { field, .. }) => field.to_string(),
        Err(e) => format!("other: {e}"),
        Ok(_) => "valid".to_string(),
    }
}

// ---- configuration ---------------------------------------------------------------------------

#[test]
fn the_configurations_the_infrastructure_modules_write_are_accepted() {
    assert_eq!(NodeConfig::from_json(PRIMARY).unwrap().role, Role::Primary);
    assert_eq!(NodeConfig::from_json(STANDBY).unwrap().role, Role::Standby);
}

#[test]
fn unknown_fields_are_refused_so_a_typo_cannot_weaken_a_setting() {
    let text = PRIMARY.replace("\"role\"", "\"rol\": 1, \"role\"");
    assert!(matches!(
        NodeConfig::from_json(&text),
        Err(ConfigError::Parse(_))
    ));
}

#[test]
fn only_postgres_16_is_accepted() {
    assert_eq!(
        invalid_field(&PRIMARY.replace("16", "15")),
        "postgres_version"
    );
}

#[test]
fn a_primary_must_name_standbys_and_a_standby_must_not() {
    assert_eq!(
        invalid_field(&PRIMARY.replace("ANY 1 (cell-a-n2, cell-a-n3)", "")),
        "synchronous_standby_names"
    );
    assert_eq!(
        invalid_field(&STANDBY.replace(
            "\"synchronous_standby_names\": \"\"",
            "\"synchronous_standby_names\": \"ANY 1 (x)\""
        )),
        "synchronous_standby_names"
    );
}

#[test]
fn a_primary_cannot_wait_for_itself_or_for_more_standbys_than_it_has() {
    assert_eq!(
        invalid_field(&PRIMARY.replace("cell-a-n2, cell-a-n3", "cell-a-n1, cell-a-n3")),
        "synchronous_standby_names"
    );
    assert_eq!(
        invalid_field(&PRIMARY.replace("ANY 1", "ANY 3")),
        "synchronous_standby_names"
    );
    assert_eq!(
        invalid_field(&PRIMARY.replace("ANY 1", "ANY 0")),
        "synchronous_standby_names"
    );
}

#[test]
fn sync_names_must_be_any_k_of_valid_names() {
    assert_eq!(
        parse_sync_names("ANY 1 (a, b)").unwrap(),
        (1, vec!["a".to_string(), "b".to_string()])
    );
    for bad in [
        "FIRST 1 (a)",
        "ANY 1 a",
        "ANY x (a)",
        "ANY 1 (a; drop)",
        "ANY 1 (A)",
        "ANY 1 ()",
        "1 (a)",
    ] {
        assert!(parse_sync_names(bad).is_err(), "{bad}");
    }
}

#[test]
fn identifiers_are_checked() {
    assert_eq!(
        invalid_field(&PRIMARY.replace("ten_acme", "acme")),
        "tenant"
    );
    assert_eq!(
        invalid_field(&PRIMARY.replace("cell-a-n1", "Cell A")),
        "node"
    );
    assert_eq!(
        invalid_field(&PRIMARY.replace("10.10.4.10", "host name; rm")),
        "primary_address"
    );
    assert_eq!(invalid_field(&PRIMARY.replace("\"secret_id\": \"arn:aws:secretsmanager:eu-west-2:111111111111:secret:cell-a/administrator\"", "\"secret_id\": \"\"")), "secret_id");
}

// ---- what the node runs ----------------------------------------------------------------------

#[test]
fn every_node_is_tls_1_3_only_with_no_slots_and_scram_passwords() {
    for text in [PRIMARY, STANDBY] {
        let args = plan::server_args(&NodeConfig::from_json(text).unwrap(), &paths());
        assert_eq!(arg(&args, "ssl"), Some("on"));
        assert_eq!(arg(&args, "ssl_min_protocol_version"), Some("TLSv1.3"));
        assert_eq!(arg(&args, "password_encryption"), Some("scram-sha-256"));
        assert_eq!(
            arg(&args, "max_replication_slots"),
            Some("0"),
            "no slots, so no logical slots"
        );
        assert_eq!(
            arg(&args, "wal_level"),
            Some("replica"),
            "logical decoding is not available"
        );
        assert_eq!(arg(&args, "synchronous_commit"), Some("on"));
        assert_eq!(arg(&args, "hba_file"), Some("/run/ledger/pg_hba.conf"));
    }
}

#[test]
fn the_primary_waits_for_a_standby_and_a_standby_follows_the_primary_by_certificate() {
    let primary = plan::server_args(&NodeConfig::from_json(PRIMARY).unwrap(), &paths());
    assert_eq!(
        arg(&primary, "synchronous_standby_names"),
        Some("ANY 1 (\"cell-a-n2\", \"cell-a-n3\")")
    );
    assert_eq!(arg(&primary, "primary_conninfo"), None);

    let standby = plan::server_args(&NodeConfig::from_json(STANDBY).unwrap(), &paths());
    assert_eq!(arg(&standby, "synchronous_standby_names"), None);
    let info = arg(&standby, "primary_conninfo").unwrap();
    assert!(info.contains("sslmode=verify-full"));
    assert!(info.contains("application_name=cell-a-n2"));
    assert!(info.contains("user=replicator"));
    assert!(info.contains("sslcert=/run/ledger/tls/replication.crt"));
    assert!(!info.contains("password"));
}

#[test]
fn nothing_secret_is_in_the_command_line() {
    for text in [PRIMARY, STANDBY] {
        let config = NodeConfig::from_json(text).unwrap();
        let all = plan::server_args(&config, &paths()).join(" ")
            + &plan::basebackup_args(&config, &paths()).join(" ");
        assert!(!all.to_lowercase().contains("password") || all.contains("password_encryption"));
        assert!(!all.contains(&config.secret_id));
    }
}

#[test]
fn access_rules_need_tls_and_certificates_for_replication() {
    let hba = plan::hba();
    let rules: Vec<&str> = hba.lines().filter(|l| !l.starts_with('#')).collect();
    assert!(
        rules
            .iter()
            .all(|l| !l.starts_with("host ") && !l.starts_with("hostnossl")),
        "plain connections are never allowed"
    );
    assert!(
        rules
            .iter()
            .any(|l| l.starts_with("hostssl replication") && l.trim_end().ends_with("cert"))
    );
    assert!(!hba.contains("trust"));
    assert!(!hba.contains("md5"));
    assert!(
        rules
            .iter()
            .any(|l| l.starts_with("local") && l.contains("reject"))
    );
}

#[test]
fn a_standby_is_copied_without_a_slot_and_with_verified_tls() {
    let config = NodeConfig::from_json(STANDBY).unwrap();
    let args = plan::basebackup_args(&config, &paths());
    assert!(args.contains(&"--no-slot".to_string()));
    assert!(args.contains(&"--no-password".to_string()));
    let env = plan::basebackup_env(&paths());
    assert!(env.contains(&("PGSSLMODE".to_string(), "verify-full".to_string())));
}

#[test]
fn the_bootstrap_statements_make_the_two_roles_and_nothing_else() {
    let sql = plan::bootstrap_sql("abc123");
    assert_eq!(sql.lines().count(), 2);
    assert!(sql.contains("ALTER ROLE ledger_admin WITH PASSWORD 'abc123'"));
    assert!(sql.contains("CREATE ROLE replicator WITH LOGIN REPLICATION"));
    assert!(!sql.to_lowercase().contains("extension"));
    assert!(!sql.to_lowercase().contains("slot"));
}

#[test]
fn the_tls_files_a_node_needs_depend_on_its_role() {
    assert!(!plan::tls_files(Role::Primary).contains(&"replication.key"));
    assert!(plan::tls_files(Role::Standby).contains(&"replication.key"));
}

// ---- data and role ---------------------------------------------------------------------------

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("node-agent-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn the_data_directory_is_classified() {
    let dir = temp("state");
    assert_eq!(plan::data_state(&dir).unwrap(), DataState::Empty);
    std::fs::write(dir.join("stray"), "x").unwrap();
    assert!(
        plan::data_state(&dir).is_err(),
        "never initialise over unknown files"
    );
    std::fs::remove_file(dir.join("stray")).unwrap();
    std::fs::write(dir.join("PG_VERSION"), "15\n").unwrap();
    assert!(plan::data_state(&dir).is_err(), "wrong major version");
    std::fs::write(dir.join("PG_VERSION"), "16\n").unwrap();
    assert_eq!(plan::data_state(&dir).unwrap(), DataState::Primary);
    std::fs::write(dir.join("standby.signal"), "").unwrap();
    assert_eq!(plan::data_state(&dir).unwrap(), DataState::Standby);
}

#[test]
fn a_node_refuses_to_start_in_the_wrong_role() {
    let primary = NodeConfig::from_json(PRIMARY).unwrap();
    let standby = NodeConfig::from_json(STANDBY).unwrap();
    assert!(plan::check_role(&primary, &DataState::Empty).is_ok());
    assert!(plan::check_role(&primary, &DataState::Primary).is_ok());
    assert!(plan::check_role(&standby, &DataState::Standby).is_ok());
    assert!(plan::check_role(&primary, &DataState::Standby).is_err());
    let promoted = plan::check_role(&standby, &DataState::Primary).unwrap_err();
    assert!(promoted.contains("promotion"), "{promoted}");
}

#[test]
fn a_standby_uses_the_primary_port_when_given() {
    let text = STANDBY.replace(
        "\"role\"",
        "\"primary_port\": 6000, \"port\": 6001, \"role\"",
    );
    let config = NodeConfig::from_json(&text).unwrap();
    let args = plan::server_args(&config, &paths());
    assert_eq!(arg(&args, "port"), Some("6001"));
    assert!(
        arg(&args, "primary_conninfo")
            .unwrap()
            .contains("port=6000")
    );
    assert!(
        plan::basebackup_args(&config, &paths())
            .windows(2)
            .any(|w| w[0] == "-p" && w[1] == "6000")
    );
    assert_eq!(
        invalid_field(&STANDBY.replace("\"role\"", "\"primary_port\": 0, \"role\"")),
        "port"
    );
}

// ---- the deployment files feed the agent ----------------------------------------------------

fn repo(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(path)
}

#[test]
fn the_compose_configurations_are_accepted_and_agree_with_each_other() {
    let mut primaries = 0;
    let mut names = vec![];
    for n in 1..=3 {
        let text =
            std::fs::read_to_string(repo(&format!("deploy/compose/config/pg{n}.json"))).unwrap();
        let config = NodeConfig::from_json(&text).unwrap();
        names.push(config.node.clone());
        assert_eq!(config.primary_address, "pg1");
        if config.role == Role::Primary {
            primaries += 1;
            let (_, standbys) = parse_sync_names(&config.synchronous_standby_names).unwrap();
            assert_eq!(standbys, vec!["pg2".to_string(), "pg3".to_string()]);
        }
    }
    assert_eq!(primaries, 1);
    assert_eq!(names, ["pg1", "pg2", "pg3"]);
}

#[test]
fn the_kubernetes_init_script_writes_a_configuration_the_agent_accepts_for_every_pod_and_primary() {
    let script = repo("deploy/kubernetes/render-config.sh");
    for primary in 0..3 {
        let mut primaries = vec![];
        for ordinal in 0..3 {
            let out = std::env::temp_dir().join(format!(
                "node-agent-render-{}-{primary}-{ordinal}.json",
                std::process::id()
            ));
            let status = std::process::Command::new("bash")
                .arg(&script)
                .env("LEDGER_CONFIG_OUT", &out)
                .env("POD_NAME", format!("ledger-pg-{ordinal}"))
                .env("POD_NAMESPACE", "ledger")
                .env("SERVICE_NAME", "ledger-pg")
                .env("TENANT", "ten_example")
                .env("SECRET_ID", "cell-a/administrator")
                .env("PRIMARY_ORDINAL", primary.to_string())
                .status()
                .unwrap();
            assert!(status.success());
            let config = NodeConfig::from_json(&std::fs::read_to_string(&out).unwrap())
                .unwrap_or_else(|e| panic!("pod {ordinal} with primary {primary}: {e}"));
            assert_eq!(
                config.primary_address,
                format!("ledger-pg-{primary}.ledger-pg.ledger.svc")
            );
            if config.role == Role::Primary {
                primaries.push(ordinal);
                let (k, standbys) = parse_sync_names(&config.synchronous_standby_names).unwrap();
                assert_eq!(k, 1);
                assert_eq!(standbys.len(), 2);
                assert!(!standbys.contains(&config.node));
            }
        }
        assert_eq!(
            primaries,
            vec![primary],
            "exactly the chosen pod is the primary"
        );
    }
}

#[test]
fn the_kubernetes_init_script_refuses_a_primary_that_does_not_exist() {
    let out =
        std::env::temp_dir().join(format!("node-agent-render-bad-{}.json", std::process::id()));
    let status = std::process::Command::new("bash")
        .arg(repo("deploy/kubernetes/render-config.sh"))
        .env("LEDGER_CONFIG_OUT", &out)
        .env("POD_NAME", "ledger-pg-0")
        .env("POD_NAMESPACE", "ledger")
        .env("SERVICE_NAME", "ledger-pg")
        .env("TENANT", "ten_example")
        .env("SECRET_ID", "x")
        .env("PRIMARY_ORDINAL", "3")
        .status()
        .unwrap();
    assert!(!status.success());
}

#[test]
fn every_standby_name_is_quoted_because_a_hyphen_is_not_valid_in_an_identifier() {
    // PostgreSQL refuses `ANY 1 (cell-a-n2, cell-a-n3)` with `syntax error at or near "-"`.
    assert_eq!(
        plan::standby_list("ANY 1 (cell-a-n2, cell-a-n3)"),
        "ANY 1 (\"cell-a-n2\", \"cell-a-n3\")"
    );
    assert_eq!(
        plan::standby_list("ANY 2 (n2, n3, n4)"),
        "ANY 2 (\"n2\", \"n3\", \"n4\")"
    );
    // What the configuration check refuses is passed on unchanged, and is never quoted.
    assert_eq!(plan::standby_list(""), "");
}
