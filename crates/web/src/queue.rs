//! Builds the review pages from `db`: the queue, and 記一筆's form parsed
//! into what `db::review` writes.

use anyhow::{bail, Context, Result};
use db::{
    journal::{self, Filter},
    review::Manual,
    SqlitePool,
};
use ledger_types::currency::Currency;
use rust_decimal::Decimal;

use crate::{
    entries::{self, Chart},
    model::{ChartAccount, JournalQuery, ManualInput, Queue, Review},
};

/// How many transactions wait for review.
pub async fn count(pool: &SqlitePool) -> Result<u64> {
    journal::count(pool, &Filter { reviewed: Some(false), ..Default::default() }).await
}

/// The journal held to unreviewed records, whatever the query asks.
pub async fn load(pool: &SqlitePool, query: &JournalQuery, base: Currency) -> Result<Queue> {
    let journal = entries::load(pool, &query.with_review(Review::Unreviewed), base).await?;
    Ok(Queue { journal })
}

/// Where a leg may post: accounts below a root.
fn postable(chart: &Chart, keep: &[&str]) -> Vec<ChartAccount> {
    chart.accounts_for(keep).into_iter().filter(|a| a.depth > 0).collect()
}

/// The accounts 記一筆 offers.
pub async fn manual_accounts(pool: &SqlitePool) -> Result<Vec<ChartAccount>> {
    Ok(postable(&Chart::load(pool).await?, &[]))
}

/// Grouping commas and spaces are only how the amount was typed.
fn amount(text: &str) -> Result<Decimal> {
    let clean: String = text.chars().filter(|c| *c != ',' && !c.is_whitespace()).collect();
    clean.parse().with_context(|| format!("金額不對：{text}"))
}

fn currency(text: &str) -> Result<Currency> {
    text.trim().parse().with_context(|| format!("幣別不對：{text}"))
}

impl TryFrom<&ManualInput> for Manual {
    type Error = anyhow::Error;

    fn try_from(m: &ManualInput) -> Result<Self> {
        let chosen = |text: &str| Some(text.trim().to_string()).filter(|t| !t.is_empty());
        let account = m.account.trim();
        if account.is_empty() {
            bail!("請選帳戶");
        }
        Ok(Manual {
            date: m.date.trim().parse().with_context(|| format!("日期不對：{}", m.date))?,
            amount: amount(&m.amount)?,
            currency: currency(&m.currency)?,
            account: account.to_string(),
            category: chosen(&m.category),
            counter: chosen(&m.counter),
            note: chosen(&m.note),
        })
    }
}
