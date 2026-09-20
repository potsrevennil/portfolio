//! Builds the balance sheet from T5's query layer: asset and liability
//! balances folded into the account tree, per currency, with each sum also
//! converted into the base currency. Mirrors Fava's grouped 資產 page
//! (`ledger/MandarinOverview.js` `groupHoldings`).

use std::collections::BTreeMap;

use anyhow::Result;
use chrono::NaiveDate;
use portfolio::{
    currency::Currency,
    ledger::valuation::AtCost,
    store::{
        chart::{self, ChartAccount},
        query::{self, AccountBalance, AccountType, LedgerData},
    },
};
use rust_decimal::{Decimal, RoundingStrategy};
use sqlx::SqlitePool;

use crate::model::{BalanceSheet, Converted, Money, Node, Section};

/// Section headings when the chart has no label for the root.
const SECTIONS: [(&str, &str); 2] = [("Assets", "資產"), ("Liabilities", "負債")];

pub async fn load(
    pool: &SqlitePool,
    as_of: NaiveDate,
    base: Currency,
    at_cost: &AtCost,
) -> Result<BalanceSheet> {
    let data = LedgerData::load(pool).await?;
    let chart = chart::accounts(pool).await?;
    let prices = query::load_fx_prices(pool, &data.currencies(), base, as_of).await?;
    let balances = data.balances_as_of(as_of);
    Ok(build(&chart, &balances, as_of, base, at_cost, |c| query::rate(c, base, as_of, &prices)))
}

/// Sums per currency, keyed by path so children come out in chart order.
#[derive(Default)]
struct Tree {
    sums: BTreeMap<Currency, Decimal>,
    children: BTreeMap<String, Tree>,
}

pub fn build(
    chart: &[ChartAccount],
    balances: &[AccountBalance],
    as_of: NaiveDate,
    base: Currency,
    at_cost: &AtCost,
    rate: impl Fn(Currency) -> Option<Decimal>,
) -> BalanceSheet {
    let labels: BTreeMap<&str, &str> =
        chart.iter().map(|a| (a.path.as_str(), a.label.as_str())).collect();
    let label = |path: &str| -> String {
        labels
            .get(path)
            .map(|l| l.to_string())
            .unwrap_or_else(|| path.rsplit(':').next().unwrap_or(path).to_string())
    };
    let foreign =
        |sums: &BTreeMap<Currency, Decimal>| sums.iter().any(|(c, v)| *c != base && !v.is_zero());
    let convert = |sums: &BTreeMap<Currency, Decimal>| -> Converted {
        let mut total = Decimal::ZERO;
        let mut unpriced = Vec::new();
        for (&currency, &amount) in sums {
            match rate(currency) {
                Some(r) => total += amount * r,
                None => unpriced.push(currency.to_string()),
            }
        }
        Converted { money: money(total, base), unpriced }
    };

    // Equity, income and expense never reach the tree.
    let mut roots: BTreeMap<&str, Tree> =
        SECTIONS.iter().map(|(p, _)| (*p, Tree::default())).collect();
    let shown = balances.iter().filter(|b| {
        matches!(b.account_type, AccountType::Asset | AccountType::Liability) && !b.amount.is_zero()
    });
    // A holding carried at cost belongs to no total above its own row, so the
    // sums stop at the at-cost account and the section reports it apart.
    let mut cost: BTreeMap<&str, BTreeMap<Currency, Decimal>> = BTreeMap::new();
    for b in shown {
        let Some(root) = b.path.split(':').next() else { continue };
        let Some(mut node) = roots.get_mut(root) else { continue };
        let depths = b.path.split(':').count();
        let from = (1..=depths).find(|d| at_cost.covers(node_path(&b.path, *d)));
        if from.is_some() {
            *cost.entry(root).or_default().entry(b.currency).or_default() += b.amount;
        }
        if from.is_none() {
            *node.sums.entry(b.currency).or_default() += b.amount;
        }
        for depth in 2..=depths {
            node = node.children.entry(node_path(&b.path, depth).to_string()).or_default();
            if from.is_none_or(|first| depth >= first) {
                *node.sums.entry(b.currency).or_default() += b.amount;
            }
        }
    }

    fn nodes(
        children: BTreeMap<String, Tree>,
        label: &dyn Fn(&str) -> String,
        convert: &dyn Fn(&BTreeMap<Currency, Decimal>) -> Converted,
        foreign: &dyn Fn(&BTreeMap<Currency, Decimal>) -> bool,
        at_cost: &AtCost,
    ) -> Vec<Node> {
        children
            .into_iter()
            .map(|(path, tree)| Node {
                label: label(&path),
                amounts: amounts(&tree.sums),
                converted: foreign(&tree.sums).then(|| convert(&tree.sums)),
                at_cost: at_cost.covers(&path),
                children: nodes(tree.children, label, convert, foreign, at_cost),
                path,
            })
            .collect()
    }

    let mut net = BTreeMap::<Currency, Decimal>::new();
    let mut net_cost = BTreeMap::<Currency, Decimal>::new();
    let sections = SECTIONS
        .iter()
        .map(|(root, fallback)| {
            let tree = roots.remove(root).unwrap_or_default();
            for (c, v) in &tree.sums {
                *net.entry(*c).or_default() += v;
            }
            let excluded = cost.remove(root).unwrap_or_default();
            for (c, v) in &excluded {
                *net_cost.entry(*c).or_default() += v;
            }
            let own = label(root);
            Section {
                path: root.to_string(),
                label: if own == *root { fallback.to_string() } else { own },
                amounts: amounts(&tree.sums),
                converted: foreign(&tree.sums).then(|| convert(&tree.sums)),
                total: convert(&tree.sums),
                excluded: (!excluded.is_empty()).then(|| convert(&excluded)),
                nodes: nodes(tree.children, &label, &convert, &foreign, at_cost),
            }
        })
        .collect();

    BalanceSheet {
        as_of: as_of.to_string(),
        base: base.to_string(),
        sections,
        net_worth: convert(&net),
        excluded: (!net_cost.is_empty()).then(|| convert(&net_cost)),
    }
}

