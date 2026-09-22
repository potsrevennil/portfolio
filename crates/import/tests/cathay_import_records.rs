//! T4a acceptance on the user's own records, which are gitignored and live in
//! the main checkout (or `$LEDGER_RECORDS_DIR`). Skipped where they are absent,
//! as on CI. Reads them only: the journal and databases are scratch files.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use db::load;
use import::cathay_bank::{import, Report};
use ledger::{accounts::Chart, args::Args as BuildArgs, freeze, statements::cathay};
use rust_decimal::Decimal;
use sqlx::SqlitePool;
use tempfile::TempDir;

fn records_dir() -> Option<PathBuf> {
    let dir = match std::env::var_os("LEDGER_RECORDS_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => {
            // A worktree's records are its main checkout's.
            let out = std::process::Command::new("git")
                .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
                .current_dir(env!("CARGO_MANIFEST_DIR"))
                .output()
                .ok()?;
            PathBuf::from(String::from_utf8(out.stdout).ok()?.trim()).parent()?.to_path_buf()
        }
    };
    dir.join("raw/cathay-bank").is_dir().then_some(dir)
}

fn statements(records: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for account in std::fs::read_dir(records.join("raw/cathay-bank"))? {
        let account = account?.path();
        if !account.is_dir() {
            continue;
        }
        for file in std::fs::read_dir(account)? {
            let path = file?.path();
            if path.extension().is_some_and(|e| e == "csv") {
                paths.push(path);
            }
        }
    }
    paths.sort();
    Ok(paths)
}

async fn import_all(pool: &SqlitePool, records: &Path) -> Result<Report> {
    let chart = Chart::load(records.join("ledger/mapping.toml"))?;
    let mut tx = pool.begin().await?;
    let report = import(&mut tx, &chart, &statements(records)?, &[]).await?;
    tx.commit().await?;
    Ok(report)
}

/// (a) A ledger loaded from a fresh freeze already holds every line.
#[tokio::test]
async fn importing_every_download_into_the_frozen_ledger_adds_nothing() -> Result<()> {
    let Some(records) = records_dir() else {
        eprintln!("skipped: no records");
        return Ok(());
    };
    let scratch = TempDir::new()?;
    let journal = scratch.path().join("journal.csv");
    let frozen = freeze::run(&freeze::FreezeArgs {
        journal: journal.clone(),
        build: BuildArgs {
            cathay_statements: statements(&records)?,
            daily_income_expense: None,
            daily_transfers: None,
            transactions: Some(records.join("corrected/transactions.csv")),
            backfill: false,
            ledger_dir: records.join("ledger"),
        },
    })?;
    anyhow::ensure!(frozen.ok(), "freeze failed:\n{frozen}");
    let url = format!("sqlite:{}", scratch.path().join("frozen.db").display());
    load::run(&load::Args {
        journal,
        database_url: url.clone(),
        mapping: records.join("ledger/mapping.toml"),
    })
    .await?;
    let pool = db::init_db(&url).await?;

    let report = import_all(&pool, &records).await?;
    eprintln!("{report}");
    assert_eq!(report.inserted, 0, "{report}");
    assert!(report.check.ok(), "{report}");
    Ok(())
}

/// (b) Into an empty ledger, every statement closes on its own balance.
#[tokio::test]
async fn importing_every_download_into_an_empty_ledger_closes_every_statement() -> Result<()> {
    let Some(records) = records_dir() else {
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
    let merged = cathay::load_merged(&statements(&records)?)?;
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
