use std::fmt::Write;

use chrono::NaiveDate;
use ledger_types::currency::Currency;
use rust_decimal::Decimal;

/// A single posting leg. `amount == None` lets Beancount infer the value, which
/// is how the contra-leg of a two-posting transaction is normally written.
#[derive(Debug, Clone)]
pub struct Posting {
    pub account: String,
    pub amount: Option<Decimal>,
    pub currency: Currency,
    /// `@@ total currency` — the total the other leg is worth, needed when the
    /// two legs are in different currencies. Stated as a total rather than a
    /// per-unit rate because both totals are known exactly, and a rounded rate
    /// multiplied back out does not balance.
    pub price: Option<(Decimal, Currency)>,
}

impl Posting {
    pub fn new(account: impl Into<String>, amount: Decimal, currency: Currency) -> Self {
        Posting { account: account.into(), amount: Some(amount), currency, price: None }
    }

    pub fn worth(mut self, total: Decimal, currency: Currency) -> Self {
        self.price = Some((total, currency));
        self
    }

    /// The currency is unused — an inferred leg prints only its account — so it
    /// takes the default rather than threading one through.
    pub fn inferred(account: impl Into<String>) -> Self {
        Posting {
            account: account.into(),
            amount: None,
            currency: Currency::default(),
            price: None,
        }
    }
}

/// Beancount strings are double-quoted and cannot span lines.
fn quote(s: &str) -> String {
    let cleaned: String =
        s.chars().map(|c| if c == '\n' || c == '\r' || c == '\t' { ' ' } else { c }).collect();
    format!("\"{}\"", cleaned.replace('\\', "\\\\").replace('"', "\\\"").trim())
}

/// Beancount tags are `[A-Za-z0-9._-]`; anything else is dropped so a category
/// name can be turned into a tag without hand-maintaining a second list.
pub fn tag_name(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .collect::<String>()
        .to_lowercase()
}

pub fn transaction(
    date: NaiveDate,
    payee: &str,
    narration: &str,
    tags: &[String],
    postings: &[Posting],
) -> String {
    let mut out = String::new();
    let suffix: String =
        tags.iter().filter(|t| !t.is_empty()).map(|t| format!(" #{}", t)).collect();
    if payee.is_empty() {
        writeln!(out, "{} * {}{}", date, quote(narration), suffix).unwrap();
    } else {
        writeln!(out, "{} * {} {}{}", date, quote(payee), quote(narration), suffix).unwrap();
    }
    for p in postings {
        match p.amount {
            Some(a) => {
                write!(out, "  {:<38} {} {}", p.account, a, p.currency).unwrap();
                if let Some((total, currency)) = &p.price {
                    write!(out, " @@ {} {}", total, currency).unwrap();
                }
                out.push('\n');
            }
            None => writeln!(out, "  {}", p.account).unwrap(),
        }
    }
    out
}

pub fn balance(date: NaiveDate, account: &str, amount: Decimal, currency: Currency) -> String {
    format!("{} balance {:<38} {} {}\n", date, account, amount, currency)
}
