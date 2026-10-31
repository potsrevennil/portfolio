//! The page's data, as the server sends it.

use std::fmt;

use ledger_types::{
    cadence::{Cadence, Standing},
    currency::Currency,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Coverage {
    pub today: String,
    /// The first of each month back to the oldest record, for the axis.
    pub months: Vec<Tick>,
    /// In the chart's order, instalments last.
    pub accounts: Vec<AccountRow>,
}

/// A day, and how many days before today it is: what places it on a bar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mark {
    pub date: String,
    pub ago: i64,
}

/// How many days back the earliest bar begins.
pub(super) fn oldest(rows: &[AccountRow]) -> i64 {
    rows.iter().filter_map(|r| r.first.as_ref()).map(|m| m.ago).max().unwrap_or(0)
}

/// A month's start on the axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tick {
    pub year: i32,
    pub month: u32,
    pub ago: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountRow {
    pub account: String,
    pub label: String,
    pub bank: String,
    /// The kind of account it is (銀行, 信用卡, 現金…): what the page groups
    /// by.
    pub group: String,
    pub cadence: Cadence,
    /// Days a figure may run late before it counts as overdue.
    pub grace: u32,
    pub status: Standing,
    /// The statements to fetch, while any are due.
    pub wanted: Option<String>,
    /// Days since the first missed statement day, once late.
    pub late_days: Option<i64>,
    /// The last day an outside figure vouches for, across every currency
    /// the account still holds.
    pub checked: Option<Mark>,
    /// Each currency's own day, where they differ.
    pub per_commodity: Vec<String>,
    /// The newest record.
    pub recorded: Option<Mark>,
    /// Where the bar starts.
    pub first: Option<Mark>,
    /// Records no figure has checked yet.
    pub tail: Option<Tail>,
    /// Days no statement covers, across which the balance moved.
    pub gaps: Vec<GapSpan>,
    /// Before it the bank keeps no history, so nothing there can be checked.
    pub since: Option<Mark>,
    /// What it holds now, per currency: what a manual check confirms.
    pub balances: Vec<Holding>,
    /// Payments made and left, where it is paid off in instalments.
    pub instalment: Option<Instalment>,
    /// Who owes whom and since when, where it is a split account.
    pub split: Option<SplitOpen>,
    /// Where the ledger disagrees with its outside figure.
    pub gap: Option<String>,
}

/// An account's cadence, as the page names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CadenceName(pub Cadence);

impl fmt::Display for CadenceName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Cadence::Never => f.write_str("無固定數單"),
            Cadence::Monthly { day: 31 } => f.write_str("逐月數單"),
            Cadence::Monthly { day } => write!(f, "逐月數單（{day} 號）"),
            Cadence::Quarterly => f.write_str("逐季數單"),
            Cadence::Yearly { month: 12, day: 31 } => f.write_str("逐年數單"),
            Cadence::Yearly { month, day } => write!(f, "逐年數單（{month}/{day}）"),
            Cadence::Count { every } => write!(f, "每{every}工對一擺"),
            Cadence::Instalment => f.write_str("分期付款"),
            Cadence::Payment { day: 31 } => f.write_str("逐月還款"),
            Cadence::Payment { day } => write!(f, "逐月還款（{day} 號）"),
        }
    }
}

impl AccountRow {
    /// Holds nothing in any currency: hidden unless asked for.
    pub fn zero(&self) -> bool { self.balances.is_empty() }

    /// A statement checks it otherwise, and nothing is left to confirm once
    /// checked through its last record.
    pub fn confirmable(&self) -> bool {
        self.instalment.is_none()
            && !self.cadence.by_statement()
            && (self.tail.is_some() || self.checked.is_none())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Instalment {
    pub paid: u32,
    pub total: u32,
    pub first: String,
    /// The last payment, expected at the latest payment's size.
    pub last: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SplitOpen {
    /// Positive: they owe you. Empty when settled.
    pub owed: Vec<crate::model::Money>,
    /// The first record since it last stood at zero.
    pub since: Option<Mark>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Holding {
    pub currency: Currency,
    /// Exact, as the ledger sums it.
    pub amount: String,
}

/// Days no statement covers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GapSpan {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tail {
    pub count: usize,
    /// The journal from its first day, where the records are postings.
    pub link: Option<String>,
}
