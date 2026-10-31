//! Figures a month at a time, for charts: net worth's two arms at the end of
//! each month, each income and expense account's flow within it, and one
//! subtree's own balance. Each walks the postings once in date order, so a
//! long history costs one pass, not one per point.
//!
//! Unlike [`net_worth_over_time`](crate::query::net_worth_over_time), a
//! currency with no rate does not fail the report: its amount is kept apart in
//! [`Valued::unpriced`], so a page can say the figure is incomplete.

use std::{
    collections::BTreeMap,
    ops::{AddAssign, Neg},
};

use chrono::NaiveDate;
use ledger::valuation::AtCost;
use ledger_types::currency::Currency;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::query::{
    counts, generate_periods, in_subtree, period_start_of, rate, AccountType, Grain, LedgerData,
    PostingEntry, PriceTable,
};

/// A sum in a base currency, with the amounts that had no rate kept apart
/// rather than taken at 1:1.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Valued {
    pub value: Decimal,
    /// Per currency; an entry that nets to zero needs no rate and is dropped.
    pub unpriced: BTreeMap<Currency, Decimal>,
}

impl Valued {
    /// `amount` at `rate`, or kept apart for want of one. Zero needs no rate.
    pub fn of(amount: Decimal, currency: Currency, rate: Option<Decimal>) -> Self {
        match (amount.is_zero(), rate) {
            (true, _) => Valued::default(),
            (false, Some(rate)) => Valued { value: amount * rate, unpriced: BTreeMap::new() },
            (false, None) => {
                Valued { value: Decimal::ZERO, unpriced: BTreeMap::from([(currency, amount)]) }
            }
        }
    }

    /// Every amount had a rate.
    pub fn is_complete(&self) -> bool { self.unpriced.is_empty() }
}

impl AddAssign<&Valued> for Valued {
    fn add_assign(&mut self, other: &Valued) {
        self.value += other.value;
        for (&currency, &amount) in &other.unpriced {
            let sum = *self.unpriced.entry(currency).or_default() + amount;
            match sum.is_zero() {
                true => self.unpriced.remove(&currency),
                false => self.unpriced.insert(currency, sum),
            };
        }
    }
}

impl std::iter::Sum for Valued {
    fn sum<I: Iterator<Item = Valued>>(iter: I) -> Self {
        iter.fold(Valued::default(), |mut total, v| {
            total += &v;
            total
        })
    }
}

impl<'a> std::iter::Sum<&'a Valued> for Valued {
    fn sum<I: Iterator<Item = &'a Valued>>(iter: I) -> Self {
        iter.fold(Valued::default(), |mut total, v| {
            total += v;
            total
        })
    }
}

impl std::ops::Mul<Decimal> for Valued {
    type Output = Valued;

    fn mul(self, by: Decimal) -> Valued {
        Valued {
            value: self.value * by,
            unpriced: self.unpriced.into_iter().map(|(c, a)| (c, a * by)).collect(),
        }
    }
}

impl std::ops::Sub<&Valued> for &Valued {
    type Output = Valued;

    fn sub(self, other: &Valued) -> Valued {
        let mut difference = self.clone();
        difference += &-other.clone();
        difference
    }
}

impl Neg for Valued {
    type Output = Valued;

    fn neg(self) -> Valued {
        Valued {
            value: -self.value,
            unpriced: self.unpriced.into_iter().map(|(c, a)| (c, -a)).collect(),
        }
    }
}

/// Net worth's arms at the end of one month, each converted at that day's
/// rate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorthPoint {
    pub date: NaiveDate,
    pub assets: Valued,
    /// Credit-negative, as Beancount signs it.
    pub liabilities: Valued,
    /// Holdings carried at cost, which no total includes.
    pub at_cost: Valued,
}

impl WorthPoint {
    pub fn net(&self) -> Valued { [&self.assets, &self.liabilities].into_iter().sum() }
}

/// Each income and expense account's flow within one month, keyed by path,
/// signed as Beancount signs it (income negative). Each posting is converted
/// at its own date's rate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Flows {
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub accounts: BTreeMap<String, Valued>,
}

impl LedgerData {
    /// The first posting's date, where a history starts.
    pub fn first_date(&self) -> Option<NaiveDate> { self.postings.iter().map(|p| p.date).min() }

    /// `root` and its subtree's balance in `currency` at the end of `as_of`, as
    /// a statement counts it: a leg moves the balance when it posted, not on
    /// its transaction's date.
    pub fn statement_balance(&self, root: &str, currency: Currency, as_of: NaiveDate) -> Decimal {
        self.postings
            .iter()
            .filter(|p| {
                p.currency == currency
                    && p.statement_date() <= as_of
                    && in_subtree(&self.accounts[&p.account_id].path, root)
            })
            .map(|p| p.amount)
            .sum()
    }

