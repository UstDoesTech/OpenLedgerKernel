//! Opaque identifiers, prefixed by kind (`acc_`, `jr_`, `cmd_`, ...).
//!
//! Each kind is its own type, so the compiler stops an account id being used as a journal
//! id. The text after the prefix is lower-case letters and digits, as in
//! `api/openapi.yaml`.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

const MAX_BODY: usize = 64;

/// Why an identifier was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdError {
    #[error("expected an identifier starting with {expected:?}, got {got:?}")]
    WrongPrefix { expected: &'static str, got: String },
    #[error("an identifier needs 1 to {MAX_BODY} lower-case letters or digits after {prefix:?}")]
    BadBody { prefix: &'static str },
}

fn check(prefix: &'static str, s: &str) -> Result<(), IdError> {
    let body = s.strip_prefix(prefix).ok_or_else(|| IdError::WrongPrefix {
        expected: prefix,
        got: s.to_owned(),
    })?;
    let ok = !body.is_empty()
        && body.len() <= MAX_BODY
        && body
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    if ok {
        Ok(())
    } else {
        Err(IdError::BadBody { prefix })
    }
}

macro_rules! id_type {
    ($(#[$doc:meta])* $name:ident, $prefix:literal) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// The prefix that marks this kind of identifier.
            pub const PREFIX: &'static str = $prefix;

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl FromStr for $name {
            type Err = IdError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                check($prefix, s)?;
                Ok($name(s.to_owned()))
            }
        }

        impl TryFrom<String> for $name {
            type Error = IdError;

            fn try_from(s: String) -> Result<Self, Self::Error> {
                check($prefix, &s)?;
                Ok($name(s))
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = String::deserialize(d)?;
                s.try_into().map_err(serde::de::Error::custom)
            }
        }
    };
}

id_type!(
    /// A tenant. Comes from the token, never from a path or body.
    TenantId, "ten_"
);
id_type!(
    /// A ledger account.
    AccountId, "acc_"
);
id_type!(
    /// A journal: one business event whose entries net to zero.
    JournalId, "jr_"
);
id_type!(
    /// One entry in the append-only log.
    EntryId, "ent_"
);
id_type!(
    /// A hold: an amount reserved against an account.
    HoldId, "hld_"
);
id_type!(
    /// A command sent to the command API.
    CommandId, "cmd_"
);
id_type!(
    /// A party (a person or organisation). Personal data lives behind this id, never in the ledger.
    PartyId, "pty_"
);

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn parses_and_displays_each_kind() {
        assert_eq!(
            "acc_7k2m".parse::<AccountId>().unwrap().to_string(),
            "acc_7k2m"
        );
        assert!("jr_91".parse::<JournalId>().is_ok());
        assert!("ten_acme".parse::<TenantId>().is_ok());
        assert!("ent_1".parse::<EntryId>().is_ok());
        assert!("hld_1".parse::<HoldId>().is_ok());
        assert!("cmd_1".parse::<CommandId>().is_ok());
        assert!("pty_1".parse::<PartyId>().is_ok());
    }

    #[test]
    fn a_kind_cannot_be_read_as_another() {
        assert!(matches!(
            "acc_1".parse::<JournalId>(),
            Err(IdError::WrongPrefix { .. })
        ));
    }

    #[test]
    fn bad_bodies_are_refused() {
        for bad in [
            "acc_", "acc_ABC", "acc_a-b", "acc_a b", "acc_é", "ACC_1", "acc",
        ] {
            assert!(
                bad.parse::<AccountId>().is_err(),
                "{bad:?} should be refused"
            );
        }
        assert!(
            format!("acc_{}", "a".repeat(65))
                .parse::<AccountId>()
                .is_err()
        );
        assert!(
            format!("acc_{}", "a".repeat(64))
                .parse::<AccountId>()
                .is_ok()
        );
    }

    #[test]
    fn serde_checks_on_the_way_in() {
        assert_eq!(
            serde_json::to_string(&"acc_1".parse::<AccountId>().unwrap()).unwrap(),
            "\"acc_1\""
        );
        assert!(serde_json::from_str::<AccountId>("\"jr_1\"").is_err());
    }

    proptest! {
        #[test]
        fn valid_ids_round_trip(body in "[a-z0-9]{1,64}") {
            let s = format!("acc_{body}");
            let id: AccountId = s.parse().unwrap();
            prop_assert_eq!(id.to_string(), s);
        }

        #[test]
        fn arbitrary_text_never_panics(s in ".*") {
            let _ = s.parse::<AccountId>();
        }
    }
}
