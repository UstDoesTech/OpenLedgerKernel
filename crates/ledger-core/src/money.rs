//! Money as integer minor units plus an ISO 4217 currency code.

use serde::{Deserialize, Serialize};

use crate::Currency;

/// Why a money calculation was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MoneyError {
    #[error("cannot combine {left} with {right}")]
    CurrencyMismatch { left: Currency, right: Currency },
    #[error("amount out of range")]
    Overflow,
}

/// A signed amount in minor units (pence, cents) of one currency.
///
/// Serialises as `{"minor": 6150, "currency": "GBP"}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Money {
    minor: i64,
    currency: Currency,
}

impl Money {
    pub const fn new(minor: i64, currency: Currency) -> Self {
        Money { minor, currency }
    }

    pub const fn zero(currency: Currency) -> Self {
        Money { minor: 0, currency }
    }

    pub const fn minor(&self) -> i64 {
        self.minor
    }

    pub const fn currency(&self) -> Currency {
        self.currency
    }

    pub const fn is_zero(&self) -> bool {
        self.minor == 0
    }

    fn same_currency(&self, other: &Money) -> Result<(), MoneyError> {
        if self.currency == other.currency {
            Ok(())
        } else {
            Err(MoneyError::CurrencyMismatch {
                left: self.currency,
                right: other.currency,
            })
        }
    }

    pub fn checked_add(self, other: Money) -> Result<Money, MoneyError> {
        self.same_currency(&other)?;
        let minor = self
            .minor
            .checked_add(other.minor)
            .ok_or(MoneyError::Overflow)?;
        Ok(Money::new(minor, self.currency))
    }

    pub fn checked_sub(self, other: Money) -> Result<Money, MoneyError> {
        self.same_currency(&other)?;
        let minor = self
            .minor
            .checked_sub(other.minor)
            .ok_or(MoneyError::Overflow)?;
        Ok(Money::new(minor, self.currency))
    }

    /// The same amount with the opposite sign. `i64::MIN` has no opposite, so it errors.
    pub fn checked_neg(self) -> Result<Money, MoneyError> {
        let minor = self.minor.checked_neg().ok_or(MoneyError::Overflow)?;
        Ok(Money::new(minor, self.currency))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn currency_must_be_three_upper_case_letters() {
        assert_eq!("GBP".parse::<Currency>().unwrap(), Currency::GBP);
        for bad in ["", "GB", "GBPP", "gbp", "G1P", "£££", "GB "] {
            assert!(
                bad.parse::<Currency>().is_err(),
                "{bad:?} should be refused"
            );
        }
    }

    #[test]
    fn money_json_shape_matches_the_api() {
        let m = Money::new(6150, Currency::GBP);
        assert_eq!(
            serde_json::to_string(&m).unwrap(),
            r#"{"minor":6150,"currency":"GBP"}"#
        );
        let back: Money = serde_json::from_str(r#"{"minor":-5,"currency":"EUR"}"#).unwrap();
        assert_eq!(back, Money::new(-5, Currency::EUR));
    }

    #[test]
    fn money_refuses_fractions_and_bad_currencies_in_json() {
        assert!(serde_json::from_str::<Money>(r#"{"minor":1.5,"currency":"GBP"}"#).is_err());
        assert!(serde_json::from_str::<Money>(r#"{"minor":1,"currency":"gbp"}"#).is_err());
        assert!(serde_json::from_str::<Money>(r#"{"minor":1,"currency":"ZZZ"}"#).is_err());
    }

    #[test]
    fn mixed_currencies_are_refused() {
        let a = Money::new(1, Currency::GBP);
        let b = Money::new(1, Currency::EUR);
        assert!(matches!(
            a.checked_add(b),
            Err(MoneyError::CurrencyMismatch { .. })
        ));
    }

    #[test]
    fn overflow_is_an_error_not_a_wrap() {
        let max = Money::new(i64::MAX, Currency::GBP);
        assert_eq!(
            max.checked_add(Money::new(1, Currency::GBP)),
            Err(MoneyError::Overflow)
        );
        assert_eq!(
            Money::new(i64::MIN, Currency::GBP).checked_neg(),
            Err(MoneyError::Overflow)
        );
    }

    proptest! {
        #[test]
        fn add_then_subtract_returns_the_start(a in -1_000_000_000_000i64..1_000_000_000_000, b in -1_000_000_000_000i64..1_000_000_000_000) {
            let (a, b) = (Money::new(a, Currency::GBP), Money::new(b, Currency::GBP));
            prop_assert_eq!(a.checked_add(b).unwrap().checked_sub(b).unwrap(), a);
        }
    }
}
