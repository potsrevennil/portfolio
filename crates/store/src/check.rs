//! The invariant gate: every balance assertion must equal the balance the
//! postings compute, per account, currency and period. Run after the journal
//! load and after every import, inside the writer's transaction, so a drifting
//! ledger never commits.

use std::{collections::BTreeMap, fmt};

use anyhow::Result;
use chrono::NaiveDate;
use rust_decimal::Decimal;
use sqlx::SqliteConnection;

use super::{
    assertions::{self, BalanceAssertion},
    query::{in_subtree, AccountBalance, AccountType, LedgerData},
};
use crate::currency::Currency;

/// One figure that disagrees with the postings.
#[derive(Debug, Clone, PartialEq)]
pub struct Mismatch {
    pub assertion: BalanceAssertion,
    /// The day whose balance is compared: the day before `period_start` for
    /// the opening, `period_end` for the closing.
    pub as_of: NaiveDate,
    pub expected: Decimal,
    pub computed: Decimal,
}

/// A balance no assertion vouches for up to its newest posting.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Unchecked {
    pub account: String,
    pub currency: Currency,
    /// `None` when nothing ever vouched for it.
    pub vouched_through: Option<NaiveDate>,
    pub posted_through: NaiveDate,
}

/// How one account fared, in balance figures: a statement period states two,
/// an opening and a closing.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AccountSummary {
    pub checked: usize,
    pub failed: usize,
    pub vouched_through: Option<NaiveDate>,
}

#[derive(Debug, Clone, Default)]
pub struct CheckReport {
    pub accounts: BTreeMap<String, AccountSummary>,
    pub mismatches: Vec<Mismatch>,
    /// Asset and liability balances no assertion covers through their newest
    /// posting: not a failure, but nothing vouches for them as they stand.
    pub unchecked: Vec<Unchecked>,
}

impl CheckReport {
    /// Green only when something vouched for the ledger and every figure it
    /// vouched for matched: with no assertions at all there is nothing to
    /// drift against, which is a failure, not a pass.
    pub fn ok(&self) -> bool { self.mismatches.is_empty() && self.figures() > 0 }

    /// Balance figures checked, the unit `mismatches` counts in.
    pub fn figures(&self) -> usize { self.accounts.values().map(|a| a.checked).sum() }

    pub fn failures(&self) -> &[Mismatch] { &self.mismatches }
}

impl fmt::Display for CheckReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.figures() {
            0 => writeln!(f, "check: no balance figures — nothing vouches for this ledger")?,
            n => writeln!(
                f,
                "check: {n} balance figures over {} accounts, {} failed",
                self.accounts.len(),
                self.mismatches.len()
            )?,
        }
        for (account, s) in &self.accounts {
            let status = if s.failed == 0 { "ok  " } else { "FAIL" };
            let last = s.vouched_through.map(|d| d.to_string()).unwrap_or_default();
            writeln!(
                f,
                "  {status} {account}: {}/{} through {last}",
                s.checked - s.failed,
                s.checked
            )?;
        }
        for m in &self.mismatches {
            writeln!(f, "  {m}")?;
        }
        if !self.unchecked.is_empty() {
            let listed: Vec<String> = self.unchecked.iter().map(Unchecked::to_string).collect();
            writeln!(f, "unvouched through their newest posting: {}", listed.join(", "))?;
        }
        Ok(())
    }
}

impl fmt::Display for Mismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "MISMATCH {}: at end of {} expected {}, postings sum to {} (off by {})",
            self.assertion,
            self.as_of,
            self.expected,
            self.computed,
            self.computed - self.expected
        )
    }
}

impl fmt::Display for Unchecked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} (", self.account, self.currency)?;
        match self.vouched_through {
            Some(date) => write!(f, "vouched through {date}, "),
            None => write!(f, "never vouched for, "),
        }?;
        write!(f, "posted through {})", self.posted_through)
    }
}

/// Checks every recorded assertion against the postings visible on `conn`,
/// including the caller's uncommitted writes.
pub async fn check(conn: &mut SqliteConnection) -> Result<CheckReport> {
    let data = LedgerData::load_from(conn).await?;
    let assertions = assertions::load(conn).await?;
    Ok(check_data(&data, &assertions))
}

/// [`check`], failing on any mismatch with the report as the error.
pub async fn gate(conn: &mut SqliteConnection) -> Result<CheckReport> {
    let report = check(conn).await?;
    if report.ok() {
        Ok(report)
    } else {
        anyhow::bail!("balance check failed:\n{report}")
    }
}

/// The pure part of [`check`].
pub fn check_data(data: &LedgerData, assertions: &[BalanceAssertion]) -> CheckReport {
    let mut report = CheckReport::default();
    for a in assertions {
        let points = a.points().count();
        let failed: Vec<Mismatch> = a
            .points()
            .filter_map(|(as_of, expected)| {
                let computed = subtree_balance(&data.balances_as_of(as_of), &a.account, a.currency);
                if computed == expected {
                    None
                } else {
                    Some(Mismatch { assertion: a.clone(), as_of, expected, computed })
                }
            })
            .collect();

        let summary = report.accounts.entry(a.account.clone()).or_default();
        summary.checked += points;
        summary.failed += failed.len();
        summary.vouched_through = summary.vouched_through.max(Some(a.period_end));
        report.mismatches.extend(failed);
    }

    let last_posting = data.last_posting_dates();
    report.unchecked = data
        .balances_as_of(NaiveDate::MAX)
        .into_iter()
        .filter(|b| matches!(b.account_type, AccountType::Asset | AccountType::Liability))
        .filter_map(|b| {
            let posted_through = *last_posting.get(&(b.account_id, b.currency))?;
            let vouched_through = vouched_through(assertions, &b.path, b.currency);
            match vouched_through {
                Some(date) if date >= posted_through => None,
                _ => Some(Unchecked {
                    account: b.path,
                    currency: b.currency,
                    vouched_through,
                    posted_through,
                }),
            }
        })
        .collect();
    report
}

/// An assertion on an account covers its subtree, as a Beancount `balance`
/// does.
fn subtree_balance(balances: &[AccountBalance], root: &str, currency: Currency) -> Decimal {
    balances
        .iter()
        .filter(|b| b.currency == currency && in_subtree(&b.path, root))
        .map(|b| b.amount)
        .sum()
}

/// The newest date an assertion vouches for this balance. Only the currency it
/// names counts — a 外幣 account asserted in USD says nothing about its JPY.
fn vouched_through(
    assertions: &[BalanceAssertion],
    path: &str,
    currency: Currency,
) -> Option<NaiveDate> {
    assertions
        .iter()
        .filter(|a| a.currency == currency && in_subtree(path, &a.account))
        .map(|a| a.period_end)
        .max()
}
