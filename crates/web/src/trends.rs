//! Builds 圖表 from the series walks in `db::query`: net worth at each
//! period's end, income against expense within each period, and each income
//! and expense account's part of the whole range.

use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};
use chrono::{Datelike, NaiveDate};
use db::{
    chart::{root_label, tree_label},
    query::{self, Grain, LedgerData},
    series::{Flows, Valued, WorthPoint},
    SqlitePool,
};
use ledger::valuation::AtCost;
use ledger_types::currency::Currency;

use crate::model::{ChartQuery, Charts, Converted, Money, Series, Share, Step, Trend, Unpriced};

/// Root headings when the chart has no label for the root.
const ASSETS: (&str, &str) = ("Assets", "資產");
const LIABILITIES: (&str, &str) = ("Liabilities", "負債");
const INCOME: (&str, &str) = ("Income", "收入");
const EXPENSE: (&str, &str) = ("Expenses", "支出");

impl From<Step> for Grain {
    fn from(step: Step) -> Self {
        match step {
            Step::Month => Grain::Month,
            Step::Quarter => Grain::Quarter,
            Step::Year => Grain::Year,
        }
    }
}

/// The range a query asks for, a blank bound filled in: from the ledger's
/// first day to `today`, as Fava charts the whole ledger unfiltered.
pub struct Range {
    pub from: NaiveDate,
    pub to: NaiveDate,
    pub step: Step,
}

impl Range {
    pub fn of(query: &ChartQuery, first: Option<NaiveDate>, today: NaiveDate) -> Result<Self> {
        let date = |d: &Option<String>| {
            d.as_deref().map(|d| d.parse().with_context(|| format!("日期不對：{d}"))).transpose()
        };
        let to = date(&query.to)?.unwrap_or(today);
        let from = date(&query.from)?.unwrap_or(first.unwrap_or(to).min(to));
        let step = query.step.as_deref().map(str::parse).transpose().map_err(anyhow::Error::msg)?;
        match from <= to {
            true => Ok(Range { from, to, step: step.unwrap_or_default() }),
            false => bail!("「從」{from} 在「到」{to} 之後"),
        }
    }
}

/// The range as the form shows it back.
impl From<&Range> for ChartQuery {
    fn from(range: &Range) -> Self {
        ChartQuery {
            from: Some(range.from.to_string()),
            to: Some(range.to.to_string()),
            step: Some(range.step.to_string()),
        }
    }
}

pub async fn load(
    pool: &SqlitePool,
    query: &ChartQuery,
    today: NaiveDate,
    base: Currency,
    at_cost: &AtCost,
) -> Result<Charts> {
    let data = LedgerData::load(pool).await?;
    let range = Range::of(query, data.first_date(), today)?;
    let prices = query::load_fx_prices(pool, &data.currencies(), base, range.to).await?;
    let grain = Grain::from(range.step);
    let labels = data.labels();
    let label = |path: &str| tree_label(&labels, path);
    let root = |(path, fallback): (&str, &str)| root_label(&labels, path, fallback);

    let points = data.worth_points(base, at_cost, range.from, range.to, grain, &prices);
    let periods = points.iter().map(|p| period_name(p.date, range.step)).collect();
    let series = |name: &str, figure: &dyn Fn(&WorthPoint) -> Valued| Series {
        name: name.to_string(),
        figures: points.iter().map(|p| converted(&figure(p), base)).collect(),
    };
    let worth = Trend {
        periods,
        series: vec![
            series("淨資產", &|p| p.net()),
            series(&root(ASSETS), &|p| p.assets.clone()),
            series(&root(LIABILITIES), &|p| p.liabilities.clone()),
        ],
        noted: Vec::new(),
    };

    let flows = data.flows(base, range.from, range.to, grain, &prices);
    let income_of = |f: &Flows| -> Valued { -sum_under(f, INCOME.0) };
    let expense_of = |f: &Flows| -> Valued { sum_under(f, EXPENSE.0) };
    let figures = |of: &dyn Fn(&Flows) -> Valued| -> Vec<Converted> {
        flows.iter().map(|f| converted(&of(f), base)).collect()
    };
    let income_name = root(INCOME);
    let expense_name = root(EXPENSE);
    let flow_trend = Trend {
        periods: flows.iter().map(|f| period_name(f.start, range.step)).collect(),
        series: vec![Series { name: income_name.clone(), figures: figures(&income_of) }, Series {
            name: expense_name.clone(),
            figures: figures(&expense_of),
        }],
        noted: vec![Series {
            name: "結餘".into(),
            figures: figures(&|f| {
                let mut net = income_of(f);
                net += &-expense_of(f);
                net
            }),
        }],
    };

    // The whole range, per account: income negated, so both read as amounts.
    let mut totals: BTreeMap<String, Valued> = BTreeMap::new();
    for f in &flows {
        for (path, v) in &f.accounts {
            *totals.entry(path.clone()).or_default() += v;
        }
    }
    let income = share(INCOME.0, income_name, &totals, true, &label, base);
    let expense = share(EXPENSE.0, expense_name, &totals, false, &label, base);

    Ok(Charts { query: ChartQuery::from(&range), worth, flows: flow_trend, income, expense })
}

