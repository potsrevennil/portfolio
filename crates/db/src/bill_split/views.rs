//! What T09's page reads: the splits, how each closed, where a line could
//! attach, and what the repayment matcher works from.

use std::{cmp::Reverse, collections::BTreeSet};

use anyhow::Result;
use chrono::{Datelike, NaiveDate};
use ledger::accounts::in_subtree;
use ledger_types::currency::Currency;
use rust_decimal::Decimal;
use sqlx::{SqliteConnection, SqlitePool};

use super::{
    store::{line, load, Line, Owners},
    Books, Closed, Role, Shape, Split, Status,
};
use crate::review;

async fn all(db: &mut SqliteConnection) -> Result<Vec<Split>> {
    let ids: Vec<i64> =
        sqlx::query_scalar("SELECT id FROM split WHERE deleted_at IS NULL ORDER BY id")
            .fetch_all(&mut *db)
            .await?;
    let mut splits = Vec::with_capacity(ids.len());
    for id in ids {
        splits.push(load(db, id).await?);
    }
    Ok(splits)
}

/// Every split, oldest first; the caller filters by [`Split::status`].
pub async fn list(pool: &SqlitePool) -> Result<Vec<Split>> {
    all(&mut *pool.acquire().await?).await
}

pub async fn get(pool: &SqlitePool, id: i64) -> Result<Split> {
    load(&mut *pool.acquire().await?, id).await
}

/// Which closed splits to list. The default lists all of them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClosedFilter {
    pub person: Option<i64>,
    /// A group's account: splits with a share on it or below it.
    pub group: Option<String>,
    /// The year it closed.
    pub year: Option<i32>,
}

impl ClosedFilter {
    fn keeps(&self, closed: &Closed) -> bool {
        let split = &closed.split;
        self.person.is_none_or(|person| split.people().contains(&person))
            && self
                .group
                .as_deref()
                .is_none_or(|group| split.shares.iter().any(|s| in_subtree(&s.account, group)))
            && self.year.is_none_or(|year| closed.on.year() == year)
    }
}

/// Settled and written-off splits, newest first, with how each closed.
pub async fn closed(pool: &SqlitePool, filter: &ClosedFilter) -> Result<Vec<Closed>> {
    let mut out: Vec<Closed> = list(pool)
        .await?
        .into_iter()
        .filter_map(|split| split.closing().map(|(how, on)| Closed { split, how, on }))
        .filter(|closed| filter.keeps(closed))
        .collect();
    out.sort_by_key(|c| Reverse((c.on, c.split.id)));
    Ok(out)
}

/// An open or partial split a line could attach to, and why it ranks where it
/// does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub split: Split,
    /// The line already touches someone in the split, or its netted account.
    pub people: bool,
    /// The line's amount is exactly what someone there still owes.
    pub fit: bool,
}

fn candidate(split: Split, found: &Line, owners: &Owners) -> Candidate {
    let amounts = || found.legs.iter().map(|l| (l.currency, l.amount.abs()));
    let (people, fit) = match &split.shape {
        Shape::PerPerson => {
            let people = split.people();
            let touches = found
                .legs
                .iter()
                .any(|l| owners.owners.get(&l.account).is_some_and(|p| people.contains(p)));
            let fit = split.owed().iter().any(|((_, currency), owed)| {
                !owed.is_zero() && amounts().any(|a| a == (*currency, owed.abs()))
            });
            (touches, fit)
        }
        Shape::Netted { account } => {
            let fit = split
                .balance
                .iter()
                .any(|(currency, b)| !b.is_zero() && amounts().any(|a| a == (*currency, b.abs())));
            (found.legs.iter().any(|l| l.account == *account), fit)
        }
    };
    Candidate { split, people, fit }
}

/// The splits a line can attach to, best first: by people, then fit, then
/// recency.
pub async fn attachable(pool: &SqlitePool, transaction: i64) -> Result<Vec<Candidate>> {
    let conn = &mut *pool.acquire().await?;
    review::live(conn, transaction).await?;
    let found = line(conn, transaction).await?;
    let owners = Owners::load(conn).await?;
    let mut candidates: Vec<Candidate> = all(conn)
        .await?
        .into_iter()
        .filter(|s| s.status() != Status::Settled)
        .filter(|s| !s.shares.iter().any(|share| share.transaction_id == transaction))
        .map(|split| candidate(split, &found, &owners))
        .collect();
    candidates.sort_by_key(|c| Reverse((c.people, c.fit, c.split.last_date(), c.split.id)));
    Ok(candidates)
}

/// What a person still owes in one split and currency.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenShare {
    pub split_id: i64,
    pub currency: Currency,
    pub owed: Decimal,
    /// Their oldest share here.
    pub since: NaiveDate,
}

/// Everything a person still owes, per split and currency, over all their
/// accounts.
pub async fn open_for(pool: &SqlitePool, person: i64) -> Result<Vec<OpenShare>> {
    let mut out = Vec::new();
    for split in list(pool).await? {
        for ((who, currency), owed) in split.owed() {
            let since = split
                .shares
                .iter()
                .filter(|s| s.role == Role::Share && s.person == Some(who))
                .filter(|s| s.currency == currency)
                .map(|s| s.date)
                .min();
            if let (true, false, Some(since)) = (who == person, owed.is_zero(), since) {
                out.push(OpenShare { split_id: split.id, currency, owed, since });
            }
        }
    }
    Ok(out)
}

/// A leg on a person's account that looks like a repayment, with what no split
/// has taken yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unallocated {
    pub posting_id: i64,
    pub transaction_id: i64,
    pub date: NaiveDate,
    pub narration: Option<String>,
    pub account: String,
    pub currency: Currency,
    pub left: Decimal,
}

/// A person's repayments no split has fully taken, over all their accounts,
/// oldest first.
pub async fn unallocated(
    pool: &SqlitePool,
    books: &Books,
    person: i64,
) -> Result<Vec<Unallocated>> {
    let conn = &mut *pool.acquire().await?;
    let accounts: BTreeSet<String> = sqlx::query_scalar::<_, String>(
        "SELECT a.path FROM person_account o JOIN accounts a ON a.id = o.account_id
         WHERE o.person_id = ?",
    )
    .bind(person)
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .collect();
    let transactions: Vec<i64> = sqlx::query_scalar(
        "SELECT DISTINCT t.id FROM transactions t
         JOIN postings p ON p.transaction_id = t.id
         JOIN person_account o ON o.account_id = p.account_id
         WHERE o.person_id = ? ORDER BY t.date, t.id",
    )
    .bind(person)
    .fetch_all(&mut *conn)
    .await?;
    let mut out = Vec::new();
    for id in transactions {
        let found = line(conn, id).await?;
        if found.role(books) != Role::Repayment {
            continue;
        }
        for leg in &found.legs {
            if accounts.contains(&leg.account) && !leg.left().is_zero() {
                out.push(Unallocated {
                    posting_id: leg.posting_id,
                    transaction_id: id,
                    date: found.date,
                    narration: found.narration.clone(),
                    account: leg.account.clone(),
                    currency: leg.currency,
                    left: leg.left(),
                });
            }
        }
    }
    Ok(out)
}
