//! `verification_choice`: statement lines the importer could not pair with an
//! unverified record on its own, and the record a person picked for each.

use std::collections::HashMap;

use anyhow::{bail, Context, Result};
use chrono::NaiveDate;
use ledger_types::currency::Currency;
use rust_decimal::Decimal;
use sqlx::{SqliteConnection, SqlitePool};

use crate::events::{self, EventKind, Pairing};

/// A line that fits more than one unverified record, or whose one record
/// fits another line too.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ambiguity {
    pub account: String,
    pub currency: Currency,
    pub statement_ref: String,
    pub date: NaiveDate,
    pub amount: Decimal,
    pub description: String,
    /// The unverified records it fits.
    pub candidates: Vec<i64>,
}

/// An ambiguity whose line no transaction holds yet, with the pick if made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    pub id: i64,
    pub ambiguity: Ambiguity,
    pub chosen: Option<i64>,
}

#[derive(sqlx::FromRow)]
struct ChoiceRow {
    id: i64,
    path: String,
    currency: String,
    statement_ref: String,
    date: String,
    amount: String,
    description: String,
    candidates: String,
    transaction_id: Option<i64>,
}

impl TryFrom<ChoiceRow> for Choice {
    type Error = anyhow::Error;

    fn try_from(r: ChoiceRow) -> Result<Self> {
        Ok(Choice {
            id: r.id,
            ambiguity: Ambiguity {
                account: r.path,
                currency: r
                    .currency
                    .parse()
                    .with_context(|| format!("currency {:?}", r.currency))?,
                date: r.date.parse().with_context(|| format!("line date {:?}", r.date))?,
                amount: r.amount.parse().with_context(|| format!("line amount {:?}", r.amount))?,
                candidates: serde_json::from_str(&r.candidates).context("choice candidates")?,
                statement_ref: r.statement_ref,
                description: r.description,
            },
            chosen: r.transaction_id,
        })
    }
}

/// Records lines an import refused to pair. A line recorded before keeps
/// its pick; one not yet picked takes the candidates seen now.
pub async fn record(db: &mut SqliteConnection, lines: &[Ambiguity]) -> Result<()> {
    for a in lines {
        sqlx::query(
            "INSERT INTO verification_choice
                (account_id, currency, statement_ref, date, amount, description, candidates)
             SELECT id, ?, ?, ?, ?, ?, ? FROM accounts WHERE path = ?
             ON CONFLICT (statement_ref) DO UPDATE SET candidates = excluded.candidates
             WHERE transaction_id IS NULL",
        )
        .bind(a.currency.to_string())
        .bind(&a.statement_ref)
        .bind(a.date.to_string())
        .bind(a.amount.to_string())
        .bind(&a.description)
        .bind(serde_json::to_string(&a.candidates)?)
        .bind(&a.account)
        .execute(&mut *db)
        .await
        .with_context(|| format!("recording the choice for {}", a.statement_ref))?;
    }
    Ok(())
}

const OPEN: &str = "SELECT c.id, a.path, c.currency, c.statement_ref, c.date, c.amount,
        c.description, c.candidates, c.transaction_id
    FROM verification_choice c JOIN accounts a ON a.id = c.account_id
    WHERE c.statement_ref NOT IN (SELECT external_ref FROM transaction_refs
        UNION ALL SELECT external_ref FROM transactions WHERE external_ref IS NOT NULL)";

/// Lines no import has used yet, newest first.
pub async fn open(pool: &SqlitePool) -> Result<Vec<Choice>> {
    let rows: Vec<ChoiceRow> = sqlx::query_as(&format!("{OPEN} ORDER BY c.date DESC, c.id DESC"))
        .fetch_all(pool)
        .await
        .context("reading pairing choices")?;
    rows.into_iter().map(Choice::try_from).collect()
}

/// The picks on `account` in `currency`, by statement ref: what the next
/// import verifies with.
pub async fn chosen(
    db: &mut SqliteConnection,
    account: &str,
    currency: Currency,
) -> Result<HashMap<String, i64>> {
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT c.statement_ref, c.transaction_id FROM verification_choice c
         JOIN accounts a ON a.id = c.account_id
         WHERE a.path = ? AND c.currency = ? AND c.transaction_id IS NOT NULL",
    )
    .bind(account)
    .bind(currency.to_string())
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().collect())
}

/// Picks the record the line of choice `id` verifies. A record picked for
/// another open line cannot be picked again: one line verifies one record.
pub async fn choose(pool: &SqlitePool, id: i64, transaction_id: i64) -> Result<()> {
    let mut db = pool.begin().await?;
    let row: ChoiceRow = sqlx::query_as(&format!("{OPEN} AND c.id = ?"))
        .bind(id)
        .fetch_optional(&mut *db)
        .await?
        .with_context(|| format!("查無待配對的對帳單明細 {id}"))?;
    let choice = Choice::try_from(row)?;
    if !choice.ambiguity.candidates.contains(&transaction_id) {
        bail!("這筆不在可配對的紀錄裡");
    }
    let taken: Option<String> = sqlx::query_scalar(&format!(
        "SELECT c.statement_ref FROM ({OPEN}) c WHERE c.transaction_id = ? AND c.id <> ?"
    ))
    .bind(transaction_id)
    .bind(id)
    .fetch_optional(&mut *db)
    .await?;
    if taken.is_some() {
        bail!("這筆紀錄已配給另一筆明細");
    }
    sqlx::query(
        "UPDATE verification_choice SET transaction_id = ?,
            chosen_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?",
    )
    .bind(transaction_id)
    .bind(id)
    .execute(&mut *db)
    .await?;
    let a = &choice.ambiguity;
    let pairing =
        Pairing { statement_ref: a.statement_ref.clone(), date: a.date, amount: a.amount };
    events::record(&mut db, transaction_id, EventKind::Paired, &pairing).await?;
    db.commit().await?;
    Ok(())
}
