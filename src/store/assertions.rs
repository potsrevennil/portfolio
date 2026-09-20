//! The `balance_assertion` table: the outside figures `check` holds the ledger
//! to.

use std::fmt;

use anyhow::{bail, Context, Result};
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::SqliteConnection;
use strum_macros::{Display, EnumString};

use crate::currency::Currency;

/// Who vouches for the figure.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Display, EnumString,
)]
#[strum(serialize_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum AssertionSource {
    /// An institution's statement.
    Statement,
    /// 天天記帳's own closing balance, for accounts it was the record of.
    Tiantian,
    /// A balance the user counted (cash).
    Counted,
}

/// One assertion. Dates are inclusive: `opening` is the balance before
/// `period_start`, `closing` the balance at the end of `period_end`. Both cover
/// `account` and its subtree in `currency`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BalanceAssertion {
    pub source: AssertionSource,
    pub account: String,
    pub currency: Currency,
    pub period_start: Option<NaiveDate>,
    pub opening: Option<Decimal>,
    pub period_end: NaiveDate,
    pub closing: Decimal,
}

impl fmt::Display for BalanceAssertion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} ({} ", self.account, self.currency, self.source)?;
        match self.period_start {
            Some(start) => write!(f, "period {start}..={})", self.period_end),
            None => write!(f, "as of {})", self.period_end),
        }
    }
}

impl BalanceAssertion {
    /// The (day, balance at the end of it) pairs this states: the opening, if
    /// any, then the closing.
    pub fn points(&self) -> impl Iterator<Item = (NaiveDate, Decimal)> {
        let opening = self.period_start.zip(self.opening).map(|(start, opening)| {
            (start.pred_opt().expect("a day before period_start"), opening)
        });
        opening.into_iter().chain([(self.period_end, self.closing)])
    }
}

const SELECT: &str = "SELECT b.source, a.path AS account, b.currency, b.period_start, b.opening, \
                      b.period_end, b.closing FROM balance_assertion b JOIN accounts a ON a.id = \
                      b.account_id";

#[derive(sqlx::FromRow)]
struct Row {
    source: String,
    account: String,
    currency: String,
    period_start: Option<String>,
    opening: Option<String>,
    period_end: String,
    closing: String,
}

impl TryFrom<Row> for BalanceAssertion {
    type Error = anyhow::Error;

    fn try_from(r: Row) -> Result<Self> {
        let parsed = || -> Result<Self> {
            Ok(Self {
                source: r.source.parse()?,
                currency: r.currency.parse()?,
                period_start: r.period_start.as_deref().map(str::parse).transpose()?,
                opening: r.opening.as_deref().map(str::parse).transpose()?,
                period_end: r.period_end.parse()?,
                closing: r.closing.parse()?,
                account: r.account.clone(),
            })
        };
        parsed().with_context(|| format!("invalid balance_assertion row for {}", r.account))
    }
}

/// Records an assertion. Idempotent: re-inserting the same figures for the
/// same (account, currency, source, period_end) is a no-op, but a different
/// figure for that key is an error, since two statements cannot both be right.
pub async fn insert(conn: &mut SqliteConnection, a: &BalanceAssertion) -> Result<()> {
    if a.period_start.is_some() != a.opening.is_some() {
        bail!("assertion on {} has only one of period_start and opening", a.account);
    }
    let account_id: i64 = sqlx::query_scalar("SELECT id FROM accounts WHERE path = ?")
        .bind(&a.account)
        .fetch_optional(&mut *conn)
        .await?
        .with_context(|| format!("assertion names an unknown account {}", a.account))?;

    sqlx::query(
        "INSERT INTO balance_assertion (account_id, currency, source, period_start, opening, \
         period_end, closing) VALUES (?, ?, ?, ?, ?, ?, ?) ON CONFLICT DO NOTHING",
    )
    .bind(account_id)
    .bind(a.currency.to_string())
    .bind(a.source.to_string())
    .bind(a.period_start.map(|d| d.to_string()))
    .bind(a.opening.map(|d| d.to_string()))
    .bind(a.period_end.to_string())
    .bind(a.closing.to_string())
    .execute(&mut *conn)
    .await
    .with_context(|| format!("inserting assertion for {}", a.account))?;

    let stored: BalanceAssertion = sqlx::query_as::<_, Row>(&format!(
        "{SELECT} WHERE b.account_id = ? AND b.currency = ? AND b.source = ? AND b.period_end = ?"
    ))
    .bind(account_id)
    .bind(a.currency.to_string())
    .bind(a.source.to_string())
    .bind(a.period_end.to_string())
    .fetch_one(&mut *conn)
    .await?
    .try_into()?;
    if stored == *a {
        Ok(())
    } else {
        bail!("assertion {a:?} conflicts with the recorded {stored:?}")
    }
}

/// Every assertion, by account path, currency and date.
pub async fn load(conn: &mut SqliteConnection) -> Result<Vec<BalanceAssertion>> {
    let rows: Vec<Row> =
        sqlx::query_as(&format!("{SELECT} ORDER BY a.path, b.currency, b.period_end, b.source"))
            .fetch_all(conn)
            .await
            .context("loading balance assertions")?;
    rows.into_iter().map(BalanceAssertion::try_from).collect()
}
