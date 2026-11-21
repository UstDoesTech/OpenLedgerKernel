//! The service's connection to its cell's database.
//!
//! The nodes make their certificates from their own authority, so the service can be given that
//! authority's certificate (`db-ca.crt` in its secret) and trusts it as well as the system's
//! roots. The service connects once, at start. When it cannot, it says which of four things went
//! wrong and nothing else: never the host, the user, the password or the driver's own text.

use native_tls::{Certificate, TlsConnector};
use postgres_native_tls::MakeTlsConnector;
use tokio_postgres::{Client, Config, NoTls};

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

#[cfg(test)]
mod tests {
    use super::*;

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
