//! Builds the review pages from `db`: the queue with the lines waiting to be
//! paired and what stopped the last imports, a transaction opened for
//! editing, and the forms' text parsed into what `db::review` writes.

use anyhow::{bail, Context, Result};
use db::{
    events,
    journal::{self, Filter},
    pairing,
    review::{Edit, LegEdit, Manual},
    stops::{self, Pick, Record, Stop},
    SqlitePool,
};
use ledger_types::currency::Currency;
use rust_decimal::Decimal;

use crate::{
    entries::{self, Chart},
    model::{
        Change, ChartAccount, Choice, Editing, Entry, ImportStop, JournalQuery, LegInput,
        ManualInput, Money, Place, Queue, Review, StopKind, Unplaced,
    },
};

/// How many transactions wait for review.
pub async fn count(pool: &SqlitePool) -> Result<u64> {
    journal::count(pool, &Filter { reviewed: Some(false), ..Default::default() }).await
}

/// The journal held to unreviewed records, whatever the query asks.
pub async fn load(pool: &SqlitePool, query: &JournalQuery, base: Currency) -> Result<Queue> {
    let journal = entries::load(pool, &query.with_review(Review::Unreviewed), base).await?;
    let chart = Chart::load(pool).await?;
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
    let stops = import_stops(pool, &chart, base).await?;
    Ok(Queue { journal, choices, stops })
}

async fn entries(
    pool: &SqlitePool,
    chart: &Chart,
    ids: &[i64],
    base: Currency,
) -> Result<Vec<Entry>> {
    Ok(journal::by_ids(pool, ids).await?.into_iter().map(|e| chart.entry(e, base, &[])).collect())
}

/// The transaction `id`, unless it is gone.
async fn entry(pool: &SqlitePool, chart: &Chart, id: i64, base: Currency) -> Result<Option<Entry>> {
    Ok(entries(pool, chart, &[id], base).await?.pop())
}

/// What stopped the last imports, each as the queue shows it. One about a
/// record deleted since is moot, and left out.
async fn import_stops(pool: &SqlitePool, chart: &Chart, base: Currency) -> Result<Vec<ImportStop>> {
    let place = |path: String| Place { label: chart.label(&path), path };
    let unplaced = |r: Record| Unplaced {
        date: r.date.to_string(),
        account: chart.label(&r.account),
        money: Money::new(r.amount, r.currency, base),
        counter: r.counter.iter().map(|a| chart.label(a)).collect(),
        narration: r.narration,
    };
    let mut out = Vec::new();
    for open in stops::open(pool).await? {
        let kind = match open.stop {
            // Listed by `pairing::open`, as 待配對.
            Stop::LineAmbiguous(_) => continue,
            Stop::RecordAmbiguous { record, lines } => StopKind::RecordAmbiguous {
                record: unplaced(record),
                lines: entries(pool, chart, &lines, base).await?,
            },
            Stop::RecordUnexplained { record, through, lines, accounts } => {
                StopKind::RecordUnexplained {
                    record: unplaced(record),
                    through: through.to_string(),
                    lines: entries(pool, chart, &lines, base).await?,
                    accounts: accounts.into_iter().map(place).collect(),
                }
            }
            Stop::NotOnStatement { transaction_id, account, through, siblings, .. } => {
                let Some(entry) = entry(pool, chart, transaction_id, base).await? else { continue };
                StopKind::NotOnStatement {
                    entry,
                    account: chart.label(&account),
                    through: through.to_string(),
                    siblings: siblings.into_iter().map(place).collect(),
                }
            }
            Stop::SharedDeferral { transaction_id, account, other, .. } => {
                let Some(entry) = entry(pool, chart, transaction_id, base).await? else { continue };
                StopKind::SharedDeferral {
                    entry,
                    account: chart.label(&account),
                    other: chart.label(&other),
                }
            }
            Stop::TwoDates { transaction_id, dates, .. } => {
                let Some(entry) = entry(pool, chart, transaction_id, base).await? else { continue };
                let dates =
                    dates.into_iter().map(|(a, d)| (chart.label(&a), d.to_string())).collect();
                StopKind::TwoDates { entry, dates }
            }
            Stop::SeveralOpenings { account, .. } => {
                StopKind::SeveralOpenings { account: chart.label(&account) }
            }
            Stop::Unchained { account, currency, day, ledger, statement } => StopKind::Unchained {
                account: chart.label(&account),
                day: day.to_string(),
                ledger: Money::new(ledger, currency, base),
                statement: Money::new(statement, currency, base),
            },
        };
        let picked = open.pick.map(|p| match p {
            Pick::Line(id) => ImportStop::line(id),
            Pick::Account(path) => ImportStop::account(&path),
        });
        out.push(ImportStop { id: open.id, kind, picked, resolved: open.resolved });
    }
    Ok(out)
}

/// The pick a radio value names.
pub fn pick(value: &str) -> Result<Pick> {
    match value.split_once(':') {
        Some(("line", id)) => {
            Ok(Pick::Line(id.parse().with_context(|| format!("毋知是佗一筆明細：{id}"))?))
        }
        Some(("account", path)) => Ok(Pick::Account(path.to_string())),
        _ => bail!("請揀一个"),
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
