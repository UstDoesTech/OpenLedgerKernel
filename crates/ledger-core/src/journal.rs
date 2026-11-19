//! Journals: balanced sets of postings.
//!
//! Sign convention: a positive posting is a debit and a negative posting is a credit.
//! A journal exists only if its postings net to zero in every currency, so a journal that
//! does not balance is unrepresentable.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{AccountId, Currency, JournalId, Money, MoneyError};

/// One leg of a journal: an account and a signed amount.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Posting {
    pub account: AccountId,
    pub amount: Money,
}

impl Posting {
    pub fn new(account: AccountId, amount: Money) -> Self {
        Posting { account, amount }
    }
}

/// Why a journal was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JournalError {
    #[error("a journal needs at least two postings")]
    TooFewPostings,
    #[error("a posting of zero moves nothing")]
    ZeroPosting,
    #[error("{currency} postings net to {net}, not zero")]
    Unbalanced { currency: Currency, net: i128 },
    #[error(transparent)]
    Money(#[from] MoneyError),
}

/// A set of postings that net to zero in every currency.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawJournal")]
pub struct Journal {
    id: JournalId,
    postings: Vec<Posting>,
}

#[derive(Deserialize)]
struct RawJournal {
    id: JournalId,
    postings: Vec<Posting>,
}

impl TryFrom<RawJournal> for Journal {
    type Error = JournalError;

    fn try_from(raw: RawJournal) -> Result<Self, Self::Error> {
        Journal::new(raw.id, raw.postings)
    }
}

impl Journal {
    /// Builds a journal, or says why the postings cannot form one.
    pub fn new(id: JournalId, postings: Vec<Posting>) -> Result<Self, JournalError> {
        if postings.len() < 2 {
            return Err(JournalError::TooFewPostings);
        }
        let mut net: BTreeMap<Currency, i128> = BTreeMap::new();
        for p in &postings {
            if p.amount.is_zero() {
                return Err(JournalError::ZeroPosting);
            }
            // i128 cannot overflow when adding a realistic number of i64 values.
            let slot = net.entry(p.amount.currency()).or_insert(0);
            *slot = slot
                .checked_add(i128::from(p.amount.minor()))
                .ok_or(MoneyError::Overflow)?;
        }
        if let Some((&currency, &net)) = net.iter().find(|(_, n)| **n != 0) {
            return Err(JournalError::Unbalanced { currency, net });
        }
        Ok(Journal { id, postings })
    }

    pub fn id(&self) -> &JournalId {
        &self.id
    }

    pub fn postings(&self) -> &[Posting] {
        &self.postings
    }

    /// The journal that undoes this one: same accounts, opposite signs. Corrections are new
    /// journals, never edits.
    pub fn reversal(&self, id: JournalId) -> Result<Journal, JournalError> {
        let postings = self
            .postings
            .iter()
            .map(|p| Ok(Posting::new(p.account.clone(), p.amount.checked_neg()?)))
            .collect::<Result<Vec<_>, MoneyError>>()?;
        Journal::new(id, postings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn acc(n: usize) -> AccountId {
        format!("acc_{n}").parse().unwrap()
    }

    fn jr(s: &str) -> JournalId {
        s.parse().unwrap()
    }

    fn p(n: usize, minor: i64, c: Currency) -> Posting {
        Posting::new(acc(n), Money::new(minor, c))
    }

    #[test]
    fn a_balanced_transfer_is_accepted() {
        let j = Journal::new(
            jr("jr_1"),
            vec![p(1, 500, Currency::GBP), p(2, -500, Currency::GBP)],
        )
        .unwrap();
        assert_eq!(j.postings().len(), 2);
    }

    #[test]
    fn an_unbalanced_journal_is_refused() {
        let err = Journal::new(
            jr("jr_1"),
            vec![p(1, 500, Currency::GBP), p(2, -499, Currency::GBP)],
        )
        .unwrap_err();
        assert_eq!(
            err,
            JournalError::Unbalanced {
                currency: Currency::GBP,
                net: 1
            }
        );
    }

    #[test]
    fn too_few_or_zero_postings_are_refused() {
        assert_eq!(
            Journal::new(jr("jr_1"), vec![]),
            Err(JournalError::TooFewPostings)
        );
        assert_eq!(
            Journal::new(jr("jr_1"), vec![p(1, 5, Currency::GBP)]),
            Err(JournalError::TooFewPostings)
        );
        assert_eq!(
            Journal::new(
                jr("jr_1"),
                vec![p(1, 0, Currency::GBP), p(2, 0, Currency::GBP)]
            ),
            Err(JournalError::ZeroPosting)
        );
    }

    #[test]
    fn each_currency_must_net_to_zero_on_its_own() {
        // GBP and EUR cancel only if you wrongly add them together.
        let err = Journal::new(
            jr("jr_1"),
            vec![p(1, 100, Currency::GBP), p(2, -100, Currency::EUR)],
        )
        .unwrap_err();
        assert!(matches!(err, JournalError::Unbalanced { .. }));

        // An FX conversion balances in both currencies.
        let fx = Journal::new(
            jr("jr_2"),
            vec![
                p(1, 100, Currency::GBP),
                p(2, -100, Currency::GBP),
                p(3, 115, Currency::EUR),
                p(4, -115, Currency::EUR),
            ],
        );
        assert!(fx.is_ok());
    }

    #[test]
    fn extreme_amounts_cannot_wrap_around_to_balance() {
        let err = Journal::new(
            jr("jr_1"),
            vec![
                p(1, i64::MAX, Currency::GBP),
                p(2, i64::MAX, Currency::GBP),
                p(3, 2, Currency::GBP),
            ],
        )
        .unwrap_err();
        assert!(matches!(err, JournalError::Unbalanced { .. }));
    }

    #[test]
    fn deserialising_checks_the_balance() {
        let bad = r#"{"id":"jr_1","postings":[
            {"account":"acc_1","amount":{"minor":5,"currency":"GBP"}},
            {"account":"acc_2","amount":{"minor":-4,"currency":"GBP"}}]}"#;
        assert!(serde_json::from_str::<Journal>(bad).is_err());
        let good = bad.replace("-4", "-5");
        assert!(serde_json::from_str::<Journal>(&good).is_ok());
    }

