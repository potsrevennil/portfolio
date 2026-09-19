//! Import-time dedup on `(source, external_ref)`.
//!
//! `source` is the coarse class (`import`/`manual`/`tiantian`), so all
//! importers share one scope: `external_ref` must be unique across importers.
//! For Cathay lines, build it with `BankStatement::dedup_refs` — the same
//! helper the freeze bake uses — or frozen and re-imported lines won't collide.

use std::collections::BTreeMap;

use anyhow::{bail, Result};
use chrono::NaiveDate;
use rust_decimal::Decimal;
use sqlx::SqlitePool;

#[derive(Clone, Debug)]
pub struct NewTransaction {
    pub date: NaiveDate,
    pub payee: Option<String>,
    pub narration: Option<String>,
    pub source: String,
    /// `None` is exempt from dedup (hand-entered rows).
    pub external_ref: Option<String>,
}

#[derive(Clone, Debug)]
pub struct NewPosting {
    pub account_id: i64,
    pub amount: Decimal,
    pub currency: String,
    pub tags: Option<String>,
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

#[derive(Clone)]
pub struct ImportStore {
    pool: SqlitePool,
}

impl ImportStore {
    pub fn new(pool: SqlitePool) -> Self { ImportStore { pool } }

    pub fn pool(&self) -> &SqlitePool { &self.pool }

    /// Inserts atomically unless `(source, external_ref)` already exists.
    /// Rejects postings that don't balance per currency before writing.
    ///
    /// The conflict is resolved by the insert itself, not a prior lookup, so
    /// two overlapping imports of one statement still yield `Duplicate` rather
    /// than a unique-index error.
    pub async fn insert_deduped(
        &self,
        txn: &NewTransaction,
        postings: &[NewPosting],
    ) -> Result<InsertOutcome> {
        validate_balanced(postings)?;

        let mut db = self.pool.begin().await?;
        let inserted = sqlx::query_scalar!(
            r#"
            INSERT INTO transactions (date, payee, narration, source, external_ref)
            VALUES (?, ?, ?, ?, ?)
            ON CONFLICT (source, external_ref) WHERE external_ref IS NOT NULL DO NOTHING
            RETURNING id
            "#,
            txn.date,
            txn.payee,
            txn.narration,
            txn.source,
            txn.external_ref,
        )
        .fetch_optional(&mut *db)
        .await?;

        match inserted {
            Some(txn_id) => {
                for p in postings {
                    let amount = p.amount.to_string();
                    sqlx::query!(
                        r#"
                        INSERT INTO postings (transaction_id, account_id, amount, currency, tags)
                        VALUES (?, ?, ?, ?, ?)
                        "#,
                        txn_id,
                        p.account_id,
                        amount,
                        p.currency,
                        p.tags,
                    )
                    .execute(&mut *db)
                    .await?;
                }
                db.commit().await?;
                Ok(InsertOutcome::Inserted(txn_id))
            }
            None => {
                let existing = sqlx::query_scalar!(
                    r#"SELECT id AS "id!" FROM transactions WHERE source = ? AND external_ref = ?"#,
                    txn.source,
                    txn.external_ref,
                )
                .fetch_one(&mut *db)
                .await?;
                Ok(InsertOutcome::Duplicate(existing))
            }
        }
    }
}

/// Summed with `rust_decimal`, not SQL `SUM`, which would CAST to REAL.
fn validate_balanced(postings: &[NewPosting]) -> Result<()> {
    if postings.is_empty() {
        bail!("transaction has no postings");
    }
    let mut per_currency: BTreeMap<&str, Decimal> = BTreeMap::new();
    for p in postings {
        *per_currency.entry(p.currency.as_str()).or_default() += p.amount;
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

    fn posting(account_id: i64, amount: Decimal, ccy: &str) -> NewPosting {
        NewPosting { account_id, amount, currency: ccy.into(), tags: None }
    }

    #[test]
    fn balanced_single_currency_passes() {
        let ps = vec![posting(1, dec!(-100), "TWD"), posting(2, dec!(100), "TWD")];
        assert!(validate_balanced(&ps).is_ok());
    }

    #[test]
    fn balanced_per_currency_passes() {
        let ps = vec![
            posting(1, dec!(-100), "TWD"),
            posting(2, dec!(100), "TWD"),
            posting(3, dec!(-5), "USD"),
            posting(4, dec!(5), "USD"),
        ];
        assert!(validate_balanced(&ps).is_ok());
    }

    #[test]
    fn unbalanced_is_rejected() {
        let ps = vec![posting(1, dec!(-100), "TWD"), posting(2, dec!(99), "TWD")];
        assert!(validate_balanced(&ps).is_err());
    }

    #[test]
    fn empty_is_rejected() {
        assert!(validate_balanced(&[]).is_err());
    }
}
