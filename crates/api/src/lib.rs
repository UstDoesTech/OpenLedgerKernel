//! The ledger's HTTP API, run next to its own cell.
//!
//! This is the thin write path (tasks K-11 to K-14): opening accounts, reading them back and
//! transfers, within a cell and between cells. A cell never touches another cell's database: it
//! asks that cell's service, on the internal listener. It stands on the kernel (`ledger-store`) and the control plane, and it
//! stands in for the command API until the command catalogue, the simulate-and-commit engine and
//! real sign-in (A-01 to A-06) replace its pieces one at a time.
//!
//! Rules it keeps, always:
//! - the tenant comes from the token, never from a path or a body, and another tenant's account is
//!   a `404`, so its existence does not show;
//! - an account is served only by its home cell, holding a valid lease, and a request to any other
//!   cell is refused with `wrong_home`, naming the home;
//! - money is integer minor units plus an ISO 4217 code;
//! - errors are problem details with a stable `code`, and a client never sees an epoch error.

#![forbid(unsafe_code)]

mod app;
pub mod auth;
pub mod dbconn;
pub mod error;
mod internal;
pub mod login;
pub mod peer;
pub mod reload;
pub mod tls;

pub use app::{AppState, Peers, ROUTES, router};
pub use internal::{INTERNAL_ROUTES, internal_router};
pub mod ui;
