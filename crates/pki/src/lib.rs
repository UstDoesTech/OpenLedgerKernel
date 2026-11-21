//! Issuing and rotating the API's certificates and cell credentials (task K-17).
//!
//! One central authority signs every cell's certificates (decision D-17). Its key lives in a KMS
//! that can sign but never export, so this crate only ever holds a [`Signer`], not a key. A
//! rotation reads each cell's API secret, builds the new content for all cells in memory,
//! checks it, and then writes it in three steps with a wait between them, so a cell never calls
//! another with a credential the other side does not yet list.
//!
//! This crate prints counts, never content. Errors say which step failed, never which cell, which
//! secret or what was in it, because the job's log is public (see `docs/credentialed-workflows.md`).

#![forbid(unsafe_code)]

pub mod database;
pub mod error;
pub mod manifest;
pub mod rotate;
pub mod runner;
pub mod signer;
pub mod store;

pub use database::{AdministratorCopy, DatabaseSource, Simulated};
pub use error::PkiError;
pub use manifest::{Cloud, Manifest};
pub use rotate::{Kind, Report, Rotation};
pub use signer::{KmsSigner, MemorySigner, Signer};
pub use store::{CliStore, MemoryStore, SecretStore};
