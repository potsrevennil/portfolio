//! What the 對帳進度 page reads and keeps: every figure that vouches for an
//! account, every record on one, so the page can tell what has been checked
//! from what has merely been recorded, and each account's statement
//! schedule.

use anyhow::{bail, Context, Result};
use chrono::NaiveDate;
use ledger::{accounts::Chart, journal::UNVERIFIED_TAG, labels::Labels};
use ledger_types::{
    assertion::{AssertionSource, BalanceAssertion},
    cadence::{Cadence, CadenceSpec},
    currency::Currency,
};
use rust_decimal::Decimal;
use sqlx::{SqliteConnection, SqlitePool};

use crate::{
    assertions,
    import::ensure_account,
    query::LedgerData,
    review::{self, Manual, StatementAccounts},
};

/// One row of `assertion_coverage`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Figure {
    pub account: String,
    /// A currency code, or a ticker for a position.
    pub commodity: String,
    /// Cash or a position, which broker records answer to; otherwise a
    /// balance, which postings do.
    pub held_to_broker: bool,
    pub source: AssertionSource,
    /// Where a statement states its period.
    pub period_start: Option<NaiveDate>,
    pub as_of: NaiveDate,
}

#[derive(sqlx::FromRow)]
struct FigureRow {
    account: String,
    commodity: String,
    held_to_broker: bool,
    source: String,
    period_start: Option<String>,
    as_of: String,
}

impl TryFrom<FigureRow> for Figure {
    type Error = anyhow::Error;

    fn try_from(r: FigureRow) -> Result<Self> {
        let invalid = || format!("invalid assertion_coverage row for {}", r.account);
        Ok(Figure {
            held_to_broker: r.held_to_broker,
            source: r.source.parse().with_context(invalid)?,
            period_start: r
                .period_start
                .as_deref()
                .map(str::parse)
                .transpose()
                .with_context(invalid)?,
            as_of: r.as_of.parse().with_context(invalid)?,
            account: r.account,
            commodity: r.commodity,
        })
    }
}

/// Every figure that vouches for an account, oldest first per account and
/// commodity.
pub async fn figures(conn: &mut SqliteConnection) -> Result<Vec<Figure>> {
    let rows: Vec<FigureRow> = sqlx::query_as(
        "SELECT a.path AS account, c.commodity, c.kind <> 'balance' AS held_to_broker, c.source,
                c.period_start, c.as_of
         FROM assertion_coverage c JOIN accounts a ON a.id = c.account_id
         ORDER BY a.path, c.commodity, c.as_of, c.period_start",
    )
    .fetch_all(conn)
    .await
    .context("loading assertion coverage")?;
    rows.into_iter().map(Figure::try_from).collect()
}

/// One record on an account, per commodity it moves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// The transaction's id, or the broker record's.
    pub id: i64,
    /// The account it posts to.
    pub account: String,
    /// When it moves the balance a figure states.
    pub date: NaiveDate,
    pub commodity: String,
    /// What it moves the account by; zero for a broker record, whose cash
    /// and shares are figured apart.
    pub amount: Decimal,
    /// Tagged [`UNVERIFIED_TAG`]: waiting for a statement to show it.
    pub unverified: bool,
}

#[derive(sqlx::FromRow)]
struct RecordRow {
    id: i64,
    account: String,
    date: String,
    commodity: String,
    /// The legs' amounts, space-separated: summed exactly here, never in SQL.
    amounts: String,
    unverified: bool,
}

impl TryFrom<RecordRow> for Record {
    type Error = anyhow::Error;

    fn try_from(r: RecordRow) -> Result<Self> {
        Ok(Record {
            id: r.id,
            account: r.account,
            date: r.date.parse().with_context(|| format!("record date {:?}", r.date))?,
            amount: sum(&r.amounts)?,
            commodity: r.commodity,
            unverified: r.unverified,
        })
    }
}

/// Where an account stands in one currency, without its history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Position {
    pub account: String,
    pub commodity: String,
    /// All it moved, summed; zero for a broker's, as on a [`Record`].
    pub amount: Decimal,
    /// The day it last moved.
    pub last: NaiveDate,
}

#[derive(sqlx::FromRow)]
struct PositionRow {
    account: String,
    commodity: String,
    last: String,
    amounts: String,
}

impl TryFrom<PositionRow> for Position {
    type Error = anyhow::Error;

    fn try_from(r: PositionRow) -> Result<Self> {
        Ok(Position {
            last: r.last.parse().with_context(|| format!("position date {:?}", r.last))?,
            amount: sum(&r.amounts)?,
            account: r.account,
            commodity: r.commodity,
        })
    }
}

fn sum(amounts: &str) -> Result<Decimal> {
    amounts
        .split_whitespace()
        .map(str::parse::<Decimal>)
        .sum::<Result<Decimal, _>>()
        .with_context(|| format!("amounts {amounts:?}"))
}

