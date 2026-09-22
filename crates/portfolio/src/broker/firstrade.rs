//! Firstrade monthly statements (Apex Clearing PDFs), read through
//! `pdftotext -layout`.
//!
//! Settled activity is listed by settlement date; a trade executed near month
//! end shows first under "pending settlement" with both dates, and settles in
//! the next statement. Activity names a security, not its ticker, so tickers
//! come from the holdings pages of the statements read together.

use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
    process::Command,
    sync::LazyLock,
};

use anyhow::{bail, ensure, Context, Result};
use chrono::NaiveDate;
use ledger_types::currency::Currency;
use regex::Regex;
use rust_decimal::Decimal;

use super::record::{number_repeats, BrokerRecord, BrokerStatement, Holdings, RecordKind};
use crate::portfolio::Broker;

pub const REF_PREFIX: &str = "firstrade:";

/// The text of a statement PDF.
pub fn pdf_text(path: &Path) -> Result<String> {
    let out = Command::new("pdftotext")
        .arg("-layout")
        .arg(path)
        .arg("-")
        .output()
        .context("running pdftotext (poppler)")?;
    ensure!(out.status.success(), "pdftotext failed on {}", path.display());
    String::from_utf8(out.stdout).with_context(|| format!("{} is not UTF-8 text", path.display()))
}

/// Parses statements read together: tickers found on any statement's holdings
/// pages (or in `names`) resolve activity everywhere, and a pending trade's
/// execution date fills in the line that settles it next month.
pub fn parse_all(
    texts: &[String],
    names: &HashMap<String, String>,
) -> Result<Vec<BrokerStatement>> {
    let parsed = texts.iter().map(|t| parse(t)).collect::<Result<Vec<_>>>()?;

    let mut tickers: HashMap<String, String> = names.clone();
    for p in &parsed {
        tickers.extend(p.tickers.iter().map(|(n, s)| (n.clone(), s.clone())));
    }
    let pending: HashMap<(NaiveDate, String, Decimal, Decimal), NaiveDate> = parsed
        .iter()
        .flat_map(|p| &p.pending)
        .map(|l| ((l.settle_date, l.name.clone(), l.quantity.abs(), l.price), l.trade_date))
        .collect();

    let mut statements = Vec::with_capacity(parsed.len());
    for p in parsed {
        let mut records = Vec::with_capacity(p.lines.len());
        for line in p.lines {
            let symbol = match &line.name {
                Some(name) => Some(tickers.get(name).cloned().with_context(|| {
                    format!(
                        "no ticker for {name:?} (settled {}); add it under [symbols] in \
                         securities.toml",
                        line.record.settle_date
                    )
                })?),
                None => None,
            };
            let mut r = line.record;
            if let Some(name) = &line.name {
                let key = (r.settle_date, name.clone(), r.quantity.abs(), r.price);
                if let (RecordKind::Buy | RecordKind::Sell, Some(&traded)) =
                    (r.kind, pending.get(&key))
                {
                    r.trade_date = Some(traded);
                }
            }
            r.symbol = symbol;
            records.push(r);
        }
        number_repeats(&mut records);
        statements.push(BrokerStatement { records, ..p.statement });
    }
    Ok(statements)
}

struct Parsed {
    statement: BrokerStatement,
    lines: Vec<Line>,
    pending: Vec<Pending>,
    tickers: HashMap<String, String>,
}

/// A record whose ticker is still to be resolved from `name`.
struct Line {
    name: Option<String>,
    record: BrokerRecord,
}

struct Pending {
    trade_date: NaiveDate,
    settle_date: NaiveDate,
    name: String,
    quantity: Decimal,
    price: Decimal,
}

static PERIOD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"([A-Z][a-z]+ \d{1,2}, \d{4}) - ([A-Z][a-z]+ \d{1,2}, \d{4})").unwrap()
});
static ACCOUNT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"ACCOUNT NUMBER\s+(\S+)").unwrap());
static HOLDING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^\s*(?:[A-Z]\s+)?\*?\s*(?P<name>\S.*?)\s{2,}(?P<symbol>[A-Z][A-Z0-9.]*)\s+(?P<type>[A-Z])\s+(?P<qty>[\d,]*\.?\d+)\s+\$?[\d,]*\.?\d+\s+\$?[\d,]*\.?\d+",
    )
    .unwrap()
});
static ACTIVITY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^\s*(?:[A-Z]\s+)?(?P<tx>BOUGHT|SOLD|REINVEST|DIVIDEND|INTEREST|WIRE|FEE|TRANSFER|TFO|TFI|JOURNAL|CSH)\s+(?P<date>\d\d/\d\d/\d\d)(?:\s+(?P<settle>\d\d/\d\d/\d\d))?\s+(?P<type>[A-Z])\s+(?P<rest>\S.*)$",
    )
    .unwrap()
});
static TOTAL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*(?:[A-Z]\s+)?Total ").unwrap());

