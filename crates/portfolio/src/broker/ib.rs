//! Interactive Brokers activity statements (the CSV download).
//!
//! The file stacks sections, each with its own `Header` row. Lines carry no
//! ids and identical lines genuinely repeat (a withholding charged, refunded
//! and charged again), so every line stays a record and repeats are numbered.

use std::collections::{BTreeMap, HashMap};

use anyhow::{bail, ensure, Context, Result};
use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime, NaiveTime, Utc, Weekday};
use ledger_types::currency::Currency;
use rust_decimal::Decimal;

use super::record::{number_repeats, BrokerRecord, BrokerStatement, Holdings, RecordKind};
use crate::portfolio::Broker;

pub const REF_PREFIX: &str = "ib:";

pub fn parse(text: &str) -> Result<BrokerStatement> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .from_reader(text.trim_start_matches('\u{feff}').as_bytes());

    let mut headers: HashMap<String, csv::StringRecord> = HashMap::new();
    let mut fields: HashMap<(String, String), String> = HashMap::new();
    let mut rows: Vec<(String, csv::StringRecord)> = Vec::new();
    for record in reader.records() {
        let record = record.context("reading an IB statement row")?;
        let section = record.get(0).unwrap_or_default().to_string();
        match record.get(1).unwrap_or_default() {
            "Header" => {
                headers.insert(section, record);
            }
            "Data" if matches!(section.as_str(), "Statement" | "Account Information") => {
                fields.insert(
                    (section, record.get(2).unwrap_or_default().to_string()),
                    record.get(3).unwrap_or_default().to_string(),
                );
            }
            "Data" => rows.push((section, record)),
            _ => {}
        }
    }

    let field = |section: &str, name: &str| {
        fields
            .get(&(section.to_string(), name.to_string()))
            .with_context(|| format!("the IB statement has no {section} {name}"))
    };
    let (period_start, period_end) = period(field("Statement", "Period")?)?;
    let generated = generated(field("Statement", "WhenGenerated")?)?;
    // A consolidated statement prints "U0000000 (Custom Consolidated)"; the
    // account number alone is what the chart is keyed on.
    let account = field("Account Information", "Account")?;
    let account = account.split_once(" (").map_or(&account[..], |(number, _)| number).to_string();
    let base: Currency =
        field("Account Information", "Base Currency")?.parse().context("IB base currency")?;

    let mut records = Vec::new();
    let mut cash_report: Vec<Row> = Vec::new();
    let mut open_positions: Vec<Row> = Vec::new();
    let mut prior_positions: Vec<Row> = Vec::new();
    for (section, record) in &rows {
        let Some(header) = headers.get(section) else { continue };
        let row = Row { header, record };
        match section.as_str() {
            "Cash Report" => cash_report.push(row),
            "Open Positions" => open_positions.push(row),
            "Mark-to-Market Performance Summary" => prior_positions.push(row),
            _ => records.extend(line(section, &row, base).with_context(|| {
                format!("IB {section} row {:?}", record.iter().collect::<Vec<_>>())
            })?),
        }
    }
    number_repeats(&mut records);

    let starting = cash(&cash_report, "Starting Cash", base)?;
    let ending = cash(&cash_report, "Ending Cash", base)?;
    let stocks = |rows: &[Row], quantity: &str, keep: fn(&Row) -> bool| -> Result<_> {
        rows.iter()
            .filter(|r| r.get("Asset Category") == "Stocks" && keep(r))
            .map(|r| Ok((r.get("Symbol").to_string(), r.decimal(quantity)?)))
            .filter(|p| !matches!(p, Ok((_, q)) if q.is_zero()))
            .collect::<Result<BTreeMap<_, _>>>()
    };
    // Prior quantities come from the performance summary; without it the
    // opening positions are unknown.
    let opening = match headers.contains_key("Mark-to-Market Performance Summary") {
        true => Some(Holdings {
            cash: starting,
            positions: stocks(&prior_positions, "Prior Quantity", |_| true)?,
        }),
        false => None,
    };
    let closing = Holdings {
        cash: ending,
        positions: stocks(&open_positions, "Quantity", |r| {
            r.get("DataDiscriminator") == "Summary"
        })?,
    };

    Ok(BrokerStatement {
        broker: Broker::InteractiveBrokers,
        account,
        period_start,
        period_end,
        generated: Some(generated),
        opening,
        closing: Some(closing),
        records,
    })
}

