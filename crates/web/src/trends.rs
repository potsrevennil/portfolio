//! Builds 總覽 from the series walks in `db::series`: net worth at each
//! period's end, day-to-day income against expense within each period, the
//! net result of investments apart, and one period's flows by category.

use std::collections::BTreeMap;

use anyhow::{anyhow, bail, Result};
use chrono::{Datelike, NaiveDate};
use db::{
    chart::{root_label, tree_label, Root, ASSETS, EXPENSES, INCOME, LIABILITIES},
    market::Market,
    query::{self, AccountType, LedgerData},
    series::{Flows, Valued, WorthPoint},
    SqlitePool,
};
use ledger::{investments::Investments, valuation::AtCost};
use ledger_types::currency::Currency;
use rust_decimal::Decimal;

use crate::{
    model::{
        Breakdown, ChartQuery, Charts, Headline, PeriodChoice, Pie, Series, Slice, Summary, Tile,
        Trend,
    },
    sheet::{converted, node_path},
};

/// What the reporting rules leave a chart to show.
pub struct Rules<'a> {
    pub at_cost: &'a AtCost,
    pub investments: &'a Investments,
}

pub async fn load(
    pool: &SqlitePool,
    market: &Market,
    query: &ChartQuery,
    today: NaiveDate,
    base: Currency,
    rules: Rules<'_>,
) -> Result<Charts> {
    let data = LedgerData::load(pool).await?;
    // The whole ledger, a month at a time, as Fava charts it unfiltered.
    let from = data.first_date().unwrap_or(today).min(today);
    let prices = query::load_fx_prices(market, &data.currencies(), base, today).await?;
    let labels = data.labels();
    let label = |path: &str| tree_label(&labels, path);
    let root = |root: Root| root_label(&labels, root);

    let points = data.worth_points(base, rules.at_cost, from, today, &prices);
    let nets: Vec<Valued> = points.iter().map(WorthPoint::net).collect();
    // Each month's change; the first month changed it from nothing.
    let changes: Vec<Valued> = std::iter::once(&Valued::default())
        .chain(&nets)
        .zip(&nets)
        .map(|(before, now)| now.since(before))
        .collect();
    let series = |name: &str, figures: &[Valued]| Series {
        name: name.to_string(),
        figures: figures.iter().map(|v| converted(v, base)).collect(),
    };
    let arm = |of: fn(&WorthPoint) -> &Valued| points.iter().map(|p| of(p).clone()).collect();
    let last = |figures: &[Valued]| figures.last().cloned().unwrap_or_default();
    let assets: Vec<Valued> = arm(|p| &p.assets);
    let liabilities: Vec<Valued> = arm(|p| &p.liabilities);
    // Net worth is the figure; its two arms and its change are read out
    // beside it. Unpicked, the newest figures.
    let worth = Trend {
        periods: points.iter().map(|p| period_name(p.date)).collect(),
        summary: Some(Summary {
            name: "至今".into(),
            figures: [last(&nets), last(&assets), last(&liabilities), last(&changes)]
                .iter()
                .map(|v| converted(v, base))
                .collect(),
        }),
        series: vec![series("淨資產", &nets)],
        noted: vec![
            series(&root(ASSETS), &assets),
            series(&root(LIABILITIES), &liabilities),
            series("變化", &changes),
        ],
    };

    let flows = data.flows(base, from, today, &prices);
    let day_to_day = |path: &String| !rules.investments.covers(path);
    // A root's day-to-day flow in a month, signed as a page reads it.
    let under = |f: &Flows, root: Root, kind: AccountType| -> Valued {
        f.accounts
            .iter()
            .filter(|(path, _)| query::in_subtree(path, root.path) && day_to_day(path))
            .map(|(_, v)| v)
            .sum::<Valued>()
            * kind.shown()
    };
    let income: Vec<Valued> = flows.iter().map(|f| under(f, INCOME, AccountType::Income)).collect();
    let expense: Vec<Valued> =
        flows.iter().map(|f| under(f, EXPENSES, AccountType::Expense)).collect();
    let left: Vec<Valued> = income.iter().zip(&expense).map(|(i, e)| i - e).collect();
    // A gain is credit-negative income, a loss a debit expense: negated, the
    // sum reads as the result.
    let invested: Vec<Valued> = flows
        .iter()
        .map(|f| {
            -f.accounts.iter().filter(|(path, _)| !day_to_day(path)).map(|(_, v)| v).sum::<Valued>()
        })
        .collect();
    let any_invested = flows.iter().any(|f| f.accounts.keys().any(|path| !day_to_day(path)));
    // Unpicked, a flow reads as its sum over the whole ledger.
    let summed = |all: &[&[Valued]]| Summary {
        name: "合計".into(),
        figures: all.iter().map(|v| converted(&v.iter().sum(), base)).collect(),
    };
    let periods: Vec<String> = flows.iter().map(|f| period_name(f.start)).collect();
    let income_name = root(INCOME);
    let expense_name = root(EXPENSES);
    let flow_trend = Trend {
        periods: periods.clone(),
        summary: Some(summed(&[&income, &expense, &left])),
        series: vec![series(&income_name, &income), series(&expense_name, &expense)],
        noted: vec![series("結餘", &left)],
    };
    let investments = any_invested.then(|| Trend {
        periods: periods.clone(),
        summary: Some(summed(&[&invested])),
        series: vec![series("投資損益", &invested)],
        noted: Vec::new(),
    });

    // The newest month anything day-to-day moved in: what 總覽 leads with,
    // and the breakdown unasked.
    let newest = flows
        .iter()
        .rposition(|f| f.accounts.keys().any(day_to_day))
        .unwrap_or(flows.len().saturating_sub(1));
    let month = flows[newest].start.month();
    let tile = |label: String, figure: &Valued| Tile { label, figure: converted(figure, base) };
    let this_year: Valued = flows
        .iter()
        .zip(&invested)
        .filter(|(f, _)| f.start.year() == today.year())
        .map(|(_, v)| v)
        .sum();
    let headline = Headline {
        as_of: today.to_string(),
        tiles: [
            Some(tile("淨資產".into(), &last(&nets))),
            Some(tile(format!("{month} 月收入"), &income[newest])),
            Some(tile(format!("{month} 月支出"), &expense[newest])),
            Some(tile(format!("{month} 月結餘"), &left[newest])),
            any_invested.then(|| tile(format!("{} 投資損益", today.year()), &this_year)),
        ]
        .into_iter()
        .flatten()
        .collect(),
    };

    let pick = match query.period.as_deref() {
        Some(asked) => asked.parse::<Pick>()?,
        None => Pick::Month(flows[newest].start),
    };
    let marked: Vec<usize> =
        flows.iter().enumerate().filter(|(_, f)| pick.covers(f.start)).map(|(i, _)| i).collect();
    if marked.is_empty() {
        bail!("不明的期間：{pick}");
    }
    let mut totals: BTreeMap<&str, Valued> = BTreeMap::new();
    let chosen = marked.iter().flat_map(|&i| &flows[i].accounts);
    for (path, v) in chosen.filter(|(path, _)| day_to_day(path)) {
        *totals.entry(path.as_str()).or_default() += v;
    }
    // Every year with a month, and the months of the year picked (or of the
    // newest, when the whole ledger is).
    let mut years: Vec<i32> = flows.iter().map(|f| f.start.year()).collect();
    years.dedup();
    let shown = match pick {
        Pick::Year(year) => year,
        Pick::Month(start) => start.year(),
        Pick::Whole => years.last().copied().unwrap_or_default(),
    };
    let choice = |key: Pick, label: String| PeriodChoice {
        selected: key == pick,
        key: key.to_string(),
        label,
    };
    let breakdown = Breakdown {
        name: match pick {
            Pick::Whole => "全部期間".into(),
            Pick::Year(year) => format!("{year} 年"),
            Pick::Month(start) => format!("{} 年 {} 月", start.year(), start.month()),
        },
        income: pie((INCOME, AccountType::Income), income_name, &totals, &label, base),
        expense: pie((EXPENSES, AccountType::Expense), expense_name, &totals, &label, base),
        years: std::iter::once(choice(Pick::Whole, "全部".into()))
            .chain(years.iter().map(|&y| choice(Pick::Year(y), y.to_string())))
            .collect(),
        periods: flows
            .iter()
            .filter(|f| f.start.year() == shown)
            .map(|f| choice(Pick::Month(f.start), format!("{}月", f.start.month())))
            .collect(),
        marked,
    };

    // Listed apart, as the balance sheet lists them: no total includes them.
    let at_cost = points
        .last()
        .map(|p| &p.at_cost)
        .filter(|v| !v.value.is_zero() || !v.is_complete())
        .map(|v| converted(v, base));

    Ok(Charts {
        headline,
        worth,
        flows: flow_trend,
        starts: flows.iter().map(|f| f.start.to_string()).collect(),
        investments,
        breakdown,
        at_cost,
    })
}

