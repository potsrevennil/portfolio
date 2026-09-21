//! What the server sends a page: display-ready, so the wasm client needs no
//! ledger types.

use std::fmt;

use rust_decimal::{Decimal, RoundingStrategy};
use serde::{Deserialize, Serialize};

/// An amount and what it is in. `decimals` is how many the currency is quoted
/// to — TWD whole, the rest to two — and applies to the rendering only; the
/// amount itself stays exact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Money {
    pub amount: Decimal,
    pub currency: String,
    pub decimals: u32,
}

impl Money {
    pub fn is_negative(&self) -> bool { self.rounded().is_sign_negative() && !self.is_zero() }

    fn is_zero(&self) -> bool { self.rounded().is_zero() }

    /// Half away from zero, as Fava rounds an account's own balance. A total
    /// is rounded once, from the exact sum; Fava instead adds up figures it has
    /// already rounded, so a group of halves can differ from it by one unit.
    fn rounded(&self) -> Decimal {
        self.amount.round_dp_with_strategy(self.decimals, RoundingStrategy::MidpointAwayFromZero)
    }
}

impl fmt::Display for Money {
    /// Grouped thousands, as Fava prints them.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let rounded = self.rounded().normalize();
        let text = rounded.abs().to_string();
        let (int, frac) = match text.split_once('.') {
            Some((int, frac)) => (int, Some(frac)),
            None => (text.as_str(), None),
        };
        if self.is_negative() {
            f.write_str("-")?;
        }
        for (i, digit) in int.chars().enumerate() {
            if i > 0 && (int.len() - i) % 3 == 0 {
                f.write_str(",")?;
            }
            write!(f, "{digit}")?;
        }
        if let Some(frac) = frac {
            write!(f, ".{frac}")?;
        }
        write!(f, " {}", self.currency)
    }
}

/// Amounts left out of a converted total for want of a rate. Renders as
/// nothing when there are none, so a caller can always print it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Unpriced(pub Vec<Money>);

impl fmt::Display for Unpriced {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0.split_first() {
            None => Ok(()),
            Some((first, rest)) => {
                write!(f, "（未換算：{first}")?;
                for money in rest {
                    write!(f, "、{money}")?;
                }
                f.write_str("）")
            }
        }
    }
}

/// A sum in the base currency. An amount with no rate is named rather than
/// converted at 1:1.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Converted {
    pub money: Money,
    pub unpriced: Unpriced,
}

/// One account in the tree, summing its own postings and its descendants'.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    /// Identifies the node (fold state); never shown.
    pub path: String,
    pub label: String,
    pub total: Converted,
    /// The account's own balance per currency, without its descendants',
    /// when it holds any currency other than the base: the figure its
    /// statement shows.
    pub native: Vec<Money>,
    /// A cost, not a valuation: shown, but left out of every total above it.
    pub at_cost: bool,
    /// The at-cost holdings below it that `total` leaves out.
    pub excluded: Option<Converted>,
    pub children: Vec<Node>,
}

/// 資產 or 負債.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Section {
    /// The root path, identifying the section's fold state; never shown.
    pub path: String,
    pub label: String,
    pub total: Converted,
    /// The at-cost holdings this section's total leaves out.
    pub excluded: Option<Converted>,
    pub nodes: Vec<Node>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BalanceSheet {
    pub as_of: String,
    pub base: String,
    pub sections: Vec<Section>,
    pub net_worth: Converted,
    /// The at-cost holdings net worth leaves out.
    pub excluded: Option<Converted>,
}

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;

    use super::*;

    fn money(amount: Decimal, currency: &str, decimals: u32) -> Money {
        Money { amount, currency: currency.into(), decimals }
    }

    #[test]
    fn amounts_print_like_fava() {
        assert_eq!(money(dec!(1234567.891), "USD", 2).to_string(), "1,234,567.89 USD");
        assert_eq!(money(dec!(12.50), "USD", 2).to_string(), "12.5 USD");
        assert_eq!(money(dec!(-1000), "TWD", 0).to_string(), "-1,000 TWD");
        // The base currency is quoted whole; the stored amount keeps its cents.
        assert_eq!(money(dec!(295.26), "TWD", 0).to_string(), "295 TWD");
        assert_eq!(money(dec!(838.5), "TWD", 0).to_string(), "839 TWD");
        assert_eq!(money(dec!(-14670.5), "TWD", 0).to_string(), "-14,671 TWD");
        // Rounding to nothing is not a negative amount.
        let dust = money(dec!(-0.001), "TWD", 0);
        assert_eq!(dust.to_string(), "0 TWD");
        assert!(!dust.is_negative());
    }

    #[test]
    fn unpriced_amounts_render_only_when_there_are_some() {
        assert_eq!(Unpriced::default().to_string(), "");
        let unpriced = Unpriced(vec![money(dec!(5000), "JPY", 2), money(dec!(-2), "VND", 2)]);
        assert_eq!(unpriced.to_string(), "（未換算：5,000 JPY、-2 VND）");
    }
}
