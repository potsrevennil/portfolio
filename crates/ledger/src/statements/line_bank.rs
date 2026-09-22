//! LINE Bank (連線商業銀行) monthly statements, read from the PDF's text as
//! `pdftotext -raw` lays it out.
//!
//! The PDF also carries a decorative layer of sample rows dated 2019; real rows
//! are dot-dated (`2026.01.06`), the sample ones slash-dated, so they are
//! dropped by that. A row's 備註 may wrap onto the lines after it. Only the
//! first line of a row prints a balance, and the account number is masked, so
//! it is taken from the statement's folder (`raw/line-bank/活存-<account>/`).

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Command,
    str::FromStr,
};

use anyhow::{bail, ensure, Context, Result};
use chrono::NaiveDate;
use ledger_types::currency::Currency;
use rust_decimal::Decimal;

use super::bank::{Bank, BankStatement, Merged, Period, StatementLine};

/// The 交易說明 of a transfer the bank called back.
pub const CANCEL: &str = "取消轉帳";
/// What the bank puts before the cancelled transfer's own 備註.
pub const CANCEL_REMARK: &str = "取消.";

/// Does a 備註 name `account_no`? LINE Bank prints only an account's tail:
/// masked in front (`***********54321`, the other bank's 16-digit form) or as
/// the last four digits after the bank's short name (`範例銀行4321`).
pub fn remark_names_account(remark: &str, account_no: &str) -> bool {
    remark.split(|c: char| !(c.is_ascii_digit() || c == '*')).any(|run| {
        let digits = run.trim_start_matches('*');
        digits.len() >= 4
            && !digits.contains('*')
            && (account_no.ends_with(digits)
                || digits.trim_start_matches('0') == account_no.trim_start_matches('0'))
    })
}

/// Does the statement's masked number (`*******00042`) fit `account_no`?
fn mask_fits(mask: &str, account_no: &str) -> bool {
    mask.len() == account_no.len()
        && mask.chars().zip(account_no.chars()).all(|(m, a)| m == '*' || m == a)
}

/// One currency's part of a statement.
#[derive(Debug, PartialEq)]
pub struct Section {
    pub currency: Currency,
    pub opening: Decimal,
    pub closing: Decimal,
    pub lines: Vec<StatementLine>,
}

/// One statement as the bank laid it out.
#[derive(Debug, PartialEq)]
pub struct Document {
    pub start: NaiveDate,
    pub end: NaiveDate,
    /// The TWD 主帳戶's masked number.
    pub account_mask: String,
    /// TWD first.
    pub sections: Vec<Section>,
}

/// A row before its balances are chained.
struct Row {
    date: NaiveDate,
    description: String,
    amount: Decimal,
    balance: Option<Decimal>,
    remark: String,
}

/// `$1,234`, `-$1,234`.
fn money(token: &str) -> Option<Decimal> {
    let (negative, rest) = match token.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, token),
    };
    let value: Decimal = rest.strip_prefix('$')?.replace(',', "").parse().ok()?;
    Some(if negative { -value } else { value })
}

/// The first token of a real row, or of a sample one.
fn row_date(line: &str, format: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(line.split_whitespace().next()?, format).ok()
}

impl FromStr for Row {
    type Err = anyhow::Error;

    fn from_str(line: &str) -> Result<Self> {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let date = row_date(line, "%Y.%m.%d").context("no date")?;
        let at = tokens.iter().position(|t| money(t).is_some()).context("no amount")?;
        ensure!(at > 1, "no 交易說明");
        let balance = tokens.get(at + 1).and_then(|t| money(t));
        let remark_from = at + 1 + usize::from(balance.is_some());
        Ok(Row {
            date,
            description: tokens[1..at].join(" "),
            amount: money(tokens[at]).expect("found by money"),
            balance,
            remark: tokens.get(remark_from..).unwrap_or_default().join(" "),
        })
    }
}

