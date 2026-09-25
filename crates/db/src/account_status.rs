//! Closing and reopening accounts: `accounts.closed`, with the dated
//! `account_events` row each change leaves.

use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};
use ledger_types::currency::Currency;
use rust_decimal::Decimal;
use sqlx::{SqliteConnection, SqlitePool};

use crate::query::AccountType;

#[derive(sqlx::FromRow)]
struct Row {
    id: i64,
    acct_type: String,
    closed: bool,
}

async fn find(db: &mut SqliteConnection, path: &str) -> Result<Row> {
    sqlx::query_as(
        "SELECT id, type AS acct_type, closed != 0 AS closed FROM accounts WHERE path = ?",
    )
    .bind(path)
    .fetch_optional(db)
    .await?
    .with_context(|| format!("查無帳戶：{path}"))
}

async fn set(db: &mut SqliteConnection, id: i64, closed: bool, note: Option<&str>) -> Result<()> {
    sqlx::query(
        "UPDATE accounts SET closed = ?, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE \
         id = ?",
    )
    .bind(closed)
    .bind(id)
    .execute(&mut *db)
    .await?;
    sqlx::query("INSERT INTO account_events (account_id, event, note) VALUES (?, ?, ?)")
        .bind(id)
        .bind(if closed { "closed" } else { "reopened" })
        .bind(note.map(str::trim).filter(|n| !n.is_empty()))
        .execute(&mut *db)
        .await?;
    Ok(())
}

/// Closes an account that holds nothing and has no open account under it.
/// A closed account holding money would drop it from every picker while it
/// still counts in the totals.
pub async fn close(pool: &SqlitePool, path: &str, note: Option<&str>) -> Result<()> {
    let mut db = pool.begin().await?;
    let row = find(&mut db, path).await?;
    if row.closed {
        bail!("帳戶已經結清：{path}");
    }
    let account_type: AccountType =
        row.acct_type.parse().with_context(|| format!("account type {:?}", row.acct_type))?;
    if account_type == AccountType::Equity || !path.contains(':') {
        bail!("這個帳戶不能結清：{path}");
    }
    let open_children: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM accounts WHERE closed = 0 AND substr(path, 1, length(?1) + 1) = ?1 \
         || ':'",
    )
    .bind(path)
    .fetch_one(&mut *db)
    .await?;
    if open_children > 0 {
        bail!("底下還有 {open_children} 個帳戶沒結清");
    }
    // Summed in Rust: amounts are exact decimal strings.
    let postings: Vec<(String, String)> = sqlx::query_as(
        "SELECT p.amount, p.currency FROM postings p JOIN accounts a ON a.id = p.account_id
         WHERE a.path = ?1 OR substr(a.path, 1, length(?1) + 1) = ?1 || ':'",
    )
    .bind(path)
    .fetch_all(&mut *db)
    .await?;
    let mut balance: BTreeMap<Currency, Decimal> = BTreeMap::new();
    for (amount, currency) in postings {
        *balance
            .entry(currency.parse().with_context(|| format!("currency {currency:?}"))?)
            .or_default() +=
            amount.parse::<Decimal>().with_context(|| format!("amount {amount:?}"))?;
    }
    let held: Vec<String> =
        balance.iter().filter(|(_, b)| !b.is_zero()).map(|(c, b)| format!("{b} {c}")).collect();
    if !held.is_empty() {
        bail!("餘額不是 0，不能結清：{}", held.join("、"));
    }
    set(&mut db, row.id, true, note).await?;
    db.commit().await?;
    Ok(())
}

/// Reopens an account, and every closed account above it so it shows again.
pub async fn reopen(pool: &SqlitePool, path: &str, note: Option<&str>) -> Result<()> {
    let mut db = pool.begin().await?;
    let row = find(&mut db, path).await?;
    if !row.closed {
        bail!("帳戶沒有結清：{path}");
    }
    let ancestors = path.match_indices(':').map(|(i, _)| &path[..i]);
    for p in ancestors.chain([path]) {
        let row = find(&mut db, p).await?;
        if row.closed {
            set(&mut db, row.id, false, note).await?;
        }
    }
    db.commit().await?;
    Ok(())
}
