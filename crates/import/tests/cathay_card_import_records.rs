//! Cathay card acceptance on the user's own records, which are gitignored and
//! live in the main checkout (or `$LEDGER_RECORDS_DIR`). Skipped where they are
//! absent, as on CI. Reads them only: the journal and databases are scratch
//! files.

use anyhow::Result;
use import::bank::import;
use ledger::{accounts::Chart, statements::bank::Bank};

mod common;

/// Every statement's lines reach its closing, whatever the ledger holds.
#[test]
fn every_statement_adds_up() -> Result<()> {
    let Some(records) = common::records::dir("cathay-card") else {
        eprintln!("skipped: no records");
        return Ok(());
    };
    let merged =
        Bank::CathayCard.load_merged(&common::records::files(&records, "cathay-card", "csv")?)?;
    assert!(!merged.is_empty());
    Ok(())
}

/// The ledger frozen from 天天記帳 meets every closing, twice over. A card
/// whose folder the mapping does not name yet is not on the ledger to check.
#[tokio::test]
async fn the_frozen_ledger_meets_every_closing() -> Result<()> {
    let Some(records) = common::records::dir("cathay-card") else {
        eprintln!("skipped: no records");
        return Ok(());
    };
    let chart = Chart::load(records.join("ledger/mapping.toml"))?;
    let paths = common::records::files(&records, "cathay-card", "csv")?;
    let merged = Bank::CathayCard.load_merged(&paths)?;
    if merged.iter().any(|m| !chart.institution.accounts.contains_key(&m.statement.account_no)) {
        eprintln!("skipped: ledger/mapping.toml does not name every card yet");
        return Ok(());
    }
    let (pool, _scratch) = common::records::frozen(
        &records,
        common::records::files(&records, "cathay-bank", "csv")?,
        common::records::files(&records, "line-bank", "pdf")?,
    )
    .await?;

    let mut tx = pool.begin().await?;
    let first = import(&mut tx, &chart, Bank::CathayCard, &merged, &[]).await?;
    let again = import(&mut tx, &chart, Bank::CathayCard, &merged, &[]).await?;
    tx.commit().await?;

    eprintln!("{first}");
    assert!(first.check.ok(), "{first}");
    assert_eq!(again.inserted, 0, "{again}");
    Ok(())
}
