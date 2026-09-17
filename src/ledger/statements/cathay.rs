//! 國泰世華 (Cathay United Bank) account statements — 活存 and 投資.
//!
//! Only this bank's format; other institutions get their own sibling module.
//! Distinct from `crate::cathay`, which reads Cathay's *brokerage trade* export
//! for the portfolio calculator.
//!
//! One row per cash movement, with a running 餘額 column. That running balance
//! is what lets the ledger assert a figure the transactions must agree with.

use std::path::Path;

use anyhow::{Context, Result};
use chrono::NaiveDate;
use rust_decimal::Decimal;

use crate::currency::Currency;

#[derive(Debug)]
pub struct StatementLine {
    pub book_date: NaiveDate,
    pub description: String,
    pub withdrawal: Decimal,
    pub deposit: Decimal,
    pub balance: Decimal,
    pub info: String,
    pub memo: String,
}

impl StatementLine {
    /// Signed effect on the account. `withdrawal` can be negative on 錯誤更正
    /// (error-correction) rows, which reverse an earlier debit.
    pub fn delta(&self) -> Decimal { self.deposit - self.withdrawal }
}

#[derive(Debug)]
pub struct BankStatement {
    pub account_no: String,
    pub account_kind: String,
    pub currency: Currency,
    /// End of the period the export covers, from the `(自 … 至 …)` header.
    pub period_end: Option<NaiveDate>,
    /// Oldest first.
    pub lines: Vec<StatementLine>,
}

impl BankStatement {
    /// Balance before the first line, reconstructed from the oldest row.
    pub fn opening_balance(&self) -> Decimal {
        self.lines.first().map(|l| l.balance - l.delta()).unwrap_or_default()
    }

    pub fn closing_balance(&self) -> Decimal {
        self.lines.last().map(|l| l.balance).unwrap_or_default()
    }

    /// Debits the bank itself undid, as `(debit, reversal)` line indices.
    ///
    /// A 錯誤更正 row carries a negative withdrawal that cancels an earlier
    /// debit to the same counterparty on the same book date. Neither row is a
    /// movement anyone recorded, so each pair nets to nothing rather than
    /// landing in both uncategorised buckets. A reversal with no such debit is
    /// left unpaired and falls through like any other line.
    pub fn reversals(&self) -> Vec<(usize, usize)> {
        let mut used = vec![false; self.lines.len()];
        let mut pairs = Vec::new();
        for (ri, reversal) in self.lines.iter().enumerate() {
            if !reversal.withdrawal.is_sign_negative() {
                continue;
            }
            let debit = (0..ri).rev().find(|&di| {
                let d = &self.lines[di];
                !used[di]
                    && d.book_date == reversal.book_date
                    && d.info == reversal.info
                    && d.withdrawal == -reversal.withdrawal
            });
            if let Some(di) = debit {
                used[di] = true;
                used[ri] = true;
                pairs.push((di, ri));
            }
        }
        pairs
    }

    /// Date to assert the closing balance on. Beancount asserts at the start of
    /// the day, so this must fall after the final transaction.
    pub fn assert_date(&self) -> NaiveDate {
        let after_last = self
            .lines
            .last()
            .map(|l| l.book_date.succ_opt().unwrap_or(l.book_date))
            .unwrap_or_default();
        match self.period_end {
            Some(end) if end >= after_last => end,
            _ => after_last,
        }
    }
}

/// `−` (U+2212) is the export's placeholder for an absent value. A real
/// negative uses an ASCII hyphen, so only the bare placeholder maps to zero.
fn parse_amount(s: &str) -> Result<Decimal> {
    let t = s.trim();
    if t.is_empty() || t == "−" || t == "-" {
        Ok(Decimal::ZERO)
    } else {
        t.replace(',', "").parse::<Decimal>().with_context(|| format!("unparseable amount {:?}", t))
    }
}

fn parse_slash_date(s: &str) -> Result<NaiveDate> {
    let first = s.trim().lines().next().unwrap_or("").trim();
    NaiveDate::parse_from_str(first, "%Y/%m/%d")
        .with_context(|| format!("unparseable date {:?}", first))
}

fn clean(s: &str) -> String {
    let t = s.replace(['\n', '\r'], " ");
    let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    if t == "−" {
        String::new()
    } else {
        t
    }
}

/// Does 交易資訊 name this account?
///
/// Outgoing rows carry the counterparty in full (`(013)0000123456789012`) but
/// incoming rows mask the middle (`(013)0000123***789012`), so a plain
/// substring test only ever sees one side of an internal transfer. Compare
/// digit runs positionally instead, treating `*` as a wildcard, with leading
/// zeros stripped from both sides since the export zero-pads inconsistently.
pub fn info_names_account(info: &str, account_no: &str) -> bool {
    let want = account_no.trim_start_matches('0');
    if want.is_empty() {
        return false;
    }
    let mut token = String::new();
    for ch in info.chars().chain(std::iter::once(' ')) {
        if ch.is_ascii_digit() || ch == '*' {
            token.push(ch);
            continue;
        }
        if !token.is_empty() {
            let candidate = token.trim_start_matches('0');
            if candidate.len() == want.len()
                && candidate.chars().zip(want.chars()).all(|(c, w)| c == '*' || c == w)
            {
                return true;
            }
            token.clear();
        }
    }
    false
}

