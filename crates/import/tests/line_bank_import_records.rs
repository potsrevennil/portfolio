//! T4d acceptance on the user's own records, which are gitignored and live in
//! the main checkout (or `$LEDGER_RECORDS_DIR`). Skipped where they are absent,
//! as on CI. Reads them only: the journal and databases are scratch files.
//! Needs `pdftotext` (the nix devShell has it) and the LINE Bank account in
//! `[institution.accounts]`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use db::load;
use import::bank::{import, Report};
use ledger::{accounts::Chart, args::Args as BuildArgs, freeze, statements::bank::Bank};
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
    dir.join("raw/line-bank").is_dir().then_some(dir)
}

/// Every file under `raw/<party>/<folder>/` with extension `ext`.
fn files(records: &Path, party: &str, ext: &str) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for folder in std::fs::read_dir(records.join("raw").join(party))? {
        let folder = folder?.path();
        if !folder.is_dir() {
            continue;
        }
        for file in std::fs::read_dir(folder)? {
            let path = file?.path();
            if path.extension().is_some_and(|e| e == ext) {
                paths.push(path);
            }
        }
    }
    paths.sort();
    Ok(paths)
}

fn chart(records: &Path) -> Result<Chart> {
    let chart = Chart::load(records.join("ledger/mapping.toml"))?;
    for path in files(records, "line-bank", "pdf")? {
        let folder = path.parent().and_then(Path::file_name).and_then(|f| f.to_str());
        let account = folder.and_then(|f| f.rsplit_once('-')).map(|(_, a)| a).unwrap_or_default();
        anyhow::ensure!(
            chart.institution.accounts.contains_key(account),
            "ledger/mapping.toml: add the LINE Bank account {account} under \
             [institution.accounts], mapped to its ledger account"
        );
    }
    Ok(chart)
}

async fn import_all(pool: &SqlitePool, records: &Path, bank: Bank) -> Result<Report> {
    let (party, ext) = match bank {
        Bank::Cathay => ("cathay-bank", "csv"),
        Bank::LineBank => ("line-bank", "pdf"),
    };
    let merged = bank.load_merged(&files(records, party, ext)?)?;
    let mut tx = pool.begin().await?;
    let report = import(&mut tx, &chart(records)?, bank, &merged, &[]).await?;
    tx.commit().await?;
    Ok(report)
}

/// (a) A ledger loaded from a fresh freeze of both banks already holds every
/// line of each.
#[tokio::test]
async fn importing_every_statement_into_the_frozen_ledger_adds_nothing() -> Result<()> {
    let Some(records) = records_dir() else {
        eprintln!("skipped: no records");
        return Ok(());
    };
    chart(&records)?;
    let scratch = TempDir::new()?;
    let journal = scratch.path().join("journal.csv");
    let frozen = freeze::run(&freeze::FreezeArgs {
        journal: journal.clone(),
        build: BuildArgs {
            cathay_statements: files(&records, "cathay-bank", "csv")?,
            line_bank_statements: files(&records, "line-bank", "pdf")?,
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

    for bank in [Bank::LineBank, Bank::Cathay] {
        let report = import_all(&pool, &records, bank).await?;
        eprintln!("{report}");
        assert_eq!(report.inserted, 0, "{report}");
        assert!(report.check.ok(), "{report}");
    }
    Ok(())
}

/// (b) Into an empty ledger, every LINE Bank statement closes on its balance.
#[tokio::test]
async fn importing_every_statement_into_an_empty_ledger_closes_each_one() -> Result<()> {
    let Some(records) = records_dir() else {
        eprintln!("skipped: no records");
        return Ok(());
    };
    let scratch = TempDir::new()?;
    let url = format!("sqlite:{}", scratch.path().join("empty.db").display());
    let pool = db::init_db(&url).await?;

    let report = import_all(&pool, &records, Bank::LineBank).await?;
    eprintln!("{report}");
    assert!(report.check.ok(), "{report}");

    let chart = chart(&records)?;
    let merged = Bank::LineBank.load_merged(&files(&records, "line-bank", "pdf")?)?;
    let mut closings = 0;
    for m in &merged {
        let s = &m.statement;
        let account = &chart.institution.accounts[&s.account_no];
        for period in &s.periods {
            let amounts: Vec<String> = sqlx::query_scalar(
                "SELECT p.amount FROM postings p JOIN accounts a ON a.id = p.account_id
                 JOIN transactions t ON t.id = p.transaction_id
                 WHERE a.path = ? AND p.currency = ? AND t.date <= ?",
            )
            .bind(account.as_ref())
            .bind(s.currency.to_string())
            .bind(period.end.to_string())
            .fetch_all(&pool)
            .await?;
            let held = amounts
                .iter()
                .map(|a| a.parse::<Decimal>().context("posting amount"))
                .sum::<Result<Decimal>>()?;
            assert_eq!(held, period.closing, "{} {} {}", &**account, s.currency, period.end);
            closings += 1;
        }
    }
    eprintln!("{closings} statement closings hold");
    Ok(())
}
