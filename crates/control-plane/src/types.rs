use std::fmt;
use std::str::FromStr;

use ledger_core::AccountId;
use serde::{Deserialize, Serialize};

/// A fencing epoch. Starts at 1 and grows by one on every change of home.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Epoch(pub u64);

impl Epoch {
    /// The epoch before the first. Used as `prev_epoch` in an account's first fence.
    pub const NONE: Epoch = Epoch(0);
    pub const FIRST: Epoch = Epoch(1);

    pub fn next(self) -> Epoch {
        Epoch(self.0 + 1)
    }
}

impl fmt::Display for Epoch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A data node (a cell's primary). Readable names such as `aws-lon-03`: 1 to 63 lower-case
/// letters, digits and hyphens, not starting or ending with a hyphen.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct NodeId(String);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "a node name is 1 to 63 lower-case letters, digits or hyphens, not starting or ending with a hyphen: {0:?}"
)]
pub struct NodeIdError(String);

impl NodeId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for NodeId {
    type Err = NodeIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let ok = (1..=63).contains(&s.len())
            && !s.starts_with('-')
            && !s.ends_with('-')
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if ok {
            Ok(NodeId(s.to_owned()))
        } else {
            Err(NodeIdError(s.to_owned()))
        }
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for NodeId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// The record pushed to every data node when an account's home changes. Entries for
/// `account` from `prev_epoch` with a sequence above `prev_cutoff_seq` are void.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fence {
    pub account: AccountId,
    pub epoch: Epoch,
    pub home: NodeId,
    pub prev_epoch: Epoch,
    pub prev_cutoff_seq: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_names_are_readable_and_checked() {
        for ok in ["a", "aws-lon-03", "azure-uk-south-1", &"a".repeat(63)] {
            assert!(ok.parse::<NodeId>().is_ok(), "{ok:?}");
        }
        for bad in ["", "-a", "a-", "A", "a_b", "a b", "é", &"a".repeat(64)] {
            assert!(bad.parse::<NodeId>().is_err(), "{bad:?}");
        }
        assert!(serde_json::from_str::<NodeId>("\"Bad Name\"").is_err());
    }

    #[test]
    fn epochs_count_up_from_one() {
        assert_eq!(Epoch::FIRST.next(), Epoch(2));
        assert!(Epoch::NONE < Epoch::FIRST);
    }
}
