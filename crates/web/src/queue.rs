//! Builds the review pages from `db`: the queue with the lines waiting to be
//! paired and what stopped the last imports, a transaction opened for
//! editing, and the forms' text parsed into what `db::review` writes.

use anyhow::{bail, Context, Result};
use db::{
    events,
    journal::{self, Filter},
    pairing,
    review::{Edit, LegEdit, Manual},
    stops::{self, Record, Stop},
    SqlitePool,
};
use ledger_types::currency::Currency;
use rust_decimal::Decimal;

use crate::{
    entries::{self, Chart},
    model::{
        Change, ChartAccount, Choice, Editing, Entry, ImportStop, JournalQuery, LegInput,
        ManualInput, Money, Queue, Review, Unplaced,
    },
};

/// How many transactions wait for review.
pub async fn count(pool: &SqlitePool) -> Result<u64> {
    journal::count(pool, &Filter { reviewed: Some(false), ..Default::default() }).await
}

/// The journal held to unreviewed records, whatever the query asks, with
/// the lines waiting to be paired and what stopped the last imports.
pub async fn load(pool: &SqlitePool, query: &JournalQuery, base: Currency) -> Result<Queue> {
    let journal = entries::load(pool, &query.with_review(Review::Unreviewed), base).await?;
    let chart = Chart::load(pool).await?;
    let open_choices = pairing::open(pool).await?;
    let open_stops = stops::open(pool).await?;
    // Every record the two sections name, read at once.
    let ids: Vec<i64> = open_choices
        .iter()
        .flat_map(|c| c.ambiguity.candidates.iter().copied())
        .chain(open_stops.iter().flat_map(Stop::transactions))
        .collect();
    let named: Vec<Entry> =
        journal::by_ids(pool, &ids).await?.into_iter().map(|e| chart.entry(e, base, &[])).collect();
    let of = |ids: &[i64]| -> Vec<Entry> {
        named.iter().filter(|e| ids.contains(&e.id)).cloned().collect()
    };
    let choices = open_choices
        .into_iter()
        .map(|c| {
            let a = c.ambiguity;
            Choice {
                id: c.id,
                date: a.date.to_string(),
                account: chart.label(&a.account),
                money: Money::new(a.amount, a.currency, base),
                candidates: of(&a.candidates),
                description: a.description,
                chosen: c.chosen,
            }
        })
        .collect();
    let stops = open_stops.into_iter().filter_map(|s| import_stop(s, &chart, base, &of)).collect();
    Ok(Queue { journal, choices, stops })
}

/// A stop as the queue lists it, the records it names taken from `of`. One
/// whose record was deleted since is moot.
fn import_stop(
    stop: Stop,
    chart: &Chart,
    base: Currency,
    of: &impl Fn(&[i64]) -> Vec<Entry>,
) -> Option<ImportStop> {
    let unplaced = |r: Record| Unplaced {
        date: r.date.to_string(),
        account: chart.label(&r.account),
        money: Money::new(r.amount, r.currency, base),
        counter: r.counter.iter().map(|a| chart.label(a)).collect(),
        narration: r.narration,
    };
    let entry = |id: i64| of(&[id]).pop();
    match stop {
        // Listed by `pairing::open`, as 待配對.
        Stop::LineAmbiguous(_) => None,
        Stop::RecordAmbiguous { record, lines } => {
            Some(ImportStop::RecordAmbiguous { record: unplaced(record), lines: of(&lines) })
        }
        Stop::RecordUnexplained { record, through, lines } => Some(ImportStop::RecordUnexplained {
            record: unplaced(record),
            through: through.to_string(),
            lines: of(&lines),
        }),
        Stop::NotOnStatement { transaction_id, account, through, siblings, .. } => {
            entry(transaction_id).map(|entry| ImportStop::NotOnStatement {
                entry,
                account: chart.label(&account),
                through: through.to_string(),
                siblings: siblings.iter().map(|s| chart.label(s)).collect(),
            })
        }
        Stop::SharedDeferral { transaction_id, account, other, .. } => {
            entry(transaction_id).map(|entry| ImportStop::SharedDeferral {
                entry,
                account: chart.label(&account),
                other: chart.label(&other),
            })
        }
        Stop::TwoDates { transaction_id, dates } => {
            entry(transaction_id).map(|entry| ImportStop::TwoDates {
                entry,
                dates: dates.into_iter().map(|(a, d)| (chart.label(&a), d.to_string())).collect(),
            })
        }
        Stop::SeveralOpenings { account, .. } => {
            Some(ImportStop::SeveralOpenings { account: chart.label(&account) })
        }
        Stop::Unchained { account, currency, day, ledger, statement } => {
            Some(ImportStop::Unchained {
                account: chart.label(&account),
                day: day.to_string(),
                ledger: Money::new(ledger, currency, base),
                statement: Money::new(statement, currency, base),
            })
        }
    }
}

/// Where a leg may post: accounts below a root.
fn postable(chart: &Chart, keep: &[&str]) -> Vec<ChartAccount> {
    chart.accounts_for(keep).into_iter().filter(|a| a.depth > 0).collect()
}

pub async fn editing(pool: &SqlitePool, id: i64, base: Currency) -> Result<Editing> {
    let chart = Chart::load(pool).await?;
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
