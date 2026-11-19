//! Money, identifiers, journals and invariants. No I/O.
//!
//! This crate is the start of the Rust kernel. The Python package in `ledger/` is the
//! reference it must match. See `docs/design.md#data-model` and
//! `docs/api-design.md#conventions`.
//!
//! Rules this crate enforces by construction:
//!
//! - money is an integer count of minor units plus an ISO 4217 code from the embedded list
//!   (refreshable from the official source), never a float;
//! - identifiers are opaque and carry a prefix for their kind, so a `AccountId` cannot be
//!   passed where a `JournalId` is expected;
//! - a [`Journal`] cannot exist unless its entries net to zero in every currency.
//!
//! The crate does no I/O and has no I/O dependencies.

#![forbid(unsafe_code)]
#![deny(clippy::float_arithmetic)]

mod currency;
mod id;
mod journal;
mod money;

pub use currency::{Currency, CurrencyError, CurrencyInfo, DataError};
pub use id::{AccountId, CommandId, EntryId, HoldId, IdError, JournalId, PartyId, TenantId};
pub use journal::{Journal, JournalError, Posting};
pub use money::{Money, MoneyError};
