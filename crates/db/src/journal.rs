//! The journal: transactions newest first with their legs, filtered and paged.
//! The journal page and T9's review queue both list through [`entries`].

use anyhow::{Context, Result};
use chrono::NaiveDate;
use ledger::{journal::UNVERIFIED_TAG, model::Source};
use ledger_types::currency::Currency;
use rust_decimal::Decimal;
use sqlx::{QueryBuilder, Sqlite, SqlitePool};

use crate::{import::Origin, query::AccountType};

/// Which transactions to list. The default lists every one.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Filter {
    /// A chart path: transactions with a leg on it or below it.
    pub account: Option<String>,
    pub from: Option<NaiveDate>,
    pub to: Option<NaiveDate>,
    /// Matched in payee or narration, ASCII case-insensitive.
    pub text: Option<String>,
    pub reviewed: Option<bool>,
    /// Only records no statement has checked yet.
    pub unverified: bool,
    pub source: Option<Source>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Leg {
    /// The posting's id.
    pub id: i64,
    pub account: String,
    pub amount: Decimal,
    pub currency: Currency,
    pub origin: Option<Origin>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub id: i64,
    pub date: NaiveDate,
    pub payee: Option<String>,
    pub narration: Option<String>,
    pub source: Source,
    pub reviewed: bool,
    pub unverified: bool,
    pub legs: Vec<Leg>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Account {
    pub path: String,
    pub label: String,
    pub account_type: AccountType,
    pub closed: bool,
}

#[derive(sqlx::FromRow)]
struct EntryRow {
    id: i64,
    date: String,
    payee: Option<String>,
    narration: Option<String>,
    source: String,
    reviewed: bool,
    unverified: bool,
}

#[derive(sqlx::FromRow)]
struct LegRow {
    id: i64,
    transaction_id: i64,
    path: String,
    amount: String,
    currency: String,
    origin: Option<String>,
}

#[derive(sqlx::FromRow)]
struct AccountRow {
    path: String,
    label: String,
    acct_type: String,
    closed: bool,
}

impl TryFrom<EntryRow> for Entry {
    type Error = anyhow::Error;

    fn try_from(r: EntryRow) -> Result<Self> {
        Ok(Entry {
            id: r.id,
            date: r.date.parse().with_context(|| format!("transaction {} date", r.id))?,
            payee: r.payee,
            narration: r.narration,
            source: r.source.parse().with_context(|| format!("source {:?}", r.source))?,
            reviewed: r.reviewed,
            unverified: r.unverified,
            legs: Vec::new(),
        })
    }
}

impl TryFrom<LegRow> for (i64, Leg) {
    type Error = anyhow::Error;

    fn try_from(r: LegRow) -> Result<Self> {
        let leg = Leg {
            id: r.id,
            origin: r.origin.map(|o| o.parse()).transpose().context("posting origin")?,
            amount: r.amount.parse().with_context(|| format!("posting amount {:?}", r.amount))?,
            currency: r.currency.parse().with_context(|| format!("currency {:?}", r.currency))?,
            account: r.path,
        };
        Ok((r.transaction_id, leg))
    }
}

impl TryFrom<AccountRow> for Account {
    type Error = anyhow::Error;

    fn try_from(r: AccountRow) -> Result<Self> {
        Ok(Account {
            account_type: r
                .acct_type
                .parse()
                .with_context(|| format!("account type {:?}", r.acct_type))?,
            path: r.path,
            label: r.label,
            closed: r.closed,
        })
    }
}

/// True when some leg of `t` carries the unverified tag.
fn push_unverified(q: &mut QueryBuilder<'_, Sqlite>) {
    q.push(
        "EXISTS (SELECT 1 FROM postings u WHERE u.transaction_id = t.id AND instr(',' || \
         coalesce(u.tags, '') || ',', ',' || ",
    )
    .push_bind(UNVERIFIED_TAG)
    .push(" || ',') > 0)");
}

fn push_conditions<'a>(q: &mut QueryBuilder<'a, Sqlite>, filter: &'a Filter) {
    q.push(" WHERE t.deleted_at IS NULL");
    if let Some(account) = &filter.account {
        q.push(
            " AND EXISTS (SELECT 1 FROM postings p JOIN accounts a ON a.id = p.account_id WHERE \
             p.transaction_id = t.id AND (a.path = ",
        )
        .push_bind(account)
        .push(" OR substr(a.path, 1, length(")
        .push_bind(account)
        .push(") + 1) = ")
        .push_bind(account)
        .push(" || ':'))");
    }
    if let Some(from) = filter.from {
        q.push(" AND t.date >= ").push_bind(from.to_string());
    }
    if let Some(to) = filter.to {
        q.push(" AND t.date <= ").push_bind(to.to_string());
    }
    if let Some(text) = filter.text.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        let pattern = format!("%{}%", escape_like(text));
        q.push(" AND (t.payee LIKE ")
            .push_bind(pattern.clone())
            .push(" ESCAPE '\\' OR t.narration LIKE ")
            .push_bind(pattern)
            .push(" ESCAPE '\\')");
    }
    if let Some(reviewed) = filter.reviewed {
        q.push(" AND t.reviewed = ").push_bind(reviewed);
    }
    if filter.unverified {
        q.push(" AND ");
        push_unverified(q);
    }
    if let Some(source) = filter.source {
        q.push(" AND t.source = ").push_bind(source.to_string());
    }
}

