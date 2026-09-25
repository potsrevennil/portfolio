//! What an importer needs around its transactions: `import_batch` provenance
//! rows and the postings already on an account.

use std::{collections::HashSet, path::Path};

use anyhow::{Context, Result};
use chrono::NaiveDate;
use ledger::{
    journal::UNVERIFIED_TAG,
    labels::Labels,
    model::{Source, OPENING_EQUITY},
};
use ledger_types::currency::Currency;
use rust_decimal::Decimal;
use sqlx::SqliteConnection;

use crate::import::{ensure_account, Posting};

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
    /// Where its transaction's other legs post.
    pub other_accounts: Vec<String>,
    /// Its transaction also posts to the opening equity.
    pub opening: bool,
    pub transaction_id: i64,
    /// Tagged [`UNVERIFIED_TAG`]: a record no statement has checked yet.
    pub unverified: bool,
}

/// `char(31)` in the queries: no ref or account path holds it.
const SEPARATOR: char = '\u{1f}';

fn split(concatenated: Option<&str>) -> Vec<String> {
    concatenated.into_iter().flat_map(|c| c.split(SEPARATOR)).map(String::from).collect()
}

#[derive(sqlx::FromRow)]
struct Row {
    date: String,
    amount: String,
    external_ref: Option<String>,
    aliases: Option<String>,
    others: Option<String>,
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
            refs: r.external_ref.into_iter().chain(split(r.aliases.as_deref())).collect(),
            other_accounts: split(r.others.as_deref()),
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
                (SELECT group_concat(b.path, char(31)) FROM postings q
                 JOIN accounts b ON b.id = q.account_id
                 WHERE q.transaction_id = t.id AND b.id <> p.account_id) AS others,
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

/// An unverified record a statement line checked, and what that leaves
/// unchecked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verification {
    pub transaction_id: i64,
    /// The line's date, or `None` to leave the record on its own: a
    /// transaction has one date, so re-dating moves its every leg.
    pub date: Option<NaiveDate>,
    pub statement_ref: String,
    /// Legs on these accounts stay unverified: no line here vouched for them.
    pub unchecked: Vec<String>,
}

