//! One rotation: new certificates, new cell credentials, or both, for every cell in the manifest.
//!
//! Everything is built in memory first, for every cell, so nothing is written unless all of it
//! could be made. It is then written in steps. A cell calls another with a credential the other
//! side must already list, so the lists go first, then the calls switch, then the old
//! credentials go. A wait sits between steps, long enough for the machines to read their secrets
//! again and the services to read their files. A step that fails stops the run, and what was
//! written before it still works: every step leaves a state that is valid on its own, and
//! running again finishes the job.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use argon2::{Argon2, PasswordHasher};
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa,
    KeyPair, KeyUsagePurpose, SanType,
};
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::{Value, json};
use time::OffsetDateTime;

use crate::database::DatabaseSource;
use crate::error::PkiError;
use crate::manifest::{CellEntry, Cloud, Database, Manifest, Node, NodeRole};
use crate::signer::{Signer, key_pair};
use crate::store::SecretStore;

/// Certificates last this long.
pub const CERTIFICATE_DAYS: i64 = 30;

/// What to make new.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Certificates and cell credentials. The first run for a cell must be this one.
    All,
    Certificates,
    /// Cell credentials only, as on demand when a cell is removed.
    Credentials,
}

impl Kind {
    fn certificates(self) -> bool {
        matches!(self, Kind::All | Kind::Certificates)
    }
    fn credentials(self) -> bool {
        matches!(self, Kind::All | Kind::Credentials)
    }
}

/// What a run did, as numbers.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub cells: usize,
    pub certificates: usize,
    /// Certificates made for the machines of self-hosted databases: a server certificate for each,
    /// and a replication certificate for each standby.
    pub node_certificates: usize,
    pub credentials: usize,
    pub versions: usize,
    /// Database connections and token files this run made, because a proving environment's secret
    /// had none. A person's own entries are never counted, and never replaced.
    pub databases: usize,
    pub tokens: usize,
    /// Test users made for the basic UI, because a proving environment's secret had no user list.
    pub users: usize,
    /// Cells whose secret still lacks a file the service needs, such as the database connection.
    pub incomplete: usize,
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "cells: {}, certificates issued: {}, database node certificates issued: {}, credentials issued: {}, secret versions written: {}, database connections made: {}, login files made: {}, test users made: {}, cells still missing a file: {}",
            self.cells,
            self.certificates,
            self.node_certificates,
            self.credentials,
            self.versions,
            self.databases,
            self.tokens,
            self.users,
            self.incomplete
        )
    }
}

type Entries = BTreeMap<String, String>;

/// The entries the service needs in its secret, in the order the API module lists them.
pub const SECRET_FILES: [&str; 10] = [
    "db.conn",
    "tokens.json",
    "internal-auth.json",
    "cells.json",
    "server.crt",
    "server.key",
    "ca.crt",
    "etcd-ca.crt",
    "etcd-client.crt",
    "etcd-client.key",
];

/// Entries the service reads when they are there and does without when they are not. A rotation
/// writes one only when the manifest gives it, and never removes one.
pub const OPTIONAL_FILES: [&str; 2] = ["db-ca.crt", "users.json"];

pub struct Rotation<'a> {
    pub manifest: &'a Manifest,
    pub signer: Arc<dyn Signer>,
    pub store: &'a dyn SecretStore,
    pub now: OffsetDateTime,
    /// How long to wait between steps.
    pub settle: Duration,
    pub sleep: &'a (dyn Fn(Duration) + Sync),
    /// Where a missing `db.conn` comes from. Used only when the manifest says `proving`.
    pub database: Option<&'a dyn DatabaseSource>,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The tokens file for a proving environment: one fresh token for each tenant. Nothing else knows
/// a token, and it is never printed.
fn proving_tokens(tenants: &[String]) -> Result<String, PkiError> {
    let list: Vec<Value> = tenants
        .iter()
        .map(|tenant| Ok(json!({"token": token()?, "tenant": tenant, "principal": "usr_proving"})))
        .collect::<Result<_, PkiError>>()?;
    Ok(Value::Array(list).to_string())
}