/// Every transaction posting to an asset or liability account, one row per
/// account and currency.
pub async fn ledger_records(conn: &mut SqliteConnection) -> Result<Vec<Record>> {
    let rows: Vec<RecordRow> = sqlx::query_as(
        "SELECT t.id, a.path AS account, max(coalesce(p.posted_date, t.date)) AS date,
                p.currency AS commodity, group_concat(p.amount, ' ') AS amounts,
                max(instr(',' || coalesce(p.tags, '') || ',', ',' || ? || ',') > 0) AS unverified
         FROM postings p
         JOIN accounts a ON a.id = p.account_id
         JOIN transactions t ON t.id = p.transaction_id
         WHERE a.type IN ('asset', 'liability') AND t.deleted_at IS NULL
         GROUP BY t.id, a.path, p.currency
         ORDER BY date, t.id",
    )
    .bind(UNVERIFIED_TAG)
    .fetch_all(conn)
    .await
    .context("loading the ledger's records")?;
    rows.into_iter().map(Record::try_from).collect()
}

/// Each account's position per currency.
pub async fn ledger_positions(conn: &mut SqliteConnection) -> Result<Vec<Position>> {
    let rows: Vec<PositionRow> = sqlx::query_as(
        "SELECT a.path AS account, p.currency AS commodity,
                max(coalesce(p.posted_date, t.date)) AS last,
                group_concat(p.amount, ' ') AS amounts
         FROM postings p
         JOIN accounts a ON a.id = p.account_id
         JOIN transactions t ON t.id = p.transaction_id
         WHERE a.type IN ('asset', 'liability') AND t.deleted_at IS NULL
         GROUP BY a.path, p.currency",
    )
    .fetch_all(conn)
    .await
    .context("loading the ledger's positions")?;
    rows.into_iter().map(Position::try_from).collect()
}

/// One row for a broker record's cash, and one for the security where it
/// moves shares, on the day it moves the balances.
const BROKER_ROWS: &str = "SELECT r.id AS id, a.path AS account, r.settle_date AS date, \
                           r.currency AS commodity
     FROM broker_record r JOIN accounts a ON a.id = r.account_id
     UNION ALL
     SELECT r.id, a.path, r.settle_date, r.symbol
     FROM broker_record r JOIN accounts a ON a.id = r.account_id
     WHERE r.symbol IS NOT NULL AND CAST(r.quantity AS REAL) <> 0";

/// Every broker record.
pub async fn broker_records(conn: &mut SqliteConnection) -> Result<Vec<Record>> {
    let sql = format!(
        "SELECT id, account, date, commodity, '0' AS amounts, 0 AS unverified
         FROM ({BROKER_ROWS}) ORDER BY date, id"
    );
    let rows: Vec<RecordRow> =
        sqlx::query_as(&sql).fetch_all(conn).await.context("loading the broker records")?;
    rows.into_iter().map(Record::try_from).collect()
}

/// [`ledger_positions`] for the broker records.
pub async fn broker_positions(conn: &mut SqliteConnection) -> Result<Vec<Position>> {
    let sql = format!(
        "SELECT account, commodity, max(date) AS last, '0' AS amounts
         FROM ({BROKER_ROWS}) GROUP BY account, commodity"
    );
    let rows: Vec<PositionRow> =
        sqlx::query_as(&sql).fetch_all(conn).await.context("loading the broker positions")?;
    rows.into_iter().map(Position::try_from).collect()
}

/// An account's cadence and the bank the page files it under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schedule {
    pub account: String,
    /// `None`: the parent account's label.
    pub bank: Option<String>,
    pub cadence: Cadence,
    pub grace: u32,
    /// Records before it cannot be checked: the bank keeps no older history.
    pub since: Option<NaiveDate>,
}

#[derive(sqlx::FromRow)]
struct ScheduleRow {
    account: String,
    bank: Option<String>,
    cadence: String,
    month: Option<u32>,
    day: Option<u32>,
    every: Option<u32>,
    grace: u32,
    since: Option<String>,
}

impl TryFrom<ScheduleRow> for Schedule {
    type Error = anyhow::Error;

    fn try_from(r: ScheduleRow) -> Result<Self> {
        let spec = CadenceSpec {
            kind: r.cadence.parse().with_context(|| format!("cadence {:?}", r.cadence))?,
            month: r.month,
            day: r.day,
            every: r.every,
        };
        Ok(Schedule {
            cadence: spec
                .try_into()
                .with_context(|| format!("statement_schedule of {}", r.account))?,
            since: r.since.as_deref().map(str::parse).transpose().context("schedule since")?,
            account: r.account,
            bank: r.bank,
            grace: r.grace,
        })
    }
}

