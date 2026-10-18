//! Figures over time, for charts: net worth's two arms at the end of each
//! period, each income and expense account's flow within it, and one
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

impl<'a> std::iter::Sum<&'a Valued> for Valued {
    fn sum<I: Iterator<Item = &'a Valued>>(iter: I) -> Self {
        let mut total = Valued::default();
        for v in iter {
            total += v;
        }
        total
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

/// Net worth's arms at the end of one period, each converted at that day's
/// rate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorthPoint {
    pub date: NaiveDate,
    pub assets: Valued,
    /// Credit-negative, as Beancount signs it.
    pub liabilities: Valued,
}

impl WorthPoint {
    pub fn net(&self) -> Valued {
        let mut net = self.assets.clone();
        net += &self.liabilities;
        net
    }
}

/// Each income and expense account's flow within one period, keyed by path,
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

    /// Assets and liabilities at the end of each period over `[start, end]`
    /// (the last point on `end` itself), leaving out what
    /// [`in_net_worth`](crate::query::in_net_worth) leaves out.
    pub fn worth_points(
        &self,
        base: Currency,
        at_cost: &AtCost,
        start: NaiveDate,
        end: NaiveDate,
        grain: Grain,
        prices: &PriceTable,
    ) -> Vec<WorthPoint> {
        let counted = |p: &PostingEntry| {
            let meta = &self.accounts[&p.account_id];
            counts(meta.account_type, &meta.path, at_cost)
        };
        self.at_period_ends(start, end, grain, counted, |date, sums| {
            let mut assets = Valued::default();
            let mut liabilities = Valued::default();
            // Per currency first, as the balance sheet converts a sum.
            let mut arms: BTreeMap<(AccountType, Currency), Decimal> = BTreeMap::new();
            for (&(id, currency), &amount) in sums {
                *arms.entry((self.accounts[&id].account_type, currency)).or_default() += amount;
            }
            for ((account_type, currency), amount) in arms {
                let valued = Valued::of(amount, currency, rate(currency, base, date, prices));
                match account_type {
                    AccountType::Asset => assets += &valued,
                    AccountType::Liability => liabilities += &valued,
                    AccountType::Equity | AccountType::Income | AccountType::Expense => {}
                }
            }
            WorthPoint { date, assets, liabilities }
        })
    }

    /// Each income and expense account's flow per period over `[start, end]`,
    /// the first and last periods clamped to it. Quiet periods are listed
    /// with no accounts.
    pub fn flows(
        &self,
        base: Currency,
        start: NaiveDate,
        end: NaiveDate,
        grain: Grain,
        prices: &PriceTable,
    ) -> Vec<Flows> {
        let mut periods: BTreeMap<NaiveDate, Flows> = generate_periods(start, end, grain)
            .into_iter()
            .map(|(from, to)| {
                (from, Flows {
                    start: from.max(start),
                    end: to.min(end),
                    accounts: BTreeMap::new(),
                })
            })
            .collect();
        for p in self.postings.iter().filter(|p| p.date >= start && p.date <= end) {
            let meta = &self.accounts[&p.account_id];
            if !matches!(meta.account_type, AccountType::Income | AccountType::Expense) {
                continue;
            }
            let period = periods
                .get_mut(&period_start_of(p.date, grain))
                .expect("the periods cover every date in [start, end]");
            let valued = Valued::of(p.amount, p.currency, rate(p.currency, base, p.date, prices));
            *period.accounts.entry(meta.path.clone()).or_default() += &valued;
        }
        periods.into_values().collect()
    }

    /// `root` and its subtree's balance per currency at the end of each
    /// period over `[start, end]`, unconverted: the figures its statements
    /// show.
    pub fn subtree_over_time(
        &self,
        root: &str,
        start: NaiveDate,
        end: NaiveDate,
        grain: Grain,
    ) -> Vec<(NaiveDate, BTreeMap<Currency, Decimal>)> {
        let under = |p: &PostingEntry| in_subtree(&self.accounts[&p.account_id].path, root);
        self.at_period_ends(start, end, grain, under, |date, sums| {
            let mut own: BTreeMap<Currency, Decimal> = BTreeMap::new();
            for (&(_, currency), &amount) in sums {
                *own.entry(currency).or_default() += amount;
            }
            (date, own)
        })
    }

    /// Calls `at` with the running per-(account, currency) sums of the kept
    /// postings at each period's end — every posting up to that day, not only
    /// those since `start`.
    fn at_period_ends<T>(
        &self,
        start: NaiveDate,
        end: NaiveDate,
        grain: Grain,
        keep: impl Fn(&PostingEntry) -> bool,
        mut at: impl FnMut(NaiveDate, &BTreeMap<(i64, Currency), Decimal>) -> T,
    ) -> Vec<T> {
        let mut kept: Vec<&PostingEntry> = self.postings.iter().filter(|p| keep(p)).collect();
        kept.sort_by_key(|p| p.date);
        let mut kept = kept.into_iter().peekable();
        let mut sums: BTreeMap<(i64, Currency), Decimal> = BTreeMap::new();
        generate_periods(start, end, grain)
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
