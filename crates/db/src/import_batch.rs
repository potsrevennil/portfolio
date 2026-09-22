//! What an importer needs around its transactions: `import_batch` provenance
//! rows and the postings already on an account.

use std::{collections::HashSet, path::Path};

use anyhow::{Context, Result};
use chrono::NaiveDate;
use ledger::{journal::UNVERIFIED_TAG, model::OPENING_EQUITY};
use ledger_types::currency::Currency;
use rust_decimal::Decimal;
use sqlx::SqliteConnection;

use crate::import::Posting;

pub async fn create(
    db: &mut SqliteConnection,
    source: &str,
    file: impl AsRef<Path>,
) -> Result<i64> {
    let file = file.as_ref().display().to_string();
    Ok(sqlx::query("INSERT INTO import_batch (source, file) VALUES (?, ?)")
        .bind(source)
        .bind(&file)
        .execute(db)
        .await
        .with_context(|| format!("recording import batch for {file}"))?
        .last_insert_rowid())
}

/// One posting already on an account.
#[derive(Clone, Debug)]
pub struct LedgerPosting {
    pub date: NaiveDate,
    pub amount: Decimal,
    pub external_ref: Option<String>,
    /// Its transaction also posts to the opening equity.
    pub opening: bool,
    pub transaction_id: i64,
    /// Tagged [`UNVERIFIED_TAG`]: a record no statement has checked yet.
    pub unverified: bool,
}

#[derive(sqlx::FromRow)]
struct Row {
    date: String,
    amount: String,
    external_ref: Option<String>,
    opening: bool,
    transaction_id: i64,
    unverified: bool,
}

impl TryFrom<Row> for LedgerPosting {
    type Error = anyhow::Error;

    fn try_from(r: Row) -> Result<Self> {
        Ok(LedgerPosting {
            date: r.date.parse().with_context(|| format!("transaction date {:?}", r.date))?,
            amount: r.amount.parse().with_context(|| format!("posting amount {:?}", r.amount))?,
            external_ref: r.external_ref,
            opening: r.opening,
            transaction_id: r.transaction_id,
            unverified: r.unverified,
        })
    }
}

/// Every posting on exactly `path` in `currency`, oldest first — the subtree
/// is not included, since only this account's own lines chain to a statement.
pub async fn postings(
    db: &mut SqliteConnection,
    path: &str,
    currency: Currency,
) -> Result<Vec<LedgerPosting>> {
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT t.date, p.amount, t.external_ref, t.id AS transaction_id,
                EXISTS (SELECT 1 FROM postings q JOIN accounts b ON b.id = q.account_id
                        WHERE q.transaction_id = t.id AND b.path = ?) AS opening,
                instr(',' || coalesce(p.tags, '') || ',', ',' || ? || ',') > 0 AS unverified
         FROM postings p
         JOIN accounts a ON a.id = p.account_id
         JOIN transactions t ON t.id = p.transaction_id
         WHERE a.path = ? AND p.currency = ?
         ORDER BY t.date, t.id",
    )
    .bind(OPENING_EQUITY)
    .bind(UNVERIFIED_TAG)
    .bind(path)
    .bind(currency.to_string())
    .fetch_all(db)
    .await?;
    rows.into_iter().map(LedgerPosting::try_from).collect()
}

pub async fn refs(db: &mut SqliteConnection, prefix: &str) -> Result<HashSet<String>> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT external_ref FROM transactions
         WHERE source = 'import' AND substr(external_ref, 1, length(?1)) = ?1",
    )
    .bind(prefix)
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().collect())
}

/// Every leg of a transaction, by account path.
pub async fn legs(db: &mut SqliteConnection, transaction_id: i64) -> Result<Vec<Posting>> {
    let rows: Vec<(String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT a.path, p.amount, p.currency, p.tags FROM postings p
         JOIN accounts a ON a.id = p.account_id WHERE p.transaction_id = ? ORDER BY p.id",
    )
    .bind(transaction_id)
    .fetch_all(db)
    .await?;
    rows.into_iter()
        .map(|(account, amount, currency, tags)| {
            Ok(Posting {
                account,
                amount: amount.parse().with_context(|| format!("posting amount {amount:?}"))?,
                currency: currency.parse().with_context(|| format!("currency {currency:?}"))?,
                tags,
            })
        })
        .collect()
}

/// Removes a transaction and, by cascade, its postings.
pub async fn delete(db: &mut SqliteConnection, transaction_id: i64) -> Result<()> {
    sqlx::query("DELETE FROM transactions WHERE id = ?")
        .bind(transaction_id)
        .execute(db)
        .await
        .with_context(|| format!("deleting transaction {transaction_id}"))?;
    Ok(())
}
