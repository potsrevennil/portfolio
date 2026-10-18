//! Builds one account's page: its balance now and month by month from
//! `db::query`, the balance checks on it from `db::check`, and its journal
//! from `entries`.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{anyhow, Result};
use chrono::NaiveDate;
use db::{
    assertions::{self, AssertionSource},
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
    let sign = match account.account_type {
        AccountType::Income => -Decimal::ONE,
        _ => Decimal::ONE,
    };

    let data = LedgerData::load(pool).await?;
    let prices = query::load_fx_prices(pool, &data.currencies(), base, today).await?;
    let from = data.first_date_under(&path).unwrap_or(today).min(today);
    let points = data.subtree_over_time(&path, from, today, Grain::Month);

    let now = points.last().map(|(_, b)| b.clone()).unwrap_or_default();
    // The base currency first, as the balance sheet's figure is.
    let first = |c: &Currency| (*c != base, *c);
    let mut balance: Vec<Money> = now
        .iter()
        .filter(|(_, a)| !a.is_zero())
        .map(|(&c, &a)| Money::new(a * sign, c, base))
        .collect();
    balance.sort_by_key(|m| first(&m.currency));
    let valued: Vec<Valued> = now
        .iter()
        .map(|(&c, &a)| Valued::of(a * sign, c, query::rate(c, base, today, &prices)))
        .collect();
    let total = converted(&valued.iter().sum(), base);

    let periods: Vec<String> = points.iter().map(|(d, _)| period_name(*d, Step::Month)).collect();
    // A currency that came and went within a month is zero at every month's
    // end: it would only draw an empty chart.
    let mut currencies: Vec<Currency> = points
        .iter()
        .flat_map(|(_, b)| b.iter().filter(|(_, a)| !a.is_zero()).map(|(c, _)| *c))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    currencies.sort_by_key(first);
    let trends = currencies
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
        .collect();

    let mut conn = pool.acquire().await?;
    let mut checks: Vec<BalanceCheck> = assertions::load(&mut conn)
        .await?
        .iter()
        .filter(|a| in_subtree(&a.account, &path))
        .flat_map(|a| {
            check::points(&data, a).into_iter().map(|p| BalanceCheck {
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

    let mut journal = entries::load(
        pool,
        &JournalQuery { account: Some(path.clone()), page: query.page, ..Default::default() },
        base,
    )
    .await?;
    // The page has its own heading.
    journal.account = None;

    Ok(AccountPage {
        label: chart.label(&path),
        trail: chart.offer(&path),
        path,
        balance,
        total,
        flow,
        trends,
        checks,
        journal,
    })
}
