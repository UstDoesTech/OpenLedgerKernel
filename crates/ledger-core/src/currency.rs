//! ISO 4217 currencies.
//!
//! The list is embedded from `data/iso4217.csv`. `scripts/refresh_iso4217.py` rebuilds that
//! file from SIX Group's List One, the official source, and a scheduled workflow opens a
//! pull request when it changes. The first line of the file says whether the data is the
//! official list or a provisional seed: see [`Currency::registry_note`].

use std::fmt;
use std::str::FromStr;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

const DATA: &str = include_str!("../data/iso4217.csv");

/// Details of one currency from the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CurrencyInfo {
    code: Currency,
    numeric: u16,
    minor_units: Option<u8>,
}

impl CurrencyInfo {
    pub fn code(&self) -> Currency {
        self.code
    }

    /// The three-digit ISO 4217 number, such as 826 for GBP.
    pub fn numeric(&self) -> u16 {
        self.numeric
    }

    /// Digits after the decimal point (2 for GBP, 0 for JPY). `None` for funds and metals
    /// that have no minor unit, such as gold.
    pub fn minor_units(&self) -> Option<u8> {
        self.minor_units
    }
}

/// An ISO 4217 alphabetic currency code, such as `GBP`, that is on the embedded list.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Currency([u8; 3]);

/// Why a currency code was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CurrencyError {
    #[error("a currency code is exactly three upper-case letters, got {0:?}")]
    BadShape(String),
    #[error("{0} is not on the ISO 4217 list")]
    Unknown(String),
}

/// Why the embedded list could not be read. A test checks the shipped file, so this means
/// a bad edit.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("line {line}: {reason}")]
pub struct DataError {
    pub line: usize,
    pub reason: &'static str,
}

struct Registry {
    note: &'static str,
    entries: Vec<CurrencyInfo>, // sorted by code
}

fn parse(data: &'static str) -> Result<Registry, DataError> {
    let mut note = "";
    let mut entries: Vec<CurrencyInfo> = Vec::new();
    let mut header_seen = false;
    for (i, line) in data.lines().enumerate() {
        let line_no = i + 1;
        let bad = |reason| DataError {
            line: line_no,
            reason,
        };
        if let Some(n) = line.strip_prefix('#') {
            if note.is_empty() {
                note = n.trim();
            }
            continue;
        }
        if line.trim().is_empty() {
            continue;
        }
        if !header_seen {
            if line != "code,numeric,minor_units" {
                return Err(bad("expected the header code,numeric,minor_units"));
            }
            header_seen = true;
            continue;
        }
        let mut f = line.split(',');
        let (Some(code), Some(num), Some(minor), None) = (f.next(), f.next(), f.next(), f.next())
        else {
            return Err(bad("expected three fields"));
        };
        let code = shape(code).ok_or_else(|| bad("bad currency code"))?;
        let numeric = (num.len() == 3)
            .then(|| num.parse::<u16>().ok())
            .flatten()
            .ok_or_else(|| bad("bad numeric code"))?;
        let minor_units = match minor {
            "" => None,
            m => Some(
                m.parse::<u8>()
                    .ok()
                    .filter(|d| *d <= 9)
                    .ok_or_else(|| bad("bad minor units"))?,
            ),
        };
        entries.push(CurrencyInfo {
            code: Currency(code),
            numeric,
            minor_units,
        });
    }
    entries.sort_by_key(|e| e.code);
    if entries.windows(2).any(|w| w[0].code == w[1].code) {
        return Err(DataError {
            line: 0,
            reason: "a currency code is listed twice",
        });
    }
    let mut numerics: Vec<u16> = entries.iter().map(|e| e.numeric).collect();
    numerics.sort_unstable();
    if numerics.windows(2).any(|w| w[0] == w[1]) {
        return Err(DataError {
            line: 0,
            reason: "a numeric code is listed twice",
        });
    }
    Ok(Registry { note, entries })
}

fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| parse(DATA).expect("embedded ISO 4217 data is checked by a test"))
}

fn shape(s: &str) -> Option<[u8; 3]> {
    match s.as_bytes() {
        [a, b, c] if [a, b, c].iter().all(|x| x.is_ascii_uppercase()) => Some([*a, *b, *c]),
        _ => None,
    }
}

