//! Builds one account's page: its balance now and month by month from
//! `db::series`, and the balance checks on it from `db::check`. Its journal
//! is the journal's own, loaded beside it.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{anyhow, Result};
use chrono::NaiveDate;
use db::{
    assertions::{self, BalanceAssertion},
    check,
    market::Market,
    query::{self, in_subtree, AccountType, LedgerData},
    series::Valued,
    SqlitePool,
};
use ledger_types::currency::Currency;
use rust_decimal::Decimal;

use crate::{
    entries::{Chart, Unlisted},
    model::{AccountPage, BalanceCheck, Converted, Money, Series, Summary, Trend, Voucher},
    sheet::converted,
    trends::period_name,
};

pub async fn load(
    pool: &SqlitePool,
    market: &Market,
    path: &str,
    today: NaiveDate,
    base: Currency,
) -> Result<AccountPage> {
    // Only names are read here, and every account has one.
    let chart = Chart::load(pool, &Unlisted::default()).await?;
    let account = chart
        .accounts
        .iter()
        .find(|a| a.path == path)
        .ok_or_else(|| anyhow!("查無口座：{path}"))?;
    let flow = matches!(account.account_type, AccountType::Income | AccountType::Expense);
    let sign = account.account_type.shown();

    // Every figure here is the subtree's: a pass over its postings, and the
    // rates of its own currencies only.
    let data = LedgerData::load(pool).await?.subtree(path);
    let prices = query::load_fx_prices(market, &data.currencies(), base, today).await?;
    let from = data.first_date().unwrap_or(today).min(today);
    let points = data.subtree_over_time(path, from, today);
    let now = points.last().map(|(_, b)| b.clone()).unwrap_or_default();

    let total: Valued = match flow {
        // Each posting at its own day's rate, as 總覽's categories are.
        true => {
            data.flows(base, from, today, &prices).iter().flat_map(|f| f.accounts.values()).sum()
        }
        // A balance at today's rate, as the balance sheet's is.
        false => {
            now.iter().map(|(&c, &a)| Valued::of(a, c, query::rate(c, base, today, &prices))).sum()
        }
    };

    let mut conn = pool.acquire().await?;
    let checks = checks(&data, &assertions::load(&mut conn).await?, path, &chart, base);

    Ok(AccountPage {
        label: chart.label(path),
        trail: chart.crumbs(path),
        balance: standing(&now, sign, base),
        total: converted(&(total * sign), base),
        trends: trends(&points, flow, sign, base),
        path: path.to_string(),
        flow,
        checks,
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
    let periods: Vec<String> = points.iter().map(|(d, _)| period_name(*d)).collect();
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
                    Some(Converted::from(Money::new(figure * sign, c, base)))
                })
                .collect();
            // Unpicked, a flow reads as its sum; a balance as its newest.
            let summary = flow.then(|| {
                let total = points.last().map(|(_, b)| at(b)).unwrap_or_default();
                Summary {
                    name: "合計".into(),
                    figures: vec![Money::new(total * sign, c, base).into()],
                }
            });
            Trend {
                periods: periods.clone(),
                series: vec![Series { name: c.to_string(), figures }],
                noted: Vec::new(),
                summary,
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
                voucher: Voucher(a.source),
                account: (a.account != path).then(|| chart.label(&a.account)),
                expected: Money::new(p.expected, a.currency, base),
                computed: Money::new(p.computed, a.currency, base),
                difference: Money { amount: p.difference(), currency: a.currency },
                ok: p.ok(),
            })
        })
        .collect();
    checks.sort_by(|a, b| b.date.cmp(&a.date));
    checks
}
