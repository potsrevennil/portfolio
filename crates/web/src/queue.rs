//! Builds the review pages from `db`: the queue with the lines waiting to be
//! paired, a transaction opened for editing, and the forms' text parsed into
//! what `db::review` writes.

use anyhow::{bail, Context, Result};
use db::{
    events,
    journal::{self, Filter},
    pairing,
    review::{Edit, LegEdit, Manual},
    SqlitePool,
};
use ledger_types::currency::Currency;

use crate::{
    entries::{self, Chart, Unlisted},
    model::{
        Change, ChartAccount, Choice, Editing, JournalQuery, LegInput, ManualInput, Money, Queue,
        Review,
    },
    typed::{amount, currency, date},
};

/// How many transactions wait for review.
pub async fn count(pool: &SqlitePool) -> Result<u64> {
    journal::count(pool, &Filter { reviewed: Some(false), ..Default::default() }).await
}

/// The journal held to unreviewed records, whatever the query asks.
pub async fn load(
    pool: &SqlitePool,
    unlisted: &Unlisted,
    query: &JournalQuery,
    base: Currency,
) -> Result<Queue> {
    let journal =
        entries::load(pool, unlisted, &query.with_review(Review::Unreviewed), base).await?;
    let chart = Chart::load(pool, unlisted).await?;
    let mut choices = Vec::new();
    for c in pairing::open(pool).await? {
        let a = c.ambiguity;
        let candidates = journal::by_ids(pool, &a.candidates).await?;
        choices.push(Choice {
            id: c.id,
            date: a.date.to_string(),
            account: chart.label(&a.account),
            money: Money::new(a.amount, a.currency, base),
            description: a.description,
            candidates: candidates.into_iter().map(|e| chart.entry(e, base, &[])).collect(),
            chosen: c.chosen,
        });
    }
    Ok(Queue { journal, choices })
}

/// Where a leg may post: accounts below a root.
fn postable(chart: &Chart, keep: &[&str]) -> Vec<ChartAccount> {
    chart.accounts_for(keep).into_iter().filter(|a| a.depth > 0).collect()
}

pub async fn editing(
    pool: &SqlitePool,
    unlisted: &Unlisted,
    id: i64,
    base: Currency,
) -> Result<Editing> {
    let chart = Chart::load(pool, unlisted).await?;
    let found =
        journal::by_ids(pool, &[id]).await?.pop().with_context(|| format!("查無交易 {id}"))?;
    let entry = chart.entry(found, base, &[]);
    let keep: Vec<&str> = entry.legs.iter().map(|l| l.path.as_str()).collect();
    let accounts = postable(&chart, &keep);
    let history = events::history(pool, id)
        .await?
        .into_iter()
        .map(|e| Change { at: e.at, kind: e.kind.to_string() })
        .collect();
    Ok(Editing { entry, accounts, history })
}

/// The accounts 記一筆 offers.
pub async fn manual_accounts(pool: &SqlitePool, unlisted: &Unlisted) -> Result<Vec<ChartAccount>> {
    Ok(postable(&Chart::load(pool, unlisted).await?, &[]))
}

impl TryFrom<&LegInput> for LegEdit {
    type Error = anyhow::Error;

    fn try_from(leg: &LegInput) -> Result<Self> {
        let posting = leg.posting.trim();
        Ok(LegEdit {
            posting_id: match posting.is_empty() {
                true => None,
                false => Some(posting.parse().with_context(|| format!("不明的一筆：{posting}"))?),
            },
            account: leg.account.trim().to_string(),
            amount: amount(&leg.amount)?,
            currency: currency(&leg.currency)?,
        })
    }
}

/// The edit a submitted form asks for. A row left without an account is one
/// of the blank rows for splitting, unused; one with an account but no
/// amount is a mistake.
pub fn edit(narration: Option<String>, legs: &[LegInput], confirm: bool) -> Result<Edit> {
    let legs = legs
        .iter()
        .filter(|l| !l.account.trim().is_empty())
        .map(LegEdit::try_from)
        .collect::<Result<_>>()?;
    Ok(Edit { narration, legs, confirm })
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
            date: date(&m.date)?,
            amount: amount(&m.amount)?,
            currency: currency(&m.currency)?,
            account: account.to_string(),
            category: chosen(&m.category),
            counter: chosen(&m.counter),
            note: chosen(&m.note),
        })
    }
}
