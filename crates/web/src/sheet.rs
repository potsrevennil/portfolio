//! Builds the balance sheet from T5's query layer: asset and liability
//! balances folded into the account tree, per currency, with each sum also
//! converted into the base currency. Mirrors Fava's grouped 資產 page
//! (`ledger/MandarinOverview.js` `groupHoldings`).

use std::collections::BTreeMap;

use anyhow::Result;
use chrono::NaiveDate;
use db::{
    query::{self, AccountBalance, AccountType, LedgerData},
    SqlitePool,
};
use ledger::valuation::AtCost;
use ledger_types::currency::Currency;
use rust_decimal::Decimal;

use crate::model::{BalanceSheet, Converted, Money, Node, Section, Unpriced};

/// Section headings when the chart has no label for the root.
const SECTIONS: [(&str, &str); 2] = [("Assets", "資產"), ("Liabilities", "負債")];

/// The at-cost section: named as Taiwanese statements name the line item.
const AT_COST: (&str, &str) = ("at-cost", "以成本衡量之投資");

pub async fn load(
    pool: &SqlitePool,
    as_of: NaiveDate,
    base: Currency,
    at_cost: &AtCost,
) -> Result<BalanceSheet> {
    let data = LedgerData::load(pool).await?;
    let prices = query::load_fx_prices(pool, &data.currencies(), base, as_of).await?;
    let balances = data.balances_as_of(as_of);
    Ok(build(&data.labels(), &balances, as_of, base, at_cost, |c| {
        query::rate(c, base, as_of, &prices)
    }))
}

/// Sums per currency, keyed by path so children come out in chart order.
#[derive(Default)]
struct Tree {
    sums: BTreeMap<Currency, Decimal>,
    /// The account's own postings, without its descendants'.
    own: BTreeMap<Currency, Decimal>,
    children: BTreeMap<String, Tree>,
}

pub fn build(
    labels: &BTreeMap<&str, &str>,
    balances: &[AccountBalance],
    as_of: NaiveDate,
    base: Currency,
    at_cost: &AtCost,
    rate: impl Fn(Currency) -> Option<Decimal>,
) -> BalanceSheet {
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
        for (&currency, &amount) in sums.iter().filter(|(_, v)| !v.is_zero()) {
            match rate(currency) {
                Some(r) => total += amount * r,
                None => unpriced.push(Money::new(amount, currency, base)),
            }
        }
        Converted { money: Money::new(total, base, base), unpriced: Unpriced(unpriced) }
    };

    // Equity, income and expense never reach the tree.
    let mut roots: BTreeMap<&str, Tree> =
        SECTIONS.iter().map(|(p, _)| (*p, Tree::default())).collect();
    let shown = balances.iter().filter(|b| {
        matches!(b.account_type, AccountType::Asset | AccountType::Liability) && !b.amount.is_zero()
    });
    // A holding carried at cost has no market value, so it stays out of the
    // tree and its totals, and is listed on its own, one row per holding.
    let mut cost: BTreeMap<&str, BTreeMap<Currency, Decimal>> = BTreeMap::new();
    for b in shown {
        let Some(root) = b.path.split(':').next() else { continue };
        let Some(mut node) = roots.get_mut(root) else { continue };
        match query::in_net_worth(b, at_cost) {
            false => {
                let holding = at_cost.holding(&b.path).unwrap_or(&b.path);
                *cost.entry(holding).or_default().entry(b.currency).or_default() += b.amount;
            }
            true => {
                *node.sums.entry(b.currency).or_default() += b.amount;
                for depth in 2..=b.path.split(':').count() {
                    node = node.children.entry(node_path(&b.path, depth).to_string()).or_default();
                    *node.sums.entry(b.currency).or_default() += b.amount;
                }
                *node.own.entry(b.currency).or_default() += b.amount;
            }
        }
    }

    fn nodes(
        children: BTreeMap<String, Tree>,
        label: &dyn Fn(&str) -> String,
        convert: &dyn Fn(&BTreeMap<Currency, Decimal>) -> Converted,
        foreign: &dyn Fn(&BTreeMap<Currency, Decimal>) -> bool,
        base: Currency,
    ) -> Vec<Node> {
        children
            .into_iter()
            .map(|(path, tree)| Node {
                label: label(&path),
                total: convert(&tree.sums),
                native: match foreign(&tree.own) {
                    true => amounts(&tree.own, base),
                    false => Vec::new(),
                },
                children: nodes(tree.children, label, convert, foreign, base),
                path,
            })
            .collect()
    }

    let mut net = BTreeMap::<Currency, Decimal>::new();
    let sections = SECTIONS
        .iter()
        .map(|(root, fallback)| {
            let tree = roots.remove(root).unwrap_or_default();
            for (c, v) in &tree.sums {
                *net.entry(*c).or_default() += v;
            }
            let own = label(root);
            Section {
                path: root.to_string(),
                label: if own == *root { fallback.to_string() } else { own },
                total: convert(&tree.sums),
                nodes: nodes(tree.children, &label, &convert, &foreign, base),
            }
        })
        .collect();

    BalanceSheet {
        as_of: as_of.to_string(),
        base,
        sections,
        net_worth: convert(&net),
        at_cost: at_cost_section(&cost, &label, &convert, &foreign, base),
    }
}

/// One row per holding, straight under the heading, as a statement's notes
/// list each investee. The heading is the context, so a holding keeps its
/// own label; only holdings sharing one take their parents' in front.
fn at_cost_section(
    cost: &BTreeMap<&str, BTreeMap<Currency, Decimal>>,
    label: &dyn Fn(&str) -> String,
    convert: &dyn Fn(&BTreeMap<Currency, Decimal>) -> Converted,
    foreign: &dyn Fn(&BTreeMap<Currency, Decimal>) -> bool,
    base: Currency,
) -> Option<Section> {
    let own: Vec<String> = cost.keys().map(|path| label(path)).collect();
    let mut uses = BTreeMap::<&str, usize>::new();
    for name in &own {
        *uses.entry(name).or_default() += 1;
    }
    let mut total = BTreeMap::<Currency, Decimal>::new();
    let nodes: Vec<Node> = cost
        .iter()
        .zip(&own)
        .map(|((path, sums), name)| {
            for (c, v) in sums {
                *total.entry(*c).or_default() += v;
            }
            Node {
                path: path.to_string(),
                label: match (uses[name.as_str()], path.rsplit_once(':')) {
                    (2.., Some((parent, _))) => format!("{}{name}", label(parent)),
                    _ => name.clone(),
                },
                total: convert(sums),
                native: match foreign(sums) {
                    true => amounts(sums, base),
                    false => Vec::new(),
                },
                children: Vec::new(),
            }
        })
        .collect();
    let (path, label) = AT_COST;
    (!nodes.is_empty()).then(|| Section {
        path: path.to_string(),
        label: label.to_string(),
        total: convert(&total),
        nodes,
    })
}

/// The first `depth` segments of `path`.
fn node_path(path: &str, depth: usize) -> &str {
    match path.match_indices(':').nth(depth - 1) {
        Some((i, _)) => &path[..i],
        None => path,
    }
}

fn amounts(sums: &BTreeMap<Currency, Decimal>, base: Currency) -> Vec<Money> {
    sums.iter().filter(|(_, v)| !v.is_zero()).map(|(c, v)| Money::new(*v, *c, base)).collect()
}
