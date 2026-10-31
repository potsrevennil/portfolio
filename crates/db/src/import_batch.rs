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

use crate::{
    events::{self, Change, EventKind},
    import::{ensure_account, Posting},
};

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
    pub id: i64,
    pub date: NaiveDate,
    /// The day a statement counted it, where that is not `date`.
    pub posted: Option<NaiveDate>,
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

impl LedgerPosting {
    /// The day it counts on a statement.
    pub fn on_statement(&self) -> NaiveDate { self.posted.unwrap_or(self.date) }
}

/// `char(31)` in the queries: no ref or account path holds it.
const SEPARATOR: char = '\u{1f}';

fn split(concatenated: Option<&str>) -> Vec<String> {
    concatenated.into_iter().flat_map(|c| c.split(SEPARATOR)).map(String::from).collect()
}

#[derive(sqlx::FromRow)]
struct Row {
    id: i64,
    date: String,
    posted_date: Option<String>,
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
            id: r.id,
            date: r.date.parse().with_context(|| format!("transaction date {:?}", r.date))?,
            posted: r.posted_date.as_deref().map(str::parse).transpose().context("posted date")?,
            amount: r.amount.parse().with_context(|| format!("posting amount {:?}", r.amount))?,
            refs: r.external_ref.into_iter().chain(split(r.aliases.as_deref())).collect(),
            other_accounts: split(r.others.as_deref()),
            opening: r.opening,
            transaction_id: r.transaction_id,
            unverified: r.unverified,
        })
    }
}

