//! T4a acceptance on the user's own records, which are gitignored and live in
//! the main checkout (or `$LEDGER_RECORDS_DIR`). Skipped where they are absent,
//! as on CI. Reads them only: the journal and databases are scratch files.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use import::bank::{import, Report};
use ledger::{accounts::Chart, statements::bank::Bank};
use rust_decimal::Decimal;
use sqlx::SqlitePool;
use tempfile::TempDir;

mod common;

fn statements(records: &Path) -> Result<Vec<PathBuf>> {
    common::records::files(records, "cathay-bank", "csv")
}

async fn import_all(pool: &SqlitePool, records: &Path) -> Result<Report> {
    let chart = Chart::load(records.join("ledger/mapping.toml"))?;
    let mut tx = pool.begin().await?;
    let merged = Bank::Cathay.load_merged(&statements(records)?)?;
    let report = import(&mut tx, &chart, Bank::Cathay, &merged, &[]).await?;
    tx.commit().await?;
    Ok(report)
}

/// (a) A ledger loaded from a fresh freeze already holds every line.
#[tokio::test]
async fn importing_every_download_into_the_frozen_ledger_adds_nothing() -> Result<()> {
    let Some(records) = common::records::dir("cathay-bank") else {
        eprintln!("skipped: no records");
        return Ok(());
    };
    let (pool, _scratch) =
        common::records::frozen(&records, statements(&records)?, Vec::new()).await?;

    let report = import_all(&pool, &records).await?;
    eprintln!("{report}");
    assert_eq!(report.inserted, 0, "{report}");
    assert!(report.check.ok(), "{report}");
    Ok(())
}

/// (b) Into an empty ledger, every statement closes on its own balance.
#[tokio::test]
async fn importing_every_download_into_an_empty_ledger_closes_every_statement() -> Result<()> {
    let Some(records) = common::records::dir("cathay-bank") else {
        eprintln!("skipped: no records");
        return Ok(());
    };
    let scratch = TempDir::new()?;
    let url = format!("sqlite:{}", scratch.path().join("empty.db").display());
    let pool = db::init_db(&url).await?;

    let report = import_all(&pool, &records).await?;
    eprintln!("{report}");
    assert!(report.check.ok(), "{report}");

    let chart = Chart::load(records.join("ledger/mapping.toml"))?;
    let merged = Bank::Cathay.load_merged(&statements(&records)?)?;
    for m in &merged {
        let s = &m.statement;
        let account = &chart.institution.accounts[&s.account_no];
        let amounts: Vec<String> = sqlx::query_scalar(
            "SELECT p.amount FROM postings p JOIN accounts a ON a.id = p.account_id
             WHERE a.path = ? AND p.currency = ?",
        )
        .bind(account.as_ref())
        .bind(s.currency.to_string())
        .fetch_all(&pool)
        .await?;
        let held = amounts
            .iter()
            .map(|a| a.parse::<Decimal>().context("posting amount"))
            .sum::<Result<Decimal>>()?;
        assert_eq!(held, s.closing_balance(), "{} {}", &**account, s.currency);
    }
    eprintln!("{} statements close on their balance", merged.len());
    Ok(())
}
