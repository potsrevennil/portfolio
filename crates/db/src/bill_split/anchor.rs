//! Splitwise as the reconciliation anchor: its signed balance per friend or
//! group against the matching person or netted account. The balances are input;
//! fetching them is the Splitwise importer's job.

use std::collections::BTreeMap;

use anyhow::Result;
use chrono::NaiveDate;
use ledger_types::currency::Currency;
use rust_decimal::Decimal;
use sqlx::SqlitePool;

use super::people::{balances, known};
use crate::{query::account_balance, review};

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
    let mut people = balances(conn, Some(as_of)).await?;
    let mut rows = Vec::new();
    for (on, theirs) in subjects {
        let ours = match on {
            Anchored::Person(person) => {
                known(conn, *person).await?;
                people.remove(person).unwrap_or_default()
            }
            Anchored::Netted(path) => {
                review::account(conn, path).await?;
                account_balance(conn, path, Some(as_of)).await?
            }
        };
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