/// The test user of a proving environment (D-21): a fake domain, so nothing real can be mistaken
/// for it. Its password is made fresh for each cell and kept in the cell's secret as `ui-test-password`,
/// which the machines do not read, so a person can. Only its Argon2id hash goes in `users.json`.
pub const TEST_USER: &str = "user@bankofmumanddad.test";
const TEST_PRINCIPAL: &str = "usr_uitest";
pub const TEST_PASSWORD_ENTRY: &str = "ui-test-password";

/// The user list and the password it was made from. Neither is ever printed.
fn proving_user(tenant: &str) -> Result<(String, String), PkiError> {
    let mut raw = [0u8; 16];
    SystemRandom::new()
        .fill(&mut raw)
        .map_err(|_| PkiError::Signer("no randomness"))?;
    let password = hex(&raw);
    let mut salt = [0u8; 16];
    SystemRandom::new()
        .fill(&mut salt)
        .map_err(|_| PkiError::Signer("no randomness"))?;
    let hash = Argon2::default()
        .hash_password_with_salt(password.as_bytes(), &salt)
        .map_err(|_| PkiError::Signer("a password could not be hashed"))?
        .to_string();
    let list = json!([{
        "username": TEST_USER,
        "password_hash": hash,
        "tenant": tenant,
        "principal": TEST_PRINCIPAL,
    }]);
    Ok((list.to_string(), password))
}

fn token() -> Result<String, PkiError> {
    let mut raw = [0u8; 32];
    SystemRandom::new()
        .fill(&mut raw)
        .map_err(|_| PkiError::Signer("no randomness"))?;
    Ok(hex(&raw))
}

fn authority_params() -> CertificateParams {
    let mut p = CertificateParams::default();
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, "Ledger authority");
    p.distinguished_name = dn;
    p.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    p.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    // Fixed, so every run builds the same issuer name and key identifier.
    p.not_before = rcgen::date_time_ymd(2026, 1, 1);
    p.not_after = rcgen::date_time_ymd(2046, 1, 1);
    p
}

/// Whether the stored certificate carries this public key, so a replaced KMS key is noticed before
/// every cell is given certificates nobody trusts.
fn certificate_has_key(pem: &str, point: &[u8]) -> bool {
    use base64::Engine;
    let body: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
    base64::engine::general_purpose::STANDARD
        .decode(body.trim())
        .is_ok_and(|der| der.windows(point.len()).any(|w| w == point))
}

struct Issued {
    server: (String, String),
    etcd: (String, String),
}

impl<'a> Rotation<'a> {
    fn authority(&self) -> Result<(rcgen::Certificate, KeyPair, String), PkiError> {
        let key = key_pair(self.signer.clone())?;
        let cert = authority_params()
            .self_signed(&key)
            .map_err(|_| PkiError::Issuing("the authority could not sign"))?;
        let a = &self.manifest.authority;
        let stored = self.store.read(Cloud::Aws, &a.certificate_secret)?;
        let pem = match stored.filter(|s| !s.trim().is_empty()) {
            Some(pem) => {
                if !certificate_has_key(&pem, self.signer.public_key()) {
                    return Err(PkiError::Issuing(
                        "the stored authority certificate is not for the signing key",
                    ));
                }
                pem
            }
            None => {
                let pem = cert.pem();
                self.store.write(Cloud::Aws, &a.certificate_secret, &pem)?;
                pem
            }
        };
        Ok((cert, key, pem))
    }

