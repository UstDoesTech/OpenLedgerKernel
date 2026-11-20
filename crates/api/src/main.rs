//! `ledger-api`: runs the API next to one cell.
//!
//! Configuration is by environment. Anything secret is read from a file whose path the
//! environment names, never from the environment or the command line, so it cannot show in a
//! process list or a log.
//!
//!   LEDGER_NODE            the cell this runs next to, such as `aws-lon-03`
//!   LEDGER_DB_FILE         a file holding this cell's Postgres connection string
//!   LEDGER_DB_TLS          `off` to connect without TLS (tests only); anything else uses TLS
//!   LEDGER_SCHEMA          the ledger schema, default `ledger`
//!   LEDGER_LISTEN          the address to listen on, default `127.0.0.1:8080`
//!   LEDGER_TOKENS_FILE     a JSON list of `{token, tenant, principal}` (see `ledger_api::auth`)
//!   LEDGER_INTERNAL_LISTEN the address of the internal listener, for calls from other cells. Not
//!                          set means none, and then only transfers within this cell can be made
//!   LEDGER_INTERNAL_AUTH_FILE  a JSON list of `{token, cell, tenants}`: the cells that may call it
//!   LEDGER_CELLS_FILE      a JSON object of other cells' names to `{url, token}`, for transfers to
//!                          accounts homed there
//!   LEDGER_TLS_CERT_FILE, LEDGER_TLS_KEY_FILE   this cell's certificate chain and key, in PEM
//!   LEDGER_TLS_CA_FILE     the authority that signs every cell's certificate, in PEM
//!
//! A listener on any address but loopback needs TLS, and the internal listener needs mutual TLS:
//! the service refuses to start otherwise. It also refuses to start if the retired
//! `LEDGER_PEERS_FILE` is set, because a cell never connects to another cell's database.
//!   LEDGER_ETCD_ENDPOINTS  comma-separated etcd client URLs
//!   LEDGER_ETCD_PREFIX     the control plane's key prefix, default `/ledger`
//!   LEDGER_ETCD_CA_FILE, LEDGER_ETCD_CERT_FILE, LEDGER_ETCD_KEY_FILE
//!                          the authority that signed etcd's certificates, and the client certificate
//!                          and key this cell presents. All three or none. An https endpoint needs
//!                          all three, and so does any plain http endpoint that is not loopback, which
//!                          is refused when they are set.
//!   LEDGER_LEASE_*         the lease guard's timings (see `control_plane::GuardConfig`)
//!
//! Exit codes: 2 for a configuration or connection problem at start, 1 for a failure after.

use std::process::ExitCode;
use std::sync::Arc;

use control_plane::{
    EtcdConfig, EtcdControlPlane, EtcdTls, GuardConfig, LeaseGuard, NodeId, SystemClock,
    check_transport,
};
use ledger_api::auth::{PeerTokens, StaticTokens};
use ledger_api::peer::PeerClient;
use ledger_api::reload::{self, Reloadable};
use ledger_api::tls::{self, TlsFiles, TlsListener};
use ledger_api::{AppState, internal_router, router};
use tokio_postgres::{Client, Config, NoTls};

fn var(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|_| format!("{name} is not set"))
}

fn file(name: &str) -> Result<String, String> {
    let path = var(name)?;
    std::fs::read_to_string(&path)
        .map(|s| s.trim().to_owned())
        .map_err(|e| format!("{name}: cannot read {path}: {e}"))
}

async fn connect(conn: &str, tls: bool) -> Result<Client, String> {
    let config: Config = conn
        .parse()
        .map_err(|_| "LEDGER_DB_FILE: not a connection string".to_owned())?;
    if tls {
        let connector = native_tls::TlsConnector::new().map_err(|e| e.to_string())?;
        let (client, connection) = config
            .connect(postgres_native_tls::MakeTlsConnector::new(connector))
            .await
            .map_err(|e| format!("cannot connect to the cell's database: {e}"))?;
        tokio::spawn(connection);
        Ok(client)
    } else {
        let (client, connection) = config
            .connect(NoTls)
            .await
            .map_err(|e| format!("cannot connect to the cell's database: {e}"))?;
        tokio::spawn(connection);
        Ok(client)
    }
}

