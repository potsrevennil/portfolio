//! The `balance_assertion` table: the outside figures `check` holds the ledger
//! to.

use anyhow::{bail, Context, Result};
pub use ledger_types::assertion::{AssertionSource, BalanceAssertion};
use sqlx::SqliteConnection;

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

/// Every assertion still in force, by account path, currency and date.
pub async fn load(conn: &mut SqliteConnection) -> Result<Vec<BalanceAssertion>> {
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "{SELECT} WHERE b.superseded_at IS NULL ORDER BY a.path, b.currency, b.period_end, \
         b.source"
    ))
    .fetch_all(conn)
    .await
    .context("loading balance assertions")?;
    rows.into_iter().map(BalanceAssertion::try_from).collect()
}

/// Marks a 天天記帳 closing no longer true: a person changed the records it
/// summed. It stays on record; `load` leaves it out.
pub async fn supersede(conn: &mut SqliteConnection, a: &BalanceAssertion) -> Result<()> {
    if a.source != AssertionSource::Tiantian {
        bail!("only a 天天記帳 closing can be superseded, not {a}");
    }
    sqlx::query(
        "UPDATE balance_assertion SET superseded_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE \
         superseded_at IS NULL AND currency = ? AND source = ? AND period_end = ? AND account_id \
         = (SELECT id FROM accounts WHERE path = ?)",
    )
    .bind(a.currency.to_string())
    .bind(a.source.to_string())
    .bind(a.period_end.to_string())
    .bind(&a.account)
    .execute(conn)
    .await
    .with_context(|| format!("superseding {a}"))?;
    Ok(())
}