/// Every posting on exactly `path` in `currency`, in the order a statement
/// counts them — the subtree is not included, since only this account's own
/// lines chain to a statement.
pub async fn postings(
    db: &mut SqliteConnection,
    path: &str,
    currency: Currency,
) -> Result<Vec<LedgerPosting>> {
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT p.id, t.date, p.posted_date, p.amount, t.external_ref, t.id AS transaction_id,
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
         ORDER BY coalesce(p.posted_date, t.date), t.id",
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

/// Takes the unverified tag off a checked record's legs, bar those on
/// `unchecked` accounts: a statement of their own has yet to vouch for them.
async fn clear_unverified(
    db: &mut SqliteConnection,
    transaction_id: i64,
    unchecked: &[String],
) -> Result<()> {
    let held = vec!["?"; unchecked.len()].join(", ");
    let sql = format!(
        "UPDATE postings SET tags = nullif(trim(replace(',' || tags || ',', ',' || ? || ',', \
         ','), ','), '') WHERE transaction_id = ? AND account_id NOT IN
         (SELECT id FROM accounts WHERE path IN ({held}))"
    );
    let mut clear = sqlx::query(&sql).bind(UNVERIFIED_TAG).bind(transaction_id);
    for account in unchecked {
        clear = clear.bind(account);
    }
    clear
        .execute(db)
        .await
        .with_context(|| format!("clearing the unverified tag of transaction {transaction_id}"))?;
    Ok(())
}

/// Moves transaction `transaction_id`'s leg of `amount` from the account at
/// `from` to the one at `to`: the bank showed it on a sibling account.
pub async fn move_leg(
    db: &mut SqliteConnection,
    labels: &Labels,
    transaction_id: i64,
    from: &str,
    to: &str,
    amount: Decimal,
) -> Result<()> {
    let to_id = ensure_account(db, labels, to).await?;
    sqlx::query(
        "UPDATE postings SET account_id = ? WHERE transaction_id = ? AND amount = ?
         AND account_id = (SELECT id FROM accounts WHERE path = ?)",
    )
    .bind(to_id)
    .bind(transaction_id)
    .bind(amount.to_string())
    .bind(from)
    .execute(db)
    .await
    .with_context(|| format!("moving transaction {transaction_id} from {from} to {to}"))?;
    Ok(())
}

/// Adds a leg's [`UNVERIFIED_TAG`], or takes it off.
pub(crate) async fn tag_unverified(
    db: &mut SqliteConnection,
    posting_id: i64,
    on: bool,
) -> Result<()> {
    let sql = match on {
        true => {
            "UPDATE postings SET tags = CASE WHEN instr(',' || coalesce(tags, '') || ',', ',' || \
             ?1 || ',') > 0 THEN tags ELSE coalesce(tags || ',', '') || ?1 END WHERE id = ?2"
        }
        false => {
            "UPDATE postings SET tags = nullif(trim(replace(',' || tags || ',', ',' || ?1 || ',', \
             ','), ','), '') WHERE id = ?2"
        }
    };
    sqlx::query(sql).bind(UNVERIFIED_TAG).bind(posting_id).execute(db).await?;
    Ok(())
}

/// What a statement says of a leg already on the ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stamp {
    pub posting_id: i64,
    pub transaction_id: i64,
    /// The day the statement counts it.
    pub posted: NaiveDate,
    pub amount: Decimal,
    pub vouched: Vouched,
    /// Accounts of its other legs that a statement of their own has yet to
    /// vouch for. Their legs stay unverified.
    pub unchecked: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Vouched {
    /// A line shows it. The line's ref goes on the first record it stands for.
    ByLine(Option<String>),
    /// No line yet: it is still to post, after the statement, and waits
    /// tagged [`UNVERIFIED_TAG`] for the next one.
    NotYet,
}

/// Records what a statement said of one leg. Only that leg: the transaction
/// stays on its own date, and any other leg stays for its own statement.
pub async fn stamp(db: &mut SqliteConnection, s: &Stamp) -> Result<()> {
    sqlx::query("UPDATE postings SET posted_date = ? WHERE id = ?")
        .bind(s.posted.to_string())
        .bind(s.posting_id)
        .execute(&mut *db)
        .await
        .with_context(|| format!("stamping posting {}", s.posting_id))?;
    match &s.vouched {
        Vouched::NotYet => tag_unverified(db, s.posting_id, true).await,
        Vouched::ByLine(line) => {
            clear_unverified(db, s.transaction_id, &s.unchecked).await?;
            match line {
                Some(line) => add_ref(db, s.transaction_id, line).await,
                None => Ok(()),
            }
        }
    }
}

/// One record's leg, and the statement lines that add up to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegSplit {
    pub posting_id: i64,
    pub transaction_id: i64,
    pub parts: Vec<LegPart>,
    /// As on [`Stamp::unchecked`].
    pub unchecked: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegPart {
    pub amount: Decimal,
    pub posted: NaiveDate,
    /// The line's ref.
    pub line: String,
}

/// Divides a leg between the lines that add up to it, each on the day it
/// posted: a leg has one day, and they posted apart. The transaction keeps
/// its id, date and category, and the change goes in its event log.
pub async fn split_leg(db: &mut SqliteConnection, s: &LegSplit) -> Result<()> {
    let before = events::snapshot(db, s.transaction_id).await?;
    let (first, rest) = s.parts.split_first().context("a leg split into nothing")?;
    sqlx::query("UPDATE postings SET amount = ?, posted_date = ? WHERE id = ?")
        .bind(first.amount.to_string())
        .bind(first.posted.to_string())
        .bind(s.posting_id)
        .execute(&mut *db)
        .await
        .with_context(|| format!("splitting posting {}", s.posting_id))?;
    clear_unverified(db, s.transaction_id, &s.unchecked).await?;
    for part in rest {
        sqlx::query(
            "INSERT INTO postings (transaction_id, account_id, amount, currency, tags, origin, \
             posted_date) SELECT transaction_id, account_id, ?, currency, tags, origin, ? FROM \
             postings WHERE id = ?",
        )
        .bind(part.amount.to_string())
        .bind(part.posted.to_string())
        .bind(s.posting_id)
        .execute(&mut *db)
        .await
        .with_context(|| format!("splitting posting {}", s.posting_id))?;
    }
    for part in &s.parts {
        add_ref(db, s.transaction_id, &part.line).await?;
    }
    let after = events::snapshot(db, s.transaction_id).await?;
    let change = Change { before: Some(before), after };
    events::record(db, s.transaction_id, EventKind::Split, &change).await
}

/// A transaction with a leg on some account, and all its legs.
#[derive(Clone, Debug)]
pub struct HeldTransaction {
    pub id: i64,
    pub date: NaiveDate,
    pub external_ref: Option<String>,
    /// The further source records it stands for.
    pub aliases: Vec<String>,
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
    aliases: Option<String>,
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
                (SELECT group_concat(r.external_ref, char(31)) FROM transaction_refs r
                 WHERE r.transaction_id = t.id) AS aliases,
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
                aliases: split(r.aliases.as_deref()),
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
            "INSERT INTO postings (transaction_id, account_id, amount, currency, tags, origin) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(transaction_id)
        .bind(account_id)
        .bind(leg.amount.to_string())
        .bind(leg.currency.to_string())
        .bind(&leg.tags)
        .bind(leg.origin.map(|o| o.to_string()))
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
        // instr, not LIKE: a memo is text, and `%` or `_` in it would make a
        // LIKE pattern that matches the wrong thing.
        "UPDATE transactions SET narration = CASE WHEN coalesce(narration, '') = '' THEN ?1 ELSE \
         narration || ' · ' || ?1 END WHERE id = ?2 AND instr(coalesce(narration, ''), ?1) = 0",
    )
    .bind(text)
    .bind(transaction_id)
    .execute(db)
    .await
    .with_context(|| format!("narrating transaction {transaction_id}"))?;
    Ok(())
}
