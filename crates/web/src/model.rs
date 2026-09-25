//! What the server sends a page: display-ready figures. The one ledger type
//! it shares with the wasm client is `Currency`, from `ledger-types`.

use std::{fmt, str::FromStr};

use ledger_types::Currency;
use leptos_router::params::ParamsMap;
use rust_decimal::{Decimal, RoundingStrategy};
use serde::{Deserialize, Serialize};

/// An amount as the page shows it: already rounded, so the client only
/// formats it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Money {
    pub amount: Decimal,
    pub currency: Currency,
}

impl Money {
    /// The base currency is quoted whole, as Fava prints TWD; the rest to two.
    /// Half away from zero, as Fava rounds an account's own balance. A total
    /// is rounded once, from the exact sum; Fava instead adds up figures it has
    /// already rounded, so a group of halves can differ from it by one unit.
    pub fn new(amount: Decimal, currency: Currency, base: Currency) -> Self {
        let decimals = if currency == base { 0 } else { 2 };
        let mut amount =
            amount.round_dp_with_strategy(decimals, RoundingStrategy::MidpointAwayFromZero);
        // Padded too, so 12.5 USD prints as 12.50.
        amount.rescale(decimals);
        Money { amount, currency }
    }

    /// Dust that rounded to zero is not negative.
    pub fn is_negative(&self) -> bool { self.amount < Decimal::ZERO }
}

impl fmt::Display for Money {
    /// Grouped thousands, as Fava prints them.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = self.amount.abs().to_string();
        let (int, frac) = match text.split_once('.') {
            Some((int, frac)) => (int, Some(frac)),
            None => (text.as_str(), None),
        };
        if self.is_negative() {
            f.write_str("-")?;
        }
        for (i, digit) in int.chars().enumerate() {
            if i > 0 && (int.len() - i) % 3 == 0 {
                f.write_str(",")?;
            }
            write!(f, "{digit}")?;
        }
        if let Some(frac) = frac {
            write!(f, ".{frac}")?;
        }
        write!(f, " {}", self.currency)
    }
}

/// Amounts left out of a converted total for want of a rate. Renders as
/// nothing when there are none, so a caller can always print it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Unpriced(pub Vec<Money>);

impl fmt::Display for Unpriced {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0.split_first() {
            None => Ok(()),
            Some((first, rest)) => {
                write!(f, "（未換算：{first}")?;
                for money in rest {
                    write!(f, "、{money}")?;
                }
                f.write_str("）")
            }
        }
    }
}

/// A sum in the base currency. An amount with no rate is named rather than
/// converted at 1:1.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Converted {
    pub money: Money,
    pub unpriced: Unpriced,
}

/// One account in the tree, summing its own postings and its descendants'.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    /// Identifies the node (fold state); never shown.
    pub path: String,
    pub label: String,
    pub total: Converted,
    /// The account's own balance per currency, without its descendants',
    /// when it holds any currency other than the base: the figure its
    /// statement shows.
    pub native: Vec<Money>,
    pub children: Vec<Node>,
}

/// 資產, 負債, or the holdings carried at cost.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Section {
    /// The root path, identifying the section's fold state; never shown.
    pub path: String,
    pub label: String,
    pub total: Converted,
    pub nodes: Vec<Node>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BalanceSheet {
    pub as_of: String,
    pub base: Currency,
    pub sections: Vec<Section>,
    pub net_worth: Converted,
    /// 以成本衡量之投資: what was paid for holdings with no market price, one
    /// row per holding, outside every total. `None` when there are none.
    pub at_cost: Option<Section>,
}

/// Which review state the journal lists.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Review {
    #[default]
    Any,
    Reviewed,
    Unreviewed,
}

impl fmt::Display for Review {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Review::Any => "",
            Review::Reviewed => "reviewed",
            Review::Unreviewed => "unreviewed",
        })
    }
}

