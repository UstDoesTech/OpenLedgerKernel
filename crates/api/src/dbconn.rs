//! The service's connection to its cell's database.
//!
//! The nodes make their certificates from their own authority, so the service can be given that
//! authority's certificate (`db-ca.crt` in its secret) and trusts it as well as the system's
//! roots. The service connects once, at start. When it cannot, it says which of four things went
//! wrong and nothing else: never the host, the user, the password or the driver's own text.

use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use native_tls::{Certificate, TlsConnector};
use postgres_native_tls::MakeTlsConnector;
use tokio_postgres::config::Host;
use tokio_postgres::{Client, Config, NoTls};

use crate::dbauth::{self, SignIn, TokenSource};

/// Why a connection failed, in terms a person can act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    /// The certificate was not made by an authority the service trusts.
    Certificate,
    /// The name in the connection string is not on the certificate.
    Name,
    /// The database refused the user or password.
    SignIn,
    /// Nothing answered.
    Unreachable,
    /// Anything else.
    Other,
}

impl Cause {
    pub fn text(self) -> &'static str {
        match self {
            Cause::Certificate => {
                "the database's certificate is not from an authority this service trusts (check db-ca.crt)"
            }
            Cause::Name => "the name in db.conn is not one the database's certificate carries",
            Cause::SignIn => "the database refused the sign-in in db.conn",
            Cause::Unreachable => "nothing answered at the address in db.conn",
            Cause::Other => "the connection failed",
        }
    }
}

/// Sorts a failure by its SQLSTATE, when the database sent one, and otherwise by the words in the
/// chain of causes. The words are only read, never printed.
pub fn classify(sqlstate: Option<&str>, chain: &str) -> Cause {
    if matches!(sqlstate, Some("28P01" | "28000")) {
        return Cause::SignIn;
    }
    let c = chain.to_ascii_lowercase();
    if c.contains("hostname mismatch")
        || c.contains("ip address mismatch")
        || c.contains("not valid for")
    {
        return Cause::Name;
    }
    if c.contains("certificate verify failed")
        || c.contains("unable to get local issuer")
        || c.contains("self-signed")
        || c.contains("self signed")
        || c.contains("unknown issuer")
        || c.contains("unknownissuer")
        || c.contains("certificate has expired")
    {
        return Cause::Certificate;
    }
    if c.contains("connection refused")
        || c.contains("timed out")
        || c.contains("failed to lookup")
        || c.contains("name or service not known")
        || c.contains("no route to host")
        || c.contains("network is unreachable")
    {
        return Cause::Unreachable;
    }
    Cause::Other
}

fn cause_of(err: &tokio_postgres::Error) -> Cause {
    let mut chain = String::new();
    let mut next: Option<&dyn std::error::Error> = Some(err);
    while let Some(e) = next {
        chain.push_str(&e.to_string());
        chain.push(' ');
        next = e.source();
    }
    let code = err.as_db_error().map(|d| d.code().code().to_owned());
    classify(code.as_deref(), &chain)
}

/// Builds the TLS connector: the system's roots, plus the given authority when there is one.
pub fn connector(ca_pem: Option<&str>) -> Result<MakeTlsConnector, String> {
    let mut builder = TlsConnector::builder();
    if let Some(pem) = ca_pem {
        let cert = Certificate::from_pem(pem.as_bytes())
            .map_err(|_| "db-ca.crt: not a certificate".to_owned())?;
        builder.add_root_certificate(cert);
    }
    let connector = builder
        .build()
        .map_err(|_| "cannot set up TLS for the database".to_owned())?;
    Ok(MakeTlsConnector::new(connector))
}

/// Connects to the cell's database. `tls` is on except in tests.
pub async fn connect(conn: &str, tls: bool, ca_pem: Option<&str>) -> Result<Client, String> {
    let config: Config = conn
        .parse()
        .map_err(|_| "LEDGER_DB_FILE: not a connection string".to_owned())?;
    open(&config, tls, ca_pem).await
}

async fn open(config: &Config, tls: bool, ca_pem: Option<&str>) -> Result<Client, String> {
    let failed = |e: tokio_postgres::Error| {
        format!(
            "cannot connect to the cell's database: {}",
            cause_of(&e).text()
        )
    };
    if tls {
        let (client, connection) = config.connect(connector(ca_pem)?).await.map_err(failed)?;
        tokio::spawn(connection);
        Ok(client)
    } else {
        let (client, connection) = config.connect(NoTls).await.map_err(failed)?;
        tokio::spawn(connection);
        Ok(client)
    }
}

/// The connection the service uses, which can be replaced when it is lost. Cloning it gives
/// another handle to the same connection.
#[derive(Clone)]
pub struct Db {
    current: Arc<RwLock<Arc<Client>>>,
}

impl Db {
    pub fn new(client: Client) -> Self {
        Db {
            current: Arc::new(RwLock::new(Arc::new(client))),
        }
    }

    /// The connection in use now. A caller holds it for one call, so a replaced connection is
    /// picked up by the next call.
    pub fn get(&self) -> Arc<Client> {
        self.current
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn set(&self, client: Client) {
        *self.current.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(client);
    }
}

/// How the service signs in to a managed database: the connection string without a password, and
/// a source of tokens to use as the password for each new connection (D-24).
pub struct Identity {
    config: Config,
    tls: bool,
    ca_pem: Option<String>,
    source: Arc<dyn TokenSource>,
}

impl Identity {
    /// For a real run. The connection string must name one host and a user, and must not hold a
    /// password: a managed cell's role has none, and a password found here is a mistake.
    pub fn new(
        sign_in: SignIn,
        conn: &str,
        tls: bool,
        ca_pem: Option<&str>,
        settings: &dbauth::Settings,
    ) -> Result<Self, String> {
        let (config, target) = Self::target(conn)?;
        let source = dbauth::source_for(sign_in, &target, settings)?;
        Ok(Identity {
            config,
            tls,
            ca_pem: ca_pem.map(str::to_owned),
            source,
        })
    }

