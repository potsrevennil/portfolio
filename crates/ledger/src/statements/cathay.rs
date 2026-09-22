//! 國泰世華 (Cathay United Bank) account statements — 活存, 投資 and 外幣.
//!
//! Only this bank's format; other institutions get their own sibling module.
//! Distinct from `portfolio::cathay`, which reads Cathay's *brokerage trade*
//! export for the portfolio calculator.
//!
//! One row per cash movement, with a running 餘額 column. That running balance
//! is what lets the ledger assert a figure the transactions must agree with.

use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use chrono::NaiveDate;
use ledger_types::{
    assertion::{AssertionSource, BalanceAssertion},
    currency::Currency,
};
use rust_decimal::Decimal;

/// Namespaces this bank's `external_ref`s: every importer shares one dedup
/// scope (`transactions.source = 'import'`).
pub const REF_PREFIX: &str = "cathay-bank:";

/// The `external_ref` of an account's opening, so a second one is a duplicate.
pub fn opening_ref(account: &str, currency: Currency) -> String {
    format!("{REF_PREFIX}opening:{account}:{currency}")
}

#[derive(Debug, PartialEq)]
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

    /// Dedup key per line, shared by the freeze bake and importers.
    ///
    /// Not the line index: statements get re-split per year, which shifts
    /// indices. Same-day out-and-back sequences (−X, +X, −X) repeat the running
    /// balance, so repeats of an identical key get `:2`, `:3`. The suffix is
    /// counted from the first line of the day this statement holds, so a
    /// statement starting mid-day numbers them differently; the importer's
    /// balance-chain check is what catches that.
    pub fn dedup_refs(&self) -> Vec<String> {
        let mut seen: HashMap<String, usize> = HashMap::new();
        self.lines
            .iter()
            .map(|l| {
                let base = format!(
                    "{REF_PREFIX}{}:{}:{}:{}",
                    self.account_no,
                    l.book_date,
                    l.delta(),
                    l.balance
                );
                let n = seen.entry(base.clone()).or_default();
                *n += 1;
                if *n == 1 {
                    base
                } else {
                    format!("{base}:{n}")
                }
            })
            .collect()
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

    /// Drops lines before `date` (their balance becomes the opening); returns
    /// how many.
    pub fn trim_before(&mut self, date: NaiveDate) -> usize {
        let keep = self.lines.partition_point(|l| l.book_date < date);
        self.lines.drain(..keep).count()
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

    /// The last day this download holds in full. A TWD export may have been
    /// made partway through the end of its stated range, so the day before.
    /// A 外幣 export states no range; its last line's balance is taken as
    /// current (the user's call until downloads carry a date), so a second
    /// download made later that same day is refused as contradicting it.
    pub fn settled_through(&self) -> NaiveDate {
        let last = self.lines.last().expect("load rejects empty statements").book_date;
        match self.period_end {
            Some(end) => end.pred_opt().expect("a day before a range end"),
            None => last,
        }
    }

    /// The statement's figures for `account`, over the lines dated after
    /// `after` (the ones before are in the account's opening), closing on
    /// [`Self::settled_through`]. `None` when no line is. Asserting a day the
    /// download may not hold in full would refuse the next one that does.
    pub fn assertion(&self, account: &str, after: NaiveDate) -> Option<BalanceAssertion> {
        let from = self.lines.partition_point(|l| l.book_date <= after);
        let first = self.lines.get(from)?;
        let before = |i: usize| self.lines[i].balance - self.lines[i].delta();
        let end = self.settled_through();
        // Read off the line after `end` where there is one, so its figures
        // stay checked too: a statement that stops adding up there still fails.
        let closing = match self.lines.partition_point(|l| l.book_date <= end) {
            n if n < self.lines.len() => before(n),
            n => self.lines[n - 1].balance,
        };
        let opening = before(from);
        let (period_start, opening, period_end, closing) = if end >= first.book_date {
            (Some(first.book_date), Some(opening), end, closing)
        } else {
            // No tracked day is settled: vouch only for the balance the first
            // one started from.
            (None, None, first.book_date.pred_opt().expect("a day before a line"), opening)
        };
        Some(BalanceAssertion {
            source: AssertionSource::Statement,
            account: account.to_string(),
            currency: self.currency,
            period_start,
            opening,
            period_end,
            closing,
        })
    }
}

/// `−` (U+2212) is the export's placeholder for an absent value. A real
/// negative uses an ASCII hyphen, so only the bare placeholder maps to zero.
/// 外幣 amounts carry a currency prefix (`USD 12.50`).
fn parse_amount(s: &str, currency: Currency) -> Result<Decimal> {
    let t = s.trim();
    let t = match t.split_once(' ') {
        Some((code, rest)) if code.len() == 3 && code.chars().all(|c| c.is_ascii_uppercase()) => {
            let prefixed: Currency =
                code.parse().with_context(|| format!("unknown amount currency {code:?}"))?;
            anyhow::ensure!(
                prefixed == currency,
                "amount {t:?} is not in the statement currency {currency}"
            );
            rest.trim()
        }
        _ => t,
    };
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

/// Columns by header name: the 外幣 export has a different layout.
struct Columns {
    book_date: usize,
    description: Option<usize>,
    withdrawal: usize,
    deposit: usize,
    balance: usize,
    info: Option<usize>,
    memo: Option<usize>,
}

impl TryFrom<&csv::StringRecord> for Columns {
    type Error = anyhow::Error;

    fn try_from(rec: &csv::StringRecord) -> Result<Self> {
        let find = |name: &str| rec.iter().position(|f| f.trim() == name);
        let need = |name: &str| find(name).with_context(|| format!("no {name} column"));
        Ok(Columns {
            book_date: need("帳務日期")?,
            description: find("說明"),
            withdrawal: need("提出")?,
            deposit: need("存入")?,
            balance: need("餘額")?,
            info: find("交易資訊"),
            memo: find("備註"),
        })
    }
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
    let mut columns: Option<Columns> = None;

    for result in rdr.records() {
        let rec = result?;
        let f0 = rec.get(0).unwrap_or("").trim();

        let Some(cols) = &columns else {
            if let Some((_, tail)) = rec.iter().find_map(|f| f.split_once('至')) {
                let end = tail.trim_matches(|c: char| !c.is_ascii_digit() && c != '/');
                period_end = parse_slash_date(end).ok();
            }
            if f0 == "交易日期" {
                columns = Some(
                    Columns::try_from(&rec)
                        .with_context(|| format!("header of {}", file_path.display()))?,
                );
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
        };

        // Data rows start with a date; the trailer rows (提出/存入 totals) do not.
        if rec.len() <= cols.balance || parse_slash_date(f0).is_err() {
            continue;
        }
        let text = |col: Option<usize>| clean(col.and_then(|c| rec.get(c)).unwrap_or(""));
        let amount = |col: usize| parse_amount(rec.get(col).unwrap_or(""), currency);

        lines.push(StatementLine {
            book_date: parse_slash_date(rec.get(cols.book_date).unwrap_or(f0))?,
            description: text(cols.description),
            withdrawal: amount(cols.withdrawal)?,
            deposit: amount(cols.deposit)?,
            balance: amount(cols.balance)?,
            info: text(cols.info),
            memo: text(cols.memo),
        });
    }

    if lines.is_empty() {
        anyhow::bail!("no statement rows found in {}", file_path.display());
    }

    // The export is newest-first.
    lines.reverse();

    Ok(BankStatement { account_no, account_kind, currency, period_end, lines })
}

pub struct Merged {
    pub statement: BankStatement,
    pub paths: Vec<PathBuf>,
    /// How many of `statement.lines` each of `paths` contributed, in order.
    pub spans: Vec<usize>,
}

/// Joins the exports of each account and currency into one statement. They
/// must follow on with no balance gap, or a missing download would hide as an
/// unexplained jump.
///
/// `raw/` keeps every download, so one can re-cover days another already
/// holds: a partial download and the full one that replaced it. Where one
/// holds exactly the other's lines from the day the later one starts, the
/// longer is kept; any other overlap is an error.
pub fn load_merged(paths: &[PathBuf]) -> Result<Vec<Merged>> {
    let mut groups: BTreeMap<(String, Currency), Vec<(BankStatement, PathBuf)>> = BTreeMap::new();
    for path in paths {
        let s = load(path)?;
        groups.entry((s.account_no.clone(), s.currency)).or_default().push((s, path.clone()));
    }

    let mut merged = Vec::new();
    for ((account_no, currency), mut parts) in groups {
        // Adjacent exports can share a boundary book date, so break ties on
        // where each ends.
        parts.sort_by_key(|(s, _)| {
            (
                s.lines.first().map(|l| l.book_date),
                s.lines.last().map(|l| l.book_date),
                s.period_end,
            )
        });
        let mut parts = parts.into_iter();
        let (mut statement, first_path) = parts.next().expect("a group has a member");
        let mut paths = vec![first_path];
        let mut spans = vec![statement.lines.len()];
        for (next, path) in parts {
            let last = statement.lines.last().expect("load rejects empty statements");
            let first = next.lines.first().expect("load rejects empty statements");
            let from = statement.lines.partition_point(|l| l.book_date < first.book_date);
            let held = &statement.lines[from..];
            let shared = held.len().min(next.lines.len());
            if !held.is_empty() && held[..shared] == next.lines[..shared] {
                if next.lines.len() > held.len() {
                    // The later download re-covers those lines, so drop them.
                    let mut dropped = held.len();
                    for span in spans.iter_mut().rev() {
                        let d = dropped.min(*span);
                        *span -= d;
                        dropped -= d;
                    }
                    statement.lines.truncate(from);
                    spans.push(next.lines.len());
                    statement.lines.extend(next.lines);
                    statement.period_end = next.period_end;
                } else {
                    spans.push(0);
                }
                paths.push(path);
                continue;
            }
            // Exports are cut by trade date, so a late 12/31 trade booked on the
            // next business day can share the next file's first book date.
            anyhow::ensure!(
                first.book_date >= last.book_date,
                "{account_no} {currency}: {} overlaps the export before it and disagrees with it \
                 (it starts {}, the other ends {})",
                path.display(),
                first.book_date,
                last.book_date
            );
            anyhow::ensure!(
                !held.is_empty() || next.opening_balance() == statement.closing_balance(),
                "{account_no} {currency}: gap between {} and {} — balance {} on {} but {} before \
                 {}; an export is missing",
                paths.last().expect("non-empty").display(),
                path.display(),
                statement.closing_balance(),
                last.book_date,
                next.opening_balance(),
                first.book_date
            );
            anyhow::ensure!(
                next.opening_balance() == statement.closing_balance(),
                "{account_no} {currency}: {} and {} both hold {} and disagree about it",
                paths.last().expect("non-empty").display(),
                path.display(),
                first.book_date
            );
            spans.push(next.lines.len());
            statement.lines.extend(next.lines);
            statement.period_end = next.period_end;
            paths.push(path);
        }
        merged.push(Merged { statement, paths, spans });
    }
    Ok(merged)
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

    fn with_balance(mut l: StatementLine, balance: Decimal) -> StatementLine {
        l.balance = balance;
        l
    }

    #[test]
    fn dedup_refs_are_content_based() {
        let s = statement(vec![with_balance(line(29, "電子轉出", dec!(500), ""), dec!(1000))]);
        assert_eq!(s.dedup_refs(), vec!["cathay-bank:123456789012:2026-06-29:-500:1000"]);
    }

    /// Out, back, out again on one day: the first and third lines match on
    /// date, amount and running balance, so only the suffix tells them
    /// apart.
    #[test]
    fn dedup_refs_disambiguate_out_and_back() {
        let s = statement(vec![
            with_balance(line(29, "網銀轉帳", dec!(500), ""), dec!(500)),
            with_balance(line(29, "網銀轉帳", dec!(-500), ""), dec!(1000)),
            with_balance(line(29, "自行提款", dec!(500), ""), dec!(500)),
        ]);
        assert_eq!(s.dedup_refs(), vec![
            "cathay-bank:123456789012:2026-06-29:-500:500",
            "cathay-bank:123456789012:2026-06-29:500:1000",
            "cathay-bank:123456789012:2026-06-29:-500:500:2",
        ]);
    }

    /// Other days in the file don't shift a day's keys, so re-splitting the
    /// statements by year leaves them unchanged.
    #[test]
    fn dedup_refs_ignore_other_days() {
        let day = || {
            vec![
                with_balance(line(29, "網銀轉帳", dec!(500), ""), dec!(500)),
                with_balance(line(29, "自行提款", dec!(500), ""), dec!(500)),
            ]
        };
        let alone = statement(day()).dedup_refs();
        let mut lines = vec![with_balance(line(28, "網銀轉帳", dec!(500), ""), dec!(500))];
        lines.extend(day());
        let with_earlier_day = statement(lines).dedup_refs();
        assert_eq!(with_earlier_day[1..], alone[..]);
    }

    fn write(dir: &tempfile::TempDir, name: &str, body: &str) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, body).expect("write statement");
        path
    }

    const FOREIGN: &str = "\
\"123456789012 活存外幣\"
\"幣別：USD\"
\"交易日期\",\"帳務日期\",\"提出\",\"存入\",\"餘額\",\"成交匯率\",\"交易資訊\"
\"2024/06/11\",\"2024/06/11\",\"USD 1,000.50\",\"−\",\"USD 0.00\",\"−\",\"網銀轉\"
\"2024/06/07\",\"2024/06/07\",\"−\",\"USD 1,000.00\",\"USD 1,000.50\",\"32.1\",\"台幣存 \
                           999999999999TWD\"
\"提出\",\"USD 1,000.50 ( 共 1 筆 )\"
";

    #[test]
    fn a_foreign_currency_statement_loads_in_its_own_currency() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let s = load(write(&dir, "fx.csv", FOREIGN)).expect("loads");
        assert_eq!(s.currency, Currency::USD);
        assert_eq!(s.lines.len(), 2);
        assert_eq!(s.opening_balance(), dec!(0.50));
        assert_eq!(s.lines[1].delta(), dec!(-1000.50));
        assert_eq!(s.lines[0].info, "台幣存 999999999999TWD");
        assert!(s.lines[0].description.is_empty() && s.lines[0].memo.is_empty());
    }

    #[test]
    fn an_amount_in_another_currency_is_rejected() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let err = load(write(
            &dir,
            "fx.csv",
            &FOREIGN.replace("USD 1,000.00\",\"USD 1,000.50", "JPY 1,000.00\",\"USD 1,000.50"),
        ))
        .expect_err("a JPY amount on a USD statement");
        assert!(format!("{err:#}").contains("not in the statement currency"), "{err:#}");
    }

    fn year(y: u32, opening: u32, deposit: u32) -> String {
        format!(
            "123456789012 \
             活存\n幣別：TWD\n交易日期,帳務日期,說明,提出,存入,餘額,交易資訊,備註\n{y}/03/01,{y}/\
             03/01,存入,,{deposit},{},,\n",
            opening + deposit
        )
    }

    #[test]
    fn yearly_exports_merge_per_account_and_currency() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let paths = [
            write(&dir, "2024.csv", &year(2024, 100, 50)),
            write(&dir, "2023.csv", &year(2023, 0, 100)),
            write(&dir, "fx.csv", FOREIGN),
        ];
        let merged = load_merged(&paths).expect("merges");
        assert_eq!(merged.len(), 2);
        let twd = merged.iter().find(|m| m.statement.currency == Currency::TWD).expect("TWD");
        assert_eq!(twd.paths, [paths[1].clone(), paths[0].clone()]);
        assert_eq!(twd.spans, [1, 1]);
        assert_eq!(twd.statement.lines.len(), 2);
        assert_eq!(twd.statement.opening_balance(), dec!(0));
        assert_eq!(twd.statement.closing_balance(), dec!(150));
    }

    #[test]
    fn a_missing_export_is_a_gap_not_a_silent_jump() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let paths = [
            write(&dir, "2023.csv", &year(2023, 0, 100)),
            write(&dir, "2025.csv", &year(2025, 400, 50)),
        ];
        let err = load_merged(&paths).err().expect("a gap must fail");
        assert!(err.to_string().contains("an export is missing"), "{err}");
    }

    #[test]
    fn overlapping_exports_are_rejected() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let paths = [
            write(
                &dir,
                "a.csv",
                &export(None, &[("2023/03/01", 100, 100), ("2023/06/01", 50, 150)]),
            ),
            write(&dir, "b.csv", &export(None, &[("2023/04/01", 0, 150)])),
        ];
        assert!(load_merged(&paths).is_err());
    }

    /// A late 12/31 trade booked on the next business day ends one export on
    /// the date the next one starts; that is not an overlap.
    #[test]
    fn exports_may_share_a_boundary_book_date() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        // Given newest first, so argument order can't be what joins them.
        let paths = [
            write(
                &dir,
                "2024.csv",
                &export(None, &[("2024/01/02", 50, 150), ("2024/03/01", 5, 155)]),
            ),
            write(&dir, "2023.csv", &export(None, &[("2024/01/02", 100, 100)])),
        ];
        let merged = load_merged(&paths).expect("merges");
        assert_eq!(merged[0].statement.closing_balance(), dec!(155));

        // Both only on the boundary date: the export period decides.
        let paths = [
            write(&dir, "b.csv", &export(Some("2024/12/31"), &[("2024/01/02", 50, 150)])),
            write(&dir, "a.csv", &export(Some("2023/12/31"), &[("2024/01/02", 100, 100)])),
        ];
        let merged = load_merged(&paths).expect("merges");
        assert_eq!(merged[0].statement.closing_balance(), dec!(150));
    }

    /// An export of deposits, given oldest first as (book date, deposit,
    /// balance).
    fn export(period_end: Option<&str>, lines: &[(&str, u32, u32)]) -> String {
        let period = period_end.map(|end| format!("筆數,(自 2023/01/01 至 {end})\n"));
        let mut out = format!(
            "123456789012 活存\n{}幣別：TWD\n交易日期,帳務日期,說明,提出,存入,餘額,交易資訊,備註\n",
            period.unwrap_or_default()
        );
        for (date, deposit, balance) in lines.iter().rev() {
            out.push_str(&format!("{date},{date},存入,,{deposit},{balance},,\n"));
        }
        out
    }

    #[test]
    fn trimming_folds_earlier_lines_into_the_opening() {
        let mut s = statement(vec![
            with_balance(line(1, "存入", dec!(-100), ""), dec!(100)),
            with_balance(line(3, "存入", dec!(-20), ""), dec!(120)),
        ]);
        let dropped = s.trim_before(NaiveDate::from_ymd_opt(2026, 6, 2).expect("date"));
        assert_eq!(dropped, 1);
        assert_eq!(s.opening_balance(), dec!(100));
    }

    #[test]
    fn info_names_an_account_by_its_digits() {
        // Masked incoming rows, zero padding, and another digit run first.
        assert!(info_names_account("(013)0000123***789012", "123456789012"));
        assert!(info_names_account("(822)000999 0123456789012", "123456789012"));
        assert!(!info_names_account("(013)0000123456789013", "123456789012"));
        // An all-zero account number names nothing.
        assert!(!info_names_account("000000", "000"));
    }

    #[test]
    fn a_dash_cell_reads_as_empty() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let s = load(write(
            &dir,
            "savings.csv",
            "123456789012 \
             活存\n幣別：TWD\n交易日期,帳務日期,說明,提出,存入,餘額,交易資訊,備註\n2024/06/01,\
             2024/06/01,存入,,100,100,−,−\n",
        ))
        .expect("loads");
        assert!(s.lines[0].info.is_empty() && s.lines[0].memo.is_empty());
    }

    #[test]
    fn a_statement_with_no_rows_is_rejected() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let err = load(write(
            &dir,
            "empty.csv",
            "123456789012 活存\n幣別：TWD\n交易日期,帳務日期,說明,提出,存入,餘額,交易資訊,備註\n",
        ))
        .expect_err("a statement with no rows");
        assert!(format!("{err:#}").contains("no statement rows"), "{err:#}");
    }

    /// Like `export`, with signed amounts: (book date, delta, balance).
    fn signed(period_end: Option<&str>, lines: &[(&str, i64, i64)]) -> String {
        let period = period_end.map(|end| format!("筆數,(自 2023/01/01 至 {end})\n"));
        let mut out = format!(
            "123456789012 活存\n{}幣別：TWD\n交易日期,帳務日期,說明,提出,存入,餘額,交易資訊,備註\n",
            period.unwrap_or_default()
        );
        for (date, delta, balance) in lines.iter().rev() {
            let (out_, in_) = match delta.is_negative() {
                true => (delta.abs().to_string(), String::new()),
                false => (String::new(), delta.to_string()),
            };
            out.push_str(&format!("{date},{date},轉帳,{out_},{in_},{balance},,\n"));
        }
        out
    }

    /// A download that stopped partway through a day, then the one that
    /// re-covers the day, given together: the day is taken once, from the
    /// later one. The partial one ends back on the day's opening balance, so
    /// balance continuity alone would have joined them and counted twice.
    #[test]
    fn a_partial_download_and_its_replacement_merge_once() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let day = [("2024/03/05", -10, 90), ("2024/03/05", 10, 100)];
        let partial = [&[("2024/03/01", 100, 100)][..], &day].concat();
        let full = [&day[..], &[("2024/03/05", -30, 70), ("2024/04/01", 1, 71)]].concat();
        let paths = [
            write(&dir, "full.csv", &signed(Some("2024/12/31"), &full)),
            write(&dir, "partial.csv", &signed(None, &partial)),
        ];
        let merged = load_merged(&paths).expect("merges");
        let m = &merged[0];
        assert_eq!(m.statement.lines.len(), 5);
        assert_eq!(m.statement.closing_balance(), dec!(71));
        assert_eq!(m.paths, [paths[1].clone(), paths[0].clone()]);
        assert_eq!(m.spans, [1, 4]);
    }

    /// A full-year re-download supersedes the partial one it started with; a
    /// download that another already holds entirely adds nothing.
    #[test]
    fn a_redownload_keeps_the_longer_of_the_two() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let lines = [("2024/03/01", 100, 100), ("2024/03/05", -10, 90), ("2024/04/01", 1, 91)];
        let paths = [
            write(&dir, "partial.csv", &signed(None, &lines[..1])),
            write(&dir, "full.csv", &signed(Some("2024/12/31"), &lines)),
            write(&dir, "inner.csv", &signed(None, &lines[1..2])),
        ];
        let merged = load_merged(&paths).expect("merges");
        let m = &merged[0];
        assert_eq!(m.statement.lines.len(), 3);
        assert_eq!(m.statement.period_end, NaiveDate::from_ymd_opt(2024, 12, 31));
        assert_eq!(m.spans.iter().sum::<usize>(), 3);
        assert_eq!(m.spans, [0, 3, 0]);
    }

    #[test]
    fn an_overlap_that_disagrees_is_rejected() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let paths = [
            write(
                &dir,
                "a.csv",
                &signed(None, &[("2024/03/01", 100, 100), ("2024/03/05", -10, 90)]),
            ),
            write(&dir, "b.csv", &signed(None, &[("2024/03/05", -20, 80)])),
        ];
        let err = load_merged(&paths).err().expect("a disagreeing overlap");
        assert!(err.to_string().contains("disagree"), "{err}");
    }

    fn day(d: u32) -> NaiveDate { NaiveDate::from_ymd_opt(2024, 3, d).expect("date") }

    fn statement_of(period_end: Option<&str>, lines: &[(&str, i64, i64)]) -> BankStatement {
        let dir = tempfile::TempDir::new().expect("temp dir");
        load(write(&dir, "s.csv", &signed(period_end, lines))).expect("loads")
    }

    /// A TWD download may have been made partway through the end of its range,
    /// so the closing is the day before's.
    #[test]
    fn the_closing_is_the_day_before_the_download_could_have_been_made() {
        let lines = [("2024/03/01", 100, 100), ("2024/03/05", 10, 110), ("2024/03/05", 5, 115)];
        let before = day(1).pred_opt().unwrap();
        let closes = |period_end: Option<&str>| {
            let a = statement_of(period_end, &lines).assertion("A", before).unwrap();
            assert_eq!((a.period_start, a.opening), (Some(day(1)), Some(dec!(0))));
            (a.period_end, a.closing)
        };
        // Made during 3/05.
        assert_eq!(closes(Some("2024/03/05")), (day(4), dec!(100)));
        // 外幣: no range, the last balance stands.
        assert_eq!(closes(None), (day(5), dec!(115)));
        // Made at noon on 3/06, nothing posted yet that day.
        assert_eq!(closes(Some("2024/03/06")), (day(5), dec!(115)));
        let year_end = NaiveDate::from_ymd_opt(2024, 12, 30).unwrap();
        assert_eq!(closes(Some("2024/12/31")), (year_end, dec!(115)));
    }

    /// Only one tracked day, not settled: all that is vouched for is the
    /// balance it started from.
    #[test]
    fn a_single_unsettled_day_asserts_only_its_opening() {
        let lines = [("2024/03/05", 10, 110), ("2024/03/05", 5, 115)];
        let s = statement_of(Some("2024/03/05"), &lines);
        let a = s.assertion("A", day(1)).unwrap();
        assert_eq!((a.period_start, a.opening), (None, None));
        assert_eq!((a.period_end, a.closing), (day(4), dec!(100)));
        assert!(s.assertion("A", day(5)).is_none());
    }
}
