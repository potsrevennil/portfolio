//! Parser for `corrected/transactions.csv`, yielding the same
//! [`Entry`] records as the native 天天記帳 exports. Only
//! `status = active` rows count; `posted_date` is ignored.

use std::{collections::HashSet, path::Path};

use anyhow::{Context, Result};
use chrono::NaiveDate;
use ledger_types::currency::Currency;
use rust_decimal::Decimal;
use serde::Deserialize;

use super::daily::Entry;

#[derive(Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
enum Status {
    Active,
    Removed,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Kind {
    Income,
    Expense,
    Transfer,
}

/// The columns the build reads; the rest are ignored. A blank currency is TWD,
/// as in the app's own export.
#[derive(Deserialize)]
struct Row {
    status: Status,
    date: NaiveDate,
    kind: Kind,
    amount: Decimal,
    currency: Option<Currency>,
    account: String,
    counter_account: String,
    counter_amount: Option<Decimal>,
    counter_currency: Option<Currency>,
    category: String,
    note: String,
    source_id: String,
}

/// Entries oldest first; within a day, income and expense before transfers, as
/// in the native exports.
pub fn load(path: impl AsRef<Path>) -> Result<Vec<Entry>> {
    let path = path.as_ref();
    let mut rdr = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .from_path(path)
        .with_context(|| format!("opening {}", path.display()))?;

    let mut flows = Vec::new();
    let mut transfers = Vec::new();
    for (i, row) in rdr.deserialize::<Row>().enumerate() {
        let row = row.with_context(|| format!("{} record {}", path.display(), i + 1))?;
        if row.status == Status::Removed {
            continue;
        }
        let currency = row.currency.unwrap_or(Currency::TWD);
        match row.kind {
            Kind::Income | Kind::Expense => flows.push(Entry::Flow {
                date: row.date,
                account: row.account,
                amount: match row.kind {
                    Kind::Income => row.amount,
                    _ => -row.amount,
                },
                currency,
                category: row.category,
                memo: row.note,
                id: row.source_id,
            }),
            Kind::Transfer => transfers.push(Entry::Transfer {
                date: row.date,
                from: row.account,
                out: row.amount,
                out_currency: currency,
                to: row.counter_account,
                inn: row.counter_amount.unwrap_or_default(),
                in_currency: row.counter_currency.unwrap_or(Currency::TWD),
                memo: row.note,
                id: row.source_id,
            }),
        }
    }

    let mut entries = flows;
    entries.extend(transfers);
    entries.sort_by_key(Entry::date);
    Ok(entries)
}

/// What a freeze of this file has already taken: every record id it names,
/// removed ones included, and the last day it covers.
#[derive(Debug, Default)]
pub struct Frozen {
    pub through: Option<NaiveDate>,
    pub ids: HashSet<String>,
}

pub fn frozen(path: impl AsRef<Path>) -> Result<Frozen> {
    #[derive(Deserialize)]
    struct Row {
        date: NaiveDate,
        source_id: String,
    }
    let path = path.as_ref();
    let mut rdr = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .from_path(path)
        .with_context(|| format!("opening {}", path.display()))?;
    let mut frozen = Frozen::default();
    for (i, row) in rdr.deserialize::<Row>().enumerate() {
        let row = row.with_context(|| format!("{} record {}", path.display(), i + 1))?;
        frozen.through = frozen.through.max(Some(row.date));
        if !row.source_id.is_empty() {
            frozen.ids.insert(row.source_id);
        }
    }
    Ok(frozen)
}

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;

    use super::*;

    const HEADER: &str = "id,status,date,posted_date,kind,amount,currency,account,counter_account,\
                          counter_amount,counter_currency,category,major_category,member,tags,\
                          note,source_party,source_file,source_id,origin,correction_note,\
                          updated_at\n";

    fn load(rows: &str) -> Result<Vec<Entry>> {
        let file = tempfile::NamedTempFile::new()?;
        std::fs::write(file.path(), format!("{HEADER}{rows}"))?;
        super::load(file.path())
    }

    #[test]
    fn reads_active_rows_and_skips_removed_ones() -> Result<()> {
        let entries = load(
            "a:1,active,2024-03-02,2024-03-04,expense,120,,錢包,,,,飲食,,自己,,午餐,app,f,U1,raw,,\
             \na:2,removed,2024-03-01,,expense,99,,錢包,,,,飲食,,自己,,,app,f,U2,removed,dup,\na:\
             3,active,2024-03-01,,transfer,300,TWD,錢包,美金,10,USD,,,,,換匯,app,f,U3,added,,\na:\
             4,active,2024-03-02,,income,50,TWD,錢包,,,,利息,,自己,,,app,f,U4,raw,,\n",
        )?;
        assert_eq!(entries.len(), 3);
        assert!(matches!(
            &entries[0],
            Entry::Transfer { out, in_currency: Currency::USD, inn, .. }
                if *out == dec!(300) && *inn == dec!(10)
        ));
        assert!(matches!(
            &entries[1],
            Entry::Flow { amount, currency: Currency::TWD, id, category, .. }
                if *amount == dec!(-120) && id == "U1" && category == "飲食"
        ));
        assert!(matches!(&entries[2], Entry::Flow { amount, .. } if *amount == dec!(50)));
        Ok(())
    }

    #[test]
    fn frozen_names_removed_records_too() -> Result<()> {
        let file = tempfile::NamedTempFile::new()?;
        std::fs::write(
            file.path(),
            format!(
                "{HEADER}a:1,active,2024-03-02,,expense,1,,錢包,,,,飲食,,,,,app,f,U1,raw,,\na:2,\
                 removed,2024-03-05,,expense,1,,錢包,,,,飲食,,,,,app,f,U2,removed,dup,\n"
            ),
        )?;
        let frozen = frozen(file.path())?;
        assert_eq!(frozen.through, NaiveDate::from_ymd_opt(2024, 3, 5));
        assert_eq!(frozen.ids, HashSet::from(["U1".to_string(), "U2".to_string()]));
        Ok(())
    }

    #[test]
    fn an_unknown_status_or_kind_is_an_error() {
        assert!(load("a,pending,2024-03-01,,expense,1,,錢包,,,,飲食,,,,,,,U,raw,,\n").is_err());
        assert!(load("a,active,2024-03-01,,refund,1,,錢包,,,,飲食,,,,,,,U,raw,,\n").is_err());
    }
}
