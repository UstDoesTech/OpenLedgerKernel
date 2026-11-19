//! The node agent: turns a node configuration into a running vanilla PostgreSQL 16 node.
//!
//! A node is a primary or a standby. The primary commits once any one of its standbys has the
//! change (`ANY 1 (...)`), so losing a zone loses nothing. Standbys join by physical streaming
//! replication; no replication slots are used at all. Connections use TLS 1.3 only. Standbys
//! authenticate to the primary with a client certificate, so there is no replication password.
//! The administrator's password is made on the primary's first start, handed to a
//! [`store::SecretStore`], and never logged or written anywhere else.
//!
//! The agent prepares the node and then replaces itself with `postgres` (exec), so PostgreSQL is
//! the container's main process and receives its stop signal directly.

pub mod aws;
pub mod azure;
pub mod config;
pub mod gcp;
pub mod http;
pub mod node;
pub mod plan;
pub mod reference;
pub mod sigv4;
pub mod store;
pub mod stores;
pub mod util;