struct Row<'a> {
    header: &'a csv::StringRecord,
    record: &'a csv::StringRecord,
}

impl Row<'_> {
    fn get(&self, name: &str) -> &str {
        self.header
            .iter()
            .position(|h| h == name)
            .and_then(|i| self.record.get(i))
            .unwrap_or_default()
    }

    fn decimal(&self, name: &str) -> Result<Decimal> {
        let s = self.get(name).replace(',', "");
        match s.trim() {
            "" | "--" => Ok(Decimal::ZERO),
            s => s.parse().with_context(|| format!("{name} {s:?}")),
        }
    }

    fn date(&self, name: &str) -> Result<NaiveDate> {
        NaiveDate::parse_from_str(self.get(name), "%Y-%m-%d")
            .with_context(|| format!("{name} {:?}", self.get(name)))
    }

    /// Summary rows put "Total …" in their first column.
    fn is_total(&self) -> bool { self.record.get(2).is_some_and(|f| f.starts_with("Total")) }
}

/// The record one data row stands for, if it moves cash or shares.
fn line(section: &str, row: &Row, base: Currency) -> Result<Option<BrokerRecord>> {
    if row.is_total() {
        return Ok(None);
    }
    let currency = |row: &Row| -> Result<Currency> {
        row.get("Currency").parse().with_context(|| format!("currency {:?}", row.get("Currency")))
    };
    let record =
        |kind, date: NaiveDate, currency, amount: Decimal, description: &str| BrokerRecord {
            kind,
            trade_date: Some(date),
            settle_date: date,
            executed_at: None,
            symbol: symbol_of(description),
            quantity: Decimal::ZERO,
            price: Decimal::ZERO,
            amount,
            commission: Decimal::ZERO,
            currency,
            description: description.to_string(),
            key: format!("{section}:{date}:{currency}:{description}:{amount}"),
        };
    let r = match section {
        "Trades" => {
            ensure!(row.get("DataDiscriminator") == "Order", "unexpected trade row");
            ensure!(
                row.get("Asset Category") == "Stocks",
                "only stock trades are understood, not {}",
                row.get("Asset Category")
            );
            let executed = eastern(row.get("Date/Time"))?;
            let date = executed.date();
            let quantity = row.decimal("Quantity")?;
            let symbol = row.get("Symbol").to_string();
            let (price, amount, commission) =
                (row.decimal("T. Price")?, row.decimal("Proceeds")?, row.decimal("Comm/Fee")?);
            BrokerRecord {
                kind: if quantity.is_sign_positive() { RecordKind::Buy } else { RecordKind::Sell },
                trade_date: Some(date),
                settle_date: date,
                executed_at: Some(to_utc(executed)),
                key: format!(
                    "{section}:{executed}:{symbol}:{quantity}:{price}:{amount}:{commission}"
                ),
                symbol: Some(symbol),
                quantity,
                price,
                amount,
                commission,
                currency: currency(row)?,
                description: row.get("Code").to_string(),
            }
        }
        "Deposits & Withdrawals" => {
            let amount = row.decimal("Amount")?;
            let kind = if amount.is_sign_positive() {
                RecordKind::Deposit
            } else {
                RecordKind::Withdrawal
            };
            let mut r = record(
                kind,
                row.date("Settle Date")?,
                currency(row)?,
                amount,
                row.get("Description"),
            );
            r.symbol = None;
            r
        }
        "Dividends" | "Withholding Tax" | "Interest" | "Fees" => {
            let kind = match section {
                "Dividends" => RecordKind::Dividend,
                "Withholding Tax" => RecordKind::Withholding,
                "Interest" => RecordKind::Interest,
                _ => RecordKind::Fee,
            };
            let mut r = record(
                kind,
                row.date("Date")?,
                currency(row)?,
                row.decimal("Amount")?,
                row.get("Description"),
            );
            if kind == RecordKind::Interest {
                r.symbol = None;
            }
            r
        }
        "Transfers" => {
            ensure!(row.get("Asset Category") == "Stocks", "only stock transfers are understood");
            let date = row.date("Date")?;
            let quantity = row.decimal("Qty")?;
            let (kind, quantity) = match row.get("Direction") {
                "In" => (RecordKind::TransferIn, quantity.abs()),
                "Out" => (RecordKind::TransferOut, -quantity.abs()),
                other => bail!("transfer direction {other:?}"),
            };
            let symbol = row.get("Symbol").to_string();
            let market_value = row.decimal("Market Value")?;
            let amount = row.decimal("Cash Amount")?;
            BrokerRecord {
                kind,
                trade_date: Some(date),
                settle_date: date,
                executed_at: None,
                key: format!("{section}:{date}:{symbol}:{quantity}:{market_value}:{amount}"),
                symbol: Some(symbol),
                quantity,
                price: market_value.checked_div(quantity.abs()).unwrap_or_default(),
                amount,
                commission: Decimal::ZERO,
                currency: currency(row)?,
                description: format!("{} {}", row.get("Type"), row.get("Xfer Account")),
            }
        }
        "Corporate Actions" => {
            let description = row.get("Description");
            ensure!(
                description.contains(") Split "),
                "only splits are understood among corporate actions: {description}"
            );
            // Booked on the report date; Date/Time is the prior evening, ET.
            let executed = eastern(row.get("Date/Time"))?;
            let date = row.date("Report Date")?;
            let quantity = row.decimal("Quantity")?;
            let mut r = record(RecordKind::Split, date, currency(row)?, Decimal::ZERO, description);
            r.trade_date = Some(executed.date());
            r.executed_at = Some(to_utc(executed));
            r.quantity = quantity;
            r.key = format!("{section}:{date}:{description}:{quantity}");
            r
        }
        "Grant Activity" => {
            let description = row.get("Description");
            let quantity = row.decimal("Quantity")?;
            let (kind, quantity) = match description {
                d if d.starts_with("Stock Award Grant") => (RecordKind::AwardGrant, quantity),
                "Stock Award Vesting" => (RecordKind::AwardVesting, Decimal::ZERO),
                "Stock Award Withholding" => (RecordKind::AwardWithholding, quantity),
                other => bail!("grant activity {other:?}"),
            };
            let date = row.date("Report Date")?;
            let symbol = row.get("Symbol").to_string();
            let (award, vesting) = (row.get("Award Date"), row.get("Vesting Date"));
            let listed = row.get("Quantity");
            BrokerRecord {
                kind,
                trade_date: Some(row.date("Award Date")?),
                settle_date: date,
                executed_at: None,
                key: format!("{section}:{date}:{symbol}:{description}:{award}:{listed}"),
                symbol: Some(symbol),
                quantity,
                price: row.decimal("Price")?,
                amount: Decimal::ZERO,
                commission: Decimal::ZERO,
                currency: base,
                description: format!("{description} {listed} (award {award}, vesting {vesting})"),
            }
        }
        _ => return Ok(None),
    };
    Ok(Some(r))
}