impl FromStr for Review {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "" => Ok(Review::Any),
            "reviewed" => Ok(Review::Reviewed),
            "unreviewed" => Ok(Review::Unreviewed),
            other => Err(format!("不明的確認狀態：{other}")),
        }
    }
}

/// Which pipeline a transaction came from: the `transactions.source` words.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Source {
    /// A bank statement.
    Import,
    /// A 天天記帳 record.
    Tiantian,
    /// Entered by hand.
    Manual,
}

impl Source {
    pub const ALL: [Source; 3] = [Source::Import, Source::Tiantian, Source::Manual];
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Source::Import => "import",
            Source::Tiantian => "tiantian",
            Source::Manual => "manual",
        })
    }
}

impl FromStr for Source {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Source::ALL
            .into_iter()
            .find(|source| source.to_string() == s)
            .ok_or_else(|| format!("不明的來源：{s}"))
    }
}

/// How a machine chose a leg's account; `None` on a leg where none did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Origin {
    /// From the 天天記帳 record the line was matched to.
    Tiantian,
    /// A mapping.toml description rule.
    Rule,
    /// No rule matched: the uncategorised account.
    Fallback,
    /// A person chose it.
    Manual,
}

/// The journal filter as a URL carries it. Every value stays as typed, so the
/// client needs no date type and the server can refuse a bad one by name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JournalQuery {
    pub account: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub text: Option<String>,
    /// A [`Review`], as text.
    pub review: Option<String>,
    pub unverified: bool,
    /// A [`Source`], as text.
    pub source: Option<String>,
    /// From 1.
    pub page: u32,
}

impl Default for JournalQuery {
    fn default() -> Self {
        JournalQuery {
            account: None,
            from: None,
            to: None,
            text: None,
            review: None,
            unverified: false,
            source: None,
            page: 1,
        }
    }
}

impl JournalQuery {
    pub fn account(path: &str) -> Self {
        JournalQuery { account: Some(path.to_string()), ..Default::default() }
    }

    pub fn with_review(&self, review: Review) -> Self {
        JournalQuery { review: Some(review.to_string()).filter(|r| !r.is_empty()), ..self.clone() }
    }

    pub fn with_page(&self, page: u32) -> Self { JournalQuery { page, ..self.clone() } }
}

/// Blank form fields are no filter.
impl From<&ParamsMap> for JournalQuery {
    fn from(params: &ParamsMap) -> Self {
        let get =
            |key: &str| params.get(key).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        JournalQuery {
            account: get("account"),
            from: get("from"),
            to: get("to"),
            text: get("q"),
            review: get("review"),
            unverified: get("unverified").is_some(),
            source: get("source"),
            page: get("page").and_then(|p| p.parse().ok()).unwrap_or(1),
        }
    }
}

/// The query string, `?` included; empty when nothing is filtered.
impl fmt::Display for JournalQuery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let page = self.page.to_string();
        let fields = [
            ("account", self.account.as_deref()),
            ("from", self.from.as_deref()),
            ("to", self.to.as_deref()),
            ("q", self.text.as_deref()),
            ("review", self.review.as_deref()),
            ("unverified", self.unverified.then_some("1")),
            ("source", self.source.as_deref()),
            ("page", (self.page > 1).then_some(page.as_str())),
        ];
        let mut query = form_urlencoded::Serializer::new(String::new());
        for (key, value) in fields {
            if let Some(value) = value {
                query.append_pair(key, value);
            }
        }
        match query.finish() {
            query if query.is_empty() => Ok(()),
            query => write!(f, "?{query}"),
        }
    }
}

/// The four roots a person files under; equity is the loader's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AccountKind {
    Asset,
    Liability,
    Income,
    Expense,
}