    /// The five files of one database node, as the node agent lists them for its role: a server
    /// certificate that names the primary's address and the node's own, the authority's
    /// certificate, and for a standby a client certificate whose common name is `replicator`.
    fn issue_node(
        &self,
        db: &Database,
        node: &Node,
        ca_pem: &str,
        issuer: &rcgen::Certificate,
        issuer_key: &KeyPair,
    ) -> Result<(Entries, usize), PkiError> {
        let ip = |a: &str| {
            a.parse::<std::net::IpAddr>()
                .map_err(|_| PkiError::Issuing("a node's address is not an IP address"))
        };
        let primary = db
            .nodes
            .iter()
            .find(|n| n.role == NodeRole::Primary)
            .ok_or(PkiError::Manifest("a cell has one primary node"))?;
        let mut names = vec![ip(&primary.address)?];
        if ip(&node.address)? != names[0] {
            names.push(ip(&node.address)?);
        }
        let make = |common_name: &str, usage: ExtendedKeyUsagePurpose, san: Vec<SanType>| {
            let key =
                KeyPair::generate().map_err(|_| PkiError::Issuing("a key could not be made"))?;
            let mut p = CertificateParams::default();
            let mut dn = DistinguishedName::new();
            dn.push(DnType::CommonName, common_name);
            p.distinguished_name = dn;
            p.subject_alt_names = san;
            p.extended_key_usages = vec![usage];
            p.key_usages = vec![KeyUsagePurpose::DigitalSignature];
            p.not_before = self.now - time::Duration::minutes(10);
            p.not_after = self.now + time::Duration::days(CERTIFICATE_DAYS);
            let cert = p
                .signed_by(&key, issuer, issuer_key)
                .map_err(|_| PkiError::Issuing("the authority could not sign it"))?;
            Ok::<_, PkiError>((cert.pem(), key.serialize_pem()))
        };
        let mut files = Entries::new();
        let (crt, key) = make(
            &node.name,
            ExtendedKeyUsagePurpose::ServerAuth,
            names.into_iter().map(SanType::IpAddress).collect(),
        )?;
        files.insert("server.crt".into(), crt);
        files.insert("server.key".into(), key);
        files.insert("ca.crt".into(), ca_pem.to_owned());
        let mut made = 1;
        if node.role == NodeRole::Standby {
            let (crt, key) = make("replicator", ExtendedKeyUsagePurpose::ClientAuth, vec![])?;
            files.insert("replication.crt".into(), crt);
            files.insert("replication.key".into(), key);
            made += 1;
        }
        Ok((files, made))
    }

    fn issue(
        &self,
        cell: &CellEntry,
        issuer: &rcgen::Certificate,
        issuer_key: &KeyPair,
    ) -> Result<Issued, PkiError> {
        let ip: std::net::IpAddr = cell
            .address
            .parse()
            .map_err(|_| PkiError::Issuing("a cell's address is not an IP address"))?;
        let make = |usages: Vec<ExtendedKeyUsagePurpose>, san: Vec<SanType>| {
            let key =
                KeyPair::generate().map_err(|_| PkiError::Issuing("a key could not be made"))?;
            let mut p = CertificateParams::default();
            let mut dn = DistinguishedName::new();
            dn.push(DnType::CommonName, cell.name.clone());
            p.distinguished_name = dn;
            p.subject_alt_names = san;
            p.extended_key_usages = usages;
            p.key_usages = vec![KeyUsagePurpose::DigitalSignature];
            // A little before now, so a machine whose clock is a few minutes behind accepts it.
            p.not_before = self.now - time::Duration::minutes(10);
            p.not_after = self.now + time::Duration::days(CERTIFICATE_DAYS);
            let cert = p
                .signed_by(&key, issuer, issuer_key)
                .map_err(|_| PkiError::Issuing("the authority could not sign it"))?;
            Ok::<_, PkiError>((cert.pem(), key.serialize_pem()))
        };
        Ok(Issued {
            // The server certificate is also what the cell shows when it calls another cell.
            server: make(
                vec![
                    ExtendedKeyUsagePurpose::ServerAuth,
                    ExtendedKeyUsagePurpose::ClientAuth,
                ],
                vec![SanType::IpAddress(ip)],
            )?,
            etcd: make(vec![ExtendedKeyUsagePurpose::ClientAuth], vec![])?,
        })
    }

