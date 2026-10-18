//! What the coverage page reads: every figure an outside party vouches for,
//! and the records under each source, so the page can tell what has been
//! checked from what has merely been imported.

use anyhow::{Context, Result};
use chrono::NaiveDate;
use ledger::journal::UNVERIFIED_TAG;
use rust_decimal::Decimal;
use sqlx::SqliteConnection;

/// One row of `assertion_coverage`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Figure {
    pub account: String,
    /// A currency code, or a ticker for a position.
    pub commodity: String,
    /// `balance` (held to postings), `cash` or `position` (held to broker
    /// records).
    pub kind: String,
    pub source: String,
    /// Where a statement states its period.
    pub period_start: Option<NaiveDate>,
    pub as_of: NaiveDate,
    pub figure: Decimal,
}

#[derive(sqlx::FromRow)]
struct FigureRow {
    account: String,
    commodity: String,
    kind: String,
    source: String,
    period_start: Option<String>,
    as_of: String,
    figure: String,
}

impl TryFrom<FigureRow> for Figure {
    type Error = anyhow::Error;

    fn try_from(r: FigureRow) -> Result<Self> {
        let parsed = || -> Result<Self> {
            Ok(Figure {
                period_start: r.period_start.as_deref().map(str::parse).transpose()?,
                as_of: r.as_of.parse()?,
                figure: r.figure.parse()?,
                account: r.account.clone(),
                commodity: r.commodity.clone(),
                kind: r.kind.clone(),
                source: r.source.clone(),
            })
        };
        parsed().with_context(|| format!("invalid assertion_coverage row for {}", r.account))
    }
}

/// Every figure from outside the ledger, oldest first per account and
/// commodity. 天天記帳's closings are left out: that app's own sum of its
/// records is no institution's figure.
pub async fn figures(conn: &mut SqliteConnection) -> Result<Vec<Figure>> {
    let rows: Vec<FigureRow> = sqlx::query_as(
        "SELECT a.path AS account, c.commodity, c.kind, c.source, c.period_start, c.as_of,
                c.figure
         FROM assertion_coverage c JOIN accounts a ON a.id = c.account_id
         WHERE c.source <> 'tiantian'
         ORDER BY a.path, c.commodity, c.as_of, c.period_start",
    )
    .fetch_all(conn)
    .await
    .context("loading assertion coverage")?;
    rows.into_iter().map(Figure::try_from).collect()
}

/// One record a source holds, per commodity it moves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// The transaction's id, or the broker record's.
    pub id: i64,
    /// The account it posts to, in the source's subtree.
    pub account: String,
    pub date: NaiveDate,
    pub commodity: String,
    /// Tagged [`UNVERIFIED_TAG`]: waiting for a statement to show it.
    pub unverified: bool,
}

#[derive(sqlx::FromRow)]
struct RecordRow {
    id: i64,
    account: String,
    date: String,
    commodity: String,
    unverified: bool,
}

impl TryFrom<RecordRow> for Record {
    type Error = anyhow::Error;

    fn try_from(r: RecordRow) -> Result<Self> {
        Ok(Record {
            id: r.id,
            account: r.account,
            date: r.date.parse().with_context(|| format!("record date {:?}", r.date))?,
            commodity: r.commodity,
            unverified: r.unverified,
        })
    }
}

/// `a.path` is `?1` or under it.
const IN_SUBTREE: &str = "(a.path = ?1 OR substr(a.path, 1, length(?1) + 1) = ?1 || ':')";

/// The transactions posting to `root` or under it, one row per account and
/// currency.
pub async fn ledger_records(conn: &mut SqliteConnection, root: &str) -> Result<Vec<Record>> {
    let rows: Vec<RecordRow> = sqlx::query_as(&format!(
        "SELECT t.id, a.path AS account, t.date, p.currency AS commodity,
                max(instr(',' || coalesce(p.tags, '') || ',', ',' || ?2 || ',') > 0) AS unverified
         FROM postings p
         JOIN accounts a ON a.id = p.account_id
         JOIN transactions t ON t.id = p.transaction_id
         WHERE {IN_SUBTREE} AND t.deleted_at IS NULL
         GROUP BY t.id, a.path, p.currency
         ORDER BY t.date, t.id"
    ))
    .bind(root)
    .bind(UNVERIFIED_TAG)
    .fetch_all(conn)
    .await
    .with_context(|| format!("loading the records under {root}"))?;
    rows.into_iter().map(Record::try_from).collect()
}

/// The broker records under `root`, on the day each moves the balances: one
/// row for the cash, and one for the security where it moves shares.
pub async fn broker_records(conn: &mut SqliteConnection, root: &str) -> Result<Vec<Record>> {
    let rows: Vec<RecordRow> = sqlx::query_as(&format!(
        "SELECT r.id AS id, a.path AS account, r.settle_date AS date, r.currency AS commodity,
                0 AS unverified
         FROM broker_record r JOIN accounts a ON a.id = r.account_id
         WHERE {IN_SUBTREE}
         UNION ALL
         SELECT r.id, a.path, r.settle_date, r.symbol, 0
         FROM broker_record r JOIN accounts a ON a.id = r.account_id
         WHERE {IN_SUBTREE} AND r.symbol IS NOT NULL AND CAST(r.quantity AS REAL) <> 0
         ORDER BY date, id"
    ))
    .bind(root)
    .fetch_all(conn)
    .await
    .with_context(|| format!("loading the broker records under {root}"))?;
    rows.into_iter().map(Record::try_from).collect()
}
