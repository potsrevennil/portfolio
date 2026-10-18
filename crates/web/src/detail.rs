//! Builds one account's page: its balance now and month by month from
//! `db::query`, the balance checks on it from `db::check`, and its journal
//! from `entries`.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{anyhow, Result};
use chrono::NaiveDate;
use db::{
    assertions::{self, AssertionSource, BalanceAssertion},
    check,
    query::{self, in_subtree, AccountType, Grain, LedgerData},
    series::Valued,
    SqlitePool,
};
use ledger_types::currency::Currency;
use rust_decimal::Decimal;

use crate::{
    entries::{self, Chart},
    model::{
        AccountPage, BalanceCheck, Converted, JournalQuery, Money, Series, Step, Trend, Unpriced,
        Voucher,
    },
    trends::{converted, period_name},
};

impl From<AssertionSource> for Voucher {
    fn from(source: AssertionSource) -> Self {
        match source {
            AssertionSource::Statement => Voucher::Statement,
            AssertionSource::Tiantian => Voucher::Tiantian,
            AssertionSource::Counted => Voucher::Counted,
        }
    }
}

pub async fn load(
    pool: &SqlitePool,
    query: &JournalQuery,
    today: NaiveDate,
    base: Currency,
) -> Result<AccountPage> {
    let path = query.account.clone().ok_or_else(|| anyhow!("沒指定口座"))?;
    let chart = Chart::load(pool).await?;
    let account = chart
        .accounts
        .iter()
        .find(|a| a.path == path)
        .ok_or_else(|| anyhow!("查無口座：{path}"))?;
    let flow = matches!(account.account_type, AccountType::Income | AccountType::Expense);
    // Income is credit-negative; the page reads it as money in.
    let income = account.account_type == AccountType::Income;
    let sign = match income {
        true => -Decimal::ONE,
        false => Decimal::ONE,
    };

    let data = LedgerData::load(pool).await?;
    let prices = query::load_fx_prices(pool, &data.currencies(), base, today).await?;
    let from = data.first_date_under(&path).unwrap_or(today).min(today);
    let points = data.subtree_over_time(&path, from, today, Grain::Month);
    let now = points.last().map(|(_, b)| b.clone()).unwrap_or_default();

    let total: Valued = match flow {
        // Each posting at its own day's rate, as the charts' categories are.
        true => data
            .flows(base, from, today, Grain::Year, &prices)
            .iter()
            .flat_map(|f| &f.accounts)
            .filter(|(p, _)| in_subtree(p, &path))
            .map(|(_, v)| v)
            .sum(),
        // A balance at today's rate, as the balance sheet's is.
        false => {
            now.iter().map(|(&c, &a)| Valued::of(a, c, query::rate(c, base, today, &prices))).sum()
        }
    };
    let total = match income {
        true => -total,
        false => total,
    };

    let mut conn = pool.acquire().await?;
    let checks = checks(&data, &assertions::load(&mut conn).await?, &path, &chart, base);
    let mut journal = entries::page(
        pool,
        &chart,
        &JournalQuery { account: Some(path.clone()), page: query.page, ..Default::default() },
        base,
    )
    .await?;
    // The page has its own heading.
    journal.account = None;

    Ok(AccountPage {
        label: chart.label(&path),
        trail: chart.offer(&path),
        balance: standing(&now, sign, base),
        total: converted(&total, base),
        trends: trends(&points, flow, sign, base),
        path,
        flow,
        checks,
        journal,
    })
}

/// The base currency first, as the balance sheet's figure is.
fn base_first(base: Currency) -> impl Fn(&Currency) -> (bool, Currency) {
    move |c| (*c != base, *c)
}

/// The balance now in each currency it holds.
fn standing(now: &BTreeMap<Currency, Decimal>, sign: Decimal, base: Currency) -> Vec<Money> {
    let mut balance: Vec<Money> = now
        .iter()
        .filter(|(_, a)| !a.is_zero())
        .map(|(&c, &a)| Money::new(a * sign, c, base))
        .collect();
    balance.sort_by_key(|m| base_first(base)(&m.currency));
    balance
}

/// One trend per currency: the balance at each month's end, or for a flow
/// what moved within each month.
fn trends(
    points: &[(NaiveDate, BTreeMap<Currency, Decimal>)],
    flow: bool,
    sign: Decimal,
    base: Currency,
) -> Vec<Trend> {
    let periods: Vec<String> = points.iter().map(|(d, _)| period_name(*d, Step::Month)).collect();
    // A currency that came and went within a month is zero at every month's
    // end: it would only draw an empty chart.
    let mut currencies: Vec<Currency> = points
        .iter()
        .flat_map(|(_, b)| b.iter().filter(|(_, a)| !a.is_zero()).map(|(c, _)| *c))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    currencies.sort_by_key(base_first(base));
    currencies
        .into_iter()
        .map(|c| {
            let at = |b: &BTreeMap<Currency, Decimal>| b.get(&c).copied().unwrap_or_default();
            let figures = points
                .iter()
                .scan(Decimal::ZERO, |before, (_, b)| {
                    let now = at(b);
                    let figure = match flow {
                        true => now - *before,
                        false => now,
                    };
                    *before = now;
                    Some(Converted {
                        money: Money::new(figure * sign, c, base),
                        unpriced: Unpriced::default(),
                    })
                })
                .collect();
            Trend {
                periods: periods.clone(),
                series: vec![Series { name: c.to_string(), figures }],
                noted: Vec::new(),
            }
        })
        .collect()
}

/// Each figure an outside source states for `path` or under it, against the
/// books, newest first.
fn checks(
    data: &LedgerData,
    assertions: &[BalanceAssertion],
    path: &str,
    chart: &Chart,
    base: Currency,
) -> Vec<BalanceCheck> {
    let mut checks: Vec<BalanceCheck> = assertions
        .iter()
        .filter(|a| in_subtree(&a.account, path))
        .flat_map(|a| {
            check::points(data, a).into_iter().map(|p| BalanceCheck {
                date: p.as_of.to_string(),
                voucher: a.source.into(),
                account: (a.account != path).then(|| chart.label(&a.account)),
                expected: Money::new(p.expected, a.currency, base),
                computed: Money::new(p.computed, a.currency, base),
                difference: Money { amount: p.computed - p.expected, currency: a.currency },
                ok: p.ok(),
            })
        })
        .collect();
    checks.sort_by(|a, b| b.date.cmp(&a.date));
    checks
}