/// Chains the rows' balances from the first one's (or back from the closing,
/// should it print none), and checks them against every balance printed and
/// the closing.
fn chain(rows: Vec<Row>, closing: Decimal) -> Result<(Decimal, Vec<StatementLine>)> {
    let opening = match rows.first().and_then(|first| Some((first.balance?, first.amount))) {
        Some((balance, amount)) => balance - amount,
        None => closing - rows.iter().map(|r| r.amount).sum::<Decimal>(),
    };
    let mut balance = opening;
    let mut lines = Vec::with_capacity(rows.len());
    for row in rows {
        balance += row.amount;
        if let Some(printed) = row.balance {
            ensure!(
                printed == balance,
                "{} {} prints a balance of {printed}, but the rows up to it add up to {balance}",
                row.date,
                row.description
            );
        }
        let (withdrawal, deposit) = match row.amount.is_sign_negative() {
            true => (-row.amount, Decimal::ZERO),
            false => (Decimal::ZERO, row.amount),
        };
        lines.push(StatementLine {
            book_date: row.date,
            description: row.description,
            withdrawal,
            deposit,
            balance,
            info: row.remark,
            memo: String::new(),
        });
    }
    ensure!(
        balance == closing,
        "the rows add up to {balance}, but the statement closes at {closing}"
    );
    Ok((opening, lines))
}

#[derive(Clone, Copy, PartialEq)]
enum Part {
    Header,
    Twd,
    Foreign,
    /// The debit-card list repeats rows already in the account's own.
    Card,
}

impl FromStr for Document {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        let mut period: Option<(NaiveDate, NaiveDate)> = None;
        let mut part = Part::Header;
        let mut twd_total: Option<Decimal> = None;
        let mut account: Option<(String, Decimal)> = None;
        let mut rows: Vec<Row> = Vec::new();
        let mut foreign: Vec<Section> = Vec::new();
        // Whether the next line may continue the last row's 備註.
        let mut wrapping = false;
        let mut previous: &str = "";

        // The decorative layer's unmapped glyphs come out as control
        // characters, and a line of only those separates a page's rows from
        // its footer.
        let clean: Vec<String> =
            text.lines().map(|l| l.chars().filter(|c| !c.is_control()).collect()).collect();
        let real = clean.iter().filter(|l| row_date(l, "%Y/%m/%d").is_none());
        for line in real {
            let t = line.trim();
            let last = std::mem::replace(&mut previous, t);
            if let Some((_, rest)) = t.split_once("對帳單期間") {
                let range: String =
                    rest.chars().filter(|c| c.is_ascii_digit() || *c == '-').collect();
                let (start, end) = range.split_once('-').context("statement period")?;
                let day = |s: &str| NaiveDate::parse_from_str(s, "%Y%m%d");
                period = Some((day(start)?, day(end)?));
                continue;
            }
            let marker = [
                ("台幣存款交易明細", Part::Twd),
                ("外幣存款交易明細", Part::Foreign),
                ("簽帳金融卡", Part::Card),
            ]
            .into_iter()
            .find(|(m, _)| t.contains(m));
            if let Some((_, next)) = marker {
                part = next;
                wrapping = false;
                continue;
            }
            if last.ends_with("台幣存款總餘額") {
                twd_total = money(t);
            }

            let tokens: Vec<&str> = t.split_whitespace().collect();
            let is_row = row_date(t, "%Y.%m.%d").is_some();
            match (part, tokens.as_slice()) {
                (Part::Twd, _) if is_row => {
                    rows.push(t.parse().with_context(|| format!("row {t:?}"))?);
                    wrapping = true;
                }
                (Part::Twd, ["主帳戶", mask, closing]) if mask.contains('*') => {
                    if let Some(closing) = money(closing) {
                        account = Some((mask.to_string(), closing));
                    }
                }
                (Part::Twd, _) if wrapping && !t.is_empty() && !t.contains("交易說明") => {
                    rows.last_mut().expect("wrapping follows a row").remark.push_str(t);
                }
                (Part::Twd, _) => wrapping = false,
                (Part::Foreign, _) if is_row => {
                    bail!("foreign-currency rows are not read yet: {t:?}")
                }
                (Part::Foreign, [_, mask, closing, code]) if mask.contains('*') => {
                    let currency: Currency =
                        code.parse().with_context(|| format!("currency in {t:?}"))?;
                    let closing: Decimal =
                        closing.replace(',', "").parse().with_context(|| format!("{t:?}"))?;
                    ensure!(
                        foreign.iter().all(|s| s.currency != currency),
                        "two {currency} accounts; only one per currency is read"
                    );
                    foreign.push(Section { currency, opening: closing, closing, lines: vec![] });
                }
                _ => {}
            }
        }

