//! The split engine over the owner's own journal, loaded into a scratch
//! database; the records are only read. Grouping all of history into a split
//! and undoing it must leave every balance and the gate exactly as they were.
//! Prints counts only: assertion messages name no account or figure.

use std::collections::BTreeSet;

use anyhow::{Context, Result};
use chrono::NaiveDate;
use db::{
    bill_split::{self, Books, Shape, Status, WriteOff},
    check, load,
    query::account_balances,
    SqlitePool,
};
use ledger::accounts::Chart;
use ledger_types::currency::Currency;
use rust_decimal::Decimal;
use tempfile::TempDir;

#[path = "common/records.rs"]
mod records;

async fn balances(pool: &SqlitePool) -> Result<Vec<(String, Currency, Decimal)>> {
    let far = NaiveDate::from_ymd_opt(2100, 1, 1).context("a date")?;
    Ok(account_balances(pool, far)
        .await?
        .into_iter()
        .map(|b| (b.path, b.currency, b.amount))
        .collect())
}

async fn gate_is_green(pool: &SqlitePool) -> Result<bool> {
    let report = check::check(&mut *pool.acquire().await?).await?;
    Ok(report.mismatches.is_empty())
}

#[tokio::test]
async fn grouping_and_ungrouping_history_changes_no_balance() -> Result<()> {
    let Some(records) = records::records_dir() else {
        eprintln!("skipped: no records here");
        return Ok(());
    };
    let dir = TempDir::new()?;
    let mapping = records.join("ledger/mapping.toml");
    let url = format!("sqlite:{}", dir.path().join("ledger.db").display());
    let market_url = format!("sqlite:{}", dir.path().join("market.db").display());
    let report = load::run(&load::Args {
        journal: records.join("ledger/journal.csv"),
        database_url: url.clone(),
        mapping: mapping.clone(),
        market_url,
    })
    .await?;
    assert!(report.check.mismatches.is_empty(), "the gate is red before the engine runs");
    let pool = db::open_db(&url).await?;
    let books = Books::from(&Chart::load(&mapping)?);
    let before = balances(&pool).await?;

    // Everyone on their own split account, except where the ledger mixes shapes.
    let paths: BTreeSet<&str> = before
        .iter()
        .map(|(path, ..)| path.as_str())
        .filter(|path| books.split_accounts.covers(path))
        .collect();
    let mut owned = Vec::new();
    for (i, path) in paths.iter().enumerate() {
        let person = bill_split::add_person(&pool, &format!("person {i}")).await?;
        if bill_split::assign(&pool, &books, person, path).await.is_ok() {
            owned.push(*path);
        }
    }
    eprintln!("{} of {} split accounts have one owner", owned.len(), paths.len());

    let on_owned: Vec<i64> = sqlx::query_scalar(
        "SELECT DISTINCT p.transaction_id FROM postings p
         JOIN person_account o ON o.account_id = p.account_id",
    )
    .fetch_all(&pool)
    .await?;
    let split = bill_split::create(&pool, &books, "history", &Shape::PerPerson).await?;
    bill_split::gather(&pool, &books, split, &on_owned).await?;
    assert_eq!(balances(&pool).await?, before, "gathering moved a balance");
    assert!(gate_is_green(&pool).await?, "gathering turned the gate red");

    let grouped = bill_split::get(&pool, split).await?;
    eprintln!("{} lines, {} shares, {:?}", on_owned.len(), grouped.shares.len(), grouped.status());
    assert_ne!(grouped.status(), Status::BalanceOnly);

    // A write-off and its undo, on whoever owes the most.
    let (&(person, currency), owed) = grouped
        .owed()
        .iter()
        .max_by_key(|(_, owed)| **owed)
        .map(|(key, owed)| (key, *owed))
        .context("nobody in the journal's splits")?;
    if owed > Decimal::ZERO {
        let account = grouped
            .shares
            .iter()
            .find(|s| s.person == Some(person) && s.currency == currency)
            .map(|s| s.account.clone())
            .context("their share")?;
        let date = NaiveDate::from_ymd_opt(2099, 1, 1).context("a date")?;
        let w = WriteOff { account, currency, date, amount: None, note: None };
        let id = bill_split::write_off(&pool, &books, split, &w).await?;
        assert!(gate_is_green(&pool).await?, "a write-off turned the gate red");
        bill_split::detach(&pool, &books, split, id).await?;
    }

    bill_split::delete(&pool, &books, split).await?;
    assert_eq!(balances(&pool).await?, before, "undoing it moved a balance");
    assert!(gate_is_green(&pool).await?, "undoing it turned the gate red");
    Ok(())
}
