//! Parser for 天天記帳 ("daily bookkeeping") exports.
//!
//! The app exports two files: 收支 (income/expense) and 轉帳 (transfers). Both
//! are needed — most money movement lives in the transfer file, and a balance
//! cannot be reconciled from 收支 alone.

use std::path::Path;

use anyhow::{Context, Result};
use chrono::NaiveDate;
use rust_decimal::Decimal;

use crate::currency::Currency;

#[derive(Debug)]
pub enum Contra {
    /// From a 收支 row: the other side is a spending or earning category.
    Category(String),
    /// From a 轉帳 row: the other side is another 天天記帳 account.
    Account(String),
}

/// One movement affecting the subject account, as the user recorded it.
#[derive(Debug)]
pub struct AppEvent {
    pub date: NaiveDate,
    /// Signed effect on the subject account.
    pub delta: Decimal,
    /// What `delta` is denominated in — the subject's own currency, not the
    /// other side's.
    pub currency: Currency,
    pub contra: Contra,
    pub memo: String,
    /// For a transfer, what the other account received or sent, and in which
    /// currency. Differs from `delta` whenever the two sides are not the same
    /// currency, which is the whole reason it is carried.
    pub far: Option<(Decimal, Currency)>,
    /// The app's UUID for the record this view came from, so a per-record
    /// correction applies whichever path the record reaches the ledger by.
    /// Empty for a 轉帳 row, whose two sides are one record with one id but two
    /// accounts — a correction there would have to name a side, and the
    /// categories a correction exists to fix are on 收支 rows only.
    pub id: String,
}

fn parse_date(s: &str) -> Result<NaiveDate> {
    NaiveDate::parse_from_str(s.trim(), "%Y%m%d")
        .with_context(|| format!("unparseable 天天記帳 date {:?}", s))
}

fn parse_amount(s: &str) -> Result<Decimal> {
    let t = s.trim().replace(',', "");
    if t.is_empty() {
        Ok(Decimal::ZERO)
    } else {
        t.parse::<Decimal>().with_context(|| format!("unparseable 天天記帳 amount {:?}", s))
    }
}

/// The currency in a 幣別 cell, defaulting to TWD when the column is absent or
/// blank — the app omits it on local-currency rows.
fn parse_currency(cell: Option<&str>) -> Result<Currency> {
    match cell.map(str::trim) {
        None | Some("") => Ok(Currency::TWD),
        Some(code) => code.parse().with_context(|| format!("unknown 天天記帳 currency {code:?}")),
    }
}

/// The subject account's view of every record touching it, oldest first.
///
/// This is a projection of `load_entries`, not a second parse. Reading the two
/// exports twice meant every column index was written down twice, and the two
/// copies had already drifted: one of them never read 幣別, so a record in a
/// foreign currency was booked as if it were TWD.
pub fn view(entries: &[Entry], subject: &str) -> Vec<AppEvent> {
    let mut out = Vec::new();
    for entry in entries {
        match entry {
            Entry::Flow { date, account, amount, currency, category, memo, id } => {
                if account != subject {
                    continue;
                }
                out.push(AppEvent {
                    date: *date,
                    delta: *amount,
                    currency: *currency,
                    contra: Contra::Category(category.clone()),
                    memo: memo.clone(),
                    far: None,
                    id: id.clone(),
                });
            }
            // A transfer between two of the subject's own accounts is recorded
            // once but seen twice, from each side.
            Entry::Transfer { date, from, out: sent, out_currency, to, inn, in_currency, memo } => {
                if from == subject {
                    out.push(AppEvent {
                        date: *date,
                        delta: -sent,
                        currency: *out_currency,
                        contra: Contra::Account(to.clone()),
                        memo: memo.clone(),
                        far: Some((*inn, *in_currency)),
                        id: String::new(),
                    });
                }
                if to == subject {
                    out.push(AppEvent {
                        date: *date,
                        delta: *inn,
                        currency: *in_currency,
                        contra: Contra::Account(from.clone()),
                        memo: memo.clone(),
                        far: Some((*sent, *out_currency)),
                        id: String::new(),
                    });
                }
            }
            // Not a movement a statement line could explain.
            Entry::Opening { .. } => {}
        }
    }
    out
}