    #[test]
    fn a_reversal_balances_and_negates() {
        let j = Journal::new(
            jr("jr_1"),
            vec![p(1, 500, Currency::GBP), p(2, -500, Currency::GBP)],
        )
        .unwrap();
        let r = j.reversal(jr("jr_2")).unwrap();
        assert_eq!(r.postings()[0].amount.minor(), -500);
        assert_eq!(r.postings()[1].amount.minor(), 500);
    }

    fn currency() -> impl Strategy<Value = Currency> {
        prop_oneof![
            Just(Currency::GBP),
            Just(Currency::EUR),
            Just(Currency::USD)
        ]
    }

    proptest! {
        /// Any set of postings built to cancel in each currency is accepted, and a journal
        /// is accepted only when the per-currency sums are zero.
        #[test]
        fn accepted_exactly_when_every_currency_nets_to_zero(
            legs in prop::collection::vec((0usize..6, -1_000_000i64..1_000_000, currency()), 0..12)
        ) {
            let postings: Vec<Posting> = legs.iter().map(|(a, m, c)| p(*a, *m, *c)).collect();
            let mut net: BTreeMap<Currency, i128> = BTreeMap::new();
            for (_, m, c) in &legs { *net.entry(*c).or_insert(0) += i128::from(*m); }
            let balanced = net.values().all(|n| *n == 0);
            let shape_ok = legs.len() >= 2 && legs.iter().all(|(_, m, _)| *m != 0);
            let result = Journal::new(jr("jr_p"), postings);
            prop_assert_eq!(result.is_ok(), balanced && shape_ok);
        }

        /// Completing any legs with one balancing posting per currency always yields a journal.
        #[test]
        fn balancing_postings_always_make_a_valid_journal(
            legs in prop::collection::vec((0usize..6, 1i64..1_000_000, currency()), 1..8)
        ) {
            let mut postings: Vec<Posting> = legs.iter().map(|(a, m, c)| p(*a, *m, *c)).collect();
            let mut net: BTreeMap<Currency, i64> = BTreeMap::new();
            for (_, m, c) in &legs { *net.entry(*c).or_insert(0) += *m; }
            for (c, n) in net { postings.push(p(9, -n, c)); }
            prop_assert!(Journal::new(jr("jr_p"), postings).is_ok());
        }

        /// A reversal of any valid journal is itself valid and cancels the original.
        #[test]
        fn reversals_cancel(
            legs in prop::collection::vec((0usize..6, 1i64..1_000_000, currency()), 1..8)
        ) {
            let mut postings: Vec<Posting> = legs.iter().map(|(a, m, c)| p(*a, *m, *c)).collect();
            let mut net: BTreeMap<Currency, i64> = BTreeMap::new();
            for (_, m, c) in &legs { *net.entry(*c).or_insert(0) += *m; }
            for (c, n) in net { postings.push(p(9, -n, c)); }
            let j = Journal::new(jr("jr_a"), postings).unwrap();
            let r = j.reversal(jr("jr_b")).unwrap();
            for (a, b) in j.postings().iter().zip(r.postings()) {
                prop_assert_eq!(a.amount.checked_add(b.amount).unwrap().minor(), 0);
            }
        }
    }
}