/// The first `depth` segments of `path`.
fn node_path(path: &str, depth: usize) -> &str {
    match path.match_indices(':').nth(depth - 1) {
        Some((i, _)) => &path[..i],
        None => path,
    }
}

fn amounts(sums: &BTreeMap<Currency, Decimal>) -> Vec<Money> {
    sums.iter().filter(|(_, v)| !v.is_zero()).map(|(c, v)| money(*v, *c)).collect()
}

fn money(amount: Decimal, currency: Currency) -> Money {
    Money {
        text: format!("{} {currency}", number(amount)),
        negative: amount.is_sign_negative() && !amount.is_zero(),
    }
}

/// Grouped thousands, at most two decimals, as Fava prints them.
pub fn number(amount: Decimal) -> String {
    let rounded =
        amount.round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero).normalize();
    let text = rounded.abs().to_string();
    let (int, frac) = text.split_once('.').map_or((text.as_str(), None), |(i, f)| (i, Some(f)));
    let mut grouped = String::new();
    for (i, ch) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    let sign = if rounded.is_sign_negative() && !rounded.is_zero() { "-" } else { "" };
    match frac {
        Some(f) => format!("{sign}{grouped}.{f}"),
        None => format!("{sign}{grouped}"),
    }
}

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;

    use super::*;

    #[test]
    fn numbers_print_like_fava() {
        assert_eq!(number(dec!(1234567.891)), "1,234,567.89");
        assert_eq!(number(dec!(-1000)), "-1,000");
        assert_eq!(number(dec!(0.005)), "0.01");
        assert_eq!(number(dec!(12.50)), "12.5");
        assert_eq!(number(dec!(-0.001)), "0");
        assert_eq!(node_path("Assets:Split:Alpha", 2), "Assets:Split");
    }
}