    /// The same ledger held to `root`'s subtree: every report over it is
    /// that subtree's, and costs a pass over its postings only.
    pub fn subtree(&self, root: &str) -> LedgerData {
        LedgerData {
            accounts: self.accounts.clone(),
            postings: self
                .postings
                .iter()
                .filter(|p| in_subtree(&self.accounts[&p.account_id].path, root))
                .cloned()
                .collect(),
        }
    }

    /// Assets and liabilities at the end of each month over `[start, end]`
    /// (the last point on `end` itself), net worth's as
    /// [`in_net_worth`](crate::query::in_net_worth) counts it and what it
    /// leaves out as carried at cost.
    pub fn worth_points(
        &self,
        base: Currency,
        at_cost: &AtCost,
        start: NaiveDate,
        end: NaiveDate,
        prices: &PriceTable,
    ) -> Vec<WorthPoint> {
        let held = |p: &PostingEntry| {
            matches!(
                self.accounts[&p.account_id].account_type,
                AccountType::Asset | AccountType::Liability
            )
        };
        self.at_period_ends(start, end, held, |date, sums| {
            // Per currency first, as the balance sheet converts a sum; an
            // account carried at cost goes to its own arm.
            let mut arms: BTreeMap<(Option<AccountType>, Currency), Decimal> = BTreeMap::new();
            for (&(id, currency), &amount) in sums {
                let meta = &self.accounts[&id];
                let arm =
                    counts(meta.account_type, &meta.path, at_cost).then_some(meta.account_type);
                *arms.entry((arm, currency)).or_default() += amount;
            }
            let mut point = WorthPoint {
                date,
                assets: Valued::default(),
                liabilities: Valued::default(),
                at_cost: Valued::default(),
            };
            for ((arm, currency), amount) in arms {
                let valued = Valued::of(amount, currency, rate(currency, base, date, prices));
                match arm {
                    Some(AccountType::Asset) => point.assets += &valued,
                    Some(AccountType::Liability) => point.liabilities += &valued,
                    _ => point.at_cost += &valued,
                }
            }
            point
        })
    }

    /// Each income and expense account's flow per month over `[start, end]`,
    /// the first and last months clamped to it. Quiet months are listed
    /// with no accounts.
    pub fn flows(
        &self,
        base: Currency,
        start: NaiveDate,
        end: NaiveDate,
        prices: &PriceTable,
    ) -> Vec<Flows> {
        // By account id while summing, so a path is cloned once per period and
        // account rather than once per posting.
        let mut sums: BTreeMap<NaiveDate, BTreeMap<i64, Valued>> = BTreeMap::new();
        for p in self.postings.iter().filter(|p| p.date >= start && p.date <= end) {
            if !matches!(
                self.accounts[&p.account_id].account_type,
                AccountType::Income | AccountType::Expense
            ) {
                continue;
            }
            let valued = Valued::of(p.amount, p.currency, rate(p.currency, base, p.date, prices));
            *sums
                .entry(period_start_of(p.date, Grain::Month))
                .or_default()
                .entry(p.account_id)
                .or_default() += &valued;
        }
        generate_periods(start, end, Grain::Month)
            .into_iter()
            .map(|(from, to)| Flows {
                start: from.max(start),
                end: to.min(end),
                accounts: sums
                    .remove(&from)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(id, v)| (self.accounts[&id].path.clone(), v))
                    .collect(),
            })
            .collect()
    }

    /// `root` and its subtree's balance per currency at the end of each
    /// month over `[start, end]`, unconverted: the figures its statements
    /// show.
    pub fn subtree_over_time(
        &self,
        root: &str,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Vec<(NaiveDate, BTreeMap<Currency, Decimal>)> {
        let under = |p: &PostingEntry| in_subtree(&self.accounts[&p.account_id].path, root);
        self.at_period_ends(start, end, under, |date, sums| {
            let mut own: BTreeMap<Currency, Decimal> = BTreeMap::new();
            for (&(_, currency), &amount) in sums {
                *own.entry(currency).or_default() += amount;
            }
            (date, own)
        })
    }

    /// Calls `at` with the running per-(account, currency) sums of the kept
    /// postings at each month's end — every posting up to that day, not only
    /// those since `start`.
    fn at_period_ends<T>(
        &self,
        start: NaiveDate,
        end: NaiveDate,
        keep: impl Fn(&PostingEntry) -> bool,
        mut at: impl FnMut(NaiveDate, &BTreeMap<(i64, Currency), Decimal>) -> T,
    ) -> Vec<T> {
        let mut kept: Vec<&PostingEntry> = self.postings.iter().filter(|p| keep(p)).collect();
        kept.sort_by_key(|p| p.date);
        let mut kept = kept.into_iter().peekable();
        let mut sums: BTreeMap<(i64, Currency), Decimal> = BTreeMap::new();
        generate_periods(start, end, Grain::Month)
            .into_iter()
            .map(|(_, period_end)| {
                let date = period_end.min(end);
                while let Some(p) = kept.next_if(|p| p.date <= date) {
                    *sums.entry((p.account_id, p.currency)).or_default() += p.amount;
                }
                at(date, &sums)
            })
            .collect()
    }
}
