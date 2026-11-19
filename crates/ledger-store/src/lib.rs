//! The ledger's Postgres schema and the migrator that applies it.
//!
//! Migrations are plain SQL files in `migrations/`, applied in order to every node. The
//! migrator records each applied migration with a checksum, so running it again changes
//! nothing and an edited migration is refused. See `docs/design.md#data-model`.

#![forbid(unsafe_code)]

pub mod cross_cell;
pub mod holds;
mod leg;
mod migrate;

pub use leg::{Leg, LegError, LegWriter, Posted, Side};

pub use migrate::{
    MIGRATIONS, Migration, MigrationError, MigrationReport, migrate, validate_schema_name,
};
