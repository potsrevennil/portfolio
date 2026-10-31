//! Statements that vouch for 天天記帳's records, on the user's own records,
//! which are gitignored and live in the main checkout (or
//! `$LEDGER_RECORDS_DIR`). Skipped where they are absent, as on CI. Reads them
//! only: the journal and databases are scratch files. Needs `pdftotext` (the
//! nix devShell has it). Once the mapping names a card, a disagreement between
//! its records and its statements fails here.

use std::path::{Path, PathBuf};

use anyhow::Result;
use import::bank::import;
use ledger::{accounts::Chart, statements::bank::Bank};

mod common;

const VOUCHING: [(Bank, &str, &[&str]); 3] = [
    (Bank::CathayCard, "cathay-card", &["csv"]),
    (Bank::TaishinCard, "taishin-card", &["pdf"]),
    (Bank::Taishin, "taishin-bank", &["pdf", "xlsx"]),
];

fn statements(records: &Path, party: &str, extensions: &[&str]) -> Result<Vec<PathBuf>> {
    let found = extensions.iter().map(|ext| common::records::files(records, party, ext));
    Ok(found.collect::<Result<Vec<_>>>()?.concat())
}

/// Every statement's lines reach its closing, whatever the ledger holds.
#[test]
fn every_statement_adds_up() -> Result<()> {
    for (bank, party, extensions) in VOUCHING {
        let Some(records) = common::records::dir(party) else {
            eprintln!("skipped {party}: no records");
            continue;
        };
        let merged = bank.load_merged(&statements(&records, party, extensions)?)?;
        assert!(!merged.is_empty());
    }
    Ok(())
}

/// The ledger frozen from 天天記帳 meets every closing, and a second import
/// adds nothing. A card whose folder the mapping does not name yet is not on
/// the ledger to check.
#[tokio::test]
async fn the_frozen_ledger_meets_every_closing() -> Result<()> {
    for (bank, party, extensions) in VOUCHING {
        let Some(records) = common::records::dir(party) else {
            eprintln!("skipped {party}: no records");
            continue;
        };
        let chart = Chart::load(records.join("ledger/mapping.toml"))?;
        let merged = bank.load_merged(&statements(&records, party, extensions)?)?;
        if merged.iter().any(|m| !chart.institution.accounts.contains_key(&m.statement.account_no))
        {
            eprintln!("skipped {party}: ledger/mapping.toml does not name every account yet");
            continue;
        }
        let (pool, _scratch) = common::records::frozen(
            &records,
            common::records::files(&records, "cathay-bank", "csv")?,
            common::records::files(&records, "line-bank", "pdf")?,
        )
        .await?;

        let mut tx = pool.begin().await?;
        let first = import(&mut tx, &chart, bank, &merged, &[]).await?;
        let again = import(&mut tx, &chart, bank, &merged, &[]).await?;
        tx.commit().await?;

        eprintln!("{first}");
        assert!(first.check.ok(), "{first}");
        assert_eq!(again.inserted, 0, "{again}");
    }
    Ok(())
}
