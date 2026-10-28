//! What the server sends a page: display-ready figures. The one ledger type
//! it shares with the wasm client is `Currency`, from `ledger-types`.

use std::{fmt, str::FromStr};

use ledger_types::{assertion::AssertionSource, Currency};
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
        if self.is_negative() {
            f.write_str("-")?;
        }
        write!(f, "{} {}", Grouped(&self.amount.abs().to_string()), self.currency)
    }
}

/// An unsigned decimal's text with its whole part grouped in thousands:
/// 1234567.89 reads 1,234,567.89.
pub struct Grouped<'a>(pub &'a str);

impl fmt::Display for Grouped<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (int, frac) = self.0.split_once('.').map_or((self.0, None), |(i, f)| (i, Some(f)));
        for (i, digit) in int.chars().enumerate() {
            if i > 0 && (int.len() - i) % 3 == 0 {
                f.write_str(",")?;
            }
            write!(f, "{digit}")?;
        }
        match frac {
            Some(frac) => write!(f, ".{frac}"),
            None => Ok(()),
        }
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

/// A sum with nothing left unconverted.
impl From<Money> for Converted {
    fn from(money: Money) -> Self { Converted { money, unpriced: Unpriced::default() } }
}

impl Converted {
    /// Every amount in it had a rate, so `money` is the whole sum.
    pub fn is_complete(&self) -> bool { self.unpriced.0.is_empty() }
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
    /// Neither it nor any account under it holds anything: shown only when
    /// asked for.
    pub zero: bool,
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

    /// Another account, every other filter kept, back at the first page.
    pub fn with_account(&self, account: Option<&str>) -> Self {
        JournalQuery { account: account.map(str::to_string), page: 1, ..self.clone() }
    }
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

/// The kinds an account may belong to, as the balance sheet names them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AccountKind {
    Asset,
    Liability,
    Income,
    Expense,
}

impl fmt::Display for AccountKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            AccountKind::Asset => "資產",
            AccountKind::Liability => "負債",
            AccountKind::Income => "收入",
            AccountKind::Expense => "支出",
        })
    }
}

/// An account the journal can be filtered to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountChoice {
    pub path: String,
    /// Its ancestors' labels from the root down, then its own.
    pub trail: Vec<String>,
}

/// The whole trail, so that typing any ancestor narrows the list and two
/// accounts sharing a label still tell themselves apart.
impl fmt::Display for AccountChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut trail = self.trail.iter();
        if let Some(first) = trail.next() {
            f.write_str(first)?;
        }
        for name in trail {
            write!(f, " › {name}")?;
        }
        Ok(())
    }
}

/// Whether the picker offers an account named `name` for what a reader typed.
/// The panel and the submitted form both go through this, so a name the panel
/// lists is a name submitting finds.
pub fn offered_for(name: &str, typed: &str) -> bool {
    name.to_lowercase().contains(&typed.trim().to_lowercase())
}

/// One account in the tree the picker browses: its own label, since the row
/// above gives the context, and whatever files under it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountNode {
    pub path: String,
    pub label: String,
    /// Open on arrival: the filtered account's ancestors, so a reader lands
    /// looking at where they are.
    pub open: bool,
    pub children: Vec<AccountNode>,
}

/// An account as the editor's pickers list it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChartAccount {
    pub path: String,
    /// Standalone, so it reads without the tree around it.
    pub label: String,
    pub depth: usize,
    /// None for equity, which holds the loader's system accounts.
    pub kind: Option<AccountKind>,
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
    /// The same account under the name the picker offers it by.
    pub chosen: Option<String>,
    pub accounts: Vec<AccountChoice>,
    /// The accounts a name the reader typed fits, when it fits more than one:
    /// they are listed together, and this offers the way down to one of them.
    pub among: Vec<AccountChoice>,
    /// The same accounts to browse a level at a time.
    pub tree: Vec<AccountNode>,
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
    pub accounts: Vec<ChartAccount>,
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

/// 總覽 as a URL carries it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ChartQuery {
    /// What is broken down by category: a month by its first day, a year,
    /// or [`ChartQuery::WHOLE`]; none for the newest month with any flow.
    pub period: Option<String>,
}

impl ChartQuery {
    /// The whole ledger broken down at once.
    pub const WHOLE: &'static str = "all";

    pub fn period(period: &str) -> Self { ChartQuery { period: Some(period.to_string()) } }
}

