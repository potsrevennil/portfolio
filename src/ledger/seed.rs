//! The **seed**: a flat CSV that is the single source of truth of reconciled
//! history (design doc T2). The freeze tool ([`super::freeze`]) writes it once
//! reconciliation passes and the loader ([`super::load`]) reads it into SQLite;
//! it stays deliberately dumb — no Beancount, no chart, no reconciliation — so
//! that load path carries none of that complexity. It is financial data, so it
//! is gitignored.
//!
//! One row per posting. Transaction-level fields (`date`, `payee`, `narration`,
//! `external_ref`) repeat across a group's rows; `tags` is per-posting. The
//! reserved `source` value `opening` marks an opening-balance row, which
//! becomes an `opening_balances` entry rather than a posting.

use std::path::Path;

use anyhow::{bail, Context, Result};
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::currency::Currency;

/// The reserved `source` value marking an opening-balance row.
pub const OPENING: &str = "opening";

/// The reserved posting tag marking a securities-placeholder leg (the ETF
/// backfill). The freeze tool stamps it, the loader reads it back to note the
/// account, and T11 keys off it to retire the placeholder.
pub const PLACEHOLDER_TAG: &str = "t11-securities-placeholder";

/// One posting of a transaction.
#[derive(Debug, Clone, PartialEq)]
pub struct Posting {
    /// Groups the legs of one transaction together.
    pub group: u64,
    /// `import` / `tiantian` / `manual` — the `transactions.source` value.
    pub source: String,
    pub date: NaiveDate,
    pub payee: Option<String>,
    pub narration: String,
    pub external_ref: Option<String>,
    pub account: String,
    pub amount: Decimal,
    pub currency: Currency,
    pub tags: Option<String>,
}

/// One opening position, one per (account, currency).
#[derive(Debug, Clone, PartialEq)]
pub struct Opening {
    pub account: String,
    pub currency: Currency,
    pub amount: Decimal,
    pub date: NaiveDate,
}

/// A parsed seed.
#[derive(Debug, Default)]
pub struct Seed {
    pub postings: Vec<Posting>,
    pub openings: Vec<Opening>,
}

/// One CSV row; field order is the file's column order. Openings and postings
/// share the schema — `source == OPENING` marks an opening (blank `group`), any
/// other value a posting leg. Serde handles the empty-cell ⇄ `None` mapping and
/// the date/decimal/currency formats.
#[derive(Serialize, Deserialize)]
struct Row {
    group: Option<u64>,
    source: String,
    date: NaiveDate,
    account: String,
    amount: Decimal,
    currency: Currency,
    payee: Option<String>,
    narration: Option<String>,
    external_ref: Option<String>,
    tags: Option<String>,
}

/// Writes the seed CSV. Openings come first so the file reads opening balances
/// then history, and postings keep the order they were reconciled in.
pub fn write(path: impl AsRef<Path>, seed: &Seed) -> Result<()> {
    let path = path.as_ref();
    let mut writer = csv::WriterBuilder::new()
        .from_path(path)
        .with_context(|| format!("creating {}", path.display()))?;

    for o in &seed.openings {
        writer.serialize(Row {
            group: None,
            source: OPENING.to_string(),
            date: o.date,
            account: o.account.clone(),
            amount: o.amount,
            currency: o.currency,
            payee: None,
            narration: None,
            external_ref: None,
            tags: None,
        })?;
    }
    for p in &seed.postings {
        writer.serialize(Row {
            group: Some(p.group),
            source: p.source.clone(),
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

/// Reads the seed CSV back — the inverse of [`write`], used by the loader and
/// by the freeze tool's own verification. Columns are matched by header name.
pub fn read(path: impl AsRef<Path>) -> Result<Seed> {
    let path = path.as_ref();
    let mut reader = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .from_path(path)
        .with_context(|| format!("opening {}", path.display()))?;

    let mut seed = Seed::default();
    for (row, record) in reader.deserialize::<Row>().enumerate() {
        let record = record.with_context(|| format!("reading seed row {}", row + 1))?;
        if record.source == OPENING {
            seed.openings.push(Opening {
                account: record.account,
                currency: record.currency,
                amount: record.amount,
                date: record.date,
            });
        } else {
            seed.postings.push(Posting {
                group: record
                    .group
                    .with_context(|| format!("posting at seed row {} has no group", row + 1))?,
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
    }
    if seed.postings.is_empty() && seed.openings.is_empty() {
        bail!("seed {} contains no rows", path.display());
    }
    Ok(seed)
}

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;

    use super::*;

    #[test]
    fn a_seed_round_trips_through_write_and_read() -> Result<()> {
        let seed = Seed {
            openings: vec![Opening {
                account: "Assets:Cash".into(),
                currency: Currency::TWD,
                amount: dec!(1000),
                date: NaiveDate::from_ymd_opt(2022, 1, 1).unwrap(),
            }],
            postings: vec![
                Posting {
                    group: 0,
                    source: "manual".into(),
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
                    source: "manual".into(),
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
        write(file.path(), &seed)?;
        let back = read(file.path())?;
        assert_eq!(back.openings, seed.openings);
        assert_eq!(back.postings, seed.postings);
        Ok(())
    }
}