pub fn load(file_path: impl AsRef<Path>) -> Result<BankStatement> {
    let file_path = file_path.as_ref();
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .from_path(file_path)
        .with_context(|| format!("opening {}", file_path.display()))?;

    let mut account_no = String::new();
    let mut account_kind = String::new();
    let mut currency = Currency::TWD;
    let mut lines: Vec<StatementLine> = Vec::new();
    let mut period_end: Option<NaiveDate> = None;
    let mut past_header = false;

    for result in rdr.records() {
        let rec = result?;
        let f0 = rec.get(0).unwrap_or("").trim();

        if !past_header {
            if let Some(range) = rec.iter().find(|f| f.contains('至')) {
                if let Some((_, tail)) = range.split_once('至') {
                    let end = tail.trim_matches(|c: char| !c.is_ascii_digit() && c != '/');
                    period_end = parse_slash_date(end).ok();
                }
            }
            if f0 == "交易日期" {
                past_header = true;
            } else if account_no.is_empty() && f0.contains(' ') {
                // e.g. "123456789012 活存"
                let mut parts = f0.split_whitespace();
                account_no = parts.next().unwrap_or("").to_string();
                account_kind = parts.next().unwrap_or("").to_string();
            } else if let Some(rest) = f0.strip_prefix("幣別：") {
                currency = rest
                    .trim()
                    .parse()
                    .with_context(|| format!("unknown statement currency {:?}", rest.trim()))?;
            }
            continue;
        }

        // Data rows start with a date; the trailer rows (提出/存入 totals) do not.
        if rec.len() < 6 || parse_slash_date(f0).is_err() {
            continue;
        }

        lines.push(StatementLine {
            book_date: parse_slash_date(rec.get(1).unwrap_or(f0))?,
            description: clean(rec.get(2).unwrap_or("")),
            withdrawal: parse_amount(rec.get(3).unwrap_or(""))?,
            deposit: parse_amount(rec.get(4).unwrap_or(""))?,
            balance: parse_amount(rec.get(5).unwrap_or(""))?,
            info: clean(rec.get(6).unwrap_or("")),
            memo: clean(rec.get(7).unwrap_or("")),
        });
    }

    if lines.is_empty() {
        anyhow::bail!("no statement rows found in {}", file_path.display());
    }

    // The export is newest-first.
    lines.reverse();

    Ok(BankStatement { account_no, account_kind, currency, period_end, lines })
}

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;

    use super::*;

    fn line(day: u32, description: &str, withdrawal: Decimal, info: &str) -> StatementLine {
        StatementLine {
            book_date: NaiveDate::from_ymd_opt(2026, 6, day).expect("valid date"),
            description: description.to_string(),
            withdrawal,
            deposit: Decimal::ZERO,
            balance: Decimal::ZERO,
            info: info.to_string(),
            memo: String::new(),
        }
    }

    fn statement(lines: Vec<StatementLine>) -> BankStatement {
        BankStatement {
            account_no: "123456789012".to_string(),
            account_kind: "活存".to_string(),
            currency: Currency::TWD,
            period_end: None,
            lines,
        }
    }

    /// A 錯誤更正 cancels the debit to the same counterparty that day, and only
    /// that one: a same-sized debit elsewhere, or on another day, is a real
    /// movement and must still reach the matcher.
    #[test]
    fn a_reversal_pairs_with_the_debit_it_undoes() {
        let s = statement(vec![
            line(28, "電子轉出", dec!(500), "(822)0000000000000001"),
            line(29, "電子轉出", dec!(500), "(807)0000000000000002"),
            line(29, "電子轉出", dec!(500), "(822)0000000000000001"),
            line(29, "錯誤更正", dec!(-500), "(822)0000000000000001"),
        ]);

        assert_eq!(s.reversals(), vec![(2, 3)]);
        assert_eq!(s.lines[2].delta() + s.lines[3].delta(), Decimal::ZERO);
    }

    /// With nothing to cancel, the reversal is left for the ordinary fallback
    /// rather than netted against an unrelated line.
    #[test]
    fn an_unmatched_reversal_stays_unpaired() {
        let s = statement(vec![
            line(29, "電子轉出", dec!(300), "(822)0000000000000001"),
            line(29, "錯誤更正", dec!(-500), "(822)0000000000000001"),
        ]);

        assert!(s.reversals().is_empty());
    }
}
