//! Cathay (國泰世華) card statements, the CSV its online bank exports. The file
//! is sections of quoted rows, found here by the labels above them.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::{Datelike, NaiveDate};
use rust_decimal::Decimal;

use super::{
    bank::{Bank, BankStatement, Merged},
    card::{self, is_instalment, Bill, Line},
};

/// What the statement prints for an empty cell.
const NONE: &str = "−";
/// The first row of the 帳單明細 table restates the opening.
const OPENING_ROW: &str = "上期帳單總額";

/// `TWD 1,234` or `-1,234`.
fn money(cell: &str) -> Result<Decimal> {
    let bare = cell.trim().trim_start_matches("TWD").trim().replace(',', "");
    bare.parse().with_context(|| format!("amount {cell:?}"))
}

/// The cell under `name` in the row after the one naming it.
fn under<'a>(rows: &'a [csv::StringRecord], name: &str) -> Result<&'a str> {
    rows.iter()
        .enumerate()
        .find_map(|(i, row)| {
            let at = row.iter().position(|cell| cell.trim() == name)?;
            rows.get(i + 1)?.get(at)
        })
        .with_context(|| format!("no {name}"))
}

/// `MM/DD`, in the year that puts it at or before the statement's month.
fn day(cell: &str, closes: NaiveDate) -> Result<NaiveDate> {
    let (month, day) = cell.split_once('/').with_context(|| format!("date {cell:?}"))?;
    let (month, day): (u32, u32) = (month.parse()?, day.parse()?);
    let year = closes.year() - i32::from(month > closes.month());
    NaiveDate::from_ymd_opt(year, month, day).with_context(|| format!("date {cell:?}"))
}

/// A printed date, or none.
fn optional_day(cell: &str, closes: NaiveDate) -> Result<Option<NaiveDate>> {
    match cell.trim() {
        NONE => Ok(None),
        date => day(date, closes).map(Some),
    }
}

impl TryFrom<&[csv::StringRecord]> for Bill {
    type Error = anyhow::Error;

    fn try_from(rows: &[csv::StringRecord]) -> Result<Self> {
        let closes = NaiveDate::parse_from_str(under(rows, "帳單結帳日")?.trim(), "%Y/%m/%d")
            .context("帳單結帳日")?;
        let opening = money(under(rows, OPENING_ROW)?)?;
        let closing = money(under(rows, "本期應繳總額")?)?;

        let mut lines = Vec::new();
        let mut table: Option<(&csv::StringRecord, usize)> = None;
        for row in rows {
            if row.get(0).is_some_and(|c| c.trim() == "消費日") {
                table = Some((row, row.len()));
                continue;
            }
            let Some((header, width)) = table else { continue };
            if row.len() != width || row.get(1).is_some_and(|c| c.trim() == OPENING_ROW) {
                continue;
            }
            let cell = |name: &str| {
                header
                    .iter()
                    .position(|c| c.trim() == name)
                    .and_then(|at| row.get(at))
                    .map(str::trim)
                    .with_context(|| format!("no {name} column"))
            };
            let description = cell("交易說明")?.to_string();
            let purchased = optional_day(cell("消費日")?, closes)?;
            // Interest is charged at the closing and printed with no date.
            let posted = optional_day(cell("入帳起息日")?, closes)?.unwrap_or(closes);
            let detail = [cell("消費國家/幣別")?, cell("消費金額")?]
                .into_iter()
                .filter(|c| *c != NONE)
                .collect::<Vec<_>>()
                .join(" ");
            let amount = money(cell("新臺幣金額")?)?;
            // A waived fee prints at zero: nothing moved.
            if !amount.is_zero() {
                lines.push(Line {
                    purchased: purchased.filter(|_| !is_instalment(&description)),
                    posted,
                    amount,
                    description,
                    detail,
                });
            }
        }
        if table.is_none() {
            bail!("no 帳單明細 table");
        }
        Ok(Bill { closes, opening, closing, lines })
    }
}

/// One statement, of the card its folder names.
pub fn load(path: impl AsRef<Path>) -> Result<BankStatement> {
    let path = path.as_ref();
    let read = || -> Result<BankStatement> {
        let bytes = std::fs::read(path)?;
        let text = String::from_utf8(bytes).context("not UTF-8")?;
        let rows = csv::ReaderBuilder::new()
            .has_headers(false)
            .flexible(true)
            .from_reader(text.trim_start_matches('\u{feff}').as_bytes())
            .records()
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Bill::try_from(rows.as_slice())?.into_statement(Bank::CathayCard, card::folder_card(path)?)
    };
    read().with_context(|| format!("reading {}", path.display()))
}

pub fn load_merged(paths: &[PathBuf]) -> Result<Vec<Merged>> {
    let statements =
        paths.iter().map(|path| Ok((load(path)?, path.clone()))).collect::<Result<Vec<_>>>()?;
    card::merge(statements)
}

#[cfg(test)]
mod tests;