fn parse(text: &str) -> Result<Parsed> {
    let caps = PERIOD.captures(text).context("no statement period")?;
    let date = |s: &str| NaiveDate::parse_from_str(s, "%B %d, %Y").context("statement period");
    let (period_start, period_end) = (date(&caps[1])?, date(&caps[2])?);
    let account = ACCOUNT.captures(text).context("no account number")?[1].to_string();
    let summary = |label: &str| -> Result<(Decimal, Decimal)> {
        let line = text
            .lines()
            .find(|l| l.trim_start().starts_with(label))
            .with_context(|| format!("no {label} line"))?;
        match numbers(line).as_slice() {
            [.., (open, _), (close, _)] => Ok((*open, *close)),
            _ => bail!("{label} line without an opening and closing: {line:?}"),
        }
    };
    let (open_cash, close_cash) = summary("NET ACCOUNT BALANCE")?;
    let (open_securities, _) = summary("TOTAL PRICED PORTFOLIO")?;

    let lines: Vec<&str> = text.lines().collect();
    let (positions, tickers) = holdings(&lines)?;
    let (activity, pending) = activity(&lines)?;

    let cash = |q| BTreeMap::from([(Currency::USD, q)]);
    let statement = BrokerStatement {
        broker: Broker::Firstrade,
        account,
        period_start,
        period_end,
        generated: None,
        // A statement opening with securities doesn't list them; the previous
        // statement's closing does.
        opening: open_securities
            .is_zero()
            .then(|| Holdings { cash: cash(open_cash), positions: BTreeMap::new() }),
        closing: Some(Holdings { cash: cash(close_cash), positions }),
        records: Vec::new(),
    };
    Ok(Parsed { statement, lines: activity, pending, tickers })
}

/// Month-end positions summed over the account's types, and name → ticker.
fn holdings(lines: &[&str]) -> Result<(BTreeMap<String, Decimal>, HashMap<String, String>)> {
    let start = lines.iter().position(|l| l.contains("SYMBOL/")).map(|i| i + 1);
    let mut positions: BTreeMap<String, Decimal> = BTreeMap::new();
    let mut tickers = HashMap::new();
    let Some(start) = start else { return Ok((positions, tickers)) };
    for line in &lines[start..] {
        if line.contains("TOTAL PRICED PORTFOLIO") {
            break;
        }
        let Some(c) = HOLDING.captures(line) else { continue };
        let quantity: Decimal = c["qty"].replace(',', "").parse()?;
        *positions.entry(c["symbol"].to_string()).or_default() += quantity;
        tickers.insert(c["name"].trim().to_string(), c["symbol"].to_string());
    }
    Ok((positions, tickers))
}

/// The numbers among a line's whitespace-separated words, each with the
/// column just past its last character.
fn numbers(line: &str) -> Vec<(Decimal, usize)> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, ch) in line.char_indices().chain([(line.len(), ' ')]) {
        match (ch.is_whitespace(), start) {
            (false, None) => start = Some(i),
            (true, Some(s)) => {
                if let Some(n) = amount(&line[s..i]) {
                    out.push((n, i));
                }
                start = None;
            }
            _ => {}
        }
    }
    out
}

