//! 台新 bank account history, as its online bank gives it: a PDF per query, or
//! an xlsx. Either says what it was asked for and when. The account number is
//! taken from the statement's folder (`raw/taishin-bank/活存-<account>/`), as
//! an xlsx doesn't print it.
//!
//! A row's balance checks every row before it, so a misread row can't pass. It
//! is also the only opening there is: nothing states one, so a query's opening
//! is its first row's balance before that row.

use std::{
    path::{Path, PathBuf},
    str::FromStr,
};

use anyhow::{bail, ensure, Context, Result};
use calamine::{Data, Reader, Xlsx};
use chrono::NaiveDate;
use ledger_types::currency::Currency;
use rust_decimal::Decimal;

use super::{
    bank::{self, folder_account, Bank, BankStatement, Merged, Period, StatementLine},
    pdf,
};

fn date(s: &str) -> Option<NaiveDate> { NaiveDate::parse_from_str(s, "%Y/%m/%d").ok() }

#[derive(Debug, PartialEq)]
struct Row {
    /// 帳務日, the day the balance moved.
    book: NaiveDate,
    description: String,
    /// What it moved the balance by.
    delta: Decimal,
    balance: Decimal,
    remark: String,
}

/// What a query gave.
#[derive(Debug, PartialEq)]
struct Query {
    from: NaiveDate,
    to: NaiveDate,
    /// The day it was made: that day may yet gain lines.
    made: NaiveDate,
    /// Oldest first.
    rows: Vec<Row>,
}

/// `資料日期：2026/09/17 17:38:52`, anywhere in a line.
fn made_on(line: &str) -> Option<NaiveDate> {
    let (_, rest) = line.split_once("資料日期：")?;
    date(rest.split_whitespace().next()?)
}

/// A row as `pdftotext -raw` prints it, its words run together: the 帳務日, the
/// 摘要 (which can wrap), 支出, 存入, 餘額, then the 備註.
impl FromStr for Row {
    type Err = anyhow::Error;

    fn from_str(words: &str) -> Result<Self> {
        let words: Vec<&str> = words.split_whitespace().collect();
        let (book, rest) = words.split_first().context("an empty row")?;
        let book = date(book).with_context(|| format!("帳務日 in {words:?}"))?;
        let amounts = rest
            .windows(3)
            .position(|w| w.iter().all(|cell| pdf::money(cell).is_some()))
            .with_context(|| format!("no amounts in {words:?}"))?;
        let cell = |i: usize| pdf::money(rest[i]).expect("found above");
        Ok(Row {
            book,
            description: rest[..amounts].concat(),
            delta: cell(amounts + 1) - cell(amounts),
            balance: cell(amounts + 2),
            remark: rest[amounts + 3..].join(" "),
        })
    }
}

impl FromStr for Query {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        let (from, to) = text
            .lines()
            .find_map(|l| l.strip_prefix("查詢期間："))
            .and_then(|period| period.split_once('~'))
            .and_then(|(from, to)| Some((date(from.trim())?, date(to.trim())?)))
            .context("no 查詢期間")?;
        let made = text.lines().find_map(made_on).context("no 資料日期")?;

        // A row opens on its 交易日 and the time, each on a line of its own.
        let lines: Vec<&str> = text.split(['\n', '\u{c}']).map(str::trim).collect();
        let is_time = |s: &str| s.len() == 8 && s.chars().filter(|c| *c == ':').count() == 2;
        let opens: Vec<usize> = (0..lines.len().saturating_sub(1))
            .filter(|&i| date(lines[i]).is_some() && is_time(lines[i + 1]))
            .collect();
        let ends = opens.iter().skip(1).copied().chain([lines.len()]);
        let rows = opens
            .iter()
            .zip(ends)
            .map(|(&open, end)| lines[open + 2..end].join(" ").parse())
            .collect::<Result<Vec<Row>>>()?;
        Ok(Query { from, to, made, rows })
    }
}

impl Query {
    /// The account number it prints, without its dashes.
    fn account_in(text: &str) -> Option<String> {
        let number = text.lines().find_map(|l| l.strip_prefix("查詢帳號："))?;
        Some(number.chars().filter(char::is_ascii_digit).collect())
    }

    /// The query in the ledger's terms. Fails unless each row's balance follows
    /// from the one before. What it holds runs to the day before it was made.
    fn into_statement(self, account_no: &str, kind: &str) -> Result<BankStatement> {
        let first = self.rows.first().context("no rows, so no opening balance to read")?;
        let opening = first.balance - first.delta;
        let mut balance = opening;
        for row in &self.rows {
            balance += row.delta;
            ensure!(
                balance == row.balance,
                "the row on {} leaves {}, but the rows before it reach {balance}",
                row.book,
                row.balance
            );
        }
        ensure!(
            self.from <= first.book,
            "a row on {} is before the query's {}",
            first.book,
            self.from
        );
        let settled = self.made.pred_opt().context("a day before the query")?;
        let held: Vec<Row> =
            self.rows.into_iter().filter(|r| r.book <= settled && !r.delta.is_zero()).collect();
        let end = held.iter().map(|r| r.book).chain([self.to.min(settled)]).max();
        let end = end.context("a period has an end")?;
        ensure!(
            self.from <= end,
            "made on {}, so nothing from {} has settled yet",
            self.made,
            self.from
        );
        // Rows run by 交易日, so the last one kept may not be the last to book.
        let closing = opening + held.iter().map(|r| r.delta).sum::<Decimal>();
        let lines = held
            .into_iter()
            .map(|r| {
                let (withdrawal, deposit) = match r.delta.is_sign_negative() {
                    true => (-r.delta, Decimal::ZERO),
                    false => (Decimal::ZERO, r.delta),
                };
                StatementLine {
                    book_date: r.book,
                    purchased: None,
                    description: r.description,
                    withdrawal,
                    deposit,
                    balance: r.balance,
                    info: r.remark,
                    memo: String::new(),
                }
            })
            .collect();
        Ok(BankStatement {
            bank: Bank::Taishin,
            account_no: account_no.to_string(),
            account_kind: kind.to_string(),
            currency: Currency::TWD,
            period_end: Some(end),
            periods: vec![Period { start: self.from, end, opening, closing }],
            lines,
        })
    }
}