    /// The object in a cell's secret now. A secret with no version, or a placeholder that is not
    /// a JSON object, counts as empty.
    fn current(&self, cell: &CellEntry) -> Result<Entries, PkiError> {
        let text = self
            .store
            .read(cell.cloud, &cell.secret)?
            .unwrap_or_default();
        Ok(match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(o)) => o
                .into_iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_owned())))
                .collect(),
            _ => Entries::new(),
        })
    }

    fn write(
        &self,
        cell: &CellEntry,
        entries: &Entries,
        report: &mut Report,
        step: u8,
    ) -> Result<(), PkiError> {
        let value = serde_json::to_string(entries).map_err(|_| PkiError::Stopped {
            step,
            why: "a secret could not be encoded",
        })?;
        self.store
            .write(cell.cloud, &cell.secret, &value)
            .map_err(|_| PkiError::Stopped {
                step,
                why: "a secret could not be written",
            })?;
        report.versions += 1;
        Ok(())
    }

    pub fn run(&self, kind: Kind) -> Result<Report, PkiError> {
        let cells = &self.manifest.cells;
        let mut report = Report {
            cells: cells.len(),
            ..Report::default()
        };

        // Step 0: read and build everything. Nothing about a cell is written until all of it exists.
        let mut now: Vec<Entries> = cells
            .iter()
            .map(|c| self.current(c))
            .collect::<Result<_, _>>()?;

        // A proving environment's missing entries. An entry that is there is a person's, or an
        // earlier run's, and is left as it is. Everything is made before anything is written, so
        // a credential that cannot be read stops the run with nothing changed.
        if self.manifest.proving {
            let source = self.database.ok_or(PkiError::Manifest(
                "a proving environment needs a source for the database connection",
            ))?;
            for (cell, entries) in cells.iter().zip(now.iter_mut()) {
                if entries.get("db.conn").is_none_or(String::is_empty) {
                    entries.insert("db.conn".into(), source.connection(cell)?);
                    report.databases += 1;
                }
                if entries.get("tokens.json").is_none_or(String::is_empty) {
                    entries.insert(
                        "tokens.json".into(),
                        proving_tokens(&self.manifest.tenants)?,
                    );
                    report.tokens += 1;
                }
                // The test user for the basic UI. A list that is there is a person's, or an
                // earlier run's, and is kept with its password.
                if entries.get("users.json").is_none_or(String::is_empty) {
                    let (list, password) = proving_user(&self.manifest.tenants[0])?;
                    entries.insert("users.json".into(), list);
                    entries.insert(TEST_PASSWORD_ENTRY.into(), password);
                    report.users += 1;
                }
            }
        }

        let (issuer, issuer_key, ca_pem) = self.authority()?;

        if kind.certificates() {
            for (cell, entries) in cells.iter().zip(now.iter_mut()) {
                let issued = self.issue(cell, &issuer, &issuer_key)?;
                entries.insert("server.crt".into(), issued.server.0);
                entries.insert("server.key".into(), issued.server.1);
                entries.insert("etcd-client.crt".into(), issued.etcd.0);
                entries.insert("etcd-client.key".into(), issued.etcd.1);
                entries.insert("ca.crt".into(), ca_pem.clone());
                // One authority signs both what the cells present and what etcd presents.
                entries.insert("etcd-ca.crt".into(), ca_pem.clone());
                report.certificates += 2;
            }
        }

        // The certificates of each self-hosted cell's database machines, made now with everything
        // else, and written first in step 1, so a machine that has been waiting for its files can start. The service
        // trusts the same authority for the database, so its secret gets that certificate as the
        // database's, unless the manifest gives another.
        let mut node_secrets: Vec<(Cloud, &str, String)> = Vec::new();
        if kind.certificates() {
            for (cell, entries) in cells.iter().zip(now.iter_mut()) {
                let Some(db) = &cell.database else { continue };
                if db.nodes.is_empty() {
                    continue;
                }
                for node in &db.nodes {
                    let (files, made) = self.issue_node(db, node, &ca_pem, &issuer, &issuer_key)?;
                    let text = serde_json::to_string(&files).map_err(|_| PkiError::Stopped {
                        step: 1,
                        why: "a secret could not be encoded",
                    })?;
                    node_secrets.push((cell.cloud, node.secret.as_str(), text));
                    report.node_certificates += made;
                }
                entries.insert("db-ca.crt".into(), ca_pem.clone());
            }
        }
        // The authority of each cell's database, when the manifest has it. Public, and not tied to
        // either kind of run, so it is written whenever it is given.
        for (cell, entries) in cells.iter().zip(now.iter_mut()) {
            if let Some(ca) = &cell.db_ca {
                entries.insert("db-ca.crt".into(), ca.clone());
            }
        }

        // For each callee, the credential each caller will use, and what it lists today.
        let tenants = &self.manifest.tenants;
        let mut accept_now: Vec<Value> = Vec::new();
        let mut accept_later: Vec<Value> = Vec::new();
        let mut calls: Vec<Value> = Vec::new();
        if kind.credentials() {
            let fresh: Vec<Vec<String>> = (0..cells.len())
                .map(|_| (0..cells.len()).map(|_| token()).collect::<Result<_, _>>())
                .collect::<Result<_, _>>()?;
            for (callee_i, callee) in cells.iter().enumerate() {
                let listed: Vec<Value> = serde_json::from_str(
                    now[callee_i]
                        .get("internal-auth.json")
                        .map_or("[]", String::as_str),
                )
                .unwrap_or_default();
                let mut later = Vec::new();
                let mut with_old = Vec::new();
                for (caller_i, caller) in cells.iter().enumerate() {
                    if caller_i == callee_i {
                        continue;
                    }
                    let entry =
                        |t: &str| json!({"token": t, "cell": caller.name, "tenants": tenants});
                    let new = &fresh[caller_i][callee_i];
                    later.push(entry(new));
                    with_old.push(entry(new));
                    // What this callee lists for this caller today stays until the last step, but
                    // only for a caller that is still in the manifest: a cell that is gone stops
                    // being accepted with the first step.
                    for old in listed.iter().filter(|o| o["cell"] == caller.name.as_str()) {
                        if let Some(t) = old["token"].as_str() {
                            with_old.push(entry(t));
                        }
                    }
                }
                accept_later.push(Value::Array(later));
                accept_now.push(Value::Array(with_old));
                report.credentials += cells.len() - 1;
                let _ = callee;
            }
            for (caller_i, _) in cells.iter().enumerate() {
                let mut o = serde_json::Map::new();
                for (callee_i, callee) in cells.iter().enumerate() {
                    if callee_i != caller_i {
                        o.insert(
                            callee.name.clone(),
                            json!({
                                "url": format!("https://{}", socket(&callee.address, callee.internal_port)),
                                "token": fresh[caller_i][callee_i],
                            }),
                        );
                    }
                }
                calls.push(Value::Object(o));
            }
        }

        // Step 1: the database machines' certificates first, so a machine that has been waiting for
        // its files can start, and then the cells' own.
        for (cloud, secret, text) in &node_secrets {
            self.store
                .write(*cloud, secret, text)
                .map_err(|_| PkiError::Stopped {
                    step: 1,
                    why: "a database node's certificates could not be written",
                })?;
            report.versions += 1;
        }
        // Every cell gets new certificates and lists the new credentials as well as the old.
        for (i, cell) in cells.iter().enumerate() {
            if kind.credentials() {
                now[i].insert("internal-auth.json".into(), accept_now[i].to_string());
            }
            self.write(cell, &now[i], &mut report, 1)?;
        }

        if kind.credentials() && cells.len() > 1 {
            (self.sleep)(self.settle);
            // Step 2: each cell starts calling the others with its new credentials.
            for (i, cell) in cells.iter().enumerate() {
                now[i].insert("cells.json".into(), calls[i].to_string());
                self.write(cell, &now[i], &mut report, 2)?;
            }
            (self.sleep)(self.settle);
            // Step 3: the old credentials are no longer listed.
            for (i, cell) in cells.iter().enumerate() {
                now[i].insert("internal-auth.json".into(), accept_later[i].to_string());
                self.write(cell, &now[i], &mut report, 3)?;
            }
        } else if kind.credentials() {
            // One cell calls nobody and is called by nobody.
            now[0].insert("cells.json".into(), "{}".into());
            now[0].insert("internal-auth.json".into(), "[]".into());
            self.write(&cells[0], &now[0], &mut report, 2)?;
        }

        report.incomplete = now
            .iter()
            .filter(|e| {
                SECRET_FILES
                    .iter()
                    .any(|f| e.get(*f).is_none_or(String::is_empty))
            })
            .count();
        Ok(report)
    }
}

fn socket(address: &str, port: u16) -> String {
    if address.contains(':') {
        format!("[{address}]:{port}")
    } else {
        format!("{address}:{port}")
    }
}
