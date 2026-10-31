//! 台新 card statements, read from the PDF's text as `pdftotext -raw` lays it
//! out. The figures head the bill, and the lines sit in a table between its
//! header row and the notices after it. A row opens on its two dates; its
//! description may wrap, and notices and page furniture may follow it, so its
//! amount is found by the shape of the line that ends it. Wherever that guess
//! could go wrong, the lines stop adding up and the statement is refused.

use std::{
    path::{Path, PathBuf},
    str::FromStr,
};

use anyhow::{Context, Result};
use chrono::NaiveDate;
use rust_decimal::Decimal;

use super::{
    bank::{Bank, BankStatement, Merged},
    card::{self, is_instalment, Bill, Line},
    pdf,
};

const TABLE_HEADER: &str = "消費日 入帳起息日消費明細";
const TABLE_ENDS: [&str; 2] = ["台端依契約", "【Richart"];

/// `115/08/09`, in 民國 years.
fn roc_date(s: &str) -> Option<NaiveDate> {
    let mut parts = s.split('/');
    let year: i32 = parts.next()?.parse().ok()?;
    let month: u32 = parts.next()?.parse().ok()?;
    let day: u32 = parts.next()?.parse().ok()?;
    match (parts.next(), s.len()) {
        (None, 9) => NaiveDate::from_ymd_opt(year + 1911, month, day),
        _ => None,
    }
}

/// `1,234` or `-30`.
fn money(s: &str) -> Option<Decimal> {
    let digits = s.strip_prefix('-').unwrap_or(s);
    let plain = digits.chars().all(|c| c.is_ascii_digit() || c == ',')
        && digits.starts_with(|c: char| c.is_ascii_digit());
    plain.then(|| s.replace(',', "").parse().ok()).flatten()
}

/// The figure after `label` at a line's start, whatever sign prefixes the
/// label.
fn figure(text: &str, label: &str) -> Result<Decimal> {
    text.lines()
        .find_map(|line| {
            let rest = line.trim_start_matches(['+', '-', '=']).strip_prefix(label)?;
            money(rest.trim())
        })
        .with_context(|| format!("no {label}"))
}

/// The end of a row: its amount, then the country, then a foreign amount.
struct Tail {
    /// What came before it on the line.
    description: String,
    amount: Decimal,
    detail: String,
    /// It prints a country or a foreign amount, so it cannot be prose.
    certain: bool,
}

fn country(s: &str) -> bool { s.len() == 2 && s.chars().all(|c| c.is_ascii_uppercase()) }

impl FromStr for Tail {
    type Err = anyhow::Error;

    fn from_str(line: &str) -> Result<Self> {
        let words: Vec<&str> = line.split_whitespace().collect();
        // `0526 US USD 55.12`: when it was converted, where, in what, how much.
        let foreign = matches!(
            words.as_slice(),
            [.., day, place, currency, _]
                if day.len() == 4
                    && day.chars().all(|c| c.is_ascii_digit())
                    && country(place)
                    && currency.len() == 3
                    && currency.chars().all(|c| c.is_ascii_uppercase())
        );
        let end = match foreign {
            true => words.len() - 4,
            false => words.len(),
        };
        let placed = !foreign && end >= 1 && country(words[end - 1]);
        let at = end - usize::from(placed);
        let amount = at
            .checked_sub(1)
            .and_then(|i| money(words[i]))
            .with_context(|| format!("no amount in {line:?}"))?;
        Ok(Tail {
            description: words[..at - 1].join(" "),
            amount,
            detail: words[at..].join(" "),
            certain: foreign || placed,
        })
    }
}

/// One row: the lines from its dates to the next row's. The first line ending
/// the way only an amount does ends it; failing that, the first with a bare
/// amount at its end, as a fee or a rebate prints.
fn row(purchased: NaiveDate, posted: NaiveDate, lines: &[&str]) -> Result<Line> {
    let tails: Vec<Option<Tail>> = lines.iter().map(|l| l.parse().ok()).collect();
    let found =
        |wanted: fn(&Tail) -> bool| tails.iter().position(|t| t.as_ref().is_some_and(wanted));
    let at = found(|t| t.certain)
        .or_else(|| found(|_| true))
        .with_context(|| format!("no amount for the row {purchased} {}", lines.join(" ")))?;
    let tail = tails[at].as_ref().expect("found above");
    let description =
        lines[..at].iter().copied().chain([tail.description.as_str()]).collect::<String>();
    Ok(Line {
        purchased: Some(purchased).filter(|_| !is_instalment(&description)),
        posted,
        description,
        amount: tail.amount,
        // Only a row abroad has anything to add.
        detail: tail.detail.strip_prefix("TW").unwrap_or(&tail.detail).trim().to_string(),
    })
}

/// A line opening a row: its two dates, and the rest of the line.
fn dated(line: &str) -> Option<(NaiveDate, NaiveDate, &str)> {
    let mut parts = line.splitn(3, ' ');
    let purchased = roc_date(parts.next()?)?;
    let posted = roc_date(parts.next()?)?;
    Some((purchased, posted, parts.next().unwrap_or("").trim()))
}

impl FromStr for Bill {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        let closes = text
            .lines()
            .find_map(|l| l.strip_prefix("帳單結帳日").and_then(|d| roc_date(d.trim())))
            .context("no 帳單結帳日")?;
        let opening = figure(text, "上期應繳總額")?;
        let closing = figure(text, "本期累計應繳金額")?;

        // A page break can fall inside a row's lines, or join two rows.
        let lines: Vec<&str> = text.split(['\n', '\u{c}']).map(str::trim).collect();
        let start =
            lines.iter().position(|l| l.starts_with(TABLE_HEADER)).context("no 消費明細 table")?;
        let table = &lines[start + 1..];
        let end = table
            .iter()
            .position(|l| TABLE_ENDS.iter().any(|e| l.starts_with(e)))
            .context("no end to the 消費明細 table")?;

        let mut rows: Vec<(NaiveDate, NaiveDate, Vec<&str>)> = Vec::new();
        for line in &table[..end] {
            match (dated(line), rows.last_mut()) {
                (Some((purchased, posted, rest)), _) => {
                    rows.push((purchased, posted, vec![rest]));
                }
                (None, Some((.., held))) if !line.is_empty() => held.push(line),
                (None, _) => {}
            }
        }
        let parsed = rows
            .iter()
            .map(|(purchased, posted, held)| row(*purchased, *posted, held))
            .collect::<Result<Vec<_>>>()?;
        // A waived fee prints at zero: nothing moved.
        let lines = parsed.into_iter().filter(|l| !l.amount.is_zero()).collect();
        Ok(Bill { closes, opening, closing, lines })
    }
}

/// One statement, of the card its folder names.
pub fn load(path: &Path) -> Result<BankStatement> {
    card::load(path, Bank::TaishinCard, |path| pdf::text(path)?.parse())
}

pub fn load_merged(paths: &[PathBuf]) -> Result<Vec<Merged>> { card::load_merged(paths, load) }

#[cfg(test)]
mod tests;