/// `$1,234.50`, `-2`, `0.00778`; not dates, rates or codes.
fn amount(word: &str) -> Option<Decimal> {
    let digits = word.trim_start_matches('-').trim_start_matches('$');
    let numeric = !digits.is_empty()
        && digits.chars().all(|c| c.is_ascii_digit() || c == ',' || c == '.')
        && digits.chars().any(|c| c.is_ascii_digit());
    if numeric {
        word.replace(['$', ','], "").parse().ok()
    } else {
        None
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Side {
    Debit,
    Credit,
}

/// Which column a printed amount sits under, going by the page's header.
fn side(end: usize, header: (usize, usize)) -> Side {
    let (debit, credit) = header;
    if end.abs_diff(debit) <= end.abs_diff(credit) {
        Side::Debit
    } else {
        Side::Credit
    }
}

fn activity(lines: &[&str]) -> Result<(Vec<Line>, Vec<Pending>)> {
    let mut out = Vec::new();
    let mut pending = Vec::new();
    let mut header = None;
    let mut section: (Decimal, Decimal) = Default::default();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        i += 1;
        if let (Some(d), Some(c)) = (line.find("DEBIT"), line.find("CREDIT")) {
            header = Some((d + "DEBIT".len(), c + "CREDIT".len()));
            continue;
        }
        if TOTAL.is_match(line) && header.is_some() {
            check_total(line, section)?;
            section = Default::default();
            continue;
        }
        let Some(c) = ACTIVITY.captures(line) else { continue };
        let header = header.context("activity before any column header")?;
        let continuation: Vec<&str> = lines[i..]
            .iter()
            .take_while(|l| !ACTIVITY.is_match(l) && !TOTAL.is_match(l))
            .copied()
            .collect();
        let rest = c.name("rest").expect("rest");
        let name = description(rest.as_str());
        // Amounts follow the description; its own words may hold digits.
        let offset = rest.start() + rest.as_str().find("  ").unwrap_or(rest.len());
        let nums: Vec<(Decimal, Side)> = numbers(&line[offset..])
            .into_iter()
            .map(|(n, end)| (n, side(offset + end, header)))
            .collect();
        let date = mdy(&c["date"])?;

        if let Some(settle) = c.name("settle") {
            let (quantity, price, net) = match nums.as_slice() {
                [(q, _), (p, _), (net, _)] => (*q, *p, *net),
                _ => bail!("pending trade without quantity, price and amount: {line:?}"),
            };
            match &c["tx"] {
                "BOUGHT" => section.0 += net,
                _ => section.1 += net,
            }
            pending.push(Pending {
                trade_date: date,
                settle_date: mdy(settle.as_str())?,
                name,
                quantity,
                price,
            });
            continue;
        }

        let parsed = lines_for(&c["tx"], &c["type"], date, &name, &nums, &continuation)
            .with_context(|| format!("Firstrade line {line:?}"))?;
        for (l, side) in parsed {
            let cash = l.record.cash().abs();
            match side {
                Some(Side::Debit) => section.0 += cash,
                Some(Side::Credit) => section.1 += cash,
                None => {}
            }
            out.push(l);
        }
    }
    Ok((out, pending))
}

/// A section's printed totals must equal its lines: one figure is a debit or
/// a credit total, two are both.
fn check_total(line: &str, (debit, credit): (Decimal, Decimal)) -> Result<()> {
    let printed: Vec<Decimal> = numbers(line).into_iter().map(|(n, _)| n).collect();
    let ok = match printed.as_slice() {
        [] => debit.is_zero() && credit.is_zero(),
        [one] => (*one == debit && credit.is_zero()) || (*one == credit && debit.is_zero()),
        [d, c] => (*d, *c) == (debit, credit),
        _ => false,
    };
    ensure!(ok, "{:?} does not match its lines (debits {debit}, credits {credit})", line.trim());
    Ok(())
}

/// The first description column: text up to the first wide gap.
fn description(rest: &str) -> String {
    rest.split("  ").next().unwrap_or_default().trim().to_string()
}

fn mdy(s: &str) -> Result<NaiveDate> {
    NaiveDate::parse_from_str(s, "%m/%d/%y").with_context(|| format!("date {s:?}"))
}

/// The records one activity line stands for, each with the side it counts
/// toward in its section's total.
fn lines_for(
    tx: &str,
    account_type: &str,
    date: NaiveDate,
    name: &str,
    nums: &[(Decimal, Side)],
    continuation: &[&str],
) -> Result<Vec<(Line, Option<Side>)>> {
    let find = |needle: &str| continuation.iter().find_map(|l| l.split_once(needle).map(|x| x.1));
    let cusip = find("CUSIP:").map(|s| s.trim().to_string());
    let base = BrokerRecord {
        kind: RecordKind::Internal,
        trade_date: None,
        settle_date: date,
        executed_at: None,
        symbol: None,
        quantity: Decimal::ZERO,
        price: Decimal::ZERO,
        amount: Decimal::ZERO,
        commission: Decimal::ZERO,
        currency: Currency::USD,
        description: name.to_string(),
        key: format!("{date}:{tx}:{account_type}:{}", cusip.as_deref().unwrap_or(name)),
    };
    let security = |kind, quantity: Decimal, price, amount, commission| Line {
        name: Some(name.to_string()),
        record: BrokerRecord {
            kind,
            quantity,
            price,
            amount,
            commission,
            key: format!("{}:{quantity}:{amount}", base.key),
            ..base.clone()
        },
    };
    let cash = |kind, amount: Decimal, name: Option<&str>| Line {
        name: name.map(str::to_string),
        record: BrokerRecord {
            kind,
            amount,
            key: format!("{}:{amount}", base.key),
            ..base.clone()
        },
    };
    let signed = |(n, side): (Decimal, Side)| match side {
        Side::Debit => -n,
        Side::Credit => n,
    };
    let one = || match nums {
        [.., last] => Ok(*last),
        [] => bail!("no amount"),
    };

    let out = match (tx, nums) {
        ("BOUGHT", [(q, _), (p, _), (net, _)]) => {
            vec![(security(RecordKind::Buy, *q, *p, -*net, Decimal::ZERO), Some(Side::Debit))]
        }
        // Proceeds are printed net of the SEC fee; the fee is what the price
        // leaves over.
        ("SOLD", [(q, _), (p, _), (net, _)]) => {
            let gross = (q * p).round_dp(2);
            vec![(security(RecordKind::Sell, -*q, *p, gross, *net - gross), Some(Side::Credit))]
        }
        ("REINVEST", [(q, _), (net, _)]) => {
            let price = find("REIN @")
                .and_then(|s| s.split_whitespace().next()?.parse().ok())
                .context("reinvestment without its REIN @ price")?;
            let mut l = security(RecordKind::Buy, *q, price, -*net, Decimal::ZERO);
            // Reinvested on the pay date, which is the date printed.
            l.record.trade_date = Some(date);
            l.record.description = format!("{name} (dividend reinvestment)");
            vec![(l, Some(Side::Debit))]
        }
        ("DIVIDEND", [.., (gross, _)]) => {
            let mut out =
                vec![(cash(RecordKind::Dividend, *gross, Some(name)), Some(Side::Credit))];
            if let Some(wh) = find(" WH ") {
                let wh: Decimal = wh.trim().replace(',', "").parse().context("WH amount")?;
                let mut l = cash(RecordKind::Withholding, -wh, Some(name));
                l.record.key = format!("{}:WH:{}", base.key, -wh);
                l.record.description = format!("{name} (non-resident tax withheld)");
                out.push((l, Some(Side::Debit)));
            }
            out
        }
        ("INTEREST", _) => {
            // Printed columns drift on these lines, so the wording decides.
            let (n, _) = one()?;
            let side = if name.contains("DEBIT BALANCE") { Side::Debit } else { Side::Credit };
            vec![(cash(RecordKind::Interest, signed((n, side)), None), Some(side))]
        }
        ("WIRE" | "TRANSFER", _) => {
            let (n, side) = one()?;
            let kind = match side {
                Side::Credit => RecordKind::Deposit,
                Side::Debit => RecordKind::Withdrawal,
            };
            vec![(cash(kind, signed((n, side)), None), Some(side))]
        }
        ("FEE", _) => {
            let (n, side) = one()?;
            vec![(cash(RecordKind::Fee, signed((n, side)), None), Some(side))]
        }
        ("CSH", _) => {
            let (n, side) = one()?;
            vec![(cash(RecordKind::Internal, signed((n, side)), None), Some(side))]
        }
        ("TFO" | "TFI", [(q, _)]) => {
            let kind = if tx == "TFO" { RecordKind::TransferOut } else { RecordKind::TransferIn };
            let mut l = security(kind, *q, Decimal::ZERO, Decimal::ZERO, Decimal::ZERO);
            l.record.description = format!("{name} ({})", find("TRANSFER").unwrap_or("").trim());
            vec![(l, None)]
        }
        ("JOURNAL", _) if find("ADR Fee").is_some() => {
            let (n, side) = one()?;
            let mut l = cash(RecordKind::Fee, signed((n, side)), Some(name));
            l.record.description = format!("{name} (ADR fee)");
            vec![(l, Some(side))]
        }
        ("JOURNAL", [(q, _)]) if find("TYPE").is_some() => {
            let mut l =
                security(RecordKind::Internal, *q, Decimal::ZERO, Decimal::ZERO, Decimal::ZERO);
            l.record.description = format!("{name} ({})", find("TYPE").unwrap_or("").trim());
            vec![(l, None)]
        }
        _ => bail!("unrecognised {tx} line with amounts {nums:?}"),
    };
    Ok(out)
}