/// Binds an address and serves `app` on it, with TLS when a config is given, until `stopped`.
async fn serve(
    addr: std::net::SocketAddr,
    app: axum::Router,
    config: Option<Reloadable<rustls::ServerConfig>>,
    mut stopped: tokio::sync::watch::Receiver<bool>,
    servers: &mut tokio::task::JoinSet<std::io::Result<()>>,
) -> Result<(), String> {
    let tcp = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("cannot listen on {addr}: {e}"))?;
    let end = async move {
        let _ = stopped.wait_for(|s| *s).await;
    };
    match config {
        Some(config) => {
            let listener = TlsListener::reloadable(tcp, config).map_err(|e| e.to_string())?;
            servers
                .spawn(async move { axum::serve(listener, app).with_graceful_shutdown(end).await });
        }
        None => {
            servers.spawn(async move { axum::serve(tcp, app).with_graceful_shutdown(end).await });
        }
    }
    Ok(())
}

/// Reads the list of other cells and builds a client for each, with this cell's certificate.
fn load_cells(text: &str, files: &TlsFiles) -> Result<ledger_api::Peers, String> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Cell {
        url: String,
        token: String,
    }
    let cells: std::collections::BTreeMap<String, Cell> = serde_json::from_str(text)
        .map_err(|_| "LEDGER_CELLS_FILE: not a JSON object of cells".to_owned())?;
    let client_tls = if files.cert.is_some() && files.key.is_some() && files.ca.is_some() {
        Some(tls::client_config(files).map_err(|e| e.to_string())?)
    } else {
        None
    };
    let mut peers = ledger_api::Peers::new();
    for (cell, c) in cells {
        let client = PeerClient::new(&c.url, &c.token, client_tls.clone())
            .map_err(|e| format!("LEDGER_CELLS_FILE: cell {cell}: {e}"))?;
        peers.insert(cell, Arc::new(client));
    }
    Ok(peers)
}