/// Every account's schedule that has one.
pub async fn schedules(conn: &mut SqliteConnection) -> Result<Vec<Schedule>> {
    let rows: Vec<ScheduleRow> = sqlx::query_as(
        "SELECT a.path AS account, s.bank, s.cadence, s.month, s.day, s.every, s.grace, s.since
         FROM statement_schedule s JOIN accounts a ON a.id = s.account_id ORDER BY a.path",
    )
    .fetch_all(conn)
    .await
    .context("loading statement schedules")?;
    rows.into_iter().map(Schedule::try_from).collect()
}

/// Records `s`, replacing the account's earlier one.
pub async fn set_schedule(conn: &mut SqliteConnection, s: &Schedule) -> Result<()> {
    let spec = CadenceSpec::from(s.cadence);
    let changed = sqlx::query(
        "INSERT INTO statement_schedule (account_id, bank, cadence, month, day, every, grace, \
         since)
         SELECT id, ?, ?, ?, ?, ?, ?, ? FROM accounts WHERE path = ?
         ON CONFLICT (account_id) DO UPDATE SET bank = excluded.bank, cadence = excluded.cadence,
             month = excluded.month, day = excluded.day, every = excluded.every,
             grace = excluded.grace, since = excluded.since",
    )
    .bind(s.bank.as_deref().map(str::trim).filter(|b| !b.is_empty()))
    .bind(spec.kind.to_string())
    .bind(spec.month)
    .bind(spec.day)
    .bind(spec.every)
    .bind(s.grace)
    .bind(s.since.map(|d| d.to_string()))
    .bind(&s.account)
    .execute(conn)
    .await
    .with_context(|| format!("saving the schedule of {}", s.account))?
    .rows_affected();
    match changed {
        0 => bail!("無這个帳戶：{}", s.account),
        _ => Ok(()),
    }
}

/// What the ledger says `account` (and its subtree) held of `currency` at the
/// end of `date`.
async fn balance(
    conn: &mut SqliteConnection,
    account: &str,
    date: NaiveDate,
    currency: Currency,
) -> Result<Decimal> {
    let data = LedgerData::load_from(conn).await?;
    Ok(data.statement_balance(account, currency, date))
}

/// A balance the reader confirmed by hand — from the bank's app, a count, a
/// shared-expense app — recorded as a `counted` figure that vouches for the
/// account as a statement's closing would. Refused unless the postings agree
/// on that day; the caller commits only on `Ok`.
pub async fn confirm(
    conn: &mut SqliteConnection,
    account: &str,
    date: NaiveDate,
    currency: Currency,
    amount: Decimal,
) -> Result<()> {
    let held = balance(conn, account, date, currency).await?;
    match held == amount {
        true => {
            let figure = BalanceAssertion::counted(account, currency, date, amount);
            assertions::insert(conn, &figure).await
        }
        false => bail!("帳簿 {date} 是 {} {currency}，毋是 {amount}", held.normalize()),
    }
}

/// Where a manual check books a difference it finds: the chart's fallback
/// categories, so the correction waits in 待確認 to be named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Corrections {
    pub income: String,
    pub expense: String,
}

/// As mapping.example.toml names them.
impl Default for Corrections {
    fn default() -> Self {
        Corrections {
            income: "Income:Uncategorized".into(),
            expense: "Expenses:Uncategorized".into(),
        }
    }
}

impl From<&Chart> for Corrections {
    fn from(chart: &Chart) -> Self {
        Corrections {
            income: chart.fallback.income.to_string(),
            expense: chart.fallback.expense.to_string(),
        }
    }
}

/// Books whatever separates the ledger from `amount` on `date`: a hand entry
/// that day against the fallback category, left unconfirmed. [`confirm`] then
/// finds them equal.
pub async fn book_difference(
    pool: &SqlitePool,
    statements: &StatementAccounts,
    to: &Corrections,
    account: &str,
    date: NaiveDate,
    currency: Currency,
    amount: Decimal,
) -> Result<()> {
    let held = balance(&mut *pool.acquire().await?, account, date, currency).await?;
    let difference = amount - held;
    match difference.is_zero() {
        true => Ok(()),
        false => {
            let category = match difference.is_sign_positive() {
                true => &to.income,
                false => &to.expense,
            };
            // A ledger that never booked an unexplained line has no such
            // account yet.
            ensure_account(&mut *pool.acquire().await?, &Labels::default(), category).await?;
            let entry = Manual {
                date,
                amount: difference.abs(),
                currency,
                account: account.to_string(),
                category: Some(category.clone()),
                counter: None,
                note: Some(format!("對帳校正：實際 {amount} {currency}")),
            };
            let id = review::enter(pool, statements, &entry).await?;
            review::unconfirm(pool, id).await
        }
    }
}
