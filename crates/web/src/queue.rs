//! Builds the review pages from `db`: the queue with what the imports suggest,
//! a transaction opened for editing, and the forms' text parsed into what
//! `db::review` writes.

use std::{collections::BTreeMap, future::Future};

use anyhow::{bail, Context, Result};
use db::{
    assertions::AssertionSource,
    check::{self, Figure, Mismatch},
    events,
    import_run::{self, Suggestion},
    journal::{self, Filter},
    review::{Edit, LegEdit, Manual},
    SqlitePool,
};
use ledger_types::currency::Currency;

use crate::{
    entries::{self, Chart, Unlisted},
    model::{
        Bulk, Change, ChartAccount, Editing, Gap, JournalQuery, LegInput, ManualInput, Queue,
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
    let mut journal =
        entries::load(pool, unlisted, &query.with_review(Review::Unreviewed), base).await?;
    let chart = Chart::load(pool, unlisted).await?;
    let ids: Vec<i64> = journal.entries.iter().map(|e| e.id).collect();
    let mut suggested = import_run::suggestions(pool, &ids).await?;
    for e in &mut journal.entries {
        let suggestions = suggested.remove(&e.id).unwrap_or_default();
        e.suggestions = suggestions
            .iter()
            // The row's own badge already says it waits for its statement.
            .filter(|s| !(e.unverified && **s == Suggestion::Waiting))
            .map(|s| said(s, &chart))
            .collect();
    }
    let confirmed = journal::by_ids(pool, &import_run::confirmed(pool).await?).await?;
    let confirmed = confirmed.into_iter().map(|e| chart.entry(e, base, &[])).collect();
    let gaps = gaps(pool, &chart).await?;
    let (accounts, parents) = (postable(&chart, &[]), chart.accounts_for(&[]));
    Ok(Queue { journal, gaps, confirmed, accounts, parents })
}

/// Runs `change` on each ticked row, keeping what it refuses to say beside the
/// row's date and name.
pub async fn each_row<F, Fut>(pool: &SqlitePool, ids: &str, change: F) -> Result<Bulk>
where
    F: Fn(i64) -> Fut,
    Fut: Future<Output = Result<()>>,
{
    let ids: Vec<i64> = ids
        .split(',')
        .filter(|id| !id.is_empty())
        .map(|id| id.parse().with_context(|| format!("不明的一筆：{id}")))
        .collect::<Result<_>>()?;
    let mut bulk = Bulk::default();
    for id in ids {
        match change(id).await {
            Ok(()) => bulk.done += 1,
            Err(e) => {
                let row = journal::by_ids(pool, &[id]).await?.pop();
                let name = row
                    .map(|r| format!("{} {}", r.date, r.payee.or(r.narration).unwrap_or_default()));
                bulk.left.push((id, format!("{}：{e:#}", name.unwrap_or_default())));
            }
        }
    }
    Ok(bulk)
}

/// A new account as the page offers it.
pub async fn opened(pool: &SqlitePool, unlisted: &Unlisted, path: &str) -> Result<ChartAccount> {
    let chart = Chart::load(pool, unlisted).await?;
    chart
        .accounts_for(&[])
        .into_iter()
        .find(|a| a.path == path)
        .with_context(|| format!("查無口座：{path}"))
}

/// A suggestion in the reader's words.
fn said(suggestion: &Suggestion, chart: &Chart) -> String {
    match suggestion {
        Suggestion::Uncategorised => "未分類，揀一个分類".to_string(),
        Suggestion::PickedNearest => "仝金額有幾筆，先配上近的".to_string(),
        Suggestion::Waiting => "等數單".to_string(),
        Suggestion::NotOnStatement { through, siblings } if siblings.is_empty() => {
            format!("數單到 {through} 無這筆")
        }
        Suggestion::NotOnStatement { through, siblings } => {
            let names: Vec<String> = siblings.iter().map(|a| chart.label(a)).collect();
            format!("數單到 {through} 無這筆，{} 有", names.join("、"))
        }
        Suggestion::Moved { from, to } => {
            format!("數單佇 {}，已經對 {} 移過去", chart.label(to), chart.label(from))
        }
        Suggestion::ExtraOpening => "這个口座有幾筆期初，用上早彼筆".to_string(),
    }
}

/// Each account's earliest disagreement with a statement, a count or a broker.
/// A 天天記帳 closing is only that app's own sum, so it is no gap.
pub async fn gaps(pool: &SqlitePool, chart: &Chart) -> Result<Vec<Gap>> {
    let mut conn = pool.acquire().await?;
    let report = check::check(&mut conn).await?;
    let mut earliest: BTreeMap<&str, &Mismatch> = BTreeMap::new();
    for m in &report.mismatches {
        let account = match &m.figure {
            Figure::Balance(a) if a.source == AssertionSource::Tiantian => continue,
            Figure::Balance(a) => a.account.as_str(),
            Figure::Holding(h) => h.account.as_str(),
        };
        let held = earliest.entry(account).or_insert(m);
        if m.as_of < held.as_of {
            *held = m;
        }
    }
    Ok(earliest
        .into_iter()
        .map(|(account, m)| {
            let off = (m.computed - m.expected).abs().normalize();
            let unit = match &m.figure {
                Figure::Balance(a) => a.currency.to_string(),
                Figure::Holding(h) => h.commodity.to_string(),
            };
            Gap {
                account: account.to_string(),
                label: chart.label(account),
                text: format!("{} 差 {off} {unit}", m.as_of),
            }
        })
        .collect())
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
    Ok(Editing { entry, accounts, parents: chart.accounts_for(&[]), history })
}

/// The accounts 記一筆 offers, and what a new one may open under.
pub async fn manual_accounts(
    pool: &SqlitePool,
    unlisted: &Unlisted,
) -> Result<(Vec<ChartAccount>, Vec<ChartAccount>)> {
    let chart = Chart::load(pool, unlisted).await?;
    Ok((postable(&chart, &[]), chart.accounts_for(&[])))
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