/// A blank field asks for nothing.
impl From<&ParamsMap> for ChartQuery {
    fn from(params: &ParamsMap) -> Self {
        ChartQuery {
            period: params.get("period").map(|v| v.trim().to_string()).filter(|v| !v.is_empty()),
        }
    }
}

/// The query string, `?` included; empty when nothing is asked.
impl fmt::Display for ChartQuery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.period {
            Some(period) => {
                let mut query = form_urlencoded::Serializer::new(String::new());
                write!(f, "?{}", query.append_pair("period", period).finish())
            }
            None => Ok(()),
        }
    }
}

/// One line, or one column per period: a figure for each.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Series {
    pub name: String,
    pub figures: Vec<Converted>,
}

/// Figures over time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Trend {
    /// Each period's name, in date order.
    pub periods: Vec<String>,
    pub series: Vec<Series>,
    /// Read out beside the drawn series, not drawn.
    pub noted: Vec<Series>,
    /// What the readout shows while no period is picked; the newest period
    /// when none.
    pub summary: Option<Summary>,
}

/// The whole ledger in one readout: a balance's newest figure, a flow's sum.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    pub name: String,
    /// One per series, then one per noted series.
    pub figures: Vec<Converted>,
}

/// One account's part of a flow, its sub-accounts' included.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Slice {
    pub path: String,
    pub label: String,
    pub total: Converted,
    /// Its part of the whole, 0 to 1; none when either is not a whole sum.
    pub part: Option<Decimal>,
}

/// A flow's whole and its parts, largest first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pie {
    pub label: String,
    pub total: Converted,
    pub slices: Vec<Slice>,
}

/// One period's income and expense by category.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Breakdown {
    /// What it covers: 2026 年 9 月, 2026 年, 全部期間.
    pub name: String,
    pub income: Pie,
    pub expense: Pie,
    /// The years to pick, 全部 first, and the periods of the year shown.
    pub years: Vec<PeriodChoice>,
    pub periods: Vec<PeriodChoice>,
    /// Which of 收支's periods it covers, to mark them there.
    pub marked: Vec<usize>,
}

/// The figures 總覽 leads with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Headline {
    pub as_of: String,
    pub tiles: Vec<Tile>,
}

/// One headline figure, and a second one beside it, named.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tile {
    pub label: String,
    pub figure: Converted,
    pub beside: Option<(String, Converted)>,
}

/// Something the breakdown can be set to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PeriodChoice {
    /// A [`ChartQuery::period`] value.
    pub key: String,
    pub label: String,
    pub selected: bool,
}

/// 總覽: where things stand, net worth over time, day-to-day income against
/// expense per month with the net result of investments apart, and one
/// period's flows by category.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Charts {
    pub headline: Headline,
    pub worth: Trend,
    pub flows: Trend,
    /// Each of `flows`' months by its first day, to pick it by.
    pub starts: Vec<String>,
    /// None when nothing `[investments]` lists has moved.
    pub investments: Option<Trend>,
    pub breakdown: Breakdown,
    /// What holdings carried at cost stand at today, outside net worth;
    /// none when there are none.
    pub at_cost: Option<Converted>,
}

/// Who vouches for a balance check's figure, as the page names them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Voucher(pub AssertionSource);

impl fmt::Display for Voucher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self.0 {
            AssertionSource::Statement => "對帳單",
            AssertionSource::Tiantian => "天天記帳",
            AssertionSource::Counted => "盤點",
        })
    }
}

/// Where an account's own page is.
pub fn account_href(path: &str) -> String { format!("/account{}", JournalQuery::account(path)) }

/// One figure an outside source states for the account, against the books.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BalanceCheck {
    pub date: String,
    pub voucher: Voucher,
    /// The sub-account the figure is for, when not the page's own.
    pub account: Option<String>,
    pub expected: Money,
    pub computed: Money,
    /// The books less the figure, unrounded.
    pub difference: Money,
    /// Decided on the exact figures: rounding for display could hide a
    /// difference of cents.
    pub ok: bool,
}

/// One account's page: where it stands, how it got there, and what vouches
/// for it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountPage {
    pub path: String,
    pub label: String,
    /// Its place in the chart, from the root down.
    pub trail: String,
    /// The balance now, per currency, as its statements show it.
    pub balance: Vec<Money>,
    pub total: Converted,
    /// Income and expense show what moved each month; the rest, the balance.
    pub flow: bool,
    /// One per currency: amounts in different currencies share no axis.
    pub trends: Vec<Trend>,
    /// Newest first.
    pub checks: Vec<BalanceCheck>,
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
