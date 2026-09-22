//! What a bank statement is once parsed, whichever bank printed it. The
//! per-bank rules (ref namespace, how a line names an account, what undoes a
//! debit, how far a statement vouches) are [`Bank`]'s.

use std::{fmt, path::PathBuf};

use anyhow::Result;
use chrono::NaiveDate;
use ledger_types::{
    assertion::{AssertionSource, BalanceAssertion},
    currency::Currency,
};
use rust_decimal::Decimal;

use super::{cathay, line_bank};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bank {
    Cathay,
    LineBank,
}

impl fmt::Display for Bank {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Bank::Cathay => "Cathay bank",
            Bank::LineBank => "LINE Bank",
        })
    }
}

impl Bank {
    pub const ALL: [Bank; 2] = [Bank::Cathay, Bank::LineBank];

    /// Namespaces the bank's `external_ref`s: every importer shares one dedup
    /// scope (`transactions.source = 'import'`).
    pub fn ref_prefix(self) -> &'static str {
        match self {
            Bank::Cathay => "cathay-bank:",
            Bank::LineBank => "line-bank:",
        }
    }

    /// What its imports are recorded as in `import_batch`.
    pub fn source(self) -> &'static str {
        match self {
            Bank::Cathay => "cathay-bank",
            Bank::LineBank => "line-bank",
        }
    }

    /// Reads `paths` as this bank's statements, joined per account and
    /// currency.
    pub fn load_merged(self, paths: &[PathBuf]) -> Result<Vec<Merged>> {
        match self {
            Bank::Cathay => cathay::load_merged(paths),
            Bank::LineBank => line_bank::load_merged(paths),
        }
    }

    /// Does a line's text name `account_no`, in this bank's way of writing one?
    pub fn names_account(self, text: &str, account_no: &str) -> bool {
        match self {
            Bank::Cathay => cathay::info_names_account(text, account_no),
            Bank::LineBank => line_bank::remark_names_account(text, account_no),
        }
    }

    /// Is `line` the bank undoing the earlier `debit`?
    fn cancels(self, debit: &StatementLine, line: &StatementLine) -> bool {
        let same_day = debit.book_date == line.book_date;
        match self {
            // A 錯誤更正 row carries a negative withdrawal.
            Bank::Cathay => {
                same_day
                    && line.withdrawal.is_sign_negative()
                    && debit.info == line.info
                    && debit.withdrawal == -line.withdrawal
            }
            Bank::LineBank => {
                same_day
                    && line.description == line_bank::CANCEL
                    && line.info.strip_prefix(line_bank::CANCEL_REMARK) == Some(&debit.info)
                    && debit.delta().is_sign_negative()
                    && debit.delta() == -line.delta()
            }
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct StatementLine {
    pub book_date: NaiveDate,
    pub description: String,
    pub withdrawal: Decimal,
    pub deposit: Decimal,
    pub balance: Decimal,
    pub info: String,
    pub memo: String,
}

impl StatementLine {
    /// Signed effect on the account. `withdrawal` can be negative on 錯誤更正
    /// (error-correction) rows, which reverse an earlier debit.
    pub fn delta(&self) -> Decimal { self.deposit - self.withdrawal }
}

/// A period a statement states its own opening and closing for.
#[derive(Debug, Clone, PartialEq)]
pub struct Period {
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub opening: Decimal,
    pub closing: Decimal,
}

#[derive(Debug)]
pub struct BankStatement {
    pub bank: Bank,
    pub account_no: String,
    pub account_kind: String,
    pub currency: Currency,
    /// End of the period the statement covers.
    pub period_end: Option<NaiveDate>,
    /// Issued statements, each vouching for its own period; empty for a
    /// download, whose one period runs from its first line.
    pub periods: Vec<Period>,
    /// Oldest first. Empty only for an issued statement with no activity.
    pub lines: Vec<StatementLine>,
}

impl BankStatement {
    /// Balance before the first line, reconstructed from the oldest row.
    pub fn opening_balance(&self) -> Decimal {
        match (self.lines.first(), self.periods.first()) {
            (Some(l), _) => l.balance - l.delta(),
            (None, Some(p)) => p.opening,
            (None, None) => Decimal::ZERO,
        }
    }

    pub fn closing_balance(&self) -> Decimal {
        match (self.lines.last(), self.periods.last()) {
            (_, Some(p)) => p.closing,
            (Some(l), None) => l.balance,
            (None, None) => Decimal::ZERO,
        }
    }

    /// The first day the statement covers.
    pub fn start(&self) -> NaiveDate {
        match self.periods.first() {
            Some(p) => p.start,
            None => self.lines.first().expect("load rejects empty downloads").book_date,
        }
    }

    /// Whether a line naming this account on `date` has its far side here.
    /// A download says nothing of the days around it, so it is taken to
    /// cover them; an issued statement covers its periods.
    pub fn covers(&self, date: NaiveDate) -> bool {
        match (self.periods.first(), self.periods.last()) {
            (Some(first), Some(last)) => first.start <= date && date <= last.end,
            _ => true,
        }
    }

    /// The day an opening transaction is dated: before the first stated
    /// period, else before the first line.
    pub fn opening_date(&self) -> NaiveDate {
        self.start().pred_opt().expect("a day before a statement")
    }

    /// The `external_ref` of an account's opening, so a second one is a
    /// duplicate.
    pub fn opening_ref(&self, account: &str) -> String {
        format!("{}opening:{account}:{}", self.bank.ref_prefix(), self.currency)
    }

    pub fn names_account(&self, text: &str, account_no: &str) -> bool {
        self.bank.names_account(text, account_no)
    }

    /// Dedup key per line, shared by the freeze bake and importers.
    ///
    /// Not the line index: statements get re-split per year, which shifts
    /// indices. Same-day out-and-back sequences (−X, +X, −X) repeat the running
    /// balance, so repeats of an identical key get `:2`, `:3`. The suffix is
    /// counted from the first line of the day this statement holds, so a
    /// statement starting mid-day numbers them differently; the importer's
    /// balance-chain check is what catches that.
    pub fn dedup_refs(&self) -> Vec<String> {
        let mut seen: std::collections::HashMap<String, usize> = Default::default();
        self.lines
            .iter()
            .map(|l| {
                let base = format!(
                    "{}{}:{}:{}:{}",
                    self.bank.ref_prefix(),
                    self.account_no,
                    l.book_date,
                    l.delta(),
                    l.balance
                );
                let n = seen.entry(base.clone()).or_default();
                *n += 1;
                if *n == 1 {
                    base
                } else {
                    format!("{base}:{n}")
                }
            })
            .collect()
    }

    /// Debits the bank itself undid, as `(debit, reversal)` line indices.
    ///
    /// Both lines are real and both are kept; they are paired so neither
    /// reaches the matcher, where an app record the same size would otherwise
    /// be spent on a movement that never happened. A reversal with no such
    /// debit is left unpaired and falls through like any other line.
    pub fn reversals(&self) -> Vec<(usize, usize)> {
        let mut used = vec![false; self.lines.len()];
        let mut pairs = Vec::new();
        for (ri, reversal) in self.lines.iter().enumerate() {
            let debit =
                (0..ri).rev().find(|&di| !used[di] && self.bank.cancels(&self.lines[di], reversal));
            if let Some(di) = debit {
                used[di] = true;
                used[ri] = true;
                pairs.push((di, ri));
            }
        }
        pairs
    }

    /// Drops lines before `date` (their balance becomes the opening); returns
    /// how many.
    pub fn trim_before(&mut self, date: NaiveDate) -> usize {
        let keep = self.lines.partition_point(|l| l.book_date < date);
        self.lines.drain(..keep).count()
    }

    /// The first day this statement may not hold in full.
    fn unsettled_from(&self) -> Option<NaiveDate> {
        match self.bank {
            // A download may have been made partway through its range's end.
            Bank::Cathay => self.period_end,
            Bank::LineBank => self.period_end.and_then(|end| end.succ_opt()),
        }
    }

    /// Date to assert the closing balance on. Beancount asserts at the start of
    /// the day, so this must fall after the final transaction.
    pub fn assert_date(&self) -> NaiveDate {
        let after_last = self.lines.last().and_then(|l| l.book_date.succ_opt());
        match (self.unsettled_from(), after_last) {
            (Some(end), Some(after)) if end >= after => end,
            (Some(end), None) => end,
            (_, after) => after.unwrap_or_default(),
        }
    }

    /// The last day this statement holds in full. A Cathay TWD download may
    /// have been made partway through the end of its stated range, so the day
    /// before. A 外幣 download states no range; its last line's balance is
    /// taken as current (the user's call until downloads carry a date), so a
    /// second download made later that same day is refused as contradicting
    /// it.
    pub fn settled_through(&self) -> NaiveDate {
        match self.unsettled_from() {
            Some(from) => from.pred_opt().expect("a day before a range end"),
            None => self.lines.last().expect("load rejects empty statements").book_date,
        }
    }

    /// The statement's figures for `account`, over what it holds after
    /// `after` (earlier lines are in the account's opening). An issued
    /// statement vouches for each of its periods; one that began on or before
    /// `after` only for its closing.
    pub fn assertions(&self, account: &str, after: NaiveDate) -> Vec<BalanceAssertion> {
        let assertion = |period_start, opening, period_end, closing| BalanceAssertion {
            source: AssertionSource::Statement,
            account: account.to_string(),
            currency: self.currency,
            period_start,
            opening,
            period_end,
            closing,
        };
        match self.periods.is_empty() {
            true => self.download_assertion(account, after).into_iter().collect(),
            false => self
                .periods
                .iter()
                .filter(|p| p.end > after)
                .map(|p| match p.start > after {
                    true => assertion(Some(p.start), Some(p.opening), p.end, p.closing),
                    false => assertion(None, None, p.end, p.closing),
                })
                .collect(),
        }
    }

    /// A download's one assertion, closing on [`Self::settled_through`].
    /// `None` when no line is after `after`. Asserting a day the download may
    /// not hold in full would refuse the next one that does.
    pub fn download_assertion(&self, account: &str, after: NaiveDate) -> Option<BalanceAssertion> {
        let from = self.lines.partition_point(|l| l.book_date <= after);
        let first = self.lines.get(from)?;
        let before = |i: usize| self.lines[i].balance - self.lines[i].delta();
        let end = self.settled_through();
        // Read off the line after `end` where there is one, so its figures
        // stay checked too: a statement that stops adding up there still fails.
        let closing = match self.lines.partition_point(|l| l.book_date <= end) {
            n if n < self.lines.len() => before(n),
            n => self.lines[n - 1].balance,
        };
        let opening = before(from);
        let (period_start, opening, period_end, closing) = if end >= first.book_date {
            (Some(first.book_date), Some(opening), end, closing)
        } else {
            // No tracked day is settled: vouch only for the balance the first
            // one started from.
            (None, None, first.book_date.pred_opt().expect("a day before a line"), opening)
        };
        Some(BalanceAssertion {
            source: AssertionSource::Statement,
            account: account.to_string(),
            currency: self.currency,
            period_start,
            opening,
            period_end,
            closing,
        })
    }
}

/// One account's statements in one currency, joined.
pub struct Merged {
    pub statement: BankStatement,
    pub paths: Vec<PathBuf>,
    /// How many of `statement.lines` each of `paths` contributed, in order.
    pub spans: Vec<usize>,
}