async fn run() -> Result<(), String> {
    let node: NodeId = var("LEDGER_NODE")?
        .parse()
        .map_err(|e| format!("LEDGER_NODE: {e}"))?;
    let schema = std::env::var("LEDGER_SCHEMA").unwrap_or_else(|_| "ledger".to_owned());
    let listen = std::env::var("LEDGER_LISTEN").unwrap_or_else(|_| "127.0.0.1:8080".to_owned());
    let tls = std::env::var("LEDGER_DB_TLS")
        .map(|v| v != "off")
        .unwrap_or(true);
    let reload_every = std::time::Duration::from_secs(
        std::env::var("LEDGER_RELOAD_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(30),
    );
    let tokens = Reloadable::new(
        StaticTokens::from_json(&file("LEDGER_TOKENS_FILE")?).map_err(|e| e.to_string())?,
    );
    let guard_config = GuardConfig::from_env().map_err(|e| e.to_string())?;
    let endpoints: Vec<String> = var("LEDGER_ETCD_ENDPOINTS")?
        .split(',')
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect();
    let prefix = std::env::var("LEDGER_ETCD_PREFIX").unwrap_or_else(|_| "/ledger".to_owned());
    let etcd_files = [
        std::env::var("LEDGER_ETCD_CA_FILE").ok(),
        std::env::var("LEDGER_ETCD_CERT_FILE").ok(),
        std::env::var("LEDGER_ETCD_KEY_FILE").ok(),
    ];
    check_transport(&endpoints, etcd_files.iter().flatten().count())?;
    let etcd_tls = match etcd_files {
        [Some(ca), Some(cert), Some(key)] => Some(EtcdTls {
            ca: ca.into(),
            cert: cert.into(),
            key: key.into(),
        }),
        _ => None,
    };

    if std::env::var_os("LEDGER_PEERS_FILE").is_some() {
        return Err(
            "LEDGER_PEERS_FILE is retired: a cell no longer connects to another cell's database. \
             Use LEDGER_CELLS_FILE and LEDGER_INTERNAL_LISTEN"
                .to_owned(),
        );
    }
    let files = TlsFiles {
        cert: std::env::var("LEDGER_TLS_CERT_FILE").ok(),
        key: std::env::var("LEDGER_TLS_KEY_FILE").ok(),
        ca: std::env::var("LEDGER_TLS_CA_FILE").ok(),
    };
    let have_pair = files.cert.is_some() && files.key.is_some();
    let public_addr: std::net::SocketAddr = listen
        .parse()
        .map_err(|_| "LEDGER_LISTEN: not an address and port".to_owned())?;
    tls::check_bind(&public_addr, have_pair, "LEDGER_LISTEN")?;
    let public_tls = if have_pair {
        Some(Reloadable::from_arc(
            tls::server_config(&files).map_err(|e| e.to_string())?,
        ))
    } else {
        None
    };
    let internal = match std::env::var("LEDGER_INTERNAL_LISTEN") {
        Ok(addr) => {
            let addr: std::net::SocketAddr = addr
                .parse()
                .map_err(|_| "LEDGER_INTERNAL_LISTEN: not an address and port".to_owned())?;
            let mutual = have_pair && files.ca.is_some();
            tls::check_bind(&addr, mutual, "LEDGER_INTERNAL_LISTEN")?;
            let config = if mutual {
                Some(Reloadable::from_arc(
                    tls::mutual_server_config(&files).map_err(|e| e.to_string())?,
                ))
            } else {
                None
            };
            Some((addr, config))
        }
        Err(_) => None,
    };
    let peer_auth = if internal.is_some() {
        Some(Reloadable::new(
            PeerTokens::from_json(&file("LEDGER_INTERNAL_AUTH_FILE")?)
                .map_err(|e| e.to_string())?,
        ))
    } else {
        None
    };
    let db = connect(&file("LEDGER_DB_FILE")?, tls).await?;
    // Fail at start, not at the first request, if the schema this needs is not there.
    ledger_store::validate_schema_name(&schema).map_err(|e| e.to_string())?;
    db.query_opt(&format!("select 1 from {schema}.api_request limit 1"), &[])
        .await
        .map_err(|_| {
            "the cell's database has no ledger schema at this version: migrate it first".to_owned()
        })?;

    let mut etcd_config = EtcdConfig::new(endpoints, prefix, guard_config.lease_ttl);
    if let Some(tls) = etcd_tls {
        etcd_config = etcd_config.with_tls(tls);
    }
    let cp = EtcdControlPlane::connect(etcd_config)
        .await
        .map_err(|e| format!("cannot reach the control plane: {e}"))?;
    let guard = LeaseGuard::new(node.clone(), guard_config, Arc::new(SystemClock::new()))
        .map_err(|e| e.to_string())?;
    let mut state = AppState::new(
        node.clone(),
        &schema,
        db,
        Arc::new(cp),
        guard,
        Arc::new(tokens.clone()),
    );
    if std::env::var_os("LEDGER_CELLS_FILE").is_some() {
        let peers = load_cells(&file("LEDGER_CELLS_FILE")?, &files)?;
        state.peers_handle().set(Arc::new(peers));
    }
    if let Some(auth) = &peer_auth {
        state = state.with_peer_auth(Arc::new(auth.clone()));
    }

    // Certificates, tokens and the list of cells rotate while the service runs. Each is read
    // again when its file changes. A file that cannot be used leaves what is in force alone.
    let path = |name: &str| std::env::var(name).ok().map(std::path::PathBuf::from);
    {
        let (handle, from) = (
            tokens.clone(),
            path("LEDGER_TOKENS_FILE").into_iter().collect(),
        );
        reload::spawn_watch("tokens", from, reload_every, move || {
            let Ok(text) = file("LEDGER_TOKENS_FILE") else {
                return false;
            };
            match StaticTokens::from_json(&text) {
                Ok(t) => {
                    handle.set(Arc::new(t));
                    true
                }
                Err(_) => false,
            }
        });
    }
    if let Some(handle) = peer_auth.clone() {
        let from = path("LEDGER_INTERNAL_AUTH_FILE").into_iter().collect();
        reload::spawn_watch("cell credentials", from, reload_every, move || {
            let Ok(text) = file("LEDGER_INTERNAL_AUTH_FILE") else {
                return false;
            };
            match PeerTokens::from_json(&text) {
                Ok(t) => {
                    handle.set(Arc::new(t));
                    true
                }
                Err(_) => false,
            }
        });
    }
    if have_pair {
        let mut from: Vec<_> = [
            path("LEDGER_TLS_CERT_FILE"),
            path("LEDGER_TLS_KEY_FILE"),
            path("LEDGER_TLS_CA_FILE"),
        ]
        .into_iter()
        .flatten()
        .collect();
        from.sort();
        let (public, internal_handle) = (
            public_tls.clone(),
            internal.as_ref().and_then(|(_, c)| c.clone()),
        );
        let peers = state.peers_handle();
        let has_cells = std::env::var_os("LEDGER_CELLS_FILE").is_some();
        if let Some(c) = path("LEDGER_CELLS_FILE") {
            from.push(c);
        }
        let files = files.clone();
        reload::spawn_watch("certificates", from, reload_every, move || {
            // Everything is built first and swapped only if all of it is usable, so a certificate
            // that does not match its key, or a half-written set, changes nothing.
            let Ok(public_new) = tls::server_config(&files) else {
                return false;
            };
            let internal_new = if internal_handle.is_some() {
                match tls::mutual_server_config(&files) {
                    Ok(c) => Some(c),
                    Err(_) => return false,
                }
            } else {
                None
            };
            let peers_new = if has_cells {
                let Ok(text) = file("LEDGER_CELLS_FILE") else {
                    return false;
                };
                match load_cells(&text, &files) {
                    Ok(p) => Some(p),
                    Err(_) => return false,
                }
            } else {
                None
            };
            if let Some(p) = &public {
                p.set(public_new);
            }
            if let (Some(h), Some(c)) = (&internal_handle, internal_new) {
                h.set(c);
            }
            if let Some(p) = peers_new {
                peers.set(Arc::new(p));
            }
            true
        });
    }

    // A healthy cell renews the leases of the accounts homed here, on a timer.
    let renewing = state.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(guard_config.renew_every);
        loop {
            tick.tick().await;
            if let Err(e) = renewing.renew().await {
                eprintln!("ledger-api: could not renew leases: {e}");
            }
        }
    });

    let (stop, stopped) = tokio::sync::watch::channel(false);
    let mut servers = tokio::task::JoinSet::new();
    serve(
        public_addr,
        router(state.clone()),
        public_tls,
        stopped.clone(),
        &mut servers,
    )
    .await?;
    eprintln!("ledger-api: cell {node} listening on {public_addr}");
    if let Some((addr, config)) = internal {
        serve(
            addr,
            internal_router(state.clone()),
            config,
            stopped.clone(),
            &mut servers,
        )
        .await?;
        eprintln!("ledger-api: cell {node} listening for other cells on {addr}");
    }

    let _ = tokio::signal::ctrl_c().await;
    // Stop taking new requests, then give those in flight a moment.
    state.admission().set_draining(true);
    state
        .admission()
        .wait_idle(std::time::Duration::from_secs(10))
        .await;
    let _ = stop.send(true);
    while let Some(done) = servers.join_next().await {
        if let Ok(Err(e)) = done {
            return Err(e.to_string());
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ledger-api: {e}");
            ExitCode::from(2)
        }
    }
}
