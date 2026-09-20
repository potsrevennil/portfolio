//! The **journal**: a flat CSV that is the single source of truth of reconciled
//! history. The freeze tool ([`super::freeze`]) writes it once
//! reconciliation passes and the loader ([`super::load`]) reads it into SQLite;
//! it stays deliberately dumb — no Beancount, no chart, no reconciliation — so
//! that load path carries none of that complexity. It is financial data, so it
//! is gitignored.
//!
//! One row per posting. Transaction-level fields (`date`, `payee`, `narration`,
//! `external_ref`) repeat across a group's rows; `tags` is per-posting.

use std::path::Path;

use anyhow::{bail, Context, Result};
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use super::model::Source;
use crate::currency::Currency;

/// The reserved posting tag marking a securities-placeholder leg (the ETF
/// backfill). The freeze tool stamps it, the loader reads it back to note the
/// account, and it marks the leg to be replaced with real positions later.
pub const PLACEHOLDER_TAG: &str = "securities-placeholder";

/// One posting of a transaction.
#[derive(Debug, Clone, PartialEq)]
pub struct Posting {
    /// Groups the legs of one transaction together.
    pub group: u64,
    pub source: Source,
    pub date: NaiveDate,
    pub payee: Option<String>,
    pub narration: String,
    pub external_ref: Option<String>,
    pub account: String,
    pub amount: Decimal,
    pub currency: Currency,
    pub tags: Option<String>,
}

/// A parsed journal.
#[derive(Debug, Default)]
pub struct Journal {
    pub postings: Vec<Posting>,
}

/// One CSV row; field order is the file's column order. Serde handles the
/// empty-cell ⇄ `None` mapping and the date/decimal/currency formats.
#[derive(Serialize, Deserialize)]
struct Row {
    group: u64,
    source: Source,
    date: NaiveDate,
    account: String,
    amount: Decimal,
    currency: Currency,
    payee: Option<String>,
    narration: Option<String>,
    external_ref: Option<String>,
    tags: Option<String>,
}

/// Writes the journal CSV, postings in the order they were reconciled in.
pub fn write(path: impl AsRef<Path>, journal: &Journal) -> Result<()> {
    let path = path.as_ref();
    let mut writer = csv::WriterBuilder::new()
        .from_path(path)
        .with_context(|| format!("creating {}", path.display()))?;

    for p in &journal.postings {
        writer.serialize(Row {
            group: p.group,
            source: p.source,
            date: p.date,
            account: p.account.clone(),
            amount: p.amount,
            currency: p.currency,
            payee: p.payee.clone(),
            narration: Some(p.narration.clone()),
            external_ref: p.external_ref.clone(),
            tags: p.tags.clone(),
        })?;
    }
    writer.flush().with_context(|| format!("flushing {}", path.display()))?;
    Ok(())
}

/// Reads the journal CSV back — the inverse of [`write`], used by the loader
/// and by the freeze tool's own verification. Columns are matched by header
/// name.
pub fn read(path: impl AsRef<Path>) -> Result<Journal> {
    let path = path.as_ref();
    let mut reader = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .from_path(path)
        .with_context(|| format!("opening {}", path.display()))?;

    let mut journal = Journal::default();
    for (row, record) in reader.deserialize::<Row>().enumerate() {
        let record = record.with_context(|| format!("reading journal row {}", row + 1))?;
        journal.postings.push(Posting {
            group: record.group,
            source: record.source,
            date: record.date,
            payee: record.payee,
            narration: record.narration.unwrap_or_default(),
            external_ref: record.external_ref,
            account: record.account,
            amount: record.amount,
            currency: record.currency,
            tags: record.tags,
        });
    }
    if journal.postings.is_empty() {
        bail!("journal {} contains no rows", path.display());
    }
    Ok(journal)
}

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;

    use super::*;

    #[test]
    fn a_journal_round_trips_through_write_and_read() -> Result<()> {
        let journal = Journal {
            postings: vec![
                Posting {
                    group: 0,
                    source: Source::Manual,
                    date: NaiveDate::from_ymd_opt(2024, 7, 1).unwrap(),
                    payee: Some("Store".into()),
                    narration: "lunch".into(),
                    external_ref: None,
                    account: "Expenses:Food".into(),
                    amount: dec!(120),
                    currency: Currency::TWD,
                    tags: Some("food".into()),
                },
                Posting {
                    group: 0,
                    source: Source::Manual,
                    date: NaiveDate::from_ymd_opt(2024, 7, 1).unwrap(),
                    payee: Some("Store".into()),
                    narration: "lunch".into(),
                    external_ref: None,
                    account: "Assets:Cash".into(),
                    amount: dec!(-120),
                    currency: Currency::TWD,
                    tags: None,
                },
            ],
        };
        let file = tempfile::Builder::new().suffix(".csv").tempfile()?;
        write(file.path(), &journal)?;
        let back = read(file.path())?;
        assert_eq!(back.postings, journal.postings);
        Ok(())
    }

    #[test]
    fn a_journal_with_no_rows_is_rejected() -> Result<()> {
        let file = tempfile::Builder::new().suffix(".csv").tempfile()?;
        write(file.path(), &Journal::default())?;
        let err = read(file.path()).expect_err("an empty journal must not load");
        assert!(format!("{err:#}").contains("contains no rows"), "got: {err:#}");
        Ok(())
    }

    #[test]
    fn an_unknown_source_is_rejected_on_read() -> Result<()> {
        let file = tempfile::Builder::new().suffix(".csv").tempfile()?;
        std::fs::write(
            file.path(),
            "group,source,date,account,amount,currency,payee,narration,external_ref,tags\n0,imprt,\
             2024-07-01,Assets:Cash,-120,TWD,,,,\n",
        )?;
        let err = read(file.path()).expect_err("a misspelt source must not load");
        assert!(format!("{err:#}").contains("imprt"), "got: {err:#}");
        Ok(())
    }
}
