//! The `holding_assertion` table: what a broker statement says an account
//! held — cash per currency, shares per security — held to the broker
//! records, as `balance_assertion` is held to the postings.

use std::fmt;

use anyhow::{bail, Context, Result};
use chrono::NaiveDate;
use ledger_types::assertion::AssertionSource;
use portfolio::broker::Commodity;
use rust_decimal::Decimal;
use sqlx::SqliteConnection;

#[derive(Debug, Clone, PartialEq)]
pub struct HoldingAssertion {
    pub source: AssertionSource,
    pub account: String,
    pub as_of: NaiveDate,
    pub commodity: Commodity,
    pub quantity: Decimal,
}

impl fmt::Display for HoldingAssertion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} ({} as of {})", self.account, self.commodity, self.source, self.as_of)
    }
}

const SELECT: &str = "SELECT h.source, a.path AS account, h.as_of, h.kind, h.commodity, \
                      h.quantity FROM holding_assertion h JOIN accounts a ON a.id = h.account_id";

#[derive(sqlx::FromRow)]
struct Row {
    source: String,
    account: String,
    as_of: String,
    kind: String,
    commodity: String,
    quantity: String,
}

impl TryFrom<Row> for HoldingAssertion {
    type Error = anyhow::Error;

    fn try_from(r: Row) -> Result<Self> {
        let parsed = || -> Result<Self> {
            let commodity = match r.kind.as_str() {
                "cash" => Commodity::Cash(r.commodity.parse()?),
                "position" => Commodity::Security(r.commodity.clone()),
                other => bail!("holding kind {other:?}"),
            };
            Ok(Self {
                source: r.source.parse()?,
                account: r.account.clone(),
                as_of: r.as_of.parse()?,
                commodity,
                quantity: r.quantity.parse()?,
            })
        };
        parsed().with_context(|| format!("invalid holding_assertion row for {}", r.account))
    }
}

fn kind(c: &Commodity) -> &'static str {
    match c {
        Commodity::Cash(_) => "cash",
        Commodity::Security(_) => "position",
    }
}

/// Records an assertion; `false` when the same figure was already recorded.
/// A different figure for the same account, day and commodity is an error,
/// since two statements cannot both be right.
pub async fn insert(conn: &mut SqliteConnection, a: &HoldingAssertion) -> Result<bool> {
    let account_id: i64 = sqlx::query_scalar("SELECT id FROM accounts WHERE path = ?")
        .bind(&a.account)
        .fetch_optional(&mut *conn)
        .await?
        .with_context(|| format!("holding assertion names an unknown account {}", a.account))?;
    let inserted = sqlx::query(
        "INSERT INTO holding_assertion (account_id, as_of, kind, commodity, quantity, source)
         VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT DO NOTHING",
    )
    .bind(account_id)
    .bind(a.as_of.to_string())
    .bind(kind(&a.commodity))
    .bind(a.commodity.to_string())
    .bind(a.quantity.to_string())
    .bind(a.source.to_string())
    .execute(&mut *conn)
    .await
    .with_context(|| format!("inserting {a}"))?
    .rows_affected()
        > 0;

    let stored: HoldingAssertion = sqlx::query_as::<_, Row>(&format!(
        "{SELECT} WHERE h.account_id = ? AND h.as_of = ? AND h.kind = ? AND h.commodity = ? AND \
         h.source = ?"
    ))
    .bind(account_id)
    .bind(a.as_of.to_string())
    .bind(kind(&a.commodity))
    .bind(a.commodity.to_string())
    .bind(a.source.to_string())
    .fetch_one(&mut *conn)
    .await?
    .try_into()?;
    if stored.quantity == a.quantity {
        Ok(inserted)
    } else {
        bail!("{a} states {}, but {} was recorded", a.quantity, stored.quantity)
    }
}

/// Every holding assertion, by account, day and commodity.
pub async fn load(conn: &mut SqliteConnection) -> Result<Vec<HoldingAssertion>> {
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "{SELECT} ORDER BY a.path, h.as_of, h.kind, h.commodity, h.source"
    ))
    .fetch_all(conn)
    .await
    .context("loading holding assertions")?;
    rows.into_iter().map(HoldingAssertion::try_from).collect()
}

/// One row of the `assertion_coverage` view: a figure some outside party
/// vouches for, from either assertion table.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Coverage {
    pub account: String,
    pub commodity: String,
    pub as_of: String,
    pub source: String,
    /// `balance` (held to postings), `cash` or `position` (held to broker
    /// records).
    pub kind: String,
}

/// The newest vouched day per account, commodity and kind — what a coverage
/// report shows as 已對帳至.
pub async fn coverage(conn: &mut SqliteConnection) -> Result<Vec<Coverage>> {
    sqlx::query_as(
        "SELECT a.path AS account, c.commodity, max(c.as_of) AS as_of, c.source, c.kind
         FROM assertion_coverage c JOIN accounts a ON a.id = c.account_id
         GROUP BY a.path, c.commodity, c.source, c.kind
         ORDER BY a.path, c.kind, c.commodity",
    )
    .fetch_all(conn)
    .await
    .context("loading assertion coverage")
}
