//! A balance someone outside the ledger vouches for: a statement's closing,
//! 天天記帳's own balance, or a counted one. Freeze writes them, the database
//! keeps them, and `check` holds the ledger to them.

use std::fmt;

use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use strum_macros::{Display, EnumString};

use crate::Currency;

/// Who vouches for the figure.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Display, EnumString,
)]
#[strum(serialize_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum AssertionSource {
    /// An institution's statement.
    Statement,
    /// 天天記帳's own closing balance, for accounts it was the record of.
    Tiantian,
    /// A balance the user counted (cash).
    Counted,
}

/// One assertion. Dates are inclusive: `opening` is the balance before
/// `period_start`, `closing` the balance at the end of `period_end`. Both cover
/// `account` and its subtree in `currency`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BalanceAssertion {
    pub source: AssertionSource,
    pub account: String,
    pub currency: Currency,
    pub period_start: Option<NaiveDate>,
    pub opening: Option<Decimal>,
    pub period_end: NaiveDate,
    pub closing: Decimal,
}

impl fmt::Display for BalanceAssertion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} ({} ", self.account, self.currency, self.source)?;
        match self.period_start {
            Some(start) => write!(f, "period {start}..={})", self.period_end),
            None => write!(f, "as of {})", self.period_end),
        }
    }
}

impl BalanceAssertion {
    /// The (day, balance at the end of it) pairs this states: the opening, if
    /// any, then the closing.
    pub fn points(&self) -> impl Iterator<Item = (NaiveDate, Decimal)> {
        let opening = self.period_start.zip(self.opening).map(|(start, opening)| {
            (start.pred_opt().expect("a day before period_start"), opening)
        });
        opening.into_iter().chain([(self.period_end, self.closing)])
    }
}
