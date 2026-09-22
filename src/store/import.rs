//! Import-time dedup on `(source, external_ref)`.
//!
//! `source` is the coarse class (`import`/`manual`/`tiantian`), so all
//! importers share one scope: `external_ref` must be unique across importers.
//! For Cathay lines, build it with `BankStatement::dedup_refs` — the same
//! helper the freeze bake uses — or frozen and re-imported lines won't collide.

use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};
use chrono::NaiveDate;
use rust_decimal::Decimal;
use sqlx::{SqliteConnection, SqlitePool};

use crate::{
    currency::Currency,
    ledger::{labels::Labels, load::schema_type, model::Source},
};

/// A transaction not yet stored, with its legs.
#[derive(Clone, Debug)]
pub struct Transaction {
    pub date: NaiveDate,
    pub payee: Option<String>,
    pub narration: Option<String>,
    pub source: Source,
    /// `None` is exempt from dedup (hand-entered rows).
    pub external_ref: Option<String>,
    pub import_batch_id: Option<i64>,
    pub postings: Vec<Posting>,
}

/// A leg, by account path: an account named for the first time is created.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Posting {
    pub account: String,
    pub amount: Decimal,
    pub currency: Currency,
    pub tags: Option<String>,
}

/// An untagged leg: `(account, amount, currency).into()`.
impl<A: Into<String>> From<(A, Decimal, Currency)> for Posting {
    fn from((account, amount, currency): (A, Decimal, Currency)) -> Self {
        Posting { account: account.into(), amount, currency, tags: None }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InsertOutcome {
    Inserted(i64),
    /// Carries the existing row's id; nothing was written.
    Duplicate(i64),
}

impl InsertOutcome {
    pub fn inserted_id(&self) -> Option<i64> {
        match self {
            InsertOutcome::Inserted(id) => Some(*id),
            InsertOutcome::Duplicate(_) => None,
        }
    }

    pub fn transaction_id(&self) -> i64 {
        match self {
            InsertOutcome::Inserted(id) | InsertOutcome::Duplicate(id) => *id,
        }
    }
}

pub struct ImportStore {
    pool: SqlitePool,
    /// Names the accounts an insert creates.
    labels: Labels,
}

impl ImportStore {
    pub fn new(pool: SqlitePool, labels: Labels) -> Self { ImportStore { pool, labels } }

    pub fn pool(&self) -> &SqlitePool { &self.pool }

    /// Inserts atomically unless `(source, external_ref)` already exists.
    /// Rejects postings that don't balance per currency before writing.
    ///
    /// The conflict is resolved by the insert itself, not a prior lookup, so
    /// two overlapping imports of one statement still yield `Duplicate` rather
    /// than a unique-index error.
    pub async fn insert_deduped(&self, txn: &Transaction) -> Result<InsertOutcome> {
        let mut db = self.pool.begin().await?;
        let outcome = insert_deduped(&mut db, &self.labels, txn).await?;
        db.commit().await?;
        Ok(outcome)
    }
}

/// [`ImportStore::insert_deduped`] inside the caller's transaction, so a whole
/// import commits or rolls back as one.
pub async fn insert_deduped(
    db: &mut SqliteConnection,
    labels: &Labels,
    txn: &Transaction,
) -> Result<InsertOutcome> {
    validate_balanced(&txn.postings)?;

    let source = txn.source.to_string();
    let inserted = sqlx::query_scalar!(
        r#"
        INSERT INTO transactions (date, payee, narration, source, external_ref, import_batch_id)
        VALUES (?, ?, ?, ?, ?, ?)
        ON CONFLICT (source, external_ref) WHERE external_ref IS NOT NULL DO NOTHING
        RETURNING id
        "#,
        txn.date,
        txn.payee,
        txn.narration,
        source,
        txn.external_ref,
        txn.import_batch_id,
    )
    .fetch_optional(&mut *db)
    .await?;

    match inserted {
        Some(txn_id) => {
            for p in &txn.postings {
                let account_id = ensure_account(db, labels, &p.account).await?;
                let amount = p.amount.to_string();
                let currency = p.currency.to_string();
                sqlx::query!(
                    r#"
                    INSERT INTO postings (transaction_id, account_id, amount, currency, tags)
                    VALUES (?, ?, ?, ?, ?)
                    "#,
                    txn_id,
                    account_id,
                    amount,
                    currency,
                    p.tags,
                )
                .execute(&mut *db)
                .await?;
            }
            Ok(InsertOutcome::Inserted(txn_id))
        }
        None => {
            let existing = sqlx::query_scalar!(
                r#"SELECT id AS "id!" FROM transactions WHERE source = ? AND external_ref = ?"#,
                source,
                txn.external_ref,
            )
            .fetch_one(&mut *db)
            .await?;
            Ok(InsertOutcome::Duplicate(existing))
        }
    }
}

/// The account's id, creating it and any missing ancestor with the label
/// load-journal would give it, so the tree never shows an ASCII name.
pub async fn ensure_account(db: &mut SqliteConnection, labels: &Labels, path: &str) -> Result<i64> {
    let mut id = None;
    let ancestors = path.match_indices(':').map(|(i, _)| &path[..i]);
    for path in ancestors.chain([path]) {
        let found: Option<i64> = sqlx::query_scalar("SELECT id FROM accounts WHERE path = ?")
            .bind(path)
            .fetch_optional(&mut *db)
            .await?;
        id = match found {
            Some(id) => Some(id),
            None => {
                let created =
                    sqlx::query("INSERT INTO accounts (path, label, type) VALUES (?, ?, ?)")
                        .bind(path)
                        .bind(labels.label(path))
                        .bind(schema_type(path)?.to_string())
                        .execute(&mut *db)
                        .await
                        .with_context(|| format!("creating account {path}"))?
                        .last_insert_rowid();
                sqlx::query("INSERT INTO account_events (account_id, event) VALUES (?, 'created')")
                    .bind(created)
                    .execute(&mut *db)
                    .await?;
                Some(created)
            }
        };
    }
    id.context("empty account path")
}

/// Summed with `rust_decimal`, not SQL `SUM`, which would CAST to REAL.
fn validate_balanced(postings: &[Posting]) -> Result<()> {
    if postings.is_empty() {
        bail!("transaction has no postings");
    }
    let mut per_currency: BTreeMap<Currency, Decimal> = BTreeMap::new();
    for p in postings {
        *per_currency.entry(p.currency).or_default() += p.amount;
    }
    let unbalanced: Vec<String> = per_currency
        .iter()
        .filter(|(_, sum)| !sum.is_zero())
        .map(|(ccy, sum)| format!("{ccy}={sum}"))
        .collect();
    if unbalanced.is_empty() {
        Ok(())
    } else {
        bail!("postings do not sum to zero per currency: {}", unbalanced.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;

    use super::*;

    #[test]
    fn balanced_single_currency_passes() {
        let ps = vec![
            Posting::from(("Assets:A1", dec!(-100), Currency::TWD)),
            Posting::from(("Assets:A2", dec!(100), Currency::TWD)),
        ];
        assert!(validate_balanced(&ps).is_ok());
    }

    #[test]
    fn balanced_per_currency_passes() {
        let ps = vec![
            Posting::from(("Assets:A1", dec!(-100), Currency::TWD)),
            Posting::from(("Assets:A2", dec!(100), Currency::TWD)),
            Posting::from(("Assets:A3", dec!(-5), Currency::USD)),
            Posting::from(("Assets:A4", dec!(5), Currency::USD)),
        ];
        assert!(validate_balanced(&ps).is_ok());
    }

    #[test]
    fn unbalanced_is_rejected() {
        let ps = vec![
            Posting::from(("Assets:A1", dec!(-100), Currency::TWD)),
            Posting::from(("Assets:A2", dec!(99), Currency::TWD)),
        ];
        assert!(validate_balanced(&ps).is_err());
    }

    #[test]
    fn empty_is_rejected() {
        assert!(validate_balanced(&[]).is_err());
    }
}
