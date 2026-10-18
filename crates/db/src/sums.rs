//! Amounts summed per currency, in `rust_decimal`: SQL `SUM` would CAST the
//! exact decimal strings to REAL.

use std::{collections::BTreeMap, fmt};

use ledger_types::currency::Currency;
use rust_decimal::Decimal;

/// The non-zero sums, per currency.
#[derive(Debug, Default, PartialEq)]
pub struct Sums(BTreeMap<Currency, Decimal>);

impl Sums {
    pub fn is_empty(&self) -> bool { self.0.is_empty() }

    pub fn iter(&self) -> impl Iterator<Item = (Currency, Decimal)> + '_ {
        self.0.iter().map(|(c, b)| (*c, *b))
    }
}

impl FromIterator<(Currency, Decimal)> for Sums {
    fn from_iter<I: IntoIterator<Item = (Currency, Decimal)>>(amounts: I) -> Self {
        let mut sums: BTreeMap<Currency, Decimal> = BTreeMap::new();
        for (currency, amount) in amounts {
            *sums.entry(currency).or_default() += amount;
        }
        sums.retain(|_, b| !b.is_zero());
        Sums(sums)
    }
}

impl fmt::Display for Sums {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let amounts: Vec<String> =
            self.0.iter().map(|(c, b)| format!("{} {c}", b.normalize())).collect();
        f.write_str(&amounts.join("、"))
    }
}
