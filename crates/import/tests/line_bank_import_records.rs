//! T4d acceptance on the user's own records, which are gitignored and live in
//! the main checkout (or `$LEDGER_RECORDS_DIR`). Skipped where they are absent,
//! as on CI. Reads them only: the journal and databases are scratch files.
//! Needs `pdftotext` (the nix devShell has it) and the LINE Bank account in
//! `[institution.accounts]`.

use std::path::Path;

use anyhow::{Context, Result};
use import::bank::{import, Report};
use ledger::{accounts::Chart, statements::bank::Bank};
use rust_decimal::Decimal;
use sqlx::SqlitePool;
use tempfile::TempDir;

mod common;

fn chart(records: &Path) -> Result<Chart> {
    let chart = Chart::load(records.join("ledger/mapping.toml"))?;
    for path in common::records::files(records, "line-bank", "pdf")? {
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
        Bank::CathayCard | Bank::TaishinCard | Bank::Taishin => {
            anyhow::bail!("{bank} is not imported here")
        }
    };
    let merged = bank.load_merged(&common::records::files(records, party, ext)?)?;
    let mut tx = pool.begin().await?;
    let report = import(&mut tx, &chart(records)?, bank, &merged, &[]).await?;
    tx.commit().await?;
    Ok(report)
}

/// (a) A ledger loaded from a fresh freeze of both banks already holds every
/// line of each.
#[tokio::test]
async fn importing_every_statement_into_the_frozen_ledger_adds_nothing() -> Result<()> {
    let Some(records) = common::records::dir("line-bank") else {
        eprintln!("skipped: no records");
        return Ok(());
    };
    chart(&records)?;
    let (pool, _scratch) = common::records::frozen(
        &records,
        common::records::files(&records, "cathay-bank", "csv")?,
        common::records::files(&records, "line-bank", "pdf")?,
    )
    .await?;

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
    let Some(records) = common::records::dir("line-bank") else {
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
    let merged =
        Bank::LineBank.load_merged(&common::records::files(&records, "line-bank", "pdf")?)?;
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