/// An account the journal can be filtered to, or a leg posted to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountChoice {
    pub path: String,
    /// Standalone, so it reads without the tree around it.
    pub label: String,
    pub depth: usize,
    pub kind: AccountKind,
    pub closed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Leg {
    /// The posting's id: what an edit names it by.
    pub id: i64,
    /// Links the leg to its account's journal; never shown.
    pub path: String,
    pub label: String,
    pub money: Money,
    /// The amount as stored, for the editor to start from.
    pub exact: Decimal,
    /// On the account the journal is filtered to, or below it.
    pub focus: bool,
    pub origin: Option<Origin>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub id: i64,
    pub date: String,
    pub payee: Option<String>,
    pub narration: Option<String>,
    pub source: Source,
    pub reviewed: bool,
    /// Booked after the account's last statement: nothing has checked it.
    pub unverified: bool,
    pub legs: Vec<Leg>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Journal {
    pub query: JournalQuery,
    /// The filtered account's label, when there is one.
    pub account: Option<String>,
    pub accounts: Vec<AccountChoice>,
    pub entries: Vec<Entry>,
    pub total: u64,
    pub pages: u32,
}

/// A statement line the importer would not pair alone, and the unverified
/// records it fits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Choice {
    pub id: i64,
    pub date: String,
    pub account: String,
    pub money: Money,
    pub description: String,
    pub candidates: Vec<Entry>,
    /// The record picked, waiting for the next import.
    pub chosen: Option<i64>,
}

/// 待確認: the journal held to unreviewed records, and the lines waiting to
/// be paired.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Queue {
    pub journal: Journal,
    pub choices: Vec<Choice>,
}

/// One change the app recorded on a transaction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Change {
    pub at: String,
    /// The `transaction_events.kind` word.
    pub kind: String,
}

/// A transaction as the editor opens it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Editing {
    pub entry: Entry,
    /// Where a leg may post: open accounts, and each leg's own.
    pub accounts: Vec<AccountChoice>,
    pub history: Vec<Change>,
}

/// One leg as the editor's form sends it. Text, so the server can refuse a
/// bad amount by name; a row with no account is a blank one.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LegInput {
    /// The posting's id; blank for a leg the edit adds.
    #[serde(default)]
    pub posting: String,
    #[serde(default)]
    pub account: String,
    #[serde(default)]
    pub amount: String,
    #[serde(default)]
    pub currency: String,
}

/// 記一筆 as its form sends it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ManualInput {
    pub date: String,
    pub amount: String,
    pub currency: String,
    pub account: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub counter: String,
    #[serde(default)]
    pub note: String,
}

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;

    use super::*;

    fn money(amount: Decimal, currency: Currency) -> Money {
        Money::new(amount, currency, Currency::TWD)
    }

    #[test]
    fn amounts_print_like_fava() {
        assert_eq!(money(dec!(1234567.891), Currency::USD).to_string(), "1,234,567.89 USD");
        assert_eq!(money(dec!(12.50), Currency::USD).to_string(), "12.50 USD");
        assert_eq!(money(dec!(-1000), Currency::TWD).to_string(), "-1,000 TWD");
        // The base currency is quoted whole; the ledger keeps its cents.
        assert_eq!(money(dec!(295.26), Currency::TWD).to_string(), "295 TWD");
        assert_eq!(money(dec!(838.5), Currency::TWD).to_string(), "839 TWD");
        assert_eq!(money(dec!(-14670.5), Currency::TWD).to_string(), "-14,671 TWD");
        // Rounding to nothing is not a negative amount.
        let dust = money(dec!(-0.001), Currency::TWD);
        assert_eq!(dust.to_string(), "0 TWD");
        assert!(!dust.is_negative());
    }

    #[test]
    fn unpriced_amounts_render_only_when_there_are_some() {
        assert_eq!(Unpriced::default().to_string(), "");
        let unpriced =
            Unpriced(vec![money(dec!(5000), Currency::JPY), money(dec!(-2), Currency::VND)]);
        assert_eq!(unpriced.to_string(), "（未換算：5,000.00 JPY、-2.00 VND）");
    }
}
