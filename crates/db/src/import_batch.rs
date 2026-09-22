//! What an importer needs around its transactions: `import_batch` provenance
//! rows and the postings already on an account.

use std::{collections::HashSet, path::Path};

use anyhow::{Context, Result};
use chrono::NaiveDate;
use ledger::{journal::UNVERIFIED_TAG, model::OPENING_EQUITY};
use ledger_types::currency::Currency;
use rust_decimal::Decimal;
use sqlx::SqliteConnection;

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
    /// Its transaction's ref, then any further ones it stands for.
    pub refs: Vec<String>,
    /// Its transaction also posts to the opening equity.
    pub opening: bool,
    pub transaction_id: i64,
    /// Tagged [`UNVERIFIED_TAG`]: a record no statement has checked yet.
    pub unverified: bool,
}

/// `char(31)` in the query: no ref holds it.
const ALIAS_SEPARATOR: char = '\u{1f}';

#[derive(sqlx::FromRow)]
struct Row {
    date: String,
    amount: String,
    external_ref: Option<String>,
    aliases: Option<String>,
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
            refs: r
                .external_ref
                .into_iter()
                .chain(r.aliases.iter().flat_map(|a| a.split(ALIAS_SEPARATOR)).map(String::from))
                .collect(),
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
                (SELECT group_concat(r.external_ref, char(31)) FROM transaction_refs r
                 WHERE r.transaction_id = t.id) AS aliases,
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

/// Refs under `prefix` from any source, including those a transaction also
/// stands for.
pub async fn refs(db: &mut SqliteConnection, prefix: &str) -> Result<HashSet<String>> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT external_ref FROM transactions WHERE substr(external_ref, 1, length(?1)) = ?1
         UNION SELECT external_ref FROM transaction_refs
         WHERE substr(external_ref, 1, length(?1)) = ?1",
    )
    .bind(prefix)
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().collect())
}

/// Records that `transaction_id` also stands for the source record
/// `external_ref`, so that record's importer takes it as held.
pub async fn add_ref(
    db: &mut SqliteConnection,
    transaction_id: i64,
    external_ref: &str,
) -> Result<()> {
    sqlx::query("INSERT INTO transaction_refs (transaction_id, external_ref) VALUES (?, ?)")
        .bind(transaction_id)
        .bind(external_ref)
        .execute(db)
        .await
        .with_context(|| format!("adding {external_ref} to transaction {transaction_id}"))?;
    Ok(())
}

/// Marks an unverified record as checked by a statement line: it takes the
/// line's date, stands for the line too, and its legs lose the unverified
/// tag. Its own ref stays, so its source still knows it, and so do its
/// category and review state.
pub async fn verify(
    db: &mut SqliteConnection,
    transaction_id: i64,
    date: NaiveDate,
    statement_ref: &str,
) -> Result<()> {
    redate(db, transaction_id, date).await?;
    add_ref(db, transaction_id, statement_ref).await?;
    sqlx::query(
        "UPDATE postings SET tags = nullif(trim(replace(',' || tags || ',', ',' || ?1 || ',', \
         ','), ','), '') WHERE transaction_id = ?2",
    )
    .bind(UNVERIFIED_TAG)
    .bind(transaction_id)
    .execute(db)
    .await
    .with_context(|| format!("clearing the unverified tag of transaction {transaction_id}"))?;
    Ok(())
}

pub async fn redate(db: &mut SqliteConnection, transaction_id: i64, date: NaiveDate) -> Result<()> {
    sqlx::query("UPDATE transactions SET date = ? WHERE id = ?")
        .bind(date.to_string())
        .bind(transaction_id)
        .execute(db)
        .await
        .with_context(|| format!("re-dating transaction {transaction_id}"))?;
    Ok(())
}