/// A record as written, with both sides and their currencies intact.
///
/// `AppEvent` above describes one account's view of a movement, which is what
/// matching against a bank statement needs. This is the whole record, for
/// accounts that have no statement and must be taken from the app as written.
#[derive(Debug)]
pub enum Entry {
    /// 收支: a category on the other side.
    Flow {
        date: NaiveDate,
        account: String,
        /// Signed: positive is 收, negative is 支.
        amount: Decimal,
        currency: Currency,
        category: String,
        memo: String,
        /// The app's own UUID for this record, so a single record can be
        /// corrected by name in mapping.toml when its category is too coarse.
        id: String,
    },
    /// 轉帳: another account on the other side. The two amounts differ when the
    /// currencies do.
    Transfer {
        date: NaiveDate,
        from: String,
        out: Decimal,
        out_currency: Currency,
        to: String,
        inn: Decimal,
        in_currency: Currency,
        memo: String,
    },
    /// What an account already held before its records begin; the other side
    /// is the opening-balance equity. Signed: negative for a debt.
    Opening { date: NaiveDate, account: String, amount: Decimal, currency: Currency },
}

impl Entry {
    pub fn date(&self) -> NaiveDate {
        match self {
            Entry::Flow { date, .. }
            | Entry::Transfer { date, .. }
            | Entry::Opening { date, .. } => *date,
        }
    }

    /// Accounts this record touches.
    pub fn accounts(&self) -> Vec<&str> {
        match self {
            Entry::Flow { account, .. } | Entry::Opening { account, .. } => vec![account.as_str()],
            Entry::Transfer { from, to, .. } => vec![from.as_str(), to.as_str()],
        }
    }
}

/// Every record in both exports, oldest first.
///
/// The only place either file's layout is written down. Both are read
/// positionally rather than by header: 幣別 appears twice in the 轉帳 file, and
/// any header-keyed parser silently collapses the two currency columns into
/// one.
///
/// 收支: 0日期 1類別 2大類別 3金額 4幣別 5成員 6帳戶 7標籤 8備註 9收支區分
/// 10上次更新 11UUID 轉帳: 0日期 1從帳戶 2轉出金額 3幣別 4到帳戶 5轉入金額
/// 6幣別 7標籤 8備註 9上次更新 10UUID
pub fn load_entries(
    income_expense: impl AsRef<Path>,
    transfers: impl AsRef<Path>,
) -> Result<Vec<Entry>> {
    let income_expense = income_expense.as_ref();
    let transfers = transfers.as_ref();
    let mut out = Vec::new();

    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(true)
        .from_path(income_expense)
        .with_context(|| format!("opening {}", income_expense.display()))?;
    for result in rdr.records() {
        let rec = result?;
        let account = rec.get(6).unwrap_or("").trim();
        if account.is_empty() {
            continue;
        }
        let amount = parse_amount(rec.get(3).unwrap_or(""))?;
        let is_income = rec.get(9).unwrap_or("").trim() == "收";
        out.push(Entry::Flow {
            date: parse_date(rec.get(0).unwrap_or(""))?,
            account: account.to_string(),
            amount: if is_income { amount } else { -amount },
            currency: parse_currency(rec.get(4))?,
            category: rec.get(1).unwrap_or("").trim().to_string(),
            memo: rec.get(8).unwrap_or("").trim().to_string(),
            id: rec.get(11).unwrap_or("").trim().to_string(),
        });
    }

    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(true)
        .from_path(transfers)
        .with_context(|| format!("opening {}", transfers.display()))?;
    for result in rdr.records() {
        let rec = result?;
        let date = match rec.get(0).map(str::trim) {
            Some(d) if d.len() == 8 && d.chars().all(|c| c.is_ascii_digit()) => parse_date(d)?,
            _ => continue,
        };
        out.push(Entry::Transfer {
            date,
            from: rec.get(1).unwrap_or("").trim().to_string(),
            out: parse_amount(rec.get(2).unwrap_or(""))?,
            out_currency: parse_currency(rec.get(3))?,
            to: rec.get(4).unwrap_or("").trim().to_string(),
            inn: parse_amount(rec.get(5).unwrap_or(""))?,
            in_currency: parse_currency(rec.get(6))?,
            memo: rec.get(8).unwrap_or("").trim().to_string(),
        });
    }

    out.sort_by_key(Entry::date);
    Ok(out)
}
