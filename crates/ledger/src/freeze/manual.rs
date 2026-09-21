//! Reader for `manual.csv` — hand-entered cash and anything no statement
//! covers.
//!
//! This replaced a hand-authored `manual.beancount`, and with it the last
//! reason the freeze tool ever parsed Beancount: every input is now structured
//! data folded into [`crate::ledger::model`]. Each row is one **balanced
//! two-leg, single-currency** transaction — `amount` is the signed effect on
//! `account`, and `contra` gets its negation — so a hand-typed entry cannot
//! drift out of balance, which is the founding invariant. Cross-currency and
//! 3+-leg entries are deliberately not expressible here; those wait for the UI.
//!
//! Columns (header row, read by name; `#` starts a comment line):
//! `date, account, contra, amount, currency, payee, narration, tags`. Only
//! `date`, `account`, `contra` and `amount` are required; `currency` defaults
//! to TWD and the rest to empty. `tags` is whitespace/`;`-separated.

use std::{collections::HashMap, path::Path};

use anyhow::{bail, Context, Result};
use chrono::NaiveDate;
use rust_decimal::Decimal;

use crate::{
    currency::Currency,
    ledger::{
        model::{Source, Transaction},
        writer::Posting,
    },
};

/// Loads `manual.csv` into model transactions tagged [`Source::Manual`].
pub fn load(path: impl AsRef<Path>) -> Result<Vec<Transaction>> {
    let path = path.as_ref();
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .comment(Some(b'#'))
        .trim(csv::Trim::All)
        .flexible(true)
        .from_path(path)
        .with_context(|| format!("opening {}", path.display()))?;

    // Read columns by name so the order is the user's to choose.
    let columns: HashMap<String, usize> = reader
        .headers()
        .with_context(|| format!("reading the header of {}", path.display()))?
        .iter()
        .enumerate()
        .map(|(i, name)| (name.to_ascii_lowercase(), i))
        .collect();
    let required = |name: &str| -> Result<usize> {
        columns
            .get(name)
            .copied()
            .with_context(|| format!("{} is missing the required column {name:?}", path.display()))
    };
    let (date_col, account_col, contra_col, amount_col) =
        (required("date")?, required("account")?, required("contra")?, required("amount")?);
    let optional = |record: &csv::StringRecord, name: &str| -> String {
        columns.get(name).and_then(|&i| record.get(i)).unwrap_or("").trim().to_string()
    };

    let mut out = Vec::new();
    for (row, record) in reader.records().enumerate() {
        let record =
            record.with_context(|| format!("reading {} row {}", path.display(), row + 1))?;
        let cell = |col: usize, name: &str| -> Result<&str> {
            record
                .get(col)
                .map(str::trim)
                .with_context(|| format!("{} row {} has no {name} cell", path.display(), row + 1))
        };

        let date_cell = cell(date_col, "date")?;
        let date = NaiveDate::parse_from_str(date_cell, "%Y-%m-%d")
            .with_context(|| format!("unparseable date {date_cell:?} at row {}", row + 1))?;
        let amount_cell = cell(amount_col, "amount")?;
        let amount: Decimal = amount_cell
            .replace(',', "")
            .parse()
            .with_context(|| format!("unparseable amount {amount_cell:?} at row {}", row + 1))?;
        let account = cell(account_col, "account")?.to_string();
        let contra = cell(contra_col, "contra")?.to_string();
        if account.is_empty() || contra.is_empty() {
            bail!("{} row {} has an empty account or contra", path.display(), row + 1);
        }

        let currency = match optional(&record, "currency") {
            blank if blank.is_empty() => Currency::TWD,
            code => code
                .parse()
                .with_context(|| format!("unknown currency {code:?} at row {}", row + 1))?,
        };
        let tags = optional(&record, "tags")
            .split([';', ' ', '\t'])
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .collect();

        out.push(Transaction {
            date,
            payee: optional(&record, "payee"),
            narration: optional(&record, "narration"),
            tags,
            // Signed on `account`; the contra leg is the negation, so the two
            // legs always sum to zero and a hand entry cannot imbalance.
            postings: vec![
                Posting::new(account, amount, currency),
                Posting::new(contra, -amount, currency),
            ],
            source: Source::Manual,
            external_ref: None,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;

    use super::*;

    fn write(contents: &str) -> tempfile::NamedTempFile {
        use std::io::Write as _;
        let mut file = tempfile::Builder::new().suffix(".csv").tempfile().expect("temp file");
        file.write_all(contents.as_bytes()).expect("write");
        file
    }

    #[test]
    fn a_row_becomes_a_balanced_two_leg_transaction() -> Result<()> {
        let file = write(
            "date,account,contra,amount,currency,payee,narration,tags\n2024-07-01,Assets:Cash,\
             Expenses:Food,-120,TWD,Store,lunch,food;treat\n",
        );
        let txns = load(file.path())?;
        assert_eq!(txns.len(), 1);
        let t = &txns[0];
        assert_eq!(t.source, Source::Manual);
        assert_eq!(t.payee, "Store");
        assert_eq!(t.tags, vec!["food", "treat"]);
        assert_eq!(t.postings.len(), 2);
        assert_eq!(t.postings[0].amount, Some(dec!(-120)));
        assert_eq!(t.postings[1].account, "Expenses:Food");
        assert_eq!(t.postings[1].amount, Some(dec!(120)), "contra is the negation");
        Ok(())
    }

    #[test]
    fn currency_defaults_to_twd_and_columns_may_reorder() -> Result<()> {
        let file = write("amount,contra,account,date\n500,Income:Gift,Assets:Cash,2024-04-01\n");
        let txns = load(file.path())?;
        assert_eq!(txns[0].postings[0].currency, Currency::TWD);
        assert_eq!(txns[0].postings[0].amount, Some(dec!(500)));
        Ok(())
    }

    #[test]
    fn comment_lines_are_ignored() -> Result<()> {
        let file = write(
            "date,account,contra,amount\n# a note about the entries \
             below\n2024-04-01,Assets:Cash,Income:Gift,500\n",
        );
        assert_eq!(load(file.path())?.len(), 1);
        Ok(())
    }

    #[test]
    fn a_missing_required_column_fails_loudly() {
        let file = write("date,account,amount\n2024-04-01,Assets:Cash,500\n");
        let err = load(file.path()).expect_err("a missing contra column must fail");
        assert!(format!("{err:#}").contains("contra"), "got: {err:#}");
    }

    #[test]
    fn an_empty_account_or_contra_fails_loudly() {
        let file = write("date,account,contra,amount\n2024-04-01,Assets:Cash,,500\n");
        let err = load(file.path()).expect_err("a blank contra must fail");
        assert!(format!("{err:#}").contains("empty account or contra"), "got: {err:#}");
    }
}
