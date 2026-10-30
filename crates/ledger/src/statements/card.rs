//! What a card statement says, whichever bank printed it: what was owed at the
//! last closing, what is owed at this one, and the lines between, each on the
//! day it posted. A statement's closing is exactly what has posted by then:
//! a purchase still to post is on the next one, and only the parts an
//! instalment plan has billed are in it.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::{ensure, Context, Result};
use chrono::{Datelike, NaiveDate};
use ledger_types::{assertion::BalanceAssertion, currency::Currency};
use rust_decimal::Decimal;

use super::bank::{Bank, BankStatement, Merged, Period, StatementLine};

/// One line as a card statement prints it.
#[derive(Debug, PartialEq)]
pub struct Line {
    /// `None` where the statement gives none, or the purchase is a plan's.
    pub purchased: Option<NaiveDate>,
    pub posted: NaiveDate,
    pub description: String,
    /// What it adds to the bill; a payment or refund is negative.
    pub amount: Decimal,
    /// Where it was spent and in what, for a purchase abroad.
    pub detail: String,
}

/// One statement. Amounts are what the bank says is owed, positive.
#[derive(Debug, PartialEq)]
pub struct Bill {
    pub closes: NaiveDate,
    pub opening: Decimal,
    pub closing: Decimal,
    pub lines: Vec<Line>,
}

/// A 第 n/m 期 line bills a part of a plan on its posting date; the purchase
/// it prints is the whole plan's, months back.
pub fn is_instalment(description: &str) -> bool {
    description.split('第').skip(1).any(|rest| {
        rest.split_once('期').is_some_and(|(parts, _)| {
            parts.split_once('/').is_some_and(|(n, m)| {
                let digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
                digits(n) && digits(m)
            })
        })
    })
}

/// A statement's month count, for telling whether two are consecutive.
fn month(date: NaiveDate) -> i32 { date.year() * 12 + date.month0() as i32 }

impl Bill {
    /// The statement in the ledger's terms: a liability's balance is negative,
    /// and lines run in posting order. Fails unless the lines add up to the
    /// closing and all posted by it, so a line the parser missed cannot pass.
    pub fn into_statement(self, bank: Bank, account_no: &str) -> Result<BankStatement> {
        let moved: Decimal = self.lines.iter().map(|l| l.amount).sum();
        ensure!(
            self.opening + moved == self.closing,
            "the statement closing {} opens at {} and its lines add {moved}, not the {} it closes \
             at",
            self.closes,
            self.opening,
            self.closing
        );
        if let Some(late) = self.lines.iter().find(|l| l.posted > self.closes) {
            anyhow::bail!(
                "{} posted {}, after the statement closing {}",
                late.description,
                late.posted,
                self.closes
            );
        }
        let mut lines = self.lines;
        lines.sort_by_key(|l| l.posted);
        // Unknown until the statement before is seen; the first day it moved
        // is the latest it can be.
        let start = lines.first().map_or(self.closes, |l| l.posted);
        let mut balance = -self.opening;
        let lines = lines
            .into_iter()
            .map(|l| {
                balance -= l.amount;
                let (withdrawal, deposit) = match l.amount.is_sign_negative() {
                    true => (Decimal::ZERO, -l.amount),
                    false => (l.amount, Decimal::ZERO),
                };
                StatementLine {
                    book_date: l.posted,
                    purchased: l.purchased,
                    description: l.description,
                    withdrawal,
                    deposit,
                    balance,
                    info: l.detail,
                    memo: String::new(),
                }
            })
            .collect();
        Ok(BankStatement {
            bank,
            account_no: account_no.to_string(),
            account_kind: account_no.to_string(),
            currency: Currency::TWD,
            period_end: Some(self.closes),
            periods: vec![Period {
                start,
                end: self.closes,
                opening: -self.opening,
                closing: -self.closing,
            }],
            lines,
        })
    }
}

/// The card a statement is of: its folder's name, as `[institution.accounts]`
/// keys it.
pub fn folder_card(path: &Path) -> Result<&str> {
    path.parent()
        .and_then(Path::file_name)
        .and_then(|f| f.to_str())
        .with_context(|| format!("{} is not in a card's folder", path.display()))
}

/// Did `closing` on `closed` lead straight into a statement ending `end` that
/// opens at `opening`? Then its period began the day after: months apart, and
/// a statement is missing between them.
pub fn follows(closed: NaiveDate, closing: Decimal, end: NaiveDate, opening: Decimal) -> bool {
    month(end) - month(closed) == 1 && closing == opening
}

/// [`follows`] for a figure and those already recorded: the day its period
/// began, when the latest before it is the statement it follows.
pub fn started_after(a: &BalanceAssertion, recorded: &[BalanceAssertion]) -> Option<NaiveDate> {
    let last = recorded
        .iter()
        .filter(|r| {
            (&r.account, r.currency, r.source) == (&a.account, a.currency, a.source)
                && r.period_end < a.period_end
        })
        .max_by_key(|r| r.period_end)?;
    let opening = a.opening?;
    follows(last.period_end, last.closing, a.period_end, opening)
        .then(|| last.period_end.succ_opt().expect("a day after a statement"))
}

/// Joins each card's statements. Consecutive months must carry the balance
/// over; a missing month is allowed and stays a gap between periods, for
/// 對帳進度 to show.
pub fn merge(statements: Vec<(BankStatement, PathBuf)>) -> Result<Vec<Merged>> {
    let mut groups: BTreeMap<String, Vec<(BankStatement, PathBuf)>> = BTreeMap::new();
    for (s, path) in statements {
        groups.entry(s.account_no.clone()).or_default().push((s, path));
    }
    let mut merged = Vec::new();
    for (card, mut parts) in groups {
        parts.sort_by_key(|(s, _)| s.periods[0].end);
        let mut parts = parts.into_iter();
        let (mut statement, first) = parts.next().expect("a group has a member");
        let mut spans = vec![statement.lines.len()];
        let mut paths = vec![first];
        for (mut next, path) in parts {
            let before = statement.periods.last().expect("issued");
            let after = &mut next.periods[0];
            ensure!(
                before.end != after.end,
                "{card}: {} repeats the statement closing {}",
                path.display(),
                after.end
            );
            if month(after.end) - month(before.end) == 1 {
                ensure!(
                    follows(before.end, before.closing, after.end, after.opening),
                    "{card}: {} opens at {}, but the month before closed at {}",
                    path.display(),
                    after.opening,
                    before.closing
                );
                after.start = before.end.succ_opt().expect("a day after a statement");
            }
            spans.push(next.lines.len());
            paths.push(path);
            statement.period_end = next.period_end;
            statement.periods.extend(next.periods);
            statement.lines.extend(next.lines);
        }
        merged.push(Merged { statement, paths, spans });
    }
    Ok(merged)
}

#[cfg(test)]
mod tests;