fn sum_under(f: &Flows, root: &str) -> Valued {
    f.accounts.iter().filter(|(path, _)| query::in_subtree(path, root)).map(|(_, v)| v).sum()
}

/// `root`'s part of the flow and its sub-accounts' beneath it, largest first;
/// an account that netted to nothing is left out.
fn share(
    root: &str,
    name: String,
    totals: &BTreeMap<String, Valued>,
    negate: bool,
    label: &dyn Fn(&str) -> String,
    base: Currency,
) -> Share {
    #[derive(Default)]
    struct Tree {
        sum: Valued,
        children: BTreeMap<String, Tree>,
    }
    let mut tree = Tree::default();
    for (path, v) in totals.iter().filter(|(path, _)| query::in_subtree(path, root)) {
        let v = match negate {
            true => -v.clone(),
            false => v.clone(),
        };
        tree.sum += &v;
        let mut node = &mut tree;
        let segments = path.split(':').count();
        for depth in 2..=segments {
            let prefix = path.splitn(depth + 1, ':').take(depth).collect::<Vec<_>>().join(":");
            node = node.children.entry(prefix).or_default();
            node.sum += &v;
        }
    }

    fn shares(
        children: BTreeMap<String, Tree>,
        label: &dyn Fn(&str) -> String,
        base: Currency,
    ) -> Vec<Share> {
        let mut out: Vec<Share> = children
            .into_iter()
            .filter(|(_, t)| !t.sum.value.is_zero() || !t.sum.is_complete())
            .map(|(path, t)| Share {
                label: label(&path),
                total: converted(&t.sum, base),
                children: shares(t.children, label, base),
                path,
            })
            .collect();
        // Largest first; one with amounts left unconverted cannot be placed.
        out.sort_by(|a, b| {
            (!a.total.is_complete(), -a.total.money.amount)
                .cmp(&(!b.total.is_complete(), -b.total.money.amount))
        });
        out
    }

    Share {
        path: root.to_string(),
        label: name,
        total: converted(&tree.sum, base),
        children: shares(tree.children, label, base),
    }
}

pub fn converted(v: &Valued, base: Currency) -> Converted {
    Converted {
        money: Money::new(v.value, base, base),
        unpriced: Unpriced(v.unpriced.iter().map(|(&c, &a)| Money::new(a, c, base)).collect()),
    }
}

/// What a period is called on an axis: 2024-03, 2024 第1季, 2024.
pub fn period_name(date: NaiveDate, step: Step) -> String {
    match step {
        Step::Month => format!("{}-{:02}", date.year(), date.month()),
        Step::Quarter => format!("{} 第{}季", date.year(), date.month0() / 3 + 1),
        Step::Year => date.year().to_string(),
    }
}
