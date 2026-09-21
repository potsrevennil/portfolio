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
    store::query::{self, AccountBalance, AccountType, LedgerData},
};
use rust_decimal::Decimal;
use sqlx::SqlitePool;

use crate::model::{BalanceSheet, Converted, Money, Node, Section, Unpriced};

/// Section headings when the chart has no label for the root.
const SECTIONS: [(&str, &str); 2] = [("Assets", "資產"), ("Liabilities", "負債")];

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
    /// At-cost holdings below it, which `sums` leaves out.
    cost: BTreeMap<Currency, Decimal>,
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
    // A holding carried at cost belongs to no total above its own row, so the
    // sums stop at the at-cost account and the section reports it apart.
    let mut cost: BTreeMap<&str, BTreeMap<Currency, Decimal>> = BTreeMap::new();
    for b in shown {
        let Some(root) = b.path.split(':').next() else { continue };
        let Some(mut node) = roots.get_mut(root) else { continue };
        let counted = query::in_net_worth(b, at_cost);
        match counted {
            true => *node.sums.entry(b.currency).or_default() += b.amount,
            false => *cost.entry(root).or_default().entry(b.currency).or_default() += b.amount,
        }
        let depths = b.path.split(':').count();
        let from = (1..=depths).find(|d| at_cost.covers(node_path(&b.path, *d)));
        for depth in 2..=depths {
            node = node.children.entry(node_path(&b.path, depth).to_string()).or_default();
            let sums = match counted || from.is_some_and(|first| depth >= first) {
                true => &mut node.sums,
                false => &mut node.cost,
            };
            *sums.entry(b.currency).or_default() += b.amount;
        }
        *node.own.entry(b.currency).or_default() += b.amount;
    }

    fn nodes(
        children: BTreeMap<String, Tree>,
        label: &dyn Fn(&str) -> String,
        convert: &dyn Fn(&BTreeMap<Currency, Decimal>) -> Converted,
        foreign: &dyn Fn(&BTreeMap<Currency, Decimal>) -> bool,
        at_cost: &AtCost,
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
                at_cost: at_cost.covers(&path),
                excluded: left_out(convert(&tree.cost)),
                children: nodes(tree.children, label, convert, foreign, at_cost, base),
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
                total: convert(&tree.sums),
                excluded: left_out(convert(&excluded)),
                nodes: nodes(tree.children, &label, &convert, &foreign, at_cost, base),
            }
        })
        .collect();

    BalanceSheet {
        as_of: as_of.to_string(),
        base,
        sections,
        net_worth: convert(&net),
        excluded: left_out(convert(&net_cost)),
    }
}

/// Holdings at cost that cancel out leave nothing to mention.
fn left_out(converted: Converted) -> Option<Converted> {
    let nothing = converted.money.is_zero() && converted.unpriced.0.is_empty();
    (!nothing).then_some(converted)
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