/// What the breakdown covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pick {
    Whole,
    Year(i32),
    /// A month, by its first day.
    Month(NaiveDate),
}

impl Pick {
    fn covers(self, start: NaiveDate) -> bool {
        match self {
            Pick::Whole => true,
            Pick::Year(year) => start.year() == year,
            Pick::Month(first) => start == first,
        }
    }
}

/// The [`ChartQuery::period`] value.
impl std::fmt::Display for Pick {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Pick::Whole => f.write_str(ChartQuery::WHOLE),
            Pick::Year(year) => write!(f, "{year}"),
            Pick::Month(start) => write!(f, "{start}"),
        }
    }
}

impl std::str::FromStr for Pick {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match (s, s.parse::<i32>(), s.parse::<NaiveDate>()) {
            (ChartQuery::WHOLE, ..) => Ok(Pick::Whole),
            (_, Ok(year), _) => Ok(Pick::Year(year)),
            (_, _, Ok(start)) => Ok(Pick::Month(start)),
            _ => Err(anyhow!("不明的期間：{s}")),
        }
    }
}

/// `root`'s flow by its top-level accounts, each with its sub-accounts',
/// largest first and signed as a page reads them; one that netted to
/// nothing is left out.
fn pie(
    (root, kind): (Root, AccountType),
    label: String,
    totals: &BTreeMap<&str, Valued>,
    name: &dyn Fn(&str) -> String,
    base: Currency,
) -> Pie {
    let mut parts: BTreeMap<&str, Valued> = BTreeMap::new();
    let mut whole = Valued::default();
    for (path, v) in totals.iter().filter(|(path, _)| query::in_subtree(path, root.path)) {
        let v = v.clone() * kind.shown();
        *parts.entry(node_path(path, 2)).or_default() += &v;
        whole += &v;
    }
    let part = |v: &Valued| match (v.is_complete(), whole.is_complete()) {
        (true, true) if whole.value > Decimal::ZERO => Some(v.value / whole.value),
        _ => None,
    };
    let mut slices: Vec<Slice> = parts
        .into_iter()
        .filter(|(_, v)| !v.value.is_zero() || !v.is_complete())
        .map(|(path, v)| Slice {
            label: name(path),
            total: converted(&v, base),
            part: part(&v),
            path: path.to_string(),
        })
        .collect();
    // Largest first; one with amounts left unconverted cannot be placed.
    slices.sort_by(|a, b| {
        (!a.total.is_complete(), -a.total.money.amount)
            .cmp(&(!b.total.is_complete(), -b.total.money.amount))
    });
    Pie { label, total: converted(&whole, base), slices }
}

/// What a month is called on an axis: 2024-03.
pub fn period_name(date: NaiveDate) -> String { format!("{}-{:02}", date.year(), date.month()) }