    /// For tests: a given source of tokens.
    pub fn with_source(
        conn: &str,
        tls: bool,
        ca_pem: Option<&str>,
        source: Arc<dyn TokenSource>,
    ) -> Result<Self, String> {
        let (config, _) = Self::target(conn)?;
        Ok(Identity {
            config,
            tls,
            ca_pem: ca_pem.map(str::to_owned),
            source,
        })
    }

    fn target(conn: &str) -> Result<(Config, dbauth::Target), String> {
        let config: Config = conn
            .parse()
            .map_err(|_| "LEDGER_DB_FILE: not a connection string".to_owned())?;
        if config.get_password().is_some() {
            return Err(
                "LEDGER_DB_FILE: holds a password, but LEDGER_DB_SIGN_IN says the role belongs to an identity"
                    .to_owned(),
            );
        }
        let user = config
            .get_user()
            .ok_or("LEDGER_DB_FILE: no user, which an identity sign-in needs")?
            .to_owned();
        let host = match config.get_hosts() {
            [Host::Tcp(h)] => h.clone(),
            _ => {
                return Err("LEDGER_DB_FILE: needs exactly one host, by name or address".to_owned());
            }
        };
        let port = config.get_ports().first().copied().unwrap_or(5432);
        Ok((config, dbauth::Target { host, port, user }))
    }

    /// Asks the source for a token and connects with it.
    pub async fn connect(&self) -> Result<Client, String> {
        let token = self.source.token().await.map_err(|e| {
            format!(
                "cannot get a token to sign in to the cell's database: {}",
                e.text()
            )
        })?;
        let mut config = self.config.clone();
        config.password(token.secret());
        open(&config, self.tls, self.ca_pem.as_deref()).await
    }
}

/// Watches the connection and makes it again, with a new token, when it is lost. If it cannot be
/// made again within `give_up`, which is the lease's length, `stop` is called with a reason that
/// names no host, user or token, and the watching ends. The caller stops the service, because a
/// cell that cannot reach its database cannot hold its lease honestly.
pub fn keep_connected(
    db: Db,
    identity: Arc<Identity>,
    give_up: Duration,
    stop: impl FnOnce(String) + Send + 'static,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(250)).await;
            if !db.get().is_closed() {
                continue;
            }
            let lost = Instant::now();
            let mut wait = Duration::from_millis(250);
            loop {
                match identity.connect().await {
                    Ok(client) => {
                        db.set(client);
                        break;
                    }
                    Err(why) => {
                        if lost.elapsed() + wait > give_up {
                            stop(format!(
                                "lost the connection to the cell's database and could not sign in again within the lease: {why}"
                            ));
                            return;
                        }
                        tokio::time::sleep(wait).await;
                        wait = (wait * 2).min(Duration::from_secs(4));
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_connection_string_with_a_password_is_refused_for_an_identity() {
        let with_password = format!("host=h user=u {}=x", "password");
        assert!(Identity::target(&with_password).is_err());
        assert!(Identity::target("host=h user=u dbname=d").is_ok());
        assert!(Identity::target("host=h dbname=d").is_err());
        assert!(Identity::target("host=a,b user=u").is_err());
    }

    #[test]
    fn a_refused_sign_in_is_named_by_its_code() {
        assert_eq!(classify(Some("28P01"), "anything"), Cause::SignIn);
        assert_eq!(classify(Some("28000"), ""), Cause::SignIn);
    }

    #[test]
    fn certificate_and_name_failures_are_told_apart() {
        assert_eq!(
            classify(
                None,
                "error performing TLS handshake: certificate verify failed (self-signed certificate in certificate chain)"
            ),
            Cause::Certificate
        );
        assert_eq!(
            classify(
                None,
                "certificate verify failed: unable to get local issuer certificate"
            ),
            Cause::Certificate
        );
        assert_eq!(
            classify(None, "certificate verify failed: Hostname mismatch"),
            Cause::Name
        );
        assert_eq!(
            classify(None, "certificate verify failed: IP address mismatch"),
            Cause::Name
        );
    }

    #[test]
    fn nothing_answering_is_told_from_the_rest() {
        assert_eq!(
            classify(
                None,
                "error connecting to server: Connection refused (os error 111)"
            ),
            Cause::Unreachable
        );
        assert_eq!(
            classify(None, "failed to lookup address information"),
            Cause::Unreachable
        );
        assert_eq!(classify(None, "something else"), Cause::Other);
    }

    #[test]
    fn no_message_carries_a_host_user_or_password() {
        for c in [
            Cause::Certificate,
            Cause::Name,
            Cause::SignIn,
            Cause::Unreachable,
            Cause::Other,
        ] {
            let t = c.text();
            assert!(!t.contains("host=") && !t.contains("user=") && !t.contains("password="));
            assert!(!t.contains('@') && !t.contains("://"));
        }
    }

    #[test]
    fn a_bad_authority_file_is_refused_without_its_text() {
        let err = connector(Some("not a certificate, secret-text"))
            .err()
            .unwrap();
        assert_eq!(err, "db-ca.crt: not a certificate");
    }
}
