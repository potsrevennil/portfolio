//! Splitwise as the reconciliation anchor: its signed balance per friend or
//! group against the matching person or netted account. The balances are input;
//! fetching them is the Splitwise importer's job.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use chrono::NaiveDate;
use ledger_types::currency::Currency;
use rust_decimal::Decimal;
use sqlx::{SqliteConnection, SqlitePool};

use super::{people::balances, store::account_balance};

/// What a Splitwise balance is held against.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Anchored {
    /// A friend: the sum of the person's accounts. Splitwise's friend balance
    /// includes their groups, so pass it less any netted group's part.
    Person(i64),
    /// A group: its netted account.
    Netted(String),
}

/// One of Splitwise's balances. Its sign is the ledger's: positive, they owe
/// you.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Anchor {
    pub on: Anchored,
    pub currency: Currency,
    pub balance: Decimal,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnchorRow {
    pub on: Anchored,
    pub currency: Currency,
    pub ledger: Decimal,
    pub splitwise: Decimal,
}

impl AnchorRow {
    /// Ledger less Splitwise.
    pub fn diff(&self) -> Decimal { self.ledger - self.splitwise }
}

async fn ledger_side(
    db: &mut SqliteConnection,
    on: &Anchored,
    as_of: NaiveDate,
) -> Result<BTreeMap<Currency, Decimal>> {
    match on {
        Anchored::Person(person) => {
            let known: Option<i64> = sqlx::query_scalar("SELECT id FROM person WHERE id = ?")
                .bind(person)
                .fetch_optional(&mut *db)
                .await?;
            known.with_context(|| format!("查無此人 {person}"))?;
            Ok(balances(db, Some(as_of)).await?.remove(person).unwrap_or_default())
        }
        Anchored::Netted(path) => {
            let known: Option<i64> = sqlx::query_scalar("SELECT id FROM accounts WHERE path = ?")
                .bind(path)
                .fetch_optional(&mut *db)
                .await?;
            known.with_context(|| format!("查無帳戶：{path}"))?;
            account_balance(db, path, Some(as_of)).await
        }
    }
}

/// A row for every currency either side holds on each subject named in
/// `anchors`: Splitwise leaves zero balances out, so a currency it omits reads
/// as zero. Subjects not named are not compared, since not every split account
/// is on Splitwise.
pub async fn anchor_check(
    pool: &SqlitePool,
    anchors: &[Anchor],
    as_of: NaiveDate,
) -> Result<Vec<AnchorRow>> {
    let mut subjects: BTreeMap<&Anchored, BTreeMap<Currency, Decimal>> = BTreeMap::new();
    for a in anchors {
        *subjects.entry(&a.on).or_default().entry(a.currency).or_default() += a.balance;
    }
    let conn = &mut *pool.acquire().await?;
    let mut rows = Vec::new();
    for (on, theirs) in subjects {
        let ours = ledger_side(conn, on, as_of).await?;
        let mut currencies: Vec<Currency> = ours.keys().chain(theirs.keys()).copied().collect();
        currencies.sort();
        currencies.dedup();
        for currency in currencies {
            let ledger = ours.get(&currency).copied().unwrap_or_default();
            let splitwise = theirs.get(&currency).copied().unwrap_or_default();
            if !ledger.is_zero() || !splitwise.is_zero() {
                rows.push(AnchorRow { on: on.clone(), currency, ledger, splitwise });
            }
        }
    }
    Ok(rows)
}
