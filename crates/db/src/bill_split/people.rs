//! The explicit people list and which split accounts are whose.

use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};
use chrono::NaiveDate;
use ledger_types::currency::Currency;
use rust_decimal::Decimal;
use sqlx::{SqliteConnection, SqlitePool};

use super::{
    mix::{Asked, Uses},
    model::named,
    read::Owners,
    Books, Person,
};
use crate::{import::ensure_account, query::field};

pub async fn add_person(pool: &SqlitePool, name: &str) -> Result<i64> {
    let name = named(name, "名要寫")?;
    let added = sqlx::query("INSERT INTO person (name) VALUES (?) ON CONFLICT (name) DO NOTHING")
        .bind(name)
        .execute(pool)
        .await?;
    match added.rows_affected() {
        0 => bail!("已經有「{name}」"),
        _ => Ok(added.last_insert_rowid()),
    }
}

pub async fn rename_person(pool: &SqlitePool, id: i64, name: &str) -> Result<()> {
    let name = named(name, "名要寫")?;
    let taken: Option<i64> = sqlx::query_scalar("SELECT id FROM person WHERE name = ? AND id <> ?")
        .bind(name)
        .bind(id)
        .fetch_optional(pool)
        .await?;
    if taken.is_some() {
        bail!("已經有「{name}」");
    }
    let renamed = sqlx::query("UPDATE person SET name = ? WHERE id = ?")
        .bind(name)
        .bind(id)
        .execute(pool)
        .await?;
    match renamed.rows_affected() {
        0 => bail!("查無此人 {id}"),
        _ => Ok(()),
    }
}

/// Refuses a person who isn't on the list.
pub(super) async fn known(db: &mut SqliteConnection, person: i64) -> Result<()> {
    let found: Option<i64> = sqlx::query_scalar("SELECT id FROM person WHERE id = ?")
        .bind(person)
        .fetch_optional(db)
        .await?;
    found.map(drop).with_context(|| format!("查無此人 {person}"))
}

/// Makes `path` one of a person's accounts, creating it if it is new. Which
/// account a payee settles on is never guessed: this list and the caller say.
pub async fn assign(pool: &SqlitePool, books: &Books, person: i64, path: &str) -> Result<()> {
    let mut db = pool.begin().await?;
    if !books.holds(path) {
        bail!("「{path}」不是分帳底下的帳戶");
    }
    known(&mut db, person).await?;
    Uses::load(&mut db, books).await?.check(path, Asked::Person)?;
    let account = ensure_account(&mut db, &books.labels, path).await?;
    let linked = sqlx::query(
        "INSERT INTO person_account (account_id, person_id) VALUES (?, ?)
         ON CONFLICT (account_id) DO NOTHING",
    )
    .bind(account)
    .bind(person)
    .execute(&mut *db)
    .await?;
    if linked.rows_affected() == 0 {
        bail!("「{path}」已經是別人的帳戶");
    }
    db.commit().await?;
    Ok(())
}

/// Takes back an assignment, refused while a split still counts it as theirs.
pub async fn unassign(pool: &SqlitePool, person: i64, path: &str) -> Result<()> {
    let mut db = pool.begin().await?;
    let used: Option<i64> = sqlx::query_scalar(
        "SELECT s.id FROM share s JOIN postings p ON p.id = s.posting_id
         JOIN accounts a ON a.id = p.account_id WHERE s.person_id = ? AND a.path = ? LIMIT 1",
    )
    .bind(person)
    .bind(path)
    .fetch_optional(&mut *db)
    .await?;
    if used.is_some() {
        bail!("「{path}」還有分帳在用");
    }
    let gone = sqlx::query(
        "DELETE FROM person_account WHERE person_id = ?
         AND account_id = (SELECT id FROM accounts WHERE path = ?)",
    )
    .bind(person)
    .bind(path)
    .execute(&mut *db)
    .await?;
    if gone.rows_affected() == 0 {
        bail!("「{path}」不是這個人的帳戶");
    }
    db.commit().await?;
    Ok(())
}

/// Every person with their accounts and what they owe over all of them.
pub async fn people(pool: &SqlitePool) -> Result<Vec<Person>> {
    let persons: Vec<(i64, String)> =
        sqlx::query_as("SELECT id, name FROM person ORDER BY name").fetch_all(pool).await?;
    let conn = &mut *pool.acquire().await?;
    let owners = Owners::load(&mut *conn).await?;
    let mut balances = balances(conn, None).await?;
    Ok(persons
        .into_iter()
        .map(|(id, name)| Person {
            id,
            name,
            accounts: owners.of(id),
            balance: balances.remove(&id).unwrap_or_default(),
        })
        .collect())
}

/// Each person's balance over their accounts, through `as_of` where given.
pub(super) async fn balances(
    db: &mut SqliteConnection,
    as_of: Option<NaiveDate>,
) -> Result<BTreeMap<i64, BTreeMap<Currency, Decimal>>> {
    let rows: Vec<(i64, String, String)> = sqlx::query_as(
        "SELECT o.person_id, p.amount, p.currency FROM postings p
         JOIN person_account o ON o.account_id = p.account_id
         JOIN transactions t ON t.id = p.transaction_id
         WHERE ?1 IS NULL OR t.date <= ?1",
    )
    .bind(as_of.map(|d| d.to_string()))
    .fetch_all(db)
    .await
    .context("reading what people owe")?;
    let mut out: BTreeMap<i64, BTreeMap<Currency, Decimal>> = BTreeMap::new();
    for (person, amount, currency) in rows {
        let amount: Decimal = field("posting amount", &amount)?;
        *out.entry(person).or_default().entry(field("currency", &currency)?).or_default() += amount;
    }
    Ok(out)
}
