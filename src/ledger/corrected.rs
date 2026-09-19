//! Parser for `corrected/transactions.csv`: every non-investment money
//! movement, 天天記帳's records with corrections applied, one row per record.
//!
//! Yields the same [`daily::Entry`] records as the two native exports, so the
//! rest of the build cannot tell the sources apart. Only `status = active` rows
//! count; `removed` rows exist to retire old change-logs. `posted_date` (a
//! card line's 入帳日) is accepted and ignored here.
//!
//! A `kind = opening` row is a position an account already held before its
//! records begin (the app has no such concept). `account` is the app label, as
//! on every row, and `amount` is signed: negative for a debt owed at the start.

use std::path::Path;

use anyhow::{bail, Context, Result};
use chrono::NaiveDate;
use rust_decimal::Decimal;

use super::daily::Entry;
use crate::currency::Currency;

/// Positions of the columns the build reads, found by header name.
struct Columns {
    status: usize,
    date: usize,
    kind: usize,
    amount: usize,
    currency: usize,
    account: usize,
    counter_account: usize,
    counter_amount: usize,
    counter_currency: usize,
    category: usize,
    note: usize,
    source_id: usize,
}

impl Columns {
    fn from_header(header: &csv::StringRecord) -> Result<Self> {
        let find = |name: &str| {
            header
                .iter()
                .position(|h| h.trim() == name)
                .with_context(|| format!("no {name} column"))
        };
        Ok(Columns {
            status: find("status")?,
            date: find("date")?,
            kind: find("kind")?,
            amount: find("amount")?,
            currency: find("currency")?,
            account: find("account")?,
            counter_account: find("counter_account")?,
            counter_amount: find("counter_amount")?,
            counter_currency: find("counter_currency")?,
            category: find("category")?,
            note: find("note")?,
            source_id: find("source_id")?,
        })
    }
}

/// A starting position, from a `kind = opening` row.
#[derive(Debug)]
pub struct Opening {
    pub date: NaiveDate,
    /// The app label, mapped through `[accounts]` like any other row.
    pub account: String,
    pub amount: Decimal,
    pub currency: Currency,
}

/// The active rows: movements, and the openings kept apart from them so they
/// don't move the date the records begin.
#[derive(Debug, Default)]
pub struct Records {
    pub entries: Vec<Entry>,
    pub openings: Vec<Opening>,
}

fn amount(s: &str) -> Result<Decimal> {
    let t = s.trim();
    if t.is_empty() {
        Ok(Decimal::ZERO)
    } else {
        t.parse().with_context(|| format!("unparseable amount {t:?}"))
    }
}

/// Blank means TWD, as in the app's own export.
fn currency(s: &str) -> Result<Currency> {
    match s.trim() {
        "" => Ok(Currency::TWD),
        code => code.parse().with_context(|| format!("unknown currency {code:?}")),
    }
}

/// Every active record, entries oldest first. Within a day, income and expense
/// come before transfers, as with the native exports.
pub fn load(path: impl AsRef<Path>) -> Result<Records> {
    let path = path.as_ref();
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(true)
        .from_path(path)
        .with_context(|| format!("opening {}", path.display()))?;
    let cols = Columns::from_header(rdr.headers()?)
        .with_context(|| format!("header of {}", path.display()))?;

    let mut flows = Vec::new();
    let mut transfers = Vec::new();
    let mut openings = Vec::new();
    for (i, result) in rdr.records().enumerate() {
        let rec = result?;
        let get = |c: usize| rec.get(c).unwrap_or("").trim();
        let row = || format!("{} record {}", path.display(), i + 1);

        match get(cols.status) {
            "active" => {}
            "removed" => continue,
            other => bail!("{}: unknown status {other:?}", row()),
        }
        let date = NaiveDate::parse_from_str(get(cols.date), "%Y-%m-%d")
            .with_context(|| format!("{}: unparseable date", row()))?;
        let value = amount(get(cols.amount)).with_context(row)?;
        let entry_currency = currency(get(cols.currency)).with_context(row)?;
        let memo = get(cols.note).to_string();

        match get(cols.kind) {
            kind @ ("income" | "expense") => flows.push(Entry::Flow {
                date,
                account: get(cols.account).to_string(),
                amount: if kind == "income" { value } else { -value },
                currency: entry_currency,
                category: get(cols.category).to_string(),
                memo,
                // The app's UUID, which [overrides] and [[trips]] key on.
                id: get(cols.source_id).to_string(),
            }),
            "transfer" => transfers.push(Entry::Transfer {
                date,
                from: get(cols.account).to_string(),
                out: value,
                out_currency: entry_currency,
                to: get(cols.counter_account).to_string(),
                inn: amount(get(cols.counter_amount)).with_context(row)?,
                in_currency: currency(get(cols.counter_currency)).with_context(row)?,
                memo,
            }),
            "opening" => openings.push(Opening {
                date,
                account: get(cols.account).to_string(),
                amount: value,
                currency: entry_currency,
            }),
            other => bail!("{}: unknown kind {other:?}", row()),
        }
    }

    let mut entries = flows;
    entries.extend(transfers);
    entries.sort_by_key(Entry::date);
    Ok(Records { entries, openings })
}

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;

    use super::*;

    const HEADER: &str = "id,status,date,posted_date,kind,amount,currency,account,counter_account,\
                          counter_amount,counter_currency,category,major_category,member,tags,\
                          note,source_party,source_file,source_id,origin,correction_note,\
                          updated_at\n";

    fn records(rows: &str) -> Result<Records> {
        let file = tempfile::NamedTempFile::new()?;
        std::fs::write(file.path(), format!("{HEADER}{rows}"))?;
        super::load(file.path())
    }

    fn load(rows: &str) -> Result<Vec<Entry>> { Ok(records(rows)?.entries) }

    /// An opening keeps its sign and stays out of the entries.
    #[test]
    fn an_opening_row_is_kept_apart_with_its_sign() -> Result<()> {
        let r = records(
            "o:1,active,2022-01-01,,opening,-500,TWD,信用卡,,,,,,,,,config,m,,added,moved,\na:1,\
             active,2022-02-01,,expense,1,,錢包,,,,飲食,,,,,app,f,U,raw,,\n",
        )?;
        assert_eq!(r.entries.len(), 1);
        assert_eq!(r.openings.len(), 1);
        assert_eq!(r.openings[0].account, "信用卡");
        assert_eq!(r.openings[0].amount, dec!(-500));
        Ok(())
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
    fn an_unknown_status_or_kind_is_an_error() {
        assert!(load("a,pending,2024-03-01,,expense,1,,錢包,,,,飲食,,,,,,,U,raw,,\n").is_err());
        assert!(load("a,active,2024-03-01,,refund,1,,錢包,,,,飲食,,,,,,,U,raw,,\n").is_err());
    }
}