        let (start, end) = period.context("no 對帳單期間")?;
        let (account_mask, closing) = account.context("no 主帳戶 balance")?;
        if let Some(total) = twd_total {
            ensure!(
                total == closing,
                "台幣存款總餘額 {total} is not the 主帳戶's {closing}; sub-accounts are not read \
                 yet"
            );
        }
        let (opening, lines) = chain(rows, closing)?;
        let mut sections = vec![Section { currency: Currency::TWD, opening, closing, lines }];
        sections.extend(foreign);
        Ok(Document { start, end, account_mask, sections })
    }
}

fn pdf_text(path: &Path) -> Result<String> {
    let out = Command::new("pdftotext")
        .args(["-raw", "-enc", "UTF-8"])
        .arg(path)
        .arg("-")
        .output()
        .context("running pdftotext (poppler); it ships in the nix devShell")?;
    ensure!(
        out.status.success(),
        "pdftotext {}: {}",
        path.display(),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    String::from_utf8(out.stdout).context("pdftotext output is not UTF-8")
}

/// `<kind>-<account>`, the statement's folder.
fn folder_account(path: &Path) -> Result<(String, String)> {
    let folder = path.parent().and_then(Path::file_name).and_then(|f| f.to_str());
    let (kind, account) = folder
        .and_then(|f| f.rsplit_once('-'))
        .with_context(|| format!("{} is not in a <kind>-<account> folder", path.display()))?;
    Ok((kind.to_string(), account.to_string()))
}

/// One statement per currency it covers, including a currency with no rows.
pub fn load(path: impl AsRef<Path>) -> Result<Vec<BankStatement>> {
    let path = path.as_ref();
    let doc: Document =
        pdf_text(path)?.parse().with_context(|| format!("reading {}", path.display()))?;
    let (kind, account_no) = folder_account(path)?;
    ensure!(
        mask_fits(&doc.account_mask, &account_no),
        "{} is account {}, not the folder's {account_no}",
        path.display(),
        doc.account_mask
    );
    Ok(doc
        .sections
        .into_iter()
        .map(|s| BankStatement {
            bank: Bank::LineBank,
            account_no: account_no.clone(),
            account_kind: match s.currency {
                Currency::TWD => kind.clone(),
                other => format!("{kind} {other}"),
            },
            currency: s.currency,
            period_end: Some(doc.end),
            periods: vec![Period {
                start: doc.start,
                end: doc.end,
                opening: s.opening,
                closing: s.closing,
            }],
            lines: s.lines,
        })
        .collect())
}

/// Joins each account's monthly statements per currency. Each must start the
/// day after the one before ends, on the balance it closed on.
pub fn load_merged(paths: &[PathBuf]) -> Result<Vec<Merged>> {
    let mut loaded = Vec::new();
    for path in paths {
        loaded.extend(load(path)?.into_iter().map(|s| (s, path.clone())));
    }
    merge(loaded)
}

fn merge(statements: Vec<(BankStatement, PathBuf)>) -> Result<Vec<Merged>> {
    let mut groups: BTreeMap<(String, Currency), Vec<(BankStatement, PathBuf)>> = BTreeMap::new();
    for (s, path) in statements {
        groups.entry((s.account_no.clone(), s.currency)).or_default().push((s, path));
    }
    let mut merged = Vec::new();
    for ((account_no, currency), mut parts) in groups {
        parts.sort_by_key(|(s, _)| s.periods[0].start);
        let mut parts = parts.into_iter();
        let (mut statement, first) = parts.next().expect("a group has a member");
        let mut spans = vec![statement.lines.len()];
        let mut paths = vec![first];
        for (next, path) in parts {
            let (before, after) = (statement.periods.last().expect("issued"), &next.periods[0]);
            ensure!(
                before.end.succ_opt() == Some(after.start),
                "{account_no} {currency}: {} covers {}..{}, but the statement before ends {}; one \
                 is missing or repeated",
                path.display(),
                after.start,
                after.end,
                before.end
            );
            ensure!(
                after.opening == before.closing,
                "{account_no} {currency}: {} opens at {}, but the month before closed at {}",
                path.display(),
                after.opening,
                before.closing
            );
            spans.push(next.lines.len());
            paths.push(path);
            statement.period_end = next.period_end;
            statement.periods.extend(next.periods);
            statement.lines.extend(next.lines);
        }
        merged.push(Merged { statement, paths, spans });
    }
    Ok(merged)
}

#[cfg(test)]
mod tests;
