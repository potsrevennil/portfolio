//! Pairing the two halves of a transfer between two of the owner's own
//! statement accounts, so it is booked as one transaction from the sending
//! side, on the earlier half's day. The freeze and the importer pair by this
//! one rule.

use chrono::NaiveDate;

use super::bank::{BankStatement, StatementLine};

/// How far apart a transfer's two halves may land: a weekend and a holiday.
pub const MAX_DAYS_APART: i64 = 7;

/// One line as its statement shows it.
#[derive(Clone, Copy)]
pub struct Half<'a> {
    pub statement: &'a BankStatement,
    pub line: &'a StatementLine,
}

impl Half<'_> {
    /// Days between this half and `other`, for taking the nearest first.
    pub fn days_from(&self, other: &Half) -> i64 {
        (self.line.book_date - other.line.book_date).num_days().abs()
    }

    /// Whether `receive` is the far half of this sending half: another
    /// account in the same currency, the opposite amount, near enough, and
    /// booking both on [`Self::booked_with`] leaves every balance stated true.
    /// The later half moves back to the earlier day, so its statement must
    /// state no balance from that day up to its own; halves that would move
    /// across one keep going through the clearing account.
    pub fn pairs_with(&self, receive: &Half) -> bool {
        let (early, late) = match self.line.book_date <= receive.line.book_date {
            true => (self, receive),
            false => (receive, self),
        };
        let (from, to) = (early.line.book_date, late.line.book_date);
        receive.statement.account_no != self.statement.account_no
            && receive.statement.currency == self.statement.currency
            && receive.line.delta() == -self.line.delta()
            && self.days_from(receive) <= MAX_DAYS_APART
            && !stated_days(late.statement).any(|d| from <= d && d < to)
    }

    /// The day a pair is booked on: the earlier half's.
    pub fn booked_with(&self, other: &Half) -> NaiveDate {
        self.line.book_date.min(other.line.book_date)
    }
}

/// The days a statement states a balance for, each holding through the end
/// of that day: every day it has a line on (the running balance, which the
/// importer checks) and the figures it vouches for.
fn stated_days(statement: &BankStatement) -> impl Iterator<Item = NaiveDate> + '_ {
    let vouched = statement.assertions("", statement.opening_date());
    let vouched: Vec<NaiveDate> = vouched.iter().flat_map(|a| a.points().map(|(d, _)| d)).collect();
    statement.lines.iter().map(|l| l.book_date).chain(vouched)
}

#[cfg(test)]
mod tests;