impl Currency {
    pub const GBP: Currency = Currency(*b"GBP");
    pub const EUR: Currency = Currency(*b"EUR");
    pub const USD: Currency = Currency(*b"USD");

    /// The code as text, for example `"GBP"`.
    pub fn as_str(&self) -> &str {
        // Only ever built from ASCII upper-case letters.
        std::str::from_utf8(&self.0).unwrap_or("???")
    }

    /// Numeric code and minor-unit exponent for this currency.
    pub fn info(&self) -> CurrencyInfo {
        let r = registry();
        match r.entries.binary_search_by_key(self, |e| e.code) {
            Ok(i) => r.entries[i],
            // Unreachable for a Currency built through parsing; kept total rather than panicking.
            Err(_) => CurrencyInfo {
                code: *self,
                numeric: 0,
                minor_units: None,
            },
        }
    }

    /// Every currency on the list, in code order.
    pub fn all() -> impl Iterator<Item = CurrencyInfo> {
        registry().entries.iter().copied()
    }

    /// The provenance line of the embedded list: the source, the publication date and
    /// whether it is the official list (`status=official`) or a provisional seed.
    pub fn registry_note() -> &'static str {
        registry().note
    }
}

impl FromStr for Currency {
    type Err = CurrencyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let code = Currency(shape(s).ok_or_else(|| CurrencyError::BadShape(s.to_owned()))?);
        registry()
            .entries
            .binary_search_by_key(&code, |e| e.code)
            .map(|_| code)
            .map_err(|_| CurrencyError::Unknown(s.to_owned()))
    }
}

impl fmt::Display for Currency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Debug for Currency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Currency({})", self.as_str())
    }
}

impl Serialize for Currency {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Currency {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_list_parses() {
        let r = parse(DATA).unwrap();
        assert!(r.entries.len() >= 100, "list looks truncated");
        assert!(
            r.note.contains("status="),
            "first line must record the status"
        );
    }

    #[test]
    fn code_must_be_three_upper_case_letters_and_on_the_list() {
        assert_eq!("GBP".parse::<Currency>().unwrap(), Currency::GBP);
        for bad in ["", "GB", "GBPP", "gbp", "G1P", "£££", "GB "] {
            assert!(matches!(
                "".parse::<Currency>(),
                Err(CurrencyError::BadShape(_))
            ));
            assert!(
                matches!(bad.parse::<Currency>(), Err(CurrencyError::BadShape(_))),
                "{bad:?}"
            );
        }
        assert!(matches!(
            "ZZZ".parse::<Currency>(),
            Err(CurrencyError::Unknown(_))
        ));
    }

    #[test]
    fn the_constants_are_on_the_list() {
        for c in [Currency::GBP, Currency::EUR, Currency::USD] {
            assert_eq!(c.as_str().parse::<Currency>().unwrap(), c);
        }
    }

    #[test]
    fn numeric_codes_and_exponents() {
        assert_eq!(Currency::GBP.info().numeric(), 826);
        assert_eq!(Currency::GBP.info().minor_units(), Some(2));
        let jpy: Currency = "JPY".parse().unwrap();
        assert_eq!(jpy.info().minor_units(), Some(0));
        let kwd: Currency = "KWD".parse().unwrap();
        assert_eq!(kwd.info().minor_units(), Some(3));
        let xau: Currency = "XAU".parse().unwrap();
        assert_eq!(xau.info().minor_units(), None);
    }

    #[test]
    fn list_is_sorted_and_unique_in_code_and_number() {
        let codes: Vec<_> = Currency::all().map(|i| i.code()).collect();
        assert!(codes.windows(2).all(|w| w[0] < w[1]));
        let mut nums: Vec<_> = Currency::all().map(|i| i.numeric()).collect();
        nums.sort_unstable();
        assert!(nums.windows(2).all(|w| w[0] != w[1]));
    }

    #[test]
    fn a_malformed_list_is_rejected() {
        for bad in [
            "code,numeric,minor_units\nGBP,826\n",
            "code,numeric,minor_units\nGBP,82,2\n",
            "code,numeric,minor_units\ngbp,826,2\n",
            "code,numeric,minor_units\nGBP,826,22\n",
            "code,numeric,minor_units\nGBP,826,2\nGBP,826,2\n",
            "code,numeric,minor_units\nGBP,826,2\nEUR,826,2\n",
            "wrong header\n",
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }
}