fn text_of(cell: &Data) -> Result<&str> {
    match cell {
        Data::String(s) => Ok(s.trim()),
        Data::Empty => Ok(""),
        other => bail!("expected text, found {other:?}"),
    }
}

fn number_of(cell: &Data) -> Result<Decimal> {
    match cell {
        Data::Float(f) => Decimal::try_from(*f).with_context(|| format!("amount {f}")),
        Data::Int(i) => Ok(Decimal::from(*i)),
        other => bail!("expected an amount, found {other:?}"),
    }
}

/// `20250918-20260917`, an xlsx's name: it prints no query of its own.
fn period_in_name(stem: &str) -> Result<(NaiveDate, NaiveDate)> {
    let day = |s: &str| NaiveDate::parse_from_str(s, "%Y%m%d").ok();
    stem.split_once('-')
        .and_then(|(from, to)| Some((day(from)?, day(to)?)))
        .with_context(|| format!("{stem} is not <from>-<to>, as yyyymmdd"))
}

/// The sheet: a title, headers, rows newest first, and a last row saying how
/// many there were and when.
fn sheet_rows(rows: &[&[Data]]) -> Result<(NaiveDate, Vec<Row>)> {
    let header = rows
        .iter()
        .position(|r| r.first().is_some_and(|c| text_of(c).is_ok_and(|t| t == "交易日")))
        .context("no 交易日 header")?;
    let (footer, table) = rows[header + 1..].split_last().context("no 資料日期 row")?;
    let footer = footer.first().map(text_of).context("no 資料日期 row")??;
    let made = made_on(footer).context("no 資料日期")?;
    let count = format!("共{}筆", table.len());
    ensure!(footer.starts_with(&count), "{footer:?}, but the sheet holds {} rows", table.len());
    let mut parsed = table
        .iter()
        .map(|row| {
            let [_, book, description, delta, balance, remark] = row else {
                bail!("a row of {} cells, not 6", row.len())
            };
            Ok(Row {
                book: date(text_of(book)?).context("帳務日")?,
                description: text_of(description)?.to_string(),
                delta: number_of(delta)?,
                balance: number_of(balance)?,
                remark: text_of(remark)?.to_string(),
            })
        })
        .collect::<Result<Vec<Row>>>()?;
    parsed.reverse();
    Ok((made, parsed))
}

fn read_pdf(path: &Path) -> Result<(Query, Option<String>)> {
    let text = pdf::text(path)?;
    Ok((text.parse()?, Query::account_in(&text)))
}

fn read_xlsx(path: &Path) -> Result<Query> {
    let stem = path.file_stem().and_then(|s| s.to_str()).context("no file name")?;
    let (from, to) = period_in_name(stem)?;
    let mut book: Xlsx<_> = calamine::open_workbook(path)?;
    let sheet = book.worksheet_range_at(0).context("no sheet")??;
    let rows: Vec<&[Data]> = sheet.rows().collect();
    let (made, rows) = sheet_rows(&rows)?;
    Ok(Query { from, to, made, rows })
}

/// One query, of the account its folder names.
pub fn load(path: &Path) -> Result<BankStatement> {
    let read = || -> Result<BankStatement> {
        let (kind, account_no) = folder_account(path)?;
        let query = match path.extension().and_then(|e| e.to_str()) {
            Some("xlsx") => read_xlsx(path)?,
            Some("pdf") => {
                let (query, printed) = read_pdf(path)?;
                ensure!(
                    printed.as_deref() == Some(account_no.as_str()),
                    "its 查詢帳號 is not the account its folder names"
                );
                query
            }
            other => bail!("not a PDF or xlsx: {other:?}"),
        };
        query.into_statement(&account_no, &kind)
    };
    read().with_context(|| format!("reading {}", path.display()))
}

/// Joins each account's queries. Queries that meet must carry the balance over;
/// where they don't meet, the days between are for 對帳進度 to show.
pub fn load_merged(paths: &[PathBuf]) -> Result<Vec<Merged>> {
    merged(paths.iter().map(|path| Ok((load(path)?, path.clone()))).collect::<Result<Vec<_>>>()?)
}

fn merged(statements: Vec<(BankStatement, PathBuf)>) -> Result<Vec<Merged>> {
    bank::join_periods(statements, |account, path, before, after| {
        ensure!(
            before.end < after.start,
            "{account}: {} covers {}..{}, but the query before ends {}",
            path.display(),
            after.start,
            after.end,
            before.end
        );
        ensure!(
            before.end.succ_opt() != Some(after.start) || before.closing == after.opening,
            "{account}: {} opens at {}, but the query before closed at {}",
            path.display(),
            after.opening,
            before.closing
        );
        Ok(())
    })
}

#[cfg(test)]
mod tests;
