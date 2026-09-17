//! The **seed**: a flat, format-neutral CSV that is the single source of truth
//! of reconciled history (design doc T2).
//!
//! The freeze tool ([`super::freeze`]) writes it once reconciliation passes;
//! the seed loader ([`super::load`]) reads it into SQLite. It is deliberately
//! dumb — no Beancount, no chart, no reconciliation — so the app's
//! historical-load path carries none of that complexity. It is financial data,
//! so it is gitignored.
//!
//! One row per posting. `source` doubles as a row-kind discriminator: the
//! reserved value `opening` marks an opening-balance row (which becomes an
//! `opening_balances` entry, not a posting), and every other value
//! (`import` / `tiantian` / `manual`) is a real posting grouped into a
//! transaction by `group`. Transaction-level fields (`date`, `payee`,
//! `narration`, `external_ref`) repeat across a group's rows; `tags` is
//! per-posting.

use std::{collections::HashMap, path::Path};

use anyhow::{bail, Context, Result};
use chrono::NaiveDate;
use rust_decimal::Decimal;

use crate::currency::Currency;

/// The reserved `source` value marking an opening-balance row.
pub const OPENING: &str = "opening";

/// The reserved posting tag marking a securities-placeholder leg (the ETF
/// backfill). The freeze tool stamps it; the loader reads it back to note the
/// account, and T11 keys off it to retire the placeholder. Shared here because
/// both ends of the seed rely on the exact string.
pub const PLACEHOLDER_TAG: &str = "t11-securities-placeholder";

const COLUMNS: [&str; 10] = [
    "group",
    "source",
    "date",
    "account",
    "amount",
    "currency",
    "payee",
    "narration",
    "external_ref",
    "tags",
];

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

/// Writes the seed CSV. Openings come first so the file reads opening balances
/// then history, and postings keep the order they were reconciled in.
pub fn write(path: impl AsRef<Path>, seed: &Seed) -> Result<()> {
    let path = path.as_ref();
    let mut writer = csv::WriterBuilder::new()
        .from_path(path)
        .with_context(|| format!("creating {}", path.display()))?;
    writer.write_record(COLUMNS)?;

    for o in &seed.openings {
        writer.write_record([
            "",
            OPENING,
            &o.date.to_string(),
            &o.account,
            &o.amount.to_string(),
            &o.currency.to_string(),
            "",
            "",
            "",
            "",
        ])?;
    }
    for p in &seed.postings {
        writer.write_record([
            &p.group.to_string(),
            &p.source,
            &p.date.to_string(),
            &p.account,
            &p.amount.to_string(),
            &p.currency.to_string(),
            p.payee.as_deref().unwrap_or(""),
            &p.narration,
            p.external_ref.as_deref().unwrap_or(""),
            p.tags.as_deref().unwrap_or(""),
        ])?;
    }
    writer.flush().with_context(|| format!("flushing {}", path.display()))?;
    Ok(())
}

/// Reads the seed CSV back. The inverse of [`write`], used by the loader and by
/// the freeze tool's own verification.
pub fn read(path: impl AsRef<Path>) -> Result<Seed> {
    let path = path.as_ref();
    let mut reader = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .from_path(path)
        .with_context(|| format!("opening {}", path.display()))?;
    let columns: HashMap<String, usize> = reader
        .headers()
        .with_context(|| format!("reading the header of {}", path.display()))?
        .iter()
        .enumerate()
        .map(|(i, name)| (name.to_string(), i))
        .collect();
    let index = |name: &str| -> Result<usize> {
        columns.get(name).copied().with_context(|| format!("seed is missing column {name:?}"))
    };
    let (
        group_i,
        source_i,
        date_i,
        account_i,
        amount_i,
        currency_i,
        payee_i,
        narration_i,
        ref_i,
        tags_i,
    ) = (
        index("group")?,
        index("source")?,
        index("date")?,
        index("account")?,
        index("amount")?,
        index("currency")?,
        index("payee")?,
        index("narration")?,
        index("external_ref")?,
        index("tags")?,
    );

    let mut seed = Seed::default();
    for (row, record) in reader.records().enumerate() {
        let record = record.with_context(|| format!("reading seed row {}", row + 1))?;
        let get = |i: usize| record.get(i).unwrap_or("");
        let field = |i: usize| -> Option<String> {
            let value = get(i);
            (!value.is_empty()).then(|| value.to_string())
        };
        let date: NaiveDate = NaiveDate::parse_from_str(get(date_i), "%Y-%m-%d")
            .with_context(|| format!("unparseable date at seed row {}", row + 1))?;
        let account = get(account_i).to_string();
        let amount: Decimal = get(amount_i)
            .parse()
            .with_context(|| format!("unparseable amount at seed row {}", row + 1))?;
        let currency: Currency = get(currency_i)
            .parse()
            .with_context(|| format!("unknown currency at seed row {}", row + 1))?;

        if get(source_i) == OPENING {
            seed.openings.push(Opening { account, currency, amount, date });
        } else {
            let group: u64 = get(group_i)
                .parse()
                .with_context(|| format!("unparseable group at seed row {}", row + 1))?;
            seed.postings.push(Posting {
                group,
                source: get(source_i).to_string(),
                date,
                payee: field(payee_i),
                narration: get(narration_i).to_string(),
                external_ref: field(ref_i),
                account,
                amount,
                currency,
                tags: field(tags_i),
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