/// The Cash Report's figure per currency. A single-currency account lists only
/// the base-currency summary.
fn cash(rows: &[Row], line: &str, base: Currency) -> Result<BTreeMap<Currency, Decimal>> {
    let lines: Vec<&Row> = rows.iter().filter(|r| r.get("Currency Summary") == line).collect();
    ensure!(!lines.is_empty(), "the IB Cash Report has no {line}");
    let per_currency: Vec<&&Row> =
        lines.iter().filter(|r| r.get("Currency") != "Base Currency Summary").collect();
    if per_currency.is_empty() {
        Ok(BTreeMap::from([(base, lines[0].decimal("Total")?)]))
    } else {
        per_currency.iter().map(|r| Ok((r.get("Currency").parse()?, r.decimal("Total")?))).collect()
    }
}

/// `ZZZ(US0000000001) Cash Dividend …` → `ZZZ`.
fn symbol_of(description: &str) -> Option<String> {
    let (symbol, _) = description.split_once('(')?;
    let valid = !symbol.is_empty()
        && symbol.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '.');
    valid.then(|| symbol.to_string())
}

/// `January 1, 2025 - December 31, 2025`.
fn period(s: &str) -> Result<(NaiveDate, NaiveDate)> {
    let (start, end) = s.split_once(" - ").with_context(|| format!("IB period {s:?}"))?;
    let date = |d: &str| {
        NaiveDate::parse_from_str(d.trim(), "%B %d, %Y").with_context(|| format!("IB date {d:?}"))
    };
    Ok((date(start)?, date(end)?))
}