/// Typed text matches itself, never as a LIKE wildcard.
fn escape_like(text: &str) -> String {
    text.chars()
        .flat_map(|c| match c {
            '%' | '_' | '\\' => vec!['\\', c],
            _ => vec![c],
        })
        .collect()
}

/// How many transactions `filter` matches.
pub async fn count(pool: &SqlitePool, filter: &Filter) -> Result<u64> {
    let mut count = QueryBuilder::new("SELECT count(*) FROM transactions t");
    push_conditions(&mut count, filter);
    let total: i64 =
        count.build_query_scalar().fetch_one(pool).await.context("counting the journal")?;
    Ok(u64::try_from(total)?)
}

/// The transactions `filter` matches, newest first, `limit` of them after
/// skipping `offset`.
pub async fn entries(
    pool: &SqlitePool,
    filter: &Filter,
    limit: u32,
    offset: u64,
) -> Result<Vec<Entry>> {
    let mut page = select();
    push_conditions(&mut page, filter);
    page.push(" ORDER BY t.date DESC, t.id DESC LIMIT ")
        .push_bind(i64::from(limit))
        .push(" OFFSET ")
        .push_bind(i64::try_from(offset).context("journal offset")?);
    with_legs(pool, page).await
}

/// The transactions with these ids, newest first; unknown ids are skipped.
pub async fn by_ids(pool: &SqlitePool, ids: &[i64]) -> Result<Vec<Entry>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut page = select();
    page.push(" WHERE t.deleted_at IS NULL AND t.id IN (");
    let mut list = page.separated(", ");
    for id in ids {
        list.push_bind(*id);
    }
    page.push(") ORDER BY t.date DESC, t.id DESC");
    with_legs(pool, page).await
}

fn select<'a>() -> QueryBuilder<'a, Sqlite> {
    let mut page = QueryBuilder::new(
        "SELECT t.id, t.date, t.payee, t.narration, t.source, t.reviewed != 0 AS reviewed, ",
    );
    push_unverified(&mut page);
    page.push(" AS unverified FROM transactions t");
    page
}

async fn with_legs(pool: &SqlitePool, mut page: QueryBuilder<'_, Sqlite>) -> Result<Vec<Entry>> {
    let rows: Vec<EntryRow> =
        page.build_query_as().fetch_all(pool).await.context("reading the journal")?;
    let mut entries = rows.into_iter().map(Entry::try_from).collect::<Result<Vec<_>>>()?;

    if !entries.is_empty() {
        let mut legs = QueryBuilder::new(
            "SELECT p.id, p.transaction_id, a.path, p.amount, p.currency, p.origin FROM postings \
             p JOIN accounts a ON a.id = p.account_id WHERE p.transaction_id IN (",
        );
        let mut ids = legs.separated(", ");
        for e in &entries {
            ids.push_bind(e.id);
        }
        legs.push(") ORDER BY p.id");
        let rows: Vec<LegRow> =
            legs.build_query_as().fetch_all(pool).await.context("reading journal legs")?;
        for row in rows {
            let (id, leg) = <(i64, Leg)>::try_from(row)?;
            if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                entry.legs.push(leg);
            }
        }
    }

    Ok(entries)
}

/// Every account in chart order, group levels and closed ones included.
pub async fn accounts(pool: &SqlitePool) -> Result<Vec<Account>> {
    let rows: Vec<AccountRow> = sqlx::query_as(
        "SELECT path, label, type AS acct_type, closed != 0 AS closed FROM accounts ORDER BY path",
    )
    .fetch_all(pool)
    .await
    .context("loading accounts")?;
    rows.into_iter().map(Account::try_from).collect()
}
