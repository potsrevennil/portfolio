//! Builds the 帳戶 page's tree from T5's query layer: every account but
//! equity and the app's plumbing, each with its subtree's balance per
//! currency, unconverted.

use std::collections::BTreeMap;

use anyhow::Result;
use chrono::NaiveDate;
use db::{
    query::{self, in_subtree, AccountBalance, AccountType},
    SqlitePool,
};
use ledger_types::currency::Currency;
use rust_decimal::Decimal;

use crate::{
    entries::{Chart, Plumbing},
    model::{AccountBranch, AccountTree, Money},
};

pub async fn load(
    pool: &SqlitePool,
    plumbing: &Plumbing,
    as_of: NaiveDate,
    base: Currency,
) -> Result<AccountTree> {
    let chart = Chart::load(pool, plumbing).await?;
    let balances = query::account_balances(pool, as_of).await?;
    Ok(build(&chart, plumbing, &balances, base))
}

pub fn build(
    chart: &Chart,
    plumbing: &Plumbing,
    balances: &[AccountBalance],
    base: Currency,
) -> AccountTree {
    let mut shown: Vec<_> = chart
        .accounts
        .iter()
        .filter(|a| a.account_type != AccountType::Equity && !plumbing.contains(&a.path))
        .collect();
    shown.sort_by_key(|a| (a.account_type, a.path.as_str()));
    shown
        .into_iter()
        .map(|a| {
            let under: Vec<_> = balances.iter().filter(|b| in_subtree(&b.path, &a.path)).collect();
            let mut sums: BTreeMap<Currency, Decimal> = BTreeMap::new();
            for b in &under {
                *sums.entry(b.currency).or_default() += b.amount;
            }
            AccountBranch {
                path: a.path.clone(),
                label: chart.tree_label(&a.path),
                depth: a.path.matches(':').count(),
                balance: sums
                    .into_iter()
                    .filter(|(_, amount)| !amount.is_zero())
                    .map(|(currency, amount)| Money::new(amount, currency, base))
                    .collect(),
                zero: under.iter().all(|b| b.amount.is_zero()),
                children: Vec::new(),
            }
        })
        .collect()
}