/// Marks an unverified record as checked by a statement line: it takes the
/// line's date, stands for the line too, and its legs lose the unverified tag,
/// bar those on `unchecked` accounts. Its own ref stays, so its source still
/// knows it, and so do its category and review state.
pub async fn verify(db: &mut SqliteConnection, v: &Verification) -> Result<()> {
    if let Some(date) = v.date {
        redate(db, v.transaction_id, date).await?;
    }
    add_ref(db, v.transaction_id, &v.statement_ref).await?;
    let held = vec!["?"; v.unchecked.len()].join(", ");
    let sql = format!(
        "UPDATE postings SET tags = nullif(trim(replace(',' || tags || ',', ',' || ? || ',', \
         ','), ','), '') WHERE transaction_id = ? AND account_id NOT IN
         (SELECT id FROM accounts WHERE path IN ({held}))"
    );
    let mut clear = sqlx::query(&sql).bind(UNVERIFIED_TAG).bind(v.transaction_id);
    for account in &v.unchecked {
        clear = clear.bind(account);
    }
    clear.execute(db).await.with_context(|| {
        format!("clearing the unverified tag of transaction {}", v.transaction_id)
    })?;
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

/// A transaction with a leg on some account, and all its legs.
#[derive(Clone, Debug)]
pub struct HeldTransaction {
    pub id: i64,
    pub date: NaiveDate,
    pub external_ref: Option<String>,
    pub postings: Vec<HeldPosting>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeldPosting {
    pub id: i64,
    pub account: String,
    pub amount: Decimal,
    pub currency: Currency,
}

#[derive(sqlx::FromRow)]
struct HeldRow {
    transaction_id: i64,
    date: String,
    external_ref: Option<String>,
    posting_id: i64,
    account: String,
    amount: String,
    currency: String,
}

/// Every transaction with a leg on exactly `path` in `currency`, oldest first.
pub async fn transactions_on(
    db: &mut SqliteConnection,
    path: &str,
    currency: Currency,
) -> Result<Vec<HeldTransaction>> {
    let rows: Vec<HeldRow> = sqlx::query_as(
        "SELECT t.id AS transaction_id, t.date, t.external_ref, p.id AS posting_id,
                a.path AS account, p.amount, p.currency
         FROM transactions t
         JOIN postings p ON p.transaction_id = t.id
         JOIN accounts a ON a.id = p.account_id
         WHERE t.id IN (SELECT q.transaction_id FROM postings q JOIN accounts b
                        ON b.id = q.account_id WHERE b.path = ? AND q.currency = ?)
         ORDER BY t.date, t.id, p.id",
    )
    .bind(path)
    .bind(currency.to_string())
    .fetch_all(db)
    .await?;
    let mut out: Vec<HeldTransaction> = Vec::new();
    for r in rows {
        let posting = HeldPosting {
            id: r.posting_id,
            amount: r.amount.parse().with_context(|| format!("posting amount {:?}", r.amount))?,
            currency: r.currency.parse()?,
            account: r.account,
        };
        match out.last_mut() {
            Some(t) if t.id == r.transaction_id => t.postings.push(posting),
            _ => out.push(HeldTransaction {
                id: r.transaction_id,
                date: r.date.parse().with_context(|| format!("transaction date {:?}", r.date))?,
                external_ref: r.external_ref,
                postings: vec![posting],
            }),
        }
    }
    Ok(out)
}

/// Replaces one leg of a transaction with `legs`, in place: the transaction
/// keeps its id, date, ref and review state.
pub async fn replace_leg(
    db: &mut SqliteConnection,
    labels: &Labels,
    posting_id: i64,
    legs: &[Posting],
) -> Result<()> {
    let transaction_id: i64 =
        sqlx::query_scalar("SELECT transaction_id FROM postings WHERE id = ?")
            .bind(posting_id)
            .fetch_one(&mut *db)
            .await
            .with_context(|| format!("finding posting {posting_id}"))?;
    sqlx::query("DELETE FROM postings WHERE id = ?").bind(posting_id).execute(&mut *db).await?;
    for leg in legs {
        let account_id = ensure_account(db, labels, &leg.account).await?;
        sqlx::query(
            "INSERT INTO postings (transaction_id, account_id, amount, currency, tags) VALUES (?, \
             ?, ?, ?, ?)",
        )
        .bind(transaction_id)
        .bind(account_id)
        .bind(leg.amount.to_string())
        .bind(leg.currency.to_string())
        .bind(&leg.tags)
        .execute(&mut *db)
        .await
        .with_context(|| format!("relabelling transaction {transaction_id}"))?;
    }
    Ok(())
}

/// The currencies `path` holds postings in, its subtree included — the ground
/// an assertion on it covers.
pub async fn currencies(db: &mut SqliteConnection, path: &str) -> Result<Vec<Currency>> {
    let codes: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT p.currency FROM postings p JOIN accounts a ON a.id = p.account_id WHERE \
         (a.path = ?1 OR a.path LIKE ?1 || ':%') ORDER BY p.currency",
    )
    .bind(path)
    .fetch_all(db)
    .await?;
    codes.iter().map(|c| Ok(c.parse()?)).collect()
}

/// Every ref recorded under `source`, whose own scheme names them.
pub async fn refs_of(db: &mut SqliteConnection, source: Source) -> Result<HashSet<String>> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT external_ref FROM transactions WHERE source = ? AND external_ref IS NOT NULL",
    )
    .bind(source.to_string())
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().collect())
}

/// Adds `text` to a transaction's narration, keeping what is already there: a
/// relabelled bank line says what the bank called it and what the record did.
pub async fn append_narration(
    db: &mut SqliteConnection,
    transaction_id: i64,
    text: &str,
) -> Result<()> {
    sqlx::query(
        "UPDATE transactions SET narration = iif(coalesce(narration, '') = '', ?1, narration || ' \
         · ' || ?1) WHERE id = ?2 AND coalesce(narration, '') NOT LIKE '%' || ?1 || '%'",
    )
    .bind(text)
    .bind(transaction_id)
    .execute(db)
    .await
    .with_context(|| format!("narrating transaction {transaction_id}"))?;
    Ok(())
}