/// `2026-01-08, 22:56:25 EST`; kept in the zone it was printed in, which is
/// enough to order two downloads of one period.
fn generated(s: &str) -> Result<NaiveDateTime> {
    let local = s.rsplit_once(' ').map_or(s, |(t, _)| t);
    NaiveDateTime::parse_from_str(local, "%Y-%m-%d, %H:%M:%S")
        .with_context(|| format!("IB WhenGenerated {s:?}"))
}

/// IB prints execution times in US Eastern time.
fn eastern(s: &str) -> Result<NaiveDateTime> {
    NaiveDateTime::parse_from_str(s, "%Y-%m-%d, %H:%M:%S")
        .or_else(|_| NaiveDate::parse_from_str(s, "%Y-%m-%d").map(|d| d.and_time(NaiveTime::MIN)))
        .with_context(|| format!("IB date/time {s:?}"))
}

/// US Eastern → UTC: daylight time runs from 02:00 on the second Sunday of
/// March to 02:00 on the first Sunday of November.
pub fn to_utc(eastern: NaiveDateTime) -> DateTime<Utc> {
    let year = eastern.year();
    let sunday = |month, nth| {
        NaiveDate::from_weekday_of_month_opt(year, month, Weekday::Sun, nth)
            .expect("every month has a first and second Sunday")
            .and_hms_opt(2, 0, 0)
            .expect("02:00 exists")
    };
    let daylight = eastern >= sunday(3, 2) && eastern < sunday(11, 1);
    let offset = if daylight { 4 } else { 5 };
    (eastern + chrono::Duration::hours(offset)).and_utc()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eastern_times_become_utc_across_daylight_saving() {
        let at = |s| NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").unwrap();
        assert_eq!(to_utc(at("2025-01-10 09:30:00")).to_string(), "2025-01-10 14:30:00 UTC");
        assert_eq!(to_utc(at("2025-07-10 09:30:00")).to_string(), "2025-07-10 13:30:00 UTC");
        assert_eq!(to_utc(at("2025-03-09 01:59:59")).to_string(), "2025-03-09 06:59:59 UTC");
        assert_eq!(to_utc(at("2025-03-09 03:00:00")).to_string(), "2025-03-09 07:00:00 UTC");
    }

    #[test]
    fn a_symbol_comes_from_the_description_prefix() {
        assert_eq!(symbol_of("ZZZ(US0000000001) Cash Dividend").as_deref(), Some("ZZZ"));
        assert_eq!(symbol_of("USD Credit Interest for May-2026"), None);
    }
}
